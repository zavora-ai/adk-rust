//! Type conversion utilities for Groq API.
//!
//! Groq uses OpenAI-compatible API format, so we can reuse most types from DeepSeek.

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Groq chat message.
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

/// Tool definition for Groq.
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

/// Groq chat completion request.
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
    pub include_reasoning: Option<bool>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Sampling seed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
}

/// Groq chat completion response.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    #[serde(rename = "id")]
    pub _id: String,
    #[serde(rename = "object")]
    pub _object: String,
    #[serde(rename = "created")]
    pub _created: u64,
    #[serde(rename = "model")]
    pub _model: String,
    pub choices: Vec<Choice>,
    /// The `usage` object as received; counts are read from it leniently.
    #[serde(default)]
    pub usage: Option<Value>,
    /// Groq's extension object, which carries usage on the final stream chunk.
    #[serde(default)]
    pub x_groq: Option<XGroq>,
}

/// Groq's `x_groq` extension object.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct XGroq {
    #[serde(default)]
    pub usage: Option<Value>,
}

impl ChatCompletionResponse {
    /// The response's usage: top-level `usage`, else `x_groq.usage`, normalized with the
    /// object kept as received. A `null` or missing count never fails the response.
    pub fn usage_metadata(&self) -> Option<UsageMetadata> {
        self.usage
            .as_ref()
            .or_else(|| self.x_groq.as_ref()?.usage.as_ref())
            .and_then(crate::usage_json::chat_completions)
    }
}

/// Response choice.
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    #[serde(rename = "index")]
    pub _index: u32,
    pub message: Option<Message>,
    pub delta: Option<DeltaMessage>,
    pub finish_reason: Option<String>,
}

/// Streaming delta message.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeltaMessage {
    #[serde(default, rename = "role")]
    pub _role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<DeltaToolCall>>,
}

/// Streaming delta tool call.
#[derive(Debug, Clone, Deserialize)]
pub struct DeltaToolCall {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
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

/// Convert one ADK content to the Groq messages that carry it.
///
/// A content holding function responses becomes one `tool` message per response, in order,
/// followed by a user message with any other parts; every other content is one message.
pub fn content_to_messages(content: &Content) -> Vec<Message> {
    if matches!(content.role.as_str(), "user" | "function" | "tool")
        && let Some((responses, rest)) = crate::tool_result::split_function_responses(content)
    {
        let mut messages: Vec<Message> = responses
            .into_iter()
            .map(|(id, response)| Message {
                role: "tool".to_string(),
                content: Some(crate::tool_result::serialize_tool_result(&response.response)),
                name: None,
                tool_calls: None,
                tool_call_id: id.map(str::to_string),
            })
            .collect();
        messages.extend(rest.map(|rest| content_to_message(&rest)));
        return messages;
    }
    vec![content_to_message(content)]
}

/// Convert ADK Content to Groq Message.
pub fn content_to_message(content: &Content) -> Message {
    let role = match content.role.as_str() {
        "model" | "assistant" => "assistant",
        "user" => "user",
        "system" => "system",
        "tool" | "function" => "tool",
        other => other,
    };

    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_call_id = None;

    for part in &content.parts {
        match part {
            Part::Text { text } => text_parts.push(text.clone()),
            Part::Thinking { thinking, .. } => text_parts.push(thinking.clone()),
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
            // Server-side tool parts are Gemini-specific; skip for Groq
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

    Message {
        role: role.to_string(),
        content: content_str,
        name: None,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id,
    }
}

/// Convert ADK tools to Groq tools.
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

/// Convert Groq response to ADK LlmResponse.
pub fn from_response(response: &ChatCompletionResponse) -> LlmResponse {
    let choice = response.choices.first();

    let (content, finish_reason) = if let Some(choice) = choice {
        let finish = choice.finish_reason.as_ref().map(|fr| match fr.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::MaxTokens,
            "tool_calls" => FinishReason::Stop,
            "content_filter" => FinishReason::Safety,
            _ => FinishReason::Other,
        });

        if let Some(msg) = &choice.message {
            let mut parts = Vec::new();

            if let Some(text) = &msg.content
                && !text.is_empty()
            {
                parts.push(Part::Text { text: text.clone() });
            }

            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    let args = crate::tool_args::tool_call_arguments(
                        "groq",
                        &tc.function.name,
                        Some(&Value::String(tc.function.arguments.clone())),
                    );
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

    let usage = response.usage_metadata();

    let turn_complete = content.as_ref().is_none_or(|value| !value.has_function_calls());

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
        model: None,
        provider: None,
    }
}

/// Create a tool call response for accumulated tool calls.
pub fn create_tool_call_response(
    tool_calls: Vec<(String, String, Value)>,
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
        // A tool-call response keeps the turn open so the Runner can execute it.
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
    fn tool_call_response_keeps_the_turn_open() {
        let response = create_tool_call_response(
            vec![("call-1".to_string(), "lookup".to_string(), serde_json::json!({}))],
            Some(FinishReason::Stop),
        );

        assert!(!response.turn_complete);
        assert!(response.content.as_ref().is_some_and(Content::has_function_calls));
    }

    #[test]
    fn content_to_message_keeps_inline_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::inline_data("application/octet-stream", vec![0xCA, 0xFE])],
        };
        let message = content_to_message(&content);
        let payload = message.content.unwrap_or_default();
        assert!(payload.contains("application/octet-stream"));
        assert!(payload.contains("encoding=\"base64\""));
    }

    #[test]
    fn content_to_message_keeps_file_attachment_payload() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::file_data("application/pdf", "https://example.com/report.pdf")],
        };
        let message = content_to_message(&content);
        let payload = message.content.unwrap_or_default();
        assert!(payload.contains("application/pdf"));
        assert!(payload.contains("https://example.com/report.pdf"));
    }

    #[test]
    fn several_function_responses_become_one_tool_message_each() {
        let response = |id: &str| Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "get_weather",
                serde_json::json!({"ok": id}),
            ),
            id: Some(id.to_string()),
            annotations: None,
        };
        let content = Content {
            role: "function".to_string(),
            parts: vec![
                response("call_a"),
                response("call_b"),
                Part::Text { text: "note".to_string() },
            ],
        };
        let messages = content_to_messages(&content);
        let shape: Vec<(String, Option<String>)> =
            messages.iter().map(|m| (m.role.clone(), m.tool_call_id.clone())).collect();
        assert_eq!(
            shape,
            vec![
                ("tool".to_string(), Some("call_a".to_string())),
                ("tool".to_string(), Some("call_b".to_string())),
                ("user".to_string(), None),
            ]
        );
    }
}
