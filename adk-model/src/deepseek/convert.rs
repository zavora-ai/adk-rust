//! Type conversion utilities for DeepSeek API.

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// DeepSeek chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning content from thinking mode (only in responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Tool call in a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Tool definition for DeepSeek.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

/// Function definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// When `true` (beta), the model strictly follows the JSON schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// DeepSeek chat completion request.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Thinking mode configuration (`{"type": "enabled"}` or `{"type": "disabled"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    /// Reasoning effort level (`"high"` or `"max"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Response format configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub format_type: String,
}

/// Thinking mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
}

impl ThinkingConfig {
    /// Create a thinking config that enables chain-of-thought reasoning.
    pub fn enabled() -> Self {
        Self { thinking_type: "enabled".to_string() }
    }

    /// Create a thinking config that explicitly disables thinking.
    pub fn disabled() -> Self {
        Self { thinking_type: "disabled".to_string() }
    }
}

/// DeepSeek chat completion response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    /// Response ID (used for API tracking).
    #[serde(rename = "id")]
    pub _id: String,
    /// Object type (always "chat.completion").
    #[serde(rename = "object")]
    pub _object: String,
    /// Unix timestamp of creation.
    #[serde(rename = "created")]
    pub _created: u64,
    /// Model used for completion.
    #[serde(rename = "model")]
    pub _model: String,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Response choice.
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    /// Choice index in multi-choice responses.
    #[serde(rename = "index")]
    pub _index: u32,
    pub message: Option<Message>,
    pub delta: Option<DeltaMessage>,
    pub finish_reason: Option<String>,
}

/// Streaming delta message.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeltaMessage {
    /// Role in the message (assistant, etc.).
    #[serde(default, rename = "role")]
    pub _role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<DeltaToolCall>>,
}

/// Streaming delta tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct DeltaToolCall {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    /// Tool call type (always "function").
    #[serde(rename = "type", default)]
    pub _call_type: Option<String>,
    #[serde(default)]
    pub function: Option<DeltaFunction>,
}

/// Streaming delta function.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeltaFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Token usage information.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Tokens used for reasoning (thinking mode).
    #[serde(default)]
    pub reasoning_tokens: Option<u32>,
    /// Cache hit tokens for prefix caching.
    #[serde(default)]
    pub prompt_cache_hit_tokens: Option<u32>,
    /// Non-cached input tokens.
    #[serde(default)]
    pub prompt_cache_miss_tokens: Option<u32>,
}

/// Convert ADK Content to DeepSeek Message.
/// Builds a system message carrying `text`.
pub fn system_message(text: impl Into<String>) -> Message {
    Message {
        role: "system".to_string(),
        content: Some(text.into()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }
}

pub fn content_to_message(content: &Content) -> Message {
    let role = match content.role.as_str() {
        "model" | "assistant" => "assistant",
        "user" => "user",
        "system" => "system",
        "tool" | "function" => "tool", // DeepSeek uses "tool" for function responses
        other => other,
    };

    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_call_id = None;

    for part in &content.parts {
        match part {
            Part::Text { text } => text_parts.push(text.clone()),
            Part::FunctionCall { name, args, id, .. } => {
                tool_calls.push(ToolCall {
                    id: id.clone().unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(args).unwrap_or_default(),
                    },
                });
            }
            Part::FunctionResponse { function_response, id, .. } => {
                // Tool response - set tool_call_id and content
                tool_call_id = id.clone();
                text_parts
                    .push(crate::tool_result::serialize_tool_result(&function_response.response));
            }
            Part::InlineData { mime_type, data, .. } => {
                text_parts.push(attachment::inline_attachment_to_text(mime_type, data));
            }
            Part::FileData { mime_type, file_uri, .. } => {
                text_parts.push(attachment::file_attachment_to_text(mime_type, file_uri));
            }
            Part::Thinking { thinking, .. } => {
                reasoning_parts.push(thinking.clone());
            }
            // Server-side tool parts are Gemini-specific; skip for DeepSeek
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {}
            // Embedded resources: text → text; blob → inline-bytes text representation.
            Part::EmbeddedResource { resource } => match resource {
                adk_core::EmbeddedResource::Text(text) => text_parts.push(text.text.clone()),
                adk_core::EmbeddedResource::Blob(blob) => {
                    let mime_type = blob.mime_type.as_deref().unwrap_or("application/octet-stream");
                    text_parts.push(attachment::inline_attachment_to_text(mime_type, &blob.data));
                }
            },
        }
    }

    let content_str = if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) };
    let reasoning_content =
        if reasoning_parts.is_empty() { None } else { Some(reasoning_parts.join("\n")) };

    Message {
        role: role.to_string(),
        content: content_str,
        name: None,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id,
        reasoning_content,
    }
}

/// Reasoning that the terminal chunk still owes the consumer.
///
/// With thinking enabled, each `reasoning_content` delta is yielded as a
/// partial `Part::Thinking` while it arrives, so the terminal chunk must not
/// replay `buffer`. With thinking disabled the deltas are buffered and never
/// yielded, so the terminal chunk is the only place they can surface.
///
/// # Example
///
/// ```rust,ignore
/// // thinking enabled: already streamed, nothing left to emit
/// assert!(pending_reasoning(&mut buffer, true).is_none());
/// ```
pub fn pending_reasoning(buffer: &mut String, thinking_enabled: bool) -> Option<String> {
    if thinking_enabled || buffer.is_empty() {
        return None;
    }
    Some(std::mem::take(buffer))
}

/// Parts the terminal chunk of a stream contributes, on top of everything its
/// earlier chunks already yielded.
///
/// ADK accumulates the parts of *every* chunk it receives, partial ones
/// included, so each piece of a response must be emitted exactly once across
/// the stream. The terminal chunk therefore carries only its own `delta` —
/// plus any reasoning that was buffered but never streamed.
///
/// # Example
///
/// ```rust,ignore
/// // The usual DeepSeek finish chunk has an empty delta and contributes nothing.
/// assert!(final_chunk_parts(None, &mut String::new(), false).is_empty());
/// ```
pub fn final_chunk_parts(
    delta: Option<&DeltaMessage>,
    reasoning_buffer: &mut String,
    thinking_enabled: bool,
) -> Vec<Part> {
    let mut parts = Vec::new();
    if let Some(thinking) = pending_reasoning(reasoning_buffer, thinking_enabled) {
        parts.push(Part::Thinking { thinking, signature: None });
    }
    if let Some(text) = delta.and_then(|d| d.content.as_deref())
        && !text.is_empty()
    {
        parts.push(Part::Text { text: text.to_string() });
    }
    parts
}

/// Convert ADK tools to DeepSeek tools.
///
/// When `strict` is `true`, each tool definition includes `"strict": true`
/// for the beta strict tool mode.
pub fn convert_tools(tools: &std::collections::HashMap<String, Value>, strict: bool) -> Vec<Tool> {
    tools
        .values()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?;
            let description = tool.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let parameters = tool.get("parameters").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));

            Some(Tool {
                tool_type: "function".to_string(),
                function: FunctionDef {
                    name: name.to_string(),
                    description: description.to_string(),
                    parameters,
                    strict: if strict { Some(true) } else { None },
                },
            })
        })
        .collect()
}

/// Convert DeepSeek response to ADK LlmResponse.
pub fn from_response(response: &ChatCompletionResponse) -> LlmResponse {
    let choice = response.choices.first();

    let (content, finish_reason) = if let Some(choice) = choice {
        let finish = choice.finish_reason.as_ref().map(|fr| match fr.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::MaxTokens,
            "tool_calls" => FinishReason::Stop,
            "content_filter" => FinishReason::Safety,
            _ => FinishReason::Stop,
        });

        if let Some(msg) = &choice.message {
            let mut parts = Vec::new();

            // Add reasoning content if present (thinking mode)
            if let Some(reasoning) = &msg.reasoning_content
                && !reasoning.is_empty()
            {
                parts.push(Part::Thinking { thinking: reasoning.clone(), signature: None });
            }

            // Add main content
            if let Some(text) = &msg.content
                && !text.is_empty()
            {
                parts.push(Part::Text { text: text.clone() });
            }

            // Add tool calls
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    let args: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                    parts.push(Part::FunctionCall {
                        name: tc.function.name.clone(),
                        args,
                        id: Some(tc.id.clone()),
                        thought_signature: None,
                    });
                }
            }

            (
                if parts.is_empty() {
                    None
                } else {
                    Some(Content { role: "model".to_string(), parts })
                },
                finish,
            )
        } else {
            (None, finish)
        }
    } else {
        (None, None)
    };

    let usage = response.usage.as_ref().map(|u| UsageMetadata {
        prompt_token_count: u.prompt_tokens as i32,
        candidates_token_count: u.completion_tokens as i32,
        total_token_count: u.total_tokens as i32,
        thinking_token_count: u.reasoning_tokens.map(|t| t as i32),
        cache_read_input_token_count: u.prompt_cache_hit_tokens.map(|t| t as i32),
        cache_creation_input_token_count: u.prompt_cache_miss_tokens.map(|t| t as i32),
        ..Default::default()
    });

    // A turn that emits tool calls is not complete — tool results must still be
    // processed and sent back to the model (issue #401).
    let turn_complete = content.as_ref().is_none_or(|c| !c.has_function_calls());

    LlmResponse {
        content,
        usage_metadata: usage,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

/// Create a tool call response for accumulated tool calls.
pub fn create_tool_call_response(
    tool_calls: Vec<(String, String, Value)>, // (id, name, args)
    finish_reason: Option<FinishReason>,
    reasoning: Option<String>,
) -> LlmResponse {
    let mut parts: Vec<Part> = Vec::new();

    if let Some(ref text) = reasoning
        && !text.is_empty()
    {
        parts.push(Part::Thinking { thinking: text.clone(), signature: None });
    }

    parts.extend(tool_calls.into_iter().map(|(id, name, args)| Part::FunctionCall {
        name,
        args,
        id: Some(id),
        thought_signature: None,
    }));

    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        usage_metadata: None,
        finish_reason,
        citation_metadata: None,
        partial: false,
        // This response carries tool calls, so the turn continues (issue #401).
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pending_reasoning_is_owed_only_when_it_was_never_streamed() {
        // Thinking enabled: the deltas were yielded as they arrived.
        let mut buffer = String::from("because");
        assert_eq!(pending_reasoning(&mut buffer, true), None);
        assert_eq!(buffer, "because", "buffer must not be consumed");

        // Thinking disabled: the terminal chunk is the only place it can surface.
        assert_eq!(pending_reasoning(&mut buffer, false), Some("because".to_string()));
        assert!(buffer.is_empty(), "buffer is drained once emitted");
        assert_eq!(pending_reasoning(&mut buffer, false), None);
    }

    #[test]
    fn final_chunk_carries_only_its_own_delta() {
        // Regression: the terminal chunk used to replay the accumulated text
        // buffer on top of the deltas it had already yielded, so consumers that
        // accumulate every chunk saw the whole response twice.
        let delta = DeltaMessage { content: Some("tail".into()), ..Default::default() };
        let parts = final_chunk_parts(Some(&delta), &mut String::new(), false);
        assert_eq!(parts.len(), 1);
        assert!(matches!(&parts[0], Part::Text { text } if text == "tail"));

        // The usual DeepSeek finish chunk has an empty delta.
        assert!(final_chunk_parts(None, &mut String::new(), false).is_empty());
        let empty = DeltaMessage { content: Some(String::new()), ..Default::default() };
        assert!(final_chunk_parts(Some(&empty), &mut String::new(), false).is_empty());
    }

    #[test]
    fn final_chunk_orders_unstreamed_reasoning_before_text() {
        let delta = DeltaMessage { content: Some("answer".into()), ..Default::default() };
        let mut buffer = String::from("because");
        let parts = final_chunk_parts(Some(&delta), &mut buffer, false);
        assert!(matches!(&parts[0], Part::Thinking { thinking, .. } if thinking == "because"));
        assert!(matches!(&parts[1], Part::Text { text } if text == "answer"));
    }

    use super::*;

    #[test]
    fn tool_call_response_is_not_turn_complete() {
        // Issue #401: a response carrying tool calls must not mark the turn complete.
        let resp = create_tool_call_response(
            vec![("call_1".to_string(), "get_weather".to_string(), serde_json::json!({}))],
            Some(FinishReason::Stop),
            None,
        );
        assert!(!resp.turn_complete);
        assert!(resp.content.as_ref().unwrap().has_function_calls());
    }

    #[test]
    fn content_to_message_keeps_inline_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::inline_data("application/pdf", b"%PDF".to_vec())],
        };
        let message = content_to_message(&content);
        let payload = message.content.unwrap_or_default();
        assert!(payload.contains("application/pdf"));
        assert!(payload.contains("encoding=\"base64\""));
    }

    #[test]
    fn content_to_message_keeps_file_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::file_data("text/csv", "https://example.com/data.csv")],
        };
        let message = content_to_message(&content);
        let payload = message.content.unwrap_or_default();
        assert!(payload.contains("text/csv"));
        assert!(payload.contains("https://example.com/data.csv"));
    }

    #[test]
    fn content_to_message_maps_thinking_to_reasoning_content() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![
                Part::Thinking {
                    thinking: "reason through the tool plan".to_string(),
                    signature: None,
                },
                Part::Text { text: "let me check the prices".to_string() },
            ],
        };

        let message = content_to_message(&content);

        assert_eq!(message.role, "assistant");
        assert_eq!(message.reasoning_content.as_deref(), Some("reason through the tool plan"));
        assert_eq!(message.content.as_deref(), Some("let me check the prices"));
    }
}
