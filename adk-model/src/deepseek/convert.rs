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
    /// Thinking mode configuration for deepseek-reasoner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
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
    pub fn enabled() -> Self {
        Self { thinking_type: "enabled".to_string() }
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
    #[allow(dead_code)]
    pub prompt_cache_miss_tokens: Option<u32>,
}

/// Convert ADK Content to DeepSeek Message.
pub fn content_to_message(content: &Content) -> Message {
    let role = match content.role.as_str() {
        "model" | "assistant" => "assistant",
        "user" => "user",
        "system" => "system",
        "tool" | "function" => "tool", // DeepSeek uses "tool" for function responses
        other => other,
    };

    let mut text_parts = Vec::new();
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
            Part::FunctionResponse { function_response, id } => {
                // Tool response - set tool_call_id and content
                tool_call_id = id.clone();
                text_parts
                    .push(serde_json::to_string(&function_response.response).unwrap_or_default());
            }
            Part::InlineData { mime_type, data } => {
                text_parts.push(attachment::inline_attachment_to_text(mime_type, data));
            }
            Part::FileData { mime_type, file_uri } => {
                text_parts.push(attachment::file_attachment_to_text(mime_type, file_uri));
            }
            Part::Thinking { thinking, .. } => {
                text_parts.push(thinking.clone());
            }
        }
    }

    let content_str = if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) };

    Message {
        role: role.to_string(),
        content: content_str,
        name: None,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id,
        reasoning_content: None,
    }
}

/// Convert ADK tools to DeepSeek tools.
pub fn convert_tools(tools: &std::collections::HashMap<String, Value>) -> Vec<Tool> {
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
            if let Some(reasoning) = &msg.reasoning_content {
                if !reasoning.is_empty() {
                    parts.push(Part::Thinking { thinking: reasoning.clone(), signature: None });
                }
            }

            // Add main content
            if let Some(text) = &msg.content {
                if !text.is_empty() {
                    parts.push(Part::Text { text: text.clone() });
                }
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
        ..Default::default()
    });

    LlmResponse {
        content,
        usage_metadata: usage,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Create a tool call response for accumulated tool calls.
pub fn create_tool_call_response(
    tool_calls: Vec<(String, String, Value)>, // (id, name, args)
    finish_reason: Option<FinishReason>,
) -> LlmResponse {
    let parts: Vec<Part> = tool_calls
        .into_iter()
        .map(|(id, name, args)| Part::FunctionCall {
            name,
            args,
            id: Some(id),
            thought_signature: None,
        })
        .collect();

    LlmResponse {
        content: Some(Content { role: "model".to_string(), parts }),
        usage_metadata: None,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_to_message_keeps_inline_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "application/pdf".to_string(),
                data: b"%PDF".to_vec(),
            }],
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
            parts: vec![Part::FileData {
                mime_type: "text/csv".to_string(),
                file_uri: "https://example.com/data.csv".to_string(),
            }],
        };
        let message = content_to_message(&content);
        let payload = message.content.unwrap_or_default();
        assert!(payload.contains("text/csv"));
        assert!(payload.contains("https://example.com/data.csv"));
    }
}
