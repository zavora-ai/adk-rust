//! Type conversion utilities for the Meta Model API.
//!
//! The API takes the OpenAI Chat Completions format, so the wire types mirror it.

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Muse chat message content: plain text, or a native multimodal part array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text (the common case; keeps the wire shape a string).
    Text(String),
    /// Native multimodal parts (image, audio, video, file alongside text).
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// All text carried by this content, joined across parts.
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Parts(parts) => {
                let texts: Vec<&str> = parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if texts.is_empty() { None } else { Some(texts.join("\n")) }
            }
        }
    }
}

/// One native content part (Chat Completions shapes).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// Image by URL or base64 data URL.
    ImageUrl {
        /// The image reference.
        image_url: ImageUrl,
    },
    /// Inline audio bytes (MP3 or WAV) as base64.
    InputAudio {
        /// The audio reference.
        input_audio: InputAudio,
    },
    /// Video by URL.
    VideoUrl {
        /// The video reference.
        video_url: VideoUrl,
    },
    /// File by URL or inline data (for example PDF).
    File {
        /// The file reference.
        file: FileRef,
    },
}

/// `image_url` part body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    /// Public URL or `data:<mime>;base64,..` URL.
    pub url: String,
}

/// `input_audio` part body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputAudio {
    /// Base64-encoded audio bytes.
    pub data: String,
    /// Audio format (`mp3` or `wav`).
    pub format: String,
}

/// `video_url` part body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoUrl {
    /// Public URL of the video.
    pub url: String,
}

/// `file` part body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRef {
    /// File name the API reports to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// File URL or base64 data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
}

/// Muse chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
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

/// Tool definition for Muse.
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

/// Structured-output format (`response_format`).
#[derive(Debug, Clone, Serialize)]
pub struct ResponseFormat {
    /// Always `json_schema`.
    #[serde(rename = "type")]
    pub format_type: String,
    /// The schema body.
    pub json_schema: JsonSchema,
}

/// JSON-schema body of a `response_format`.
#[derive(Debug, Clone, Serialize)]
pub struct JsonSchema {
    /// Schema name (the API accepts letters, digits, and underscores).
    pub name: String,
    /// The JSON Schema.
    pub schema: Value,
    /// Strict conformance.
    pub strict: bool,
}

/// Muse chat completion request.
#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Canonical output cap (`max_tokens` is an accepted alias; `max_output_tokens`
    /// is rejected by the API).
    #[serde(rename = "max_completion_tokens", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
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
    /// Reasoning effort the API accepts OpenAI-style (`low`, `medium`, `high`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Structured output format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Tool choice (`auto`, `none`, `required`, or a function object).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    /// Whether tools may be called in parallel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Prompt-cache routing key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Prompt-cache retention hint (`in_memory` or `24h`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_retention: Option<String>,
}

/// Muse chat completion response.
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
}

impl ChatCompletionResponse {
    /// The response's usage, normalized with the object kept as received. A `null`
    /// or missing count never fails the response.
    pub fn usage_metadata(&self) -> Option<UsageMetadata> {
        self.usage.as_ref().and_then(crate::usage_json::chat_completions)
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

/// Convert one ADK content to the Muse messages that carry it.
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
                content: Some(MessageContent::Text(crate::tool_result::serialize_tool_result(
                    &response.response,
                ))),
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

/// Convert ADK Content to a Muse Message.
///
/// Text stays a plain string when it is all a content carries, so plain-text turns
/// keep the simplest wire shape. Images, audio, video, and files become native
/// multimodal parts; anything with no native shape degrades to a text attachment,
/// matching how the API rejects what it cannot read.
pub fn content_to_message(content: &Content) -> Message {
    let role = match content.role.as_str() {
        "model" | "assistant" => "assistant",
        "user" => "user",
        "system" => "system",
        "tool" | "function" => "tool",
        other => other,
    };

    let mut parts: Vec<ContentPart> = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_call_id = None;

    let push_text = |parts: &mut Vec<ContentPart>, text: String| {
        parts.push(ContentPart::Text { text });
    };

    for part in &content.parts {
        match part {
            Part::Text { text } => push_text(&mut parts, text.clone()),
            Part::Thinking { thinking, .. } => push_text(&mut parts, thinking.clone()),
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
                push_text(
                    &mut parts,
                    crate::tool_result::serialize_tool_result(&function_response.response),
                );
            }
            Part::InlineData { mime_type, data, .. } => {
                if let Some(native) = inline_part(mime_type, data) {
                    parts.push(native);
                } else {
                    push_text(&mut parts, attachment::inline_attachment_to_text(mime_type, data));
                }
            }
            Part::FileData { mime_type, file_uri, .. } => {
                if let Some(native) = file_part(mime_type, file_uri) {
                    parts.push(native);
                } else {
                    push_text(&mut parts, attachment::file_attachment_to_text(mime_type, file_uri));
                }
            }
            // Server-side tool parts are Gemini-specific; skip for Muse
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {}
            // Embedded resources: text → text; blob → native part when possible.
            Part::EmbeddedResource { resource } => match resource {
                adk_core::EmbeddedResource::Text(text) => push_text(&mut parts, text.text.clone()),
                adk_core::EmbeddedResource::Blob(blob) => {
                    let mime_type = blob.mime_type.as_deref().unwrap_or("application/octet-stream");
                    if let Some(native) = inline_part(mime_type, &blob.data) {
                        parts.push(native);
                    } else {
                        push_text(
                            &mut parts,
                            attachment::inline_attachment_to_text(mime_type, &blob.data),
                        );
                    }
                }
            },
        }
    }

    let content = merge_content(parts);

    Message {
        role: role.to_string(),
        content,
        name: None,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        tool_call_id,
    }
}

/// Merge converted parts into the wire content: text-only collapses to a string.
fn merge_content(parts: Vec<ContentPart>) -> Option<MessageContent> {
    if parts.is_empty() {
        return None;
    }
    if parts.iter().all(|part| matches!(part, ContentPart::Text { .. })) {
        let text = parts
            .into_iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Some(MessageContent::Text(text));
    }
    Some(MessageContent::Parts(parts))
}

/// Native part for inline bytes, when the MIME type has one.
fn inline_part(mime_type: &str, data: &[u8]) -> Option<ContentPart> {
    if mime_type.starts_with("image/") {
        return Some(ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: format!("data:{mime_type};base64,{}", attachment::encode_base64(data)),
            },
        });
    }
    if let Some(format) = match mime_type {
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        _ => None,
    } {
        return Some(ContentPart::InputAudio {
            input_audio: InputAudio {
                data: attachment::encode_base64(data),
                format: format.into(),
            },
        });
    }
    None
}

/// Native part for a file URI, when the MIME type has one.
fn file_part(mime_type: &str, file_uri: &str) -> Option<ContentPart> {
    if mime_type.starts_with("image/") {
        return Some(ContentPart::ImageUrl { image_url: ImageUrl { url: file_uri.to_string() } });
    }
    if mime_type.starts_with("video/") {
        return Some(ContentPart::VideoUrl { video_url: VideoUrl { url: file_uri.to_string() } });
    }
    Some(ContentPart::File {
        file: FileRef {
            filename: file_uri.rsplit('/').next().map(str::to_string),
            file_data: Some(file_uri.to_string()),
        },
    })
}

/// Convert ADK tools to Muse tools.
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

/// Convert a Muse response to an ADK LlmResponse.
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

            if let Some(text) = msg.content.as_ref().and_then(MessageContent::text)
                && !text.is_empty()
            {
                parts.push(Part::Text { text });
            }

            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    let args = crate::tool_args::tool_call_arguments(
                        "muse",
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
        let payload = message.content.and_then(|content| content.text()).unwrap_or_default();
        assert!(payload.contains("application/octet-stream"));
        assert!(payload.contains("encoding=\"base64\""));
    }

    #[test]
    fn inline_image_becomes_a_native_image_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "what is this?".to_string() },
                Part::inline_data("image/png", vec![1, 2, 3]),
            ],
        };
        let message = content_to_message(&content);
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(wire["content"][0]["type"], "text");
        assert_eq!(wire["content"][1]["type"], "image_url");
        assert!(
            wire["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }

    #[test]
    fn inline_audio_becomes_an_input_audio_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::inline_data("audio/mpeg", vec![1, 2])],
        };
        let message = content_to_message(&content);
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(wire["content"][0]["type"], "input_audio");
        assert_eq!(wire["content"][0]["input_audio"]["format"], "mp3");
    }

    #[test]
    fn pdf_file_data_becomes_a_file_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::file_data("application/pdf", "https://example.com/spec.pdf")],
        };
        let message = content_to_message(&content);
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(wire["content"][0]["type"], "file");
        assert_eq!(wire["content"][0]["file"]["file_data"], "https://example.com/spec.pdf");
    }

    #[test]
    fn text_only_content_stays_a_plain_string() {
        let content = Content::new("user").with_text("hello");
        let message = content_to_message(&content);
        let wire = serde_json::to_value(&message).unwrap();
        assert_eq!(wire["content"], serde_json::json!("hello"));
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
