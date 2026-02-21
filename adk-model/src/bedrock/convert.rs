//! Type conversions between ADK and Amazon Bedrock Converse API types.
//!
//! This module handles mapping between ADK's `LlmRequest`/`LlmResponse` types
//! and the Bedrock Converse API format used by `aws-sdk-bedrockruntime`.

use adk_core::{Content, FinishReason, GenerateContentConfig, LlmResponse, Part, UsageMetadata};
use aws_sdk_bedrockruntime::types::{
    self as bedrock, ContentBlock, ContentBlockDelta, ContentBlockStart, ConversationRole,
    ConverseOutput, InferenceConfiguration, Message, StopReason, SystemContentBlock, Tool,
    ToolConfiguration, ToolInputSchema, ToolResultBlock, ToolResultContentBlock, ToolSpecification,
    ToolUseBlock,
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
pub(crate) fn adk_request_to_bedrock(
    contents: &[Content],
    tools: &HashMap<String, Value>,
    config: Option<&GenerateContentConfig>,
) -> Result<BedrockConverseInput, String> {
    let mut messages = Vec::new();
    let mut system = Vec::new();

    for content in contents {
        match content.role.as_str() {
            "system" => {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        if !text.is_empty() {
                            system.push(SystemContentBlock::Text(text.clone()));
                        }
                    }
                }
            }
            role => {
                let bedrock_role = match role {
                    "user" | "function" | "tool" => ConversationRole::User,
                    "model" | "assistant" => ConversationRole::Assistant,
                    _ => ConversationRole::User,
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

    let inference_config = config.map(adk_config_to_bedrock);
    let tool_config = if tools.is_empty() { None } else { Some(adk_tools_to_bedrock(tools)) };

    Ok(BedrockConverseInput { messages, system, inference_config, tool_config })
}

/// Convert ADK `Part` list to Bedrock `ContentBlock` list.
fn adk_parts_to_bedrock(parts: &[Part]) -> Vec<ContentBlock> {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => {
                if text.is_empty() {
                    None
                } else {
                    Some(ContentBlock::Text(text.clone()))
                }
            }
            Part::FunctionCall { name, args, id } => {
                let tool_use = ToolUseBlock::builder()
                    .tool_use_id(id.clone().unwrap_or_else(|| format!("call_{name}")))
                    .name(name.clone())
                    .input(json_value_to_document(args))
                    .build()
                    .ok()?;
                Some(ContentBlock::ToolUse(tool_use))
            }
            Part::FunctionResponse { function_response, id } => {
                let tool_result = ToolResultBlock::builder()
                    .tool_use_id(id.clone().unwrap_or_else(|| "unknown".to_string()))
                    .content(ToolResultContentBlock::Text(
                        serde_json::to_string(&function_response.response).unwrap_or_default(),
                    ))
                    .build()
                    .ok()?;
                Some(ContentBlock::ToolResult(tool_result))
            }
            // InlineData and FileData are not directly supported by Bedrock Converse text API;
            // skip them for now (image/document support can be added later).
            Part::InlineData { .. } | Part::FileData { .. } => None,
        })
        .collect()
}

/// Convert ADK tool declarations to Bedrock `ToolConfiguration`.
///
/// Each tool in the ADK format is a `(name, JSON schema)` pair. The schema
/// typically contains `description` and `parameters` fields.
fn adk_tools_to_bedrock(tools: &HashMap<String, Value>) -> ToolConfiguration {
    let bedrock_tools: Vec<Tool> = tools
        .iter()
        .filter_map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);

            let input_schema = decl.get("parameters").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));

            let mut spec_builder = ToolSpecification::builder()
                .name(name.clone())
                .input_schema(ToolInputSchema::Json(json_value_to_document(&input_schema)));

            if let Some(desc) = description {
                spec_builder = spec_builder.description(desc);
            }

            spec_builder.build().ok().map(Tool::ToolSpec)
        })
        .collect();

    // ToolConfiguration requires at least one tool; caller ensures tools is non-empty.
    ToolConfiguration::builder().set_tools(Some(bedrock_tools)).build().unwrap_or_else(|_| {
        // Fallback: empty tool config (should not happen since we check tools.is_empty())
        ToolConfiguration::builder().build().expect("empty tool config")
    })
}

/// Convert `GenerateContentConfig` to Bedrock `InferenceConfiguration`.
fn adk_config_to_bedrock(config: &GenerateContentConfig) -> InferenceConfiguration {
    let mut builder = InferenceConfiguration::builder();

    if let Some(temp) = config.temperature {
        builder = builder.temperature(temp);
    }
    if let Some(top_p) = config.top_p {
        builder = builder.top_p(top_p);
    }
    if let Some(max_tokens) = config.max_output_tokens {
        builder = builder.max_tokens(max_tokens);
    }

    builder.build()
}

/// Convert a Bedrock Converse non-streaming response to an ADK `LlmResponse`.
///
/// Extracts the message content, stop reason, and token usage from the
/// Bedrock `ConverseOutput`.
pub(crate) fn bedrock_response_to_adk(
    output: &ConverseOutput,
    stop_reason: &StopReason,
    usage: Option<&bedrock::TokenUsage>,
) -> LlmResponse {
    let content = match output {
        ConverseOutput::Message(message) => {
            let parts = bedrock_content_blocks_to_parts(&message.content);
            if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) }
        }
        _ => None,
    };

    let finish_reason = Some(bedrock_stop_reason_to_adk(stop_reason));

    let usage_metadata = usage.map(|u| UsageMetadata {
        prompt_token_count: u.input_tokens,
        candidates_token_count: u.output_tokens,
        total_token_count: u.total_tokens,
    });

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Convert Bedrock `ContentBlock` list to ADK `Part` list.
fn bedrock_content_blocks_to_parts(blocks: &[ContentBlock]) -> Vec<Part> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => {
                if text.is_empty() {
                    None
                } else {
                    Some(Part::Text { text: text.clone() })
                }
            }
            ContentBlock::ToolUse(tool_use) => Some(Part::FunctionCall {
                name: tool_use.name.clone(),
                args: document_to_json_value(&tool_use.input),
                id: Some(tool_use.tool_use_id.clone()),
            }),
            _ => None,
        })
        .collect()
}

/// Map Bedrock `StopReason` to ADK `FinishReason`.
fn bedrock_stop_reason_to_adk(stop_reason: &StopReason) -> FinishReason {
    match stop_reason {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::ToolUse => FinishReason::Stop,
        StopReason::StopSequence => FinishReason::Stop,
        StopReason::ContentFiltered => FinishReason::Safety,
        StopReason::GuardrailIntervened => FinishReason::Safety,
        _ => FinishReason::Other,
    }
}

// --- Streaming conversion helpers ---

/// Convert a streaming `ContentBlockStart` event to an ADK `LlmResponse`.
///
/// This handles the start of a tool use block, which provides the tool name and ID.
/// Text blocks don't have a start event with content.
pub(crate) fn bedrock_stream_content_start_to_adk(
    start: &ContentBlockStart,
) -> Option<LlmResponse> {
    match start {
        ContentBlockStart::ToolUse(tool_start) => {
            // Return a partial response with the tool call metadata.
            // The actual arguments will come in subsequent delta events.
            Some(LlmResponse {
                content: Some(Content {
                    role: "model".to_string(),
                    parts: vec![Part::FunctionCall {
                        name: tool_start.name.clone(),
                        args: Value::Null,
                        id: Some(tool_start.tool_use_id.clone()),
                    }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: true,
                turn_complete: false,
                interrupted: false,
                error_code: None,
                error_message: None,
            })
        }
        _ => None,
    }
}

/// Convert a streaming `ContentBlockDelta` event to an ADK `LlmResponse`.
///
/// Handles text deltas and tool use input deltas.
pub(crate) fn bedrock_stream_delta_to_adk(delta: &ContentBlockDelta) -> Option<LlmResponse> {
    match delta {
        ContentBlockDelta::Text(text) => {
            if text.is_empty() {
                None
            } else {
                Some(LlmResponse {
                    content: Some(Content {
                        role: "model".to_string(),
                        parts: vec![Part::Text { text: text.clone() }],
                    }),
                    usage_metadata: None,
                    finish_reason: None,
                    citation_metadata: None,
                    partial: true,
                    turn_complete: false,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                })
            }
        }
        ContentBlockDelta::ToolUse(tool_delta) => {
            // Tool use deltas contain partial JSON argument strings.
            // We emit them as partial text so the client can accumulate.
            if tool_delta.input.is_empty() {
                None
            } else {
                Some(LlmResponse {
                    content: Some(Content {
                        role: "model".to_string(),
                        parts: vec![Part::Text { text: tool_delta.input.clone() }],
                    }),
                    usage_metadata: None,
                    finish_reason: None,
                    citation_metadata: None,
                    partial: true,
                    turn_complete: false,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                })
            }
        }
        _ => None,
    }
}

/// Convert a streaming `MessageStop` event to an ADK `LlmResponse`.
pub(crate) fn bedrock_stream_stop_to_adk(stop_reason: &StopReason) -> LlmResponse {
    LlmResponse {
        content: None,
        usage_metadata: None,
        finish_reason: Some(bedrock_stop_reason_to_adk(stop_reason)),
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

// --- JSON Value ↔ AWS Document conversion ---

/// Convert a `serde_json::Value` to an `aws_smithy_types::Document`.
///
/// This is needed because Bedrock's SDK uses `Document` for JSON-like values
/// (tool inputs, tool schemas) rather than `serde_json::Value`.
pub(crate) fn json_value_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(aws_smithy_types::Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(aws_smithy_types::Number::NegInt(i))
            } else if let Some(f) = n.as_f64() {
                Document::Number(aws_smithy_types::Number::Float(f))
            } else {
                Document::Null
            }
        }
        Value::String(s) => Document::String(s.clone()),
        Value::Array(arr) => Document::Array(arr.iter().map(json_value_to_document).collect()),
        Value::Object(obj) => Document::Object(
            obj.iter().map(|(k, v)| (k.clone(), json_value_to_document(v))).collect(),
        ),
    }
}

/// Convert an `aws_smithy_types::Document` to a `serde_json::Value`.
///
/// This is the inverse of [`json_value_to_document`], used when converting
/// Bedrock responses (e.g., tool call inputs) back to ADK types.
pub(crate) fn document_to_json_value(doc: &Document) -> Value {
    match doc {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(n) => match *n {
            aws_smithy_types::Number::PosInt(u) => Value::Number(serde_json::Number::from(u)),
            aws_smithy_types::Number::NegInt(i) => Value::Number(serde_json::Number::from(i)),
            aws_smithy_types::Number::Float(f) => {
                serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
            }
        },
        Document::String(s) => Value::String(s.clone()),
        Document::Array(arr) => Value::Array(arr.iter().map(document_to_json_value).collect()),
        Document::Object(obj) => {
            Value::Object(obj.iter().map(|(k, v)| (k.clone(), document_to_json_value(v))).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::FunctionResponseData;

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
            Content {
                role: "system".to_string(),
                parts: vec![Part::Text { text: "You are helpful.".to_string() }],
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
        ];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None).unwrap();
        assert_eq!(result.system.len(), 1);
        assert_eq!(result.messages.len(), 1);
    }

    #[test]
    fn test_role_mapping() {
        let contents = vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hi".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
            Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text { text: "How can I help?".to_string() }],
            },
        ];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None).unwrap();
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].role, ConversationRole::User);
        assert_eq!(result.messages[1].role, ConversationRole::Assistant);
        assert_eq!(result.messages[2].role, ConversationRole::Assistant);
    }

    #[test]
    fn test_function_call_conversion() {
        let contents = vec![Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "Seattle"}),
                id: Some("call_123".to_string()),
            }],
        }];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None).unwrap();
        assert_eq!(result.messages.len(), 1);

        let blocks = &result.messages[0].content;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse(_)));
    }

    #[test]
    fn test_function_response_conversion() {
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "get_weather".to_string(),
                    response: serde_json::json!({"temp": 72}),
                },
                id: Some("call_123".to_string()),
            }],
        }];

        let result = adk_request_to_bedrock(&contents, &HashMap::new(), None).unwrap();
        assert_eq!(result.messages.len(), 1);

        let blocks = &result.messages[0].content;
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn test_tool_config_conversion() {
        let mut tools = HashMap::new();
        tools.insert(
            "get_weather".to_string(),
            serde_json::json!({
                "description": "Get weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" }
                    }
                }
            }),
        );

        let result = adk_request_to_bedrock(&[], &tools, None).unwrap();
        assert!(result.tool_config.is_some());
        let tool_config = result.tool_config.unwrap();
        assert_eq!(tool_config.tools.len(), 1);
    }

    #[test]
    fn test_inference_config_conversion() {
        let config = GenerateContentConfig {
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: None,
            max_output_tokens: Some(1024),
            response_schema: None,
        };

        let result = adk_request_to_bedrock(&[], &HashMap::new(), Some(&config)).unwrap();
        let inf = result.inference_config.unwrap();
        assert_eq!(inf.temperature, Some(0.7));
        assert_eq!(inf.top_p, Some(0.9));
        assert_eq!(inf.max_tokens, Some(1024));
    }

    #[test]
    fn test_stop_reason_mapping() {
        assert_eq!(bedrock_stop_reason_to_adk(&StopReason::EndTurn), FinishReason::Stop);
        assert_eq!(bedrock_stop_reason_to_adk(&StopReason::MaxTokens), FinishReason::MaxTokens);
        assert_eq!(bedrock_stop_reason_to_adk(&StopReason::ToolUse), FinishReason::Stop);
        assert_eq!(bedrock_stop_reason_to_adk(&StopReason::StopSequence), FinishReason::Stop);
        assert_eq!(bedrock_stop_reason_to_adk(&StopReason::ContentFiltered), FinishReason::Safety);
        assert_eq!(
            bedrock_stop_reason_to_adk(&StopReason::GuardrailIntervened),
            FinishReason::Safety
        );
    }

    #[test]
    fn test_stream_text_delta() {
        let delta = ContentBlockDelta::Text("Hello world".to_string());
        let response = bedrock_stream_delta_to_adk(&delta).unwrap();
        assert!(response.partial);
        assert!(!response.turn_complete);
        let content = response.content.unwrap();
        let text = content.parts[0].text().unwrap();
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_stream_empty_text_delta_skipped() {
        let delta = ContentBlockDelta::Text(String::new());
        assert!(bedrock_stream_delta_to_adk(&delta).is_none());
    }

    #[test]
    fn test_stream_stop_event() {
        let response = bedrock_stream_stop_to_adk(&StopReason::EndTurn);
        assert!(!response.partial);
        assert!(response.turn_complete);
        assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn test_empty_contents_produces_no_messages() {
        let result = adk_request_to_bedrock(&[], &HashMap::new(), None).unwrap();
        assert!(result.messages.is_empty());
        assert!(result.system.is_empty());
        assert!(result.inference_config.is_none());
        assert!(result.tool_config.is_none());
    }
}
