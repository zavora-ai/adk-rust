//! Type conversion utilities for Azure AI Inference API.

use adk_core::{
    Content, FinishReason, LlmResponse, Part, Role, UsageMetadata,
};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Convert ADK `LlmRequest` to Azure AI request body.
pub fn build_request_body(
    messages: &[Content],
    tools: &HashMap<String, serde_json::Value>,
    cfg: &adk_core::GenerateContentConfig,
) -> Value {
    let mut body = serde_json::json!({
        "messages": messages.iter().map(content_to_message).collect::<Vec<_>>(),
    });

    if !tools.is_empty() {
        let azure_tools: Vec<Value> = tools
            .iter()
            .map(|(name, schema)| {
                let mut function = Map::new();
                function.insert("name".to_string(), Value::String(name.clone()));
                if let Some(desc) = schema.get("description").and_then(Value::as_str) {
                    function.insert("description".to_string(), Value::String(desc.to_string()));
                }
                if let Some(params) = schema.get("parameters") {
                    function.insert("parameters".to_string(), params.clone());
                }

                serde_json::json!({
                    "type": "function",
                    "function": function,
                })
            })
            .collect();
        body["tools"] = Value::Array(azure_tools);
    }

    // Add config parameters
    // Note: adk_core::RunConfig doesn't have model_config anymore, it has GenerateContentConfig in LlmRequest.
    // However, the builder pattern might still be used. 
    // Let's assume we use the defaults or provide them if available.

    body
}

/// Convert a single ADK `Content` to an Azure AI message JSON object.
fn content_to_message(content: &Content) -> Value {
    match content.role {
        Role::User => {
            let parts = extract_content_parts(&content.parts);
            if parts.len() == 1 && matches!(parts[0], Value::Object(ref m) if m.get("type").and_then(Value::as_str) == Some("text")) {
                // If it's just one text part, we can use a simple string for content (standard OpenAI/Azure AI style)
                serde_json::json!({
                    "role": "user",
                    "content": parts[0]["text"].clone(),
                })
            } else {
                serde_json::json!({
                    "role": "user",
                    "content": Value::Array(parts),
                })
            }
        }
        Role::Model => {
            let mut msg = serde_json::json!({
                "role": "assistant",
            });

            let text = get_text_content(&content.parts);
            if let Some(t) = &text {
                msg["content"] = Value::String(t.clone());
            }

            let tool_calls = extract_tool_calls(&content.parts);
            if !tool_calls.is_empty() {
                msg["tool_calls"] = Value::Array(tool_calls);
            }

            // Azure AI requires assistant messages to have either content or tool_calls.
            if text.is_none() && !msg.get("tool_calls").is_some_and(|tc| tc.is_array()) {
                msg["content"] = Value::String(" ".to_string());
            }

            msg
        }
        Role::System => {
            let text = extract_text(&content.parts);
            serde_json::json!({
                "role": "system",
                "content": text,
            })
        }
        Role::Tool => {
            if let Some(Part::FunctionResponse { name: _, response, id }) = content.parts.first() {
                let tool_call_id = id.clone().unwrap_or_else(|| "unknown".to_string());
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": serde_json::to_string(&response).unwrap_or_default(),
                })
            } else {
                serde_json::json!({
                    "role": "tool",
                    "content": "",
                })
            }
        }
        Role::Custom(ref s) => {
            let text = extract_text(&content.parts);
            serde_json::json!({
                "role": s,
                "content": text,
            })
        }
    }
}

/// Convert ADK Parts to Azure AI content parts.
fn extract_content_parts(parts: &[Part]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text(text) => Some(serde_json::json!({
                "type": "text",
                "text": text,
            })),
            Part::Thinking { thought: thinking, .. } => Some(serde_json::json!({
                "type": "text",
                "text": format!("<thinking>{}</thinking>", thinking),
            })),
            Part::InlineData { mime_type, data } => {
                let mime_str = mime_type.as_ref();
                if mime_str.starts_with("image/") {
                    #[cfg(feature = "base64")]
                    {
                        use base64::{Engine as _, engine::general_purpose::STANDARD};
                        let encoded = STANDARD.encode(data);
                        Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", mime_str, encoded),
                            }
                        }))
                    }
                    #[cfg(not(feature = "base64"))]
                    {
                        None
                    }
                } else {
                    None
                }
            }
            Part::FileData { mime_type, file_uri } => {
                if mime_type.as_ref().starts_with("image/") {
                    Some(serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": file_uri,
                        }
                    }))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

/// Extract all text parts joined by newlines.
fn extract_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text(text) => Some(text.clone()),
            Part::Thinking { thought: thinking, .. } => Some(thinking.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Get text content if any text parts exist, returning `None` for empty text.
fn get_text_content(parts: &[Part]) -> Option<String> {
    let text = extract_text(parts);
    if text.is_empty() { None } else { Some(text) }
}

/// Extract function calls from parts as Azure AI tool_calls JSON array entries.
fn extract_tool_calls(parts: &[Part]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| {
            if let Part::FunctionCall { name, args, id, .. } = part {
                Some(serde_json::json!({
                    "id": id.clone().unwrap_or_else(|| format!("call_{name}")),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(args).unwrap_or_default(),
                    }
                }))
            } else {
                None
            }
        })
        .collect()
}

/// Parse Azure AI non-streaming response body.
pub fn parse_response(body: &Value) -> LlmResponse {
    let Some(choices) = body["choices"].as_array() else {
        return LlmResponse::default();
    };

    if choices.is_empty() {
        return LlmResponse::default();
    }

    let choice = &choices[0];
    let message = &choice["message"];
    let mut parts = Vec::new();

    // 1. Handle Reasoning (Thought) - separated field in Azure AI Inference for reasoning models
    if let Some(thinking) = message.get("reasoning_content").and_then(Value::as_str) {
        if !thinking.is_empty() {
            parts.push(Part::Thinking { thought: thinking.to_string(), signature: None });
        }
    }

    // 2. Handle Text Content
    if let Some(text) = message["content"].as_str() {
        if !text.is_empty() {
            parts.push(Part::text(text.to_string()));
        }
    }

    // 3. Handle Tool Calls
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tc in tool_calls {
            let name = tc["function"]["name"].as_str().unwrap_or_default().to_string();
            let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
            let args = serde_json::from_str(args_str).unwrap_or_default();
            let id = tc["id"].as_str().map(|s| s.to_string());

            parts.push(Part::FunctionCall { name, args, id, thought_signature: None });
        }
    }

    let finish_reason = choice["finish_reason"].as_str().map(map_finish_reason);

    let mut response = LlmResponse {
        content: if parts.is_empty() { None } else { Some(Content { role: Role::Model, parts }) },
        finish_reason,
        turn_complete: true,
        ..Default::default()
    };

    // 4. Handle Usage Metadata
    if let Some(usage) = body.get("usage") {
        let prompt = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let completion = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
        let total = usage["total_tokens"].as_u64().unwrap_or(0) as u32;

        let mut metadata = UsageMetadata {
            prompt_token_count: prompt as i32,
            candidates_token_count: completion as i32,
            total_token_count: total as i32,
            ..Default::default()
        };

        // Extract reasoning and cached tokens if available
        if let Some(details) = usage.get("completion_tokens_details") {
            if let Some(reasoning) = details.get("reasoning_tokens").and_then(Value::as_u64) {
                metadata.thinking_token_count = Some(reasoning as i32);
            }
        }
        if let Some(details) = usage.get("prompt_tokens_details") {
            if let Some(cached) = details.get("cached_tokens").and_then(Value::as_u64) {
                metadata.cache_read_input_token_count = Some(cached as i32);
            }
        }

        response.usage_metadata = Some(metadata);
    }

    response
}

/// Parse Azure AI SSE chunk.
pub fn parse_sse_chunk(chunk: &Value) -> LlmResponse {
    let Some(choices) = chunk["choices"].as_array() else {
        return LlmResponse::default();
    };

    if choices.is_empty() {
        return LlmResponse::default();
    }

    let choice = &choices[0];
    let delta = &choice["delta"];
    let mut parts = Vec::new();

    // 1. Handle Reasoning Delta
    if let Some(thinking) = delta.get("reasoning_content").and_then(Value::as_str) {
        if !thinking.is_empty() {
            parts.push(Part::Thinking { thought: thinking.to_string(), signature: None });
        }
    }

    // 2. Handle Text Delta
    if let Some(text) = delta["content"].as_str() {
        if !text.is_empty() {
            parts.push(Part::text(text.to_string()));
        }
    }

    // 3. Handle Tool Call Delta
    if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for tc in tool_calls {
            if let Some(func) = tc.get("function") {
                let name = func.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                let args_str = func.get("arguments").and_then(Value::as_str).unwrap_or_default();
                let args = if args_str.is_empty() { serde_json::json!({}) } else { serde_json::from_str(args_str).unwrap_or_default() };
                let id = tc.get("id").and_then(Value::as_str).map(|s| s.to_string());

                parts.push(Part::FunctionCall { name, args, id, thought_signature: None });
            }
        }
    }

    let finish_reason = choice.get("finish_reason").and_then(Value::as_str).map(map_finish_reason);

    LlmResponse {
        content: if parts.is_empty() { None } else { Some(Content { role: Role::Model, parts }) },
        finish_reason: finish_reason.clone(),
        partial: true,
        turn_complete: finish_reason.is_some(),
        ..Default::default()
    }
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::MaxTokens,
        "content_filter" => FinishReason::Safety,
        "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Other(reason.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_to_message_user() {
        let content = Content::user().with_text("hello");
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "user");
        assert_eq!(msg["content"], "hello");
    }

    #[test]
    fn test_content_to_message_assistant_role() {
        let content = Content::model().with_text("hi");
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "hi");
    }

    #[test]
    fn test_content_to_message_tool_response() {
        let content = Content::new(Role::Tool).with_part(Part::FunctionResponse {
            name: "test".to_string(),
            response: serde_json::json!({"result": "ok"}),
            id: Some("call_123".to_string()),
        });
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "tool");
        assert_eq!(msg["tool_call_id"], "call_123");
        assert_eq!(msg["content"], r#"{"result":"ok"}"#);
    }

    #[test]
    fn test_content_to_message_thinking_as_text() {
        let content = Content {
            role: Role::User,
            parts: vec![Part::Thinking {
                thought: "Let me reason about this...".to_string(),
                signature: None,
            }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "user");
        // extract_content_parts wraps thinking in <thinking> tags
        assert_eq!(msg["content"], "<thinking>Let me reason about this...</thinking>");
    }

    #[test]
    fn test_content_to_message_assistant_thinking_as_text() {
        let content = Content {
            role: Role::Model,
            parts: vec![
                Part::Thinking {
                    thought: "Step 1: analyze the question".to_string(),
                    signature: None,
                },
                Part::text("Here is my answer.".to_string()),
            ],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "Step 1: analyze the question\nHere is my answer.");
    }

    #[test]
    fn test_parse_response_with_reasoning_content() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Let me think step by step...",
                    "content": "The answer is 42."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });

        let resp = parse_response(&body);
        assert!(resp.turn_complete);
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 2);
        assert!(content.parts[0].is_thinking());
        assert_eq!(content.parts[0].to_text(), "Let me think step by step...");
        assert_eq!(content.parts[1].as_text().unwrap(), "The answer is 42.");
    }

    #[test]
    fn test_parse_sse_chunk_reasoning_then_text() {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "Step 1...",
                    "content": "Here's the result"
                }
            }]
        });

        let resp = parse_sse_chunk(&chunk);
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 2);
        assert!(content.parts[0].is_thinking());
        assert_eq!(content.parts[0].to_text(), "Step 1...");
        assert_eq!(content.parts[1].as_text().unwrap(), "Here's the result");
    }

    #[test]
    fn test_tool_response_missing_id_defaults() {
        let content = Content::new(Role::Tool).with_part(Part::FunctionResponse {
            name: "test".to_string(),
            response: serde_json::json!({"result": "ok"}),
            id: None,
        });
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "tool");
        assert_eq!(msg["tool_call_id"], "unknown");
    }
}
