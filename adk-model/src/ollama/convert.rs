//! Type conversions between ADK core types and ollama-rs types.

use std::collections::HashSet;

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use ollama_rs::generation::chat::{ChatMessage, ChatMessageResponse};
use serde_json::Value;

/// Convert ADK Content to Ollama ChatMessage.
pub fn content_to_chat_message(content: &Content) -> Option<ChatMessage> {
    // Extract text from parts
    let text: String = content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            Part::Thinking { thinking, .. } => Some(thinking.clone()),
            Part::InlineData { mime_type, data, .. } => {
                Some(attachment::inline_attachment_to_text(mime_type, data))
            }
            Part::FileData { mime_type, file_uri, .. } => {
                Some(attachment::file_attachment_to_text(mime_type, file_uri))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    match content.role.as_str() {
        "user" => Some(ChatMessage::user(text)),
        "model" | "assistant" => Some(ChatMessage::assistant(text)),
        "system" => Some(ChatMessage::system(text)),
        "function" | "tool" => {
            // Handle function responses - combine all responses into one tool message
            let mut response_texts = Vec::new();
            for part in &content.parts {
                if let Part::FunctionResponse { function_response, .. } = part {
                    response_texts.push(format!(
                        "{}: {}",
                        function_response.name,
                        crate::tool_result::serialize_tool_result(&function_response.response)
                    ));
                }
            }
            if !response_texts.is_empty() {
                Some(ChatMessage::tool(response_texts.join("\n")))
            } else if !text.is_empty() {
                Some(ChatMessage::tool(text))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Metric fields of a final `/api/chat` line, kept in `provider_usage`.
const METRICS: [&str; 6] = [
    "total_duration",
    "load_duration",
    "prompt_eval_count",
    "prompt_eval_duration",
    "eval_count",
    "eval_duration",
];

/// Usage from a final (`done: true`) `/api/chat` line, read from the line as received.
///
/// Ollama leaves out a metric that is zero, such as `prompt_eval_count` when the whole prompt
/// came from its cache, so a missing or `null` count is zero and the counts that are present
/// are still reported. `provider_usage` keeps the line's metric fields exactly as sent.
pub fn usage(line: &Value) -> Option<UsageMetadata> {
    use crate::usage_json::count;
    let object = line.as_object()?;
    let metrics: serde_json::Map<String, Value> = METRICS
        .iter()
        .filter_map(|key| Some(((*key).to_string(), object.get(*key)?.clone())))
        .collect();
    let prompt = count(line, "/prompt_eval_count").unwrap_or_default();
    let output = count(line, "/eval_count").unwrap_or_default();
    Some(UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: output,
        total_token_count: prompt.saturating_add(output),
        provider_usage: (!metrics.is_empty()).then_some(Value::Object(metrics)),
        ..Default::default()
    })
}

/// Maps a final line's `done_reason`: `length` is the `num_predict` cap, and a line without one
/// stopped naturally.
fn finish_reason(line: &Value) -> FinishReason {
    match line.get("done_reason").and_then(Value::as_str) {
        None | Some("stop") => FinishReason::Stop,
        Some("length") => FinishReason::MaxTokens,
        Some(_) => FinishReason::Other,
    }
}

/// Convert Ollama ChatMessageResponse to ADK LlmResponse.
///
/// `line` is the same `/api/chat` line as received; a final line's usage is read from it with
/// [`usage`]. Text-encoded tool calls are recognised only for tools in `declared_tools`.
pub fn chat_response_to_llm_response(
    response: &ChatMessageResponse,
    line: &Value,
    partial: bool,
    declared_tools: &HashSet<String>,
) -> LlmResponse {
    let mut parts = Vec::new();

    // Extract thinking content if present
    if let Some(thinking) = &response.message.thinking
        && !thinking.is_empty()
    {
        parts.push(Part::Thinking { thinking: thinking.clone(), signature: None });
    }

    // Add text content
    if !response.message.content.is_empty() {
        // Check for text-based tool calls (Qwen, Llama, Mistral Nemo format)
        // as a fallback when Ollama doesn't parse them natively
        if response.message.tool_calls.is_empty() {
            if let Some(parsed_parts) = crate::tool_call_parser::parse_declared_tool_calls(
                &response.message.content,
                declared_tools,
            ) {
                parts.extend(parsed_parts);
            } else {
                parts.push(Part::Text { text: response.message.content.clone() });
            }
        } else {
            parts.push(Part::Text { text: response.message.content.clone() });
        }
    }

    // Handle tool calls if present
    for tool_call in &response.message.tool_calls {
        parts.push(Part::FunctionCall {
            name: tool_call.function.name.clone(),
            args: tool_call.function.arguments.clone(),
            id: None, // Ollama doesn't provide tool call IDs
            thought_signature: None,
        });
    }

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

    let finish_reason = response.done.then(|| finish_reason(line));

    // Not `response.final_data`: `ollama-rs` keeps the metrics only when all six are present.
    let usage_metadata = if response.done { usage(line) } else { None };

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial,
        turn_complete: response.done,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
        model: None,
        provider: None,
    }
}

/// Removes a line's `logprobs`, which the `ollama-rs` response type cannot read when Ollama
/// leaves out a token's `bytes`, and returns its entries.
pub fn take_logprobs(line: &mut Value) -> Vec<Value> {
    match line.as_object_mut().and_then(|line| line.remove("logprobs")) {
        Some(Value::Array(entries)) => entries,
        Some(_) | None => Vec::new(),
    }
}

/// `provider_metadata` carrying the turn's log probabilities as `{"logprobs": {"content":
/// [...]}}`, the shape the Chat Completions clients return.
pub fn logprobs_metadata(logprobs: Vec<Value>) -> Option<Value> {
    (!logprobs.is_empty()).then(|| serde_json::json!({"logprobs": {"content": logprobs}}))
}

/// Create a streaming delta response carrying one part of the turn the `done` line ends.
pub fn part_delta_response(part: Part) -> LlmResponse {
    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts: vec![part] }),
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: true,
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
        model: None,
        provider: None,
    }
}
/// Create a thinking delta response for streaming.
pub fn thinking_delta_response(thinking: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Thinking { thinking: thinking.to_string(), signature: None }],
        }),
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: true,
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
        model: None,
        provider: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_to_chat_message_keeps_inline_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::inline_data("application/pdf", b"%PDF".to_vec())],
        };
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("application/pdf"));
        assert!(message.content.contains("encoding=\"base64\""));
    }

    #[test]
    fn content_to_chat_message_keeps_file_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::file_data("text/csv", "https://example.com/data.csv")],
        };
        let message = content_to_chat_message(&content).expect("message should be created");
        assert!(message.content.contains("text/csv"));
        assert!(message.content.contains("https://example.com/data.csv"));
    }

    #[test]
    fn final_response_maps_done_reason() {
        let finish = |done_reason: Option<&str>| {
            let mut line = serde_json::json!({
                "model": "qwen3",
                "created_at": "2026-10-17T00:00:00Z",
                "message": {"role": "assistant", "content": "partial answer"},
                "done": true
            });
            if let Some(reason) = done_reason {
                line["done_reason"] = serde_json::json!(reason);
            }
            let response: ChatMessageResponse = serde_json::from_value(line.clone()).unwrap();
            chat_response_to_llm_response(&response, &line, false, &HashSet::new()).finish_reason
        };
        assert_eq!(
            [None, Some("stop"), Some("length"), Some("unload")].map(finish),
            [
                Some(FinishReason::Stop),
                Some(FinishReason::Stop),
                Some(FinishReason::MaxTokens),
                Some(FinishReason::Other),
            ]
        );
    }

    #[test]
    fn final_response_reads_usage_from_the_line_as_received() {
        let mut line = serde_json::json!({
            "model": "qwen3",
            "created_at": "2026-10-17T00:00:00Z",
            "message": {"role": "assistant", "content": "done"},
            "done": true,
            "total_duration": 2000,
            "load_duration": 10,
            "prompt_eval_count": 44,
            "prompt_eval_duration": 300,
            "eval_count": 8,
            "eval_duration": 700
        });
        let final_usage = |line: &Value| {
            let response: ChatMessageResponse = serde_json::from_value(line.clone()).unwrap();
            chat_response_to_llm_response(&response, line, false, &HashSet::new())
                .usage_metadata
                .map(|usage| serde_json::to_value(usage).unwrap())
        };
        assert_eq!(
            final_usage(&line),
            Some(serde_json::json!({
                "prompt_token_count": 44,
                "candidates_token_count": 8,
                "total_token_count": 52,
                "provider_usage": {
                    "total_duration": 2000,
                    "load_duration": 10,
                    "prompt_eval_count": 44,
                    "prompt_eval_duration": 300,
                    "eval_count": 8,
                    "eval_duration": 700
                }
            }))
        );

        // Ollama leaves out a zero metric; `ollama-rs` then drops every count.
        let object = line.as_object_mut().unwrap();
        object.remove("prompt_eval_count");
        object.remove("prompt_eval_duration");
        assert_eq!(
            final_usage(&line),
            Some(serde_json::json!({
                "prompt_token_count": 0,
                "candidates_token_count": 8,
                "total_token_count": 8,
                "provider_usage": {
                    "total_duration": 2000,
                    "load_duration": 10,
                    "eval_count": 8,
                    "eval_duration": 700
                }
            }))
        );

        line["done"] = serde_json::json!(false);
        assert_eq!(final_usage(&line), None);
    }
}
