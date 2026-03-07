//! Type conversions between ADK and Amazon Bedrock Converse API types.
//!
//! This module handles mapping between ADK's `LlmRequest`/`LlmResponse` types
//! and the Bedrock Converse API format used by `aws-sdk-bedrockruntime`.

use super::config::{BedrockCacheConfig, BedrockCacheTtl};
use adk_core::{
    Content, FinishReason, GenerateContentConfig, LlmResponse, Part, Role, UsageMetadata,
};
use aws_sdk_bedrockruntime::types::{
    self as bedrock, CachePointBlock, CachePointType, CacheTtl, ContentBlock, ContentBlockDelta,
    ContentBlockStart, ConversationRole, ConverseOutput, InferenceConfiguration, Message,
    StopReason, SystemContentBlock, Tool, ToolConfiguration, ToolInputSchema, ToolResultBlock,
    ToolResultContentBlock, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::Document;
use serde_json::Value;
use std::collections::HashMap;

/// Result of converting an `LlmRequest` into Bedrock Converse API inputs.
///
/// System messages are extracted separately since Bedrock's Converse API
/// takes them as a distinct parameter rather than inline with conversation messages.
pub(crate) struct BedrockConverseInput {
    /// Conversation messages (user and assistant turns).
    pub messages: Vec<Message>,
    /// System prompt content blocks extracted from contents with role "system".
    pub system: Vec<SystemContentBlock>,
    /// Inference configuration (temperature, top_p, max_tokens).
    pub inference_config: Option<InferenceConfiguration>,
    /// Tool configuration if tools are declared.
    pub tool_config: Option<ToolConfiguration>,
}

/// Convert an `LlmRequest` into Bedrock Converse API inputs.
///
/// Extracts system messages into separate system content blocks and maps
/// conversation messages, tools, and inference configuration.
///
/// When `prompt_caching` is provided, `CachePoint` blocks are appended after
/// system content and after tool definitions to enable Bedrock prompt caching.
pub(crate) fn adk_request_to_bedrock(
    contents: &[Content],
    tools: &HashMap<String, Value>,
    config: Option<&GenerateContentConfig>,
    prompt_caching: Option<&BedrockCacheConfig>,
) -> Result<BedrockConverseInput, String> {
    let mut messages = Vec::new();
    let mut system = Vec::new();

    for content in contents {
        match content.role {
            Role::System => {
                for part in &content.parts {
                    if let Some(text) = part.as_text() {
                        if !text.is_empty() {
                            system.push(SystemContentBlock::Text(text.to_string()));
                        }
                    }
                }
            }
            ref role => {
                let bedrock_role = if role.is_user() || role.is_tool() {
                    ConversationRole::User
                } else {
                    ConversationRole::Assistant
                };

                let blocks = adk_parts_to_bedrock(&content.parts);
                if !blocks.is_empty() {
                    let msg = Message::builder()
                        .role(bedrock_role)
                        .set_content(Some(blocks))
                        .build()
                        .map_err(|e| format!("Failed to build Bedrock message: {e}"))?;
                    messages.push(msg);
                }
            }
        }
    }

    // Inject CachePoint after system content when prompt caching is enabled.
    if let Some(cache_config) = prompt_caching {
        if !system.is_empty() {
            system.push(SystemContentBlock::CachePoint(build_cache_point_block(cache_config)));
        }
    }

    let inference_config = config.map(adk_config_to_bedrock);
    let tool_config =
        if tools.is_empty() { None } else { Some(adk_tools_to_bedrock(tools, prompt_caching)) };

    Ok(BedrockConverseInput { messages, system, inference_config, tool_config })
}

/// Build a `CachePointBlock` from the given cache configuration.
///
/// Sets the TTL to 1 hour when `BedrockCacheTtl::OneHour` is configured;
/// otherwise uses the default 5-minute TTL (no explicit TTL field).
fn build_cache_point_block(cache_config: &BedrockCacheConfig) -> CachePointBlock {
    let mut builder = CachePointBlock::builder().r#type(CachePointType::Default);
    if cache_config.ttl == BedrockCacheTtl::OneHour {
        builder = builder.ttl(CacheTtl::OneHour);
    }
    builder.build().expect("CachePointBlock builder with type set should not fail")
}

/// Convert ADK `Part` list to Bedrock `ContentBlock` list.
fn adk_parts_to_bedrock(parts: &[Part]) -> Vec<ContentBlock> {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => {
                if text.is_empty() {
                    None
                } else {
                    Some(ContentBlock::Text(text.clone()))
                }
            }
            Part::FunctionCall { name, args, id, .. } => {
                let tool_use = ToolUseBlock::builder()
                    .tool_use_id(id.clone().unwrap_or_else(|| format!("call_{name}")))
                    .name(name.clone())
                    .input(json_value_to_document(args))
                    .build()
                    .ok()?;
                Some(ContentBlock::ToolUse(tool_use))
            }
            Part::FunctionResponse { name: _, response, id } => {
                let tool_result = ToolResultBlock::builder()
                    .tool_use_id(id.clone().unwrap_or_else(|| "unknown".to_string()))
                    .content(ToolResultContentBlock::Text(
                        serde_json::to_string(&response).unwrap_or_default(),
                    ))
                    .build()
                    .ok()?;
                Some(ContentBlock::ToolResult(tool_result))
            }
            Part::Thinking { thought: thinking, .. } => {
                if thinking.is_empty() {
                    None
                } else {
                    // Bedrock Converse API doesn't accept thinking blocks in input,
                    // convert to text for conversation history
                    Some(ContentBlock::Text(thinking.clone()))
                }
            }
            // InlineData and FileData are not directly supported by Bedrock Converse text API;
            // they require the multimodal Converse API or S3 URIs.
            _ => None,
        })
        .collect()
}

/// Convert ADK `GenerateContentConfig` to Bedrock `InferenceConfiguration`.
fn adk_config_to_bedrock(config: &GenerateContentConfig) -> bedrock::InferenceConfiguration {
    let mut builder = bedrock::InferenceConfiguration::builder();
    if let Some(t) = config.temperature {
        builder = builder.temperature(t);
    }
    if let Some(p) = config.top_p {
        builder = builder.top_p(p);
    }
    if let Some(m) = config.max_output_tokens {
        builder = builder.max_tokens(m as i32);
    }
    builder.build()
}

/// Convert a map of ADK tools to Bedrock `ToolConfiguration`.
fn adk_tools_to_bedrock(
    tools: &HashMap<String, Value>,
    prompt_caching: Option<&BedrockCacheConfig>,
) -> ToolConfiguration {
    let mut specs = Vec::new();
    for (name, schema) in tools {
        let mut spec_builder = ToolSpecification::builder()
            .name(name)
            .input_schema(ToolInputSchema::Json(json_value_to_document(&schema["parameters"])));

        if let Some(desc) = schema.get("description").and_then(|v| v.as_str()) {
            spec_builder = spec_builder.description(desc);
        }

        specs.push(Tool::ToolSpec(spec_builder.build().expect("ToolSpec should be valid")));
    }

    // Inject CachePoint after tool definitions if prompt caching is enabled.
    if let Some(cache_config) = prompt_caching {
        specs.push(Tool::CachePoint(build_cache_point_block(cache_config)));
    }

    ToolConfiguration::builder()
        .set_tools(Some(specs))
        .build()
        .expect("ToolConfiguration should be valid")
}

/// Convert ADK Bedrock response to ADK `LlmResponse`.
pub(crate) fn bedrock_response_to_adk(
    output: &ConverseOutput,
    stop_reason: &StopReason,
    usage: Option<bedrock::TokenUsage>,
) -> LlmResponse {
    let mut parts = Vec::new();

    if let ConverseOutput::Message(msg) = output {
        for block in &msg.content {
            match block {
                ContentBlock::Text(text) => {
                    parts.push(Part::text(text.clone()));
                }
                ContentBlock::ToolUse(tool_use) => {
                    parts.push(Part::FunctionCall {
                        name: tool_use.name().to_string(),
                        args: document_to_json_value(tool_use.input()),
                        id: Some(tool_use.tool_use_id().to_string()),
                        thought_signature: None,
                    });
                }
                ContentBlock::ReasoningContent(reasoning) => {
                    if let bedrock::ReasoningContentBlock::ReasoningText(rt) = reasoning {
                        parts.push(Part::Thinking {
                            thought: rt.text().to_string(),
                            signature: rt.signature().map(|s| s.to_string()),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    LlmResponse {
        content: if parts.is_empty() { None } else { Some(Content { role: Role::Model, parts }) },
        finish_reason: Some(map_stop_reason(stop_reason)),
        usage_metadata: usage.map(map_usage),
        turn_complete: true,
        ..Default::default()
    }
}

/// Convert Bedrock stream delta to ADK `LlmResponse`.
pub(crate) fn bedrock_stream_delta_to_adk(delta: &ContentBlockDelta) -> Option<LlmResponse> {
    let part = match delta {
        ContentBlockDelta::Text(text) => {
            if text.is_empty() {
                return None;
            }
            Part::text(text.clone())
        }
        ContentBlockDelta::ToolUse(tool_use) => {
            // Partial tool use (arguments)
            let args = tool_use.input();
            Part::FunctionCall {
                name: String::new(), // Name is usually in the Start event
                args: serde_json::from_str(args).unwrap_or_default(),
                id: None,
                thought_signature: None,
            }
        }
        ContentBlockDelta::ReasoningContent(reasoning) => {
            if let bedrock::ReasoningContentBlockDelta::Text(text) = reasoning {
                if text.is_empty() {
                    return None;
                }
                Part::Thinking { thought: text.clone(), signature: None }
            } else {
                return None;
            }
        }
        _ => return None,
    };

    Some(LlmResponse {
        content: Some(Content { role: Role::Model, parts: vec![part] }),
        partial: true,
        ..Default::default()
    })
}

/// Convert Bedrock `ContentBlockStart` to ADK `LlmResponse`.
pub(crate) fn bedrock_stream_start_to_adk(start: &ContentBlockStart) -> Option<LlmResponse> {
    if let ContentBlockStart::ToolUse(tool_use) = start {
        Some(LlmResponse {
            content: Some(Content {
                role: Role::Model,
                parts: vec![Part::FunctionCall {
                    name: tool_use.name().to_string(),
                    args: serde_json::json!({}),
                    id: Some(tool_use.tool_use_id().to_string()),
                    thought_signature: None,
                }],
            }),
            partial: true,
            ..Default::default()
        })
    } else {
        None
    }
}

pub(crate) fn map_stop_reason(reason: &StopReason) -> FinishReason {
    match reason {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::ContentFiltered => FinishReason::Safety,
        StopReason::ToolUse => FinishReason::ToolCalls,
        _ => FinishReason::Other(format!("{reason:?}")),
    }
}

fn map_usage(usage: bedrock::TokenUsage) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: usage.input_tokens() as i32,
        candidates_token_count: usage.output_tokens() as i32,
        total_token_count: usage.total_tokens() as i32,
        ..Default::default()
    }
}

/// Convert Serde JSON Value to AWS SDK Document.
pub fn json_value_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Document::Number(aws_smithy_types::Number::NegInt(i))
            } else if let Some(u) = n.as_u64() {
                Document::Number(aws_smithy_types::Number::PosInt(u))
            } else {
                Document::Number(aws_smithy_types::Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        Value::String(s) => Document::String(s.clone()),
        Value::Array(a) => Document::Array(a.iter().map(json_value_to_document).collect()),
        Value::Object(o) => Document::Object(
            o.iter().map(|(k, v)| (k.clone(), json_value_to_document(v))).collect(),
        ),
    }
}

/// Convert AWS SDK Document to Serde JSON Value.
pub fn document_to_json_value(doc: &Document) -> Value {
    match doc {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(n) => match n {
            aws_smithy_types::Number::PosInt(u) => Value::Number((*u).into()),
            aws_smithy_types::Number::NegInt(i) => Value::Number((*i).into()),
            aws_smithy_types::Number::Float(f) => {
                serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null)
            }
        },
        Document::String(s) => Value::String(s.clone()),
        Document::Array(a) => Value::Array(a.iter().map(document_to_json_value).collect()),
        Document::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), document_to_json_value(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_value_to_document_roundtrip() {
        let value = serde_json::json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" },
                "count": 42,
                "active": true,
                "tags": ["a", "b"]
            }
        });
        let doc = json_value_to_document(&value);
        let back = document_to_json_value(&doc);
        assert_eq!(value, back);
    }

    #[test]
    fn test_json_null_roundtrip() {
        let doc = json_value_to_document(&Value::Null);
        assert_eq!(document_to_json_value(&doc), Value::Null);
    }

    #[test]
    fn test_system_message_extraction() {
        let contents = vec![
            Content { role: Role::System, parts: vec![Part::text("You are helpful.".to_string())] },
            Content { role: Role::User, parts: vec![Part::text("Hello".to_string())] },
        ];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None, None).unwrap();
        assert_eq!(result.system.len(), 1);
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn test_role_mapping() {
        let contents = vec![
            Content { role: Role::User, parts: vec![Part::text("Hi".to_string())] },
            Content { role: Role::Model, parts: vec![Part::text("Hello".to_string())] },
            Content { role: Role::Model, parts: vec![Part::text("How can I help?".to_string())] },
        ];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None, None).unwrap();
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].role, ConversationRole::User);
        assert_eq!(result.messages[1].role, ConversationRole::Assistant);
        assert_eq!(result.messages[2].role, ConversationRole::Assistant);
    }

    #[test]
    fn test_function_call_conversion() {
        let contents = vec![Content {
            role: Role::Model,
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "Seattle"}),
                id: Some("call_123".to_string()),
                thought_signature: None,
            }],
        }];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None, None).unwrap();
        assert_eq!(result.messages.len(), 1);

        let blocks = &result.messages[0].content;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn test_function_response_conversion() {
        let contents = vec![Content {
            role: Role::User,
            parts: vec![Part::FunctionResponse {
                name: "get_weather".to_string(),
                response: serde_json::json!({"temp": 72}),
                id: Some("call_123".to_string()),
            }],
        }];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None, None).unwrap();
        assert_eq!(result.messages.len(), 1);

        let blocks = &result.messages[0].content;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn test_reasoning_content_block_to_thinking_part() {
        let rt = bedrock::ReasoningTextBlock::builder()
            .text("Thinking about this...")
            .signature("sig_123")
            .build()
            .unwrap();
        let block =
            ContentBlock::ReasoningContent(bedrock::ReasoningContentBlock::ReasoningText(rt));
        let message =
            Message::builder().role(ConversationRole::Assistant).content(block).build().unwrap();
        let output = ConverseOutput::Message(message);

        let response = bedrock_response_to_adk(&output, &StopReason::EndTurn, None);
        let content = response.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            Part::Thinking { thought, signature } => {
                assert_eq!(thought, "Thinking about this...");
                assert_eq!(signature.as_deref(), Some("sig_123"));
            }
            _ => panic!("Expected Thinking part"),
        }
    }

    #[test]
    fn test_inference_config_conversion() {
        let config = adk_core::GenerateContentConfig {
            temperature: Some(0.7),
            top_p: Some(0.9),
            max_output_tokens: Some(100),
            ..Default::default()
        };

        let bedrock_cfg = adk_config_to_bedrock(&config);
        assert_eq!(bedrock_cfg.temperature, Some(0.7));
        assert_eq!(bedrock_cfg.top_p, Some(0.9));
        assert_eq!(bedrock_cfg.max_tokens, Some(100));
    }

    #[test]
    fn test_cache_point_not_injected_when_none() {
        let contents = vec![Content::user().with_text("Hi")];
        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None, None).unwrap();
        assert_eq!(result.system.len(), 0);
    }

    #[test]
    fn test_cache_point_injected_after_system_content() {
        let contents = vec![Content::new(Role::System).with_text("Be nice.")];
        let cache_cfg = BedrockCacheConfig { ttl: BedrockCacheTtl::FiveMinutes };
        let result =
            adk_request_to_bedrock(&contents, &HashMap::new(), None, Some(&cache_cfg)).unwrap();

        // system[0] = text, system[1] = cache_point
        assert_eq!(result.system.len(), 2);
        assert!(matches!(result.system[1], SystemContentBlock::CachePoint(_)));
    }

    #[test]
    fn test_cache_point_injected_after_tools() {
        let mut tools = HashMap::new();
        tools.insert("t1".to_string(), serde_json::json!({"parameters": {}}));
        let cache_cfg = BedrockCacheConfig { ttl: BedrockCacheTtl::FiveMinutes };
        let result = adk_request_to_bedrock(&[], &tools, None, Some(&cache_cfg)).unwrap();

        let tool_cfg = result.tool_config.unwrap();
        let tools = tool_cfg.tools();
        // tools[0] = spec, tools[1] = cache_point
        assert_eq!(tools.len(), 2);
        assert!(matches!(tools[1], Tool::CachePoint(_)));
    }

    #[test]
    fn test_cache_point_with_one_hour_ttl() {
        let contents = vec![Content::new(Role::System).with_text("Be nice.")];
        let cache_cfg = BedrockCacheConfig { ttl: BedrockCacheTtl::OneHour };
        let result =
            adk_request_to_bedrock(&contents, &HashMap::new(), None, Some(&cache_cfg)).unwrap();

        if let SystemContentBlock::CachePoint(cp) = &result.system[1] {
            assert_eq!(cp.ttl(), Some(&CacheTtl::OneHour));
        } else {
            panic!("Expected cache point");
        }
    }

    #[test]
    fn test_cache_point_with_five_minutes_ttl_no_explicit_ttl() {
        let contents = vec![Content::new(Role::System).with_text("Be nice.")];
        let cache_cfg = BedrockCacheConfig { ttl: BedrockCacheTtl::FiveMinutes };
        let result =
            adk_request_to_bedrock(&contents, &HashMap::new(), None, Some(&cache_cfg)).unwrap();

        if let SystemContentBlock::CachePoint(cp) = &result.system[1] {
            // Default TTL doesn't have an explicit value in the SDK's CachePointBlock
            assert!(cp.ttl().is_none());
        } else {
            panic!("Expected cache point");
        }
    }
}
