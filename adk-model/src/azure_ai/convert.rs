//! Type conversions between ADK and Azure AI Inference chat completions format.
//!
//! The Azure AI Inference API uses a format very similar to OpenAI's chat
//! completions API. Key differences:
//! - Authentication uses `api-key` header instead of `Authorization: Bearer`
//! - Endpoint URL is `{endpoint}/chat/completions?api-version=2024-05-01-preview`
//!
//! All functions in this module are `pub(crate)` since they are internal
//! implementation details of the Azure AI provider.

use adk_core::{Content, FinishReason, GenerateContentConfig, LlmResponse, Part, UsageMetadata};
use serde_json::Value;
use std::collections::HashMap;

/// Build an Azure AI Inference chat completions request body from an ADK `LlmRequest`.
///
/// Converts ADK contents, tools, and generation config into the JSON format
/// expected by the Azure AI Inference REST API.
pub(crate) fn build_request_body(
    model: &str,
    contents: &[Content],
    tools: &HashMap<String, Value>,
    config: Option<&GenerateContentConfig>,
    stream: bool,
) -> Value {
    let messages: Vec<Value> = contents.iter().map(content_to_message).collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": stream,
    });

    if !tools.is_empty() {
        let tool_array: Vec<Value> = tools
            .iter()
            .map(|(name, decl)| {
                let description =
                    decl.get("description").and_then(|d| d.as_str()).unwrap_or_default();
                let parameters = decl.get("parameters").cloned().unwrap_or(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }));
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": parameters,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tool_array);
    }

    if let Some(cfg) = config {
        if let Some(temp) = cfg.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(top_p) = cfg.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }
        if let Some(max_tokens) = cfg.max_output_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
    }

    body
}

/// Convert a single ADK `Content` to an Azure AI message JSON object.
fn content_to_message(content: &Content) -> Value {
    match content.role.as_str() {
        "user" => {
            let text = extract_text(&content.parts);
            serde_json::json!({
                "role": "user",
                "content": text,
            })
        }
        "model" | "assistant" => {
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
        "system" => {
            let text = extract_text(&content.parts);
            serde_json::json!({
                "role": "system",
                "content": text,
            })
        }
        "function" | "tool" => {
            if let Some(Part::FunctionResponse { function_response, id }) = content.parts.first() {
                let tool_call_id = id.clone().unwrap_or_else(|| "unknown".to_string());
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": serde_json::to_string(&function_response.response).unwrap_or_default(),
                })
            } else {
                serde_json::json!({
                    "role": "user",
                    "content": "",
                })
            }
        }
        _ => {
            let text = extract_text(&content.parts);
            serde_json::json!({
                "role": "user",
                "content": text,
            })
        }
    }
}

/// Extract all text parts joined by newlines.
fn extract_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
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
            if let Part::FunctionCall { name, args, id } = part {
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

/// Parse a non-streaming Azure AI Inference response JSON into an ADK `LlmResponse`.
///
/// Expected format:
/// ```json
/// {
///   "choices": [{"message": {"role": "assistant", "content": "...", "tool_calls": [...]}, "finish_reason": "stop"}],
///   "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30}
/// }
/// ```
pub(crate) fn parse_response(body: &Value) -> LlmResponse {
    let content = body.get("choices").and_then(|c| c.get(0)).and_then(|choice| {
        let message = choice.get("message")?;
        let mut parts = Vec::new();

        // Extract text content
        if let Some(text) = message.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                parts.push(Part::Text { text: text.to_string() });
            }
        }

        // Extract tool calls
        if let Some(tool_calls) = message.get("tool_calls").and_then(|tc| tc.as_array()) {
            for tc in tool_calls {
                if let Some(func) = tc.get("function") {
                    let name =
                        func.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                    let args: Value = func
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .and_then(|a| serde_json::from_str(a).ok())
                        .unwrap_or(serde_json::json!({}));
                    let id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
                    parts.push(Part::FunctionCall { name, args, id });
                }
            }
        }

        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) }
    });

    let finish_reason = body
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|fr| fr.as_str())
        .map(map_finish_reason);

    let usage_metadata = body.get("usage").map(|u| UsageMetadata {
        prompt_token_count: u.get("prompt_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        candidates_token_count: u.get("completion_tokens").and_then(|v| v.as_i64()).unwrap_or(0)
            as i32,
        total_token_count: u.get("total_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
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

/// Parse an Azure AI Inference SSE stream chunk into an ADK `LlmResponse`.
///
/// Expected format:
/// ```json
/// {"choices": [{"delta": {"content": "partial text", "tool_calls": [...]}, "finish_reason": null}]}
/// ```
pub(crate) fn parse_sse_chunk(chunk: &Value) -> LlmResponse {
    let content = chunk.get("choices").and_then(|c| c.get(0)).and_then(|choice| {
        let delta = choice.get("delta")?;
        let mut parts = Vec::new();

        // Extract text delta
        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                parts.push(Part::Text { text: text.to_string() });
            }
        }

        // Extract tool call deltas
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
            for tc in tool_calls {
                if let Some(func) = tc.get("function") {
                    let name = func.get("name").and_then(|n| n.as_str());
                    // Only emit a FunctionCall part if we have a name (start of tool call).
                    // Partial argument chunks without a name are accumulated by the caller.
                    if let Some(name) = name {
                        if !name.is_empty() {
                            let args: Value = func
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .and_then(|a| serde_json::from_str(a).ok())
                                .unwrap_or(serde_json::json!({}));
                            let id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
                            parts.push(Part::FunctionCall { name: name.to_string(), args, id });
                        }
                    }
                }
            }
        }

        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) }
    });

    let finish_reason = chunk
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(|fr| fr.as_str())
        .map(map_finish_reason);

    let is_final = finish_reason.is_some();

    LlmResponse {
        content,
        usage_metadata: None,
        finish_reason,
        citation_metadata: None,
        partial: !is_final,
        turn_complete: is_final,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Map Azure AI finish_reason string to ADK `FinishReason`.
fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::MaxTokens,
        "tool_calls" => FinishReason::Stop,
        "content_filter" => FinishReason::Safety,
        _ => FinishReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::FunctionResponseData;

    #[test]
    fn test_build_request_body_basic() {
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hello".to_string() }],
        }];
        let body = build_request_body("my-model", &contents, &HashMap::new(), None, false);

        assert_eq!(body["model"], "my-model");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Hello");
        assert!(body.get("tools").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn test_build_request_body_with_config() {
        let config = GenerateContentConfig {
            temperature: Some(0.5),
            top_p: Some(0.5),
            max_output_tokens: Some(1024),
            top_k: None,
            response_schema: None,
        };
        let body = build_request_body("m", &[], &HashMap::new(), Some(&config), true);

        assert_eq!(body["stream"], true);
        // Use f32-safe values (0.5 is exactly representable in f32)
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["top_p"], 0.5);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn test_build_request_body_with_tools() {
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
        let body = build_request_body("m", &[], &tools, None, false);

        let tool_array = body["tools"].as_array().unwrap();
        assert_eq!(tool_array.len(), 1);
        assert_eq!(tool_array[0]["type"], "function");
        assert_eq!(tool_array[0]["function"]["name"], "get_weather");
        assert_eq!(tool_array[0]["function"]["description"], "Get weather for a city");
    }

    #[test]
    fn test_content_to_message_user() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hi there".to_string() }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "user");
        assert_eq!(msg["content"], "Hi there");
    }

    #[test]
    fn test_content_to_message_system() {
        let content = Content {
            role: "system".to_string(),
            parts: vec![Part::Text { text: "You are helpful.".to_string() }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "system");
        assert_eq!(msg["content"], "You are helpful.");
    }

    #[test]
    fn test_content_to_message_model_role() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hello!".to_string() }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "Hello!");
    }

    #[test]
    fn test_content_to_message_assistant_role() {
        let content = Content {
            role: "assistant".to_string(),
            parts: vec![Part::Text { text: "Sure".to_string() }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
    }

    #[test]
    fn test_content_to_message_with_tool_calls() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "Seattle"}),
                id: Some("call_123".to_string()),
            }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
        let tool_calls = msg["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_123");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_content_to_message_tool_response() {
        let content = Content {
            role: "tool".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "get_weather".to_string(),
                    response: serde_json::json!({"temp": 72}),
                },
                id: Some("call_123".to_string()),
            }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "tool");
        assert_eq!(msg["tool_call_id"], "call_123");
        assert!(msg["content"].as_str().unwrap().contains("72"));
    }

    #[test]
    fn test_content_to_message_empty_assistant_gets_placeholder() {
        let content = Content { role: "model".to_string(), parts: vec![] };
        let msg = content_to_message(&content);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], " ");
    }

    #[test]
    fn test_parse_response_text() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello world"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let resp = parse_response(&body);
        assert!(resp.turn_complete);
        assert!(!resp.partial);
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));

        let content = resp.content.unwrap();
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1);
        assert_eq!(content.parts[0].text().unwrap(), "Hello world");

        let usage = resp.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, 10);
        assert_eq!(usage.candidates_token_count, 5);
        assert_eq!(usage.total_token_count, 15);
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Seattle\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 10,
                "total_tokens": 30
            }
        });

        let resp = parse_response(&body);
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));

        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        if let Part::FunctionCall { name, args, id } = &content.parts[0] {
            assert_eq!(name, "get_weather");
            assert_eq!(args["city"], "Seattle");
            assert_eq!(id.as_deref(), Some("call_abc"));
        } else {
            panic!("Expected FunctionCall part");
        }
    }

    #[test]
    fn test_parse_response_length_finish() {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "truncated" },
                "finish_reason": "length"
            }]
        });
        let resp = parse_response(&body);
        assert_eq!(resp.finish_reason, Some(FinishReason::MaxTokens));
    }

    #[test]
    fn test_parse_response_content_filter() {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "" },
                "finish_reason": "content_filter"
            }]
        });
        let resp = parse_response(&body);
        assert_eq!(resp.finish_reason, Some(FinishReason::Safety));
    }

    #[test]
    fn test_parse_sse_chunk_text() {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": { "content": "Hello" },
                "finish_reason": null
            }]
        });

        let resp = parse_sse_chunk(&chunk);
        assert!(resp.partial);
        assert!(!resp.turn_complete);
        assert!(resp.finish_reason.is_none());

        let content = resp.content.unwrap();
        assert_eq!(content.parts[0].text().unwrap(), "Hello");
    }

    #[test]
    fn test_parse_sse_chunk_final() {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": "stop"
            }]
        });

        let resp = parse_sse_chunk(&chunk);
        assert!(!resp.partial);
        assert!(resp.turn_complete);
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert!(resp.content.is_none());
    }

    #[test]
    fn test_parse_sse_chunk_tool_call() {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "id": "call_xyz",
                        "function": {
                            "name": "search",
                            "arguments": "{\"q\":\"rust\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });

        let resp = parse_sse_chunk(&chunk);
        assert!(resp.partial);
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        if let Part::FunctionCall { name, args, id } = &content.parts[0] {
            assert_eq!(name, "search");
            assert_eq!(args["q"], "rust");
            assert_eq!(id.as_deref(), Some("call_xyz"));
        } else {
            panic!("Expected FunctionCall part");
        }
    }

    #[test]
    fn test_parse_sse_chunk_empty_delta() {
        let chunk = serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": null
            }]
        });

        let resp = parse_sse_chunk(&chunk);
        assert!(resp.content.is_none());
        assert!(resp.partial);
    }

    #[test]
    fn test_map_finish_reason_variants() {
        assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(map_finish_reason("length"), FinishReason::MaxTokens);
        assert_eq!(map_finish_reason("tool_calls"), FinishReason::Stop);
        assert_eq!(map_finish_reason("content_filter"), FinishReason::Safety);
        assert_eq!(map_finish_reason("unknown"), FinishReason::Other);
    }

    #[test]
    fn test_multiple_text_parts_joined() {
        let parts = vec![
            Part::Text { text: "Hello".to_string() },
            Part::Text { text: "World".to_string() },
        ];
        assert_eq!(extract_text(&parts), "Hello\nWorld");
    }

    #[test]
    fn test_round_trip_text_content() {
        let contents = vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "What is Rust?".to_string() }],
        }];
        let body = build_request_body("test-model", &contents, &HashMap::new(), None, false);

        // Simulate a response with the same text
        let response_json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Rust is a systems programming language."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 8,
                "total_tokens": 13
            }
        });

        let resp = parse_response(&response_json);
        assert!(resp.content.is_some());
        assert_eq!(resp.content.unwrap().role, "model");
        assert_eq!(body["messages"][0]["content"], "What is Rust?");
    }

    #[test]
    fn test_function_call_id_defaults() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "my_func".to_string(),
                args: serde_json::json!({}),
                id: None,
            }],
        };
        let msg = content_to_message(&content);
        let tool_calls = msg["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["id"], "call_my_func");
    }

    #[test]
    fn test_tool_response_missing_id_defaults() {
        let content = Content {
            role: "tool".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "test".to_string(),
                    response: serde_json::json!({"ok": true}),
                },
                id: None,
            }],
        };
        let msg = content_to_message(&content);
        assert_eq!(msg["tool_call_id"], "unknown");
    }
}
