//! Type conversions between ADK and async-openai types.

use crate::attachment;
use adk_core::{
    Content, FinishReason, LlmResponse, Part, SchemaAdapter, SchemaCache, UsageMetadata,
};
use async_openai::types::chat::{
    ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartAudio, ChatCompletionRequestMessageContentPartImage,
    ChatCompletionRequestMessageContentPartText, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessageArgs,
    ChatCompletionRequestUserMessageContent, ChatCompletionRequestUserMessageContentPart,
    ChatCompletionTool, ChatCompletionTools, CreateChatCompletionResponse,
    FinishReason as OaiFinishReason, FunctionCall, FunctionObject, ImageDetail, ImageUrl,
    InputAudio, InputAudioFormat,
};
use std::collections::HashMap;

/// Convert ADK Content to OpenAI ChatCompletionRequestMessage.
pub fn content_to_message(content: &Content) -> ChatCompletionRequestMessage {
    match content.role.as_str() {
        "user" => {
            let has_attachments = content
                .parts
                .iter()
                .any(|part| matches!(part, Part::InlineData { .. } | Part::FileData { .. }));
            if has_attachments {
                let content_parts: Vec<ChatCompletionRequestUserMessageContentPart> = content
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => {
                            Some(ChatCompletionRequestUserMessageContentPart::Text(
                                ChatCompletionRequestMessageContentPartText { text: text.clone() },
                            ))
                        }
                        Part::Thinking { thinking, .. } => {
                            Some(ChatCompletionRequestUserMessageContentPart::Text(
                                ChatCompletionRequestMessageContentPartText {
                                    text: thinking.clone(),
                                },
                            ))
                        }
                        Part::InlineData { mime_type, data, .. } => {
                            Some(inline_data_part_to_openai(mime_type, data))
                        }
                        Part::FileData { mime_type, file_uri, .. } => {
                            if mime_type.starts_with("image/") {
                                Some(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                    ChatCompletionRequestMessageContentPartImage {
                                        // Emit an explicit "auto" (the API default) rather than
                                        // relying on dependency serialization defaults. This keeps
                                        // requests compatible with strict OpenAI-compatible gateways
                                        // that reject `"detail": null`. See issue #395.
                                        image_url: ImageUrl {
                                            url: file_uri.clone(),
                                            detail: Some(ImageDetail::Auto),
                                        },
                                    },
                                ))
                            } else {
                                Some(ChatCompletionRequestUserMessageContentPart::Text(
                                    ChatCompletionRequestMessageContentPartText {
                                        text: attachment::file_attachment_to_text(
                                            mime_type, file_uri,
                                        ),
                                    },
                                ))
                            }
                        }
                        _ => None,
                    })
                    .collect();
                if content_parts.is_empty() {
                    ChatCompletionRequestUserMessageArgs::default()
                        .content(ChatCompletionRequestUserMessageContent::Text(extract_text(
                            &content.parts,
                        )))
                        .build()
                        .unwrap()
                        .into()
                } else {
                    ChatCompletionRequestUserMessageArgs::default()
                        .content(ChatCompletionRequestUserMessageContent::Array(content_parts))
                        .build()
                        .unwrap()
                        .into()
                }
            } else {
                let text = extract_text(&content.parts);
                ChatCompletionRequestUserMessageArgs::default()
                    .content(ChatCompletionRequestUserMessageContent::Text(text))
                    .build()
                    .unwrap()
                    .into()
            }
        }
        "model" | "assistant" => {
            let mut builder = ChatCompletionRequestAssistantMessageArgs::default();

            // Extract text content
            let text_content = get_text_content(&content.parts);
            if let Some(ref text) = text_content {
                builder.content(text.clone());
            }

            // Extract tool calls
            let tool_calls = extract_tool_calls(&content.parts);
            if !tool_calls.is_empty() {
                builder.tool_calls(tool_calls.clone());
            }

            // OpenAI requires assistant messages to have either content or tool_calls
            // If both are empty, provide a placeholder to avoid 400 Bad Request
            if text_content.is_none() && tool_calls.is_empty() {
                builder.content(" ".to_string()); // Minimal non-empty content
            }

            builder.build().unwrap().into()
        }
        "system" => {
            let text = extract_text(&content.parts);
            ChatCompletionRequestSystemMessageArgs::default().content(text).build().unwrap().into()
        }
        "function" | "tool" => {
            // Tool response message
            if let Some(Part::FunctionResponse { function_response, id, .. }) =
                content.parts.first()
            {
                let tool_call_id = id.clone().unwrap_or_else(|| "unknown".to_string());
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id)
                    .content(crate::tool_result::serialize_tool_result(&function_response.response))
                    .build()
                    .unwrap()
                    .into()
            } else {
                // Fallback to user message
                ChatCompletionRequestUserMessageArgs::default()
                    .content(ChatCompletionRequestUserMessageContent::Text(String::new()))
                    .build()
                    .unwrap()
                    .into()
            }
        }
        _ => {
            let text = extract_text(&content.parts);
            ChatCompletionRequestUserMessageArgs::default()
                .content(ChatCompletionRequestUserMessageContent::Text(text))
                .build()
                .unwrap()
                .into()
        }
    }
}

fn inline_data_part_to_openai(
    mime_type: &str,
    data: &[u8],
) -> ChatCompletionRequestUserMessageContentPart {
    if mime_type.starts_with("image/") {
        let data_uri = format!("data:{mime_type};base64,{}", attachment::encode_base64(data));
        return ChatCompletionRequestUserMessageContentPart::ImageUrl(
            ChatCompletionRequestMessageContentPartImage {
                // Explicit "auto" (API default) instead of `None`; see issue #395 — a
                // serialized `"detail": null` is rejected by strict gateways.
                image_url: ImageUrl { url: data_uri, detail: Some(ImageDetail::Auto) },
            },
        );
    }

    if let Some(audio_format) = input_audio_format(mime_type) {
        return ChatCompletionRequestUserMessageContentPart::InputAudio(
            ChatCompletionRequestMessageContentPartAudio {
                input_audio: InputAudio {
                    data: attachment::encode_base64(data),
                    format: audio_format,
                },
            },
        );
    }

    ChatCompletionRequestUserMessageContentPart::Text(ChatCompletionRequestMessageContentPartText {
        text: attachment::inline_attachment_to_text(mime_type, data),
    })
}

fn input_audio_format(mime_type: &str) -> Option<InputAudioFormat> {
    match mime_type {
        "audio/wav" | "audio/x-wav" => Some(InputAudioFormat::Wav),
        "audio/mp3" | "audio/mpeg" => Some(InputAudioFormat::Mp3),
        _ => None,
    }
}

/// Extract text content from parts.
fn extract_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.clone()),
            Part::Thinking { thinking, .. } => Some(thinking.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Get text content if any exists.
fn get_text_content(parts: &[Part]) -> Option<String> {
    let text = extract_text(parts);
    if text.is_empty() { None } else { Some(text) }
}

/// Extract tool calls from parts.
fn extract_tool_calls(parts: &[Part]) -> Vec<ChatCompletionMessageToolCalls> {
    parts
        .iter()
        .filter_map(|part| {
            if let Part::FunctionCall { name, args, id, .. } = part {
                Some(ChatCompletionMessageToolCalls::Function(ChatCompletionMessageToolCall {
                    id: id.clone().unwrap_or_else(|| format!("call_{}", name)),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(args).unwrap_or_default(),
                    },
                }))
            } else {
                None
            }
        })
        .collect()
}

/// Convert ADK tools to OpenAI ChatCompletionTools.
pub fn convert_tools(
    tools: &HashMap<String, serde_json::Value>,
    adapter: &dyn SchemaAdapter,
    cache: &SchemaCache,
) -> Vec<ChatCompletionTools> {
    let mut tools = tools.iter().collect::<Vec<_>>();
    tools.sort_unstable_by_key(|(name, _)| *name);
    tools
        .into_iter()
        .map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);

            // Normalize tool name via the schema adapter
            let normalized_name = adapter.normalize_tool_name(name);

            // Get the parameters schema from the declaration, or use the
            // adapter's empty_schema fallback when none is provided.
            let parameters = decl
                .get("parameters")
                .cloned()
                .map(|schema| cache.normalize(&schema))
                .or_else(|| Some(adapter.empty_schema()));

            ChatCompletionTools::Function(ChatCompletionTool {
                function: FunctionObject {
                    name: normalized_name.into_owned(),
                    description,
                    parameters,
                    strict: None,
                },
            })
        })
        .collect()
}

/// Convert OpenAI response to ADK LlmResponse (for non-streaming use).
///
/// Used by [`AzureOpenAIClient`](super::client::AzureOpenAIClient) which still
/// goes through `async-openai`'s typed client.
#[allow(dead_code)]
pub fn from_openai_response(resp: &CreateChatCompletionResponse) -> LlmResponse {
    let content = resp
        .choices
        .first()
        .map(|choice| -> Result<Content, String> {
            let mut parts = Vec::new();

            // Add text content (skip empty strings from reasoning models)
            if let Some(text) = &choice.message.content
                && !text.is_empty()
            {
                parts.push(Part::Text { text: text.clone() });
            }

            // Add tool calls with IDs
            if let Some(tool_calls) = &choice.message.tool_calls {
                for tc in tool_calls {
                    if let ChatCompletionMessageToolCalls::Function(func_call) = tc {
                        let encoded =
                            serde_json::Value::String(func_call.function.arguments.clone());
                        let args = decode_tool_call_arguments(Some(&encoded)).map_err(|error| {
                            format!(
                                "invalid arguments for tool '{}': {error}",
                                func_call.function.name
                            )
                        })?;
                        parts.push(Part::FunctionCall {
                            name: func_call.function.name.clone(),
                            args,
                            id: Some(func_call.id.clone()),
                            thought_signature: None,
                        });
                    }
                }
            }

            Ok(Content { role: "model".to_string(), parts })
        })
        .transpose();
    let (content, argument_error) = match content {
        Ok(content) => (content, None),
        Err(error) => (None, Some(error)),
    };

    let usage_metadata = resp.usage.as_ref().map(|u| {
        let mut meta = UsageMetadata {
            prompt_token_count: u.prompt_tokens as i32,
            candidates_token_count: u.completion_tokens as i32,
            total_token_count: u.total_tokens as i32,
            ..Default::default()
        };
        if let Some(ref details) = u.prompt_tokens_details {
            meta.cache_read_input_token_count = details.cached_tokens.map(|t| t as i32);
            meta.audio_input_token_count = details.audio_tokens.map(|t| t as i32);
        }
        if let Some(ref details) = u.completion_tokens_details {
            meta.thinking_token_count = details.reasoning_tokens.map(|t| t as i32);
            meta.audio_output_token_count = details.audio_tokens.map(|t| t as i32);
        }
        meta
    });

    let finish_reason = resp.choices.first().and_then(|c| c.finish_reason).map(|fr| match fr {
        OaiFinishReason::Stop => FinishReason::Stop,
        OaiFinishReason::Length => FinishReason::MaxTokens,
        OaiFinishReason::ToolCalls => FinishReason::Stop,
        OaiFinishReason::ContentFilter => FinishReason::Safety,
        OaiFinishReason::FunctionCall => FinishReason::Stop,
    });
    let tool_call_turn = argument_error.is_none()
        && (resp.choices.first().is_some_and(|choice| {
            matches!(
                choice.finish_reason,
                Some(OaiFinishReason::ToolCalls | OaiFinishReason::FunctionCall)
            )
        }) || content.as_ref().is_some_and(Content::has_function_calls));

    LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: argument_error.is_some() || !tool_call_turn,
        interrupted: false,
        error_code: argument_error
            .as_ref()
            .map(|_| "model.openai.invalid_tool_arguments".to_owned()),
        error_message: argument_error,
        provider_metadata: None,
        interaction_id: None,
    }
}

/// Normalize one raw Chat Completions `usage` object while retaining the
/// provider payload for diagnostics and future provider-specific projections.
pub(crate) fn usage_metadata_from_raw(usage: &serde_json::Value) -> Option<UsageMetadata> {
    let usage = usage.as_object()?;
    let prompt_details = usage.get("prompt_tokens_details");
    let completion_details = usage.get("completion_tokens_details");

    Some(UsageMetadata {
        prompt_token_count: json_i32(usage.get("prompt_tokens")).unwrap_or_default(),
        candidates_token_count: json_i32(usage.get("completion_tokens")).unwrap_or_default(),
        total_token_count: json_i32(usage.get("total_tokens")).unwrap_or_default(),
        cache_read_input_token_count: prompt_details
            .and_then(|details| json_i32(details.get("cached_tokens"))),
        cache_creation_input_token_count: prompt_details
            .and_then(|details| json_i32(details.get("cache_write_tokens"))),
        thinking_token_count: completion_details
            .and_then(|details| json_i32(details.get("reasoning_tokens"))),
        audio_input_token_count: prompt_details
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        audio_output_token_count: completion_details
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        provider_usage: Some(serde_json::Value::Object(usage.clone())),
        ..Default::default()
    })
}

fn json_i32(value: Option<&serde_json::Value>) -> Option<i32> {
    i32::try_from(value?.as_i64()?).ok()
}

/// Convert a raw OpenAI JSON response to ADK LlmResponse.
///
/// Unlike [`from_openai_response`], this parses the raw JSON directly so it can
/// extract fields that `async-openai` does not model, such as `reasoning_content`
/// returned by reasoning models (o3, gpt-5-mini, etc.).
pub(crate) fn from_raw_openai_response(json: &serde_json::Value) -> Result<LlmResponse, String> {
    let choice = json.get("choices").and_then(|c| c.get(0));

    let content = choice
        .map(|choice| -> Result<Content, String> {
            let message = &choice["message"];
            let mut parts = Vec::new();

            // Extract reasoning_content (returned by reasoning models like o3, gpt-5-mini)
            // Fallback to "reasoning" field for OpenRouter, Kilo Gateway, SambaNova, Cerebras, Groq
            let reasoning = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .and_then(|v| v.as_str());
            if let Some(reasoning) = reasoning
                && !reasoning.is_empty()
            {
                parts.push(Part::Thinking { thinking: reasoning.to_string(), signature: None });
            }

            // Extract visible text content (skip empty strings)
            if let Some(text) = message.get("content").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                // Check for text-based tool calls (Qwen, Llama, Mistral Nemo format)
                // before adding as plain text
                if let Some(parsed_parts) = crate::tool_call_parser::parse_text_tool_calls(text) {
                    parts.extend(parsed_parts);
                } else {
                    parts.push(Part::Text { text: text.to_string() });
                }
            }

            // Extract structured tool calls (OpenAI native format)
            if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls {
                    let func = &tc["function"];
                    if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                        let args =
                            decode_tool_call_arguments(func.get("arguments")).map_err(|error| {
                                format!("invalid arguments for tool '{name}': {error}")
                            })?;
                        let id = tc.get("id").and_then(|i| i.as_str()).map(String::from);
                        parts.push(Part::FunctionCall {
                            name: name.to_string(),
                            args,
                            id,
                            thought_signature: None,
                        });
                    }
                }
            }

            Ok(Content { role: "model".to_string(), parts })
        })
        .transpose()?;

    // Parse usage metadata
    let usage_metadata = json.get("usage").and_then(usage_metadata_from_raw);

    // Parse finish reason
    let finish_reason =
        choice.and_then(|c| c.get("finish_reason")).and_then(|v| v.as_str()).map(|fr| match fr {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::MaxTokens,
            "tool_calls" => FinishReason::Stop,
            "content_filter" => FinishReason::Safety,
            "function_call" => FinishReason::Stop,
            _ => FinishReason::Stop,
        });
    let tool_call_turn = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| matches!(reason, "tool_calls" | "function_call"))
        || content.as_ref().is_some_and(Content::has_function_calls);

    Ok(LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: !tool_call_turn,
        interrupted: false,
        error_code: None,
        error_message: None,
        provider_metadata: None,
        interaction_id: None,
    })
}

/// Normalizes tool arguments returned by OpenAI-compatible wire protocols.
///
/// Compatible providers sometimes encode a no-argument call as a missing value,
/// `null`, an empty string, or an empty array. These representations are all
/// equivalent to an empty JSON object. Non-empty arguments must still resolve
/// to an object so malformed payloads cannot silently invoke a tool.
pub(crate) fn decode_tool_call_arguments(
    arguments: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let decoded = match arguments {
        None | Some(serde_json::Value::Null) => serde_json::json!({}),
        Some(serde_json::Value::String(encoded)) if encoded.trim().is_empty() => {
            serde_json::json!({})
        }
        Some(serde_json::Value::String(encoded)) => {
            let decoded: serde_json::Value = serde_json::from_str(encoded)
                .map_err(|error| format!("arguments are not valid JSON: {error}"))?;
            match decoded {
                serde_json::Value::Null => serde_json::json!({}),
                serde_json::Value::Array(items) if items.is_empty() => serde_json::json!({}),
                decoded => decoded,
            }
        }
        Some(serde_json::Value::Object(fields)) => serde_json::Value::Object(fields.clone()),
        Some(serde_json::Value::Array(items)) if items.is_empty() => serde_json::json!({}),
        Some(other) => {
            return Err(format!(
                "arguments must be a JSON object or an encoded JSON object, got {}",
                match other {
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Bool(_) => "boolean",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Null => "null",
                    serde_json::Value::Object(_) => "object",
                }
            ));
        }
    };
    if decoded.is_object() {
        Ok(decoded)
    } else {
        Err("arguments must decode to a JSON object".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text() {
        let parts = vec![
            Part::Text { text: "Hello".to_string() },
            Part::Text { text: "World".to_string() },
        ];
        assert_eq!(extract_text(&parts), "Hello\nWorld");
    }

    #[test]
    fn test_user_message_with_inline_data_produces_array_content() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "What is in this image?".to_string() },
                Part::inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]), // PNG magic bytes
            ],
        };
        let msg = content_to_message(&content);

        // Should produce a user message with Array content (not Text)
        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            match &user_msg.content {
                ChatCompletionRequestUserMessageContent::Array(parts) => {
                    assert_eq!(parts.len(), 2);
                    // First part should be text
                    assert!(matches!(
                        &parts[0],
                        ChatCompletionRequestUserMessageContentPart::Text(t) if t.text == "What is in this image?"
                    ));
                    // Second part should be image URL with data URI
                    if let ChatCompletionRequestUserMessageContentPart::ImageUrl(img) = &parts[1] {
                        assert!(img.image_url.url.starts_with("data:image/png;base64,"));
                        // Regression (issue #395): detail must be an explicit level,
                        // never `None` (which serializes as invalid `"detail": null`).
                        assert_eq!(img.image_url.detail, Some(ImageDetail::Auto));
                    } else {
                        panic!("Expected ImageUrl part");
                    }
                }
                _ => panic!("Expected Array content for message with InlineData"),
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_with_multiple_attachments() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Compare these".to_string() },
                Part::inline_data("image/jpeg", vec![0xFF, 0xD8]),
                Part::inline_data("image/png", vec![0x89, 0x50]),
            ],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &user_msg.content {
                assert_eq!(parts.len(), 3); // 1 text + 2 images
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_with_audio_inline_data_uses_input_audio_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Transcribe this".to_string() },
                Part::inline_data("audio/wav", vec![0x52, 0x49, 0x46, 0x46]),
            ],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &user_msg.content {
                assert_eq!(parts.len(), 2);
                assert!(
                    matches!(&parts[1], ChatCompletionRequestUserMessageContentPart::InputAudio(_)),
                    "expected input audio part for wav mime type"
                );
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_with_pdf_inline_data_falls_back_to_text_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::inline_data("application/pdf", b"%PDF".to_vec())],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &user_msg.content {
                assert_eq!(parts.len(), 1);
                if let ChatCompletionRequestUserMessageContentPart::Text(text_part) = &parts[0] {
                    assert!(text_part.text.contains("application/pdf"));
                    assert!(text_part.text.contains("encoding=\"base64\""));
                } else {
                    panic!("Expected fallback text part for pdf inline data");
                }
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_with_file_data_falls_back_to_text_part() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "application/pdf".to_string(),
                file_uri: "https://example.com/report.pdf".to_string(),
                annotations: None,
            }],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &user_msg.content {
                assert_eq!(parts.len(), 1);
                if let ChatCompletionRequestUserMessageContentPart::Text(text_part) = &parts[0] {
                    assert!(text_part.text.contains("https://example.com/report.pdf"));
                    assert!(text_part.text.contains("application/pdf"));
                } else {
                    panic!("Expected text part for file uri attachment");
                }
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_with_image_file_data_maps_to_image_url() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Describe this".to_string() },
                Part::FileData {
                    mime_type: "image/jpeg".to_string(),
                    file_uri: "https://example.com/photo.jpg".to_string(),
                    annotations: None,
                },
            ],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            if let ChatCompletionRequestUserMessageContent::Array(parts) = &user_msg.content {
                assert_eq!(parts.len(), 2);
                if let ChatCompletionRequestUserMessageContentPart::ImageUrl(img) = &parts[1] {
                    assert_eq!(img.image_url.url, "https://example.com/photo.jpg");
                    // Regression (issue #395): explicit detail level, never `None`.
                    assert_eq!(img.image_url.detail, Some(ImageDetail::Auto));
                } else {
                    panic!("Expected ImageUrl part for image FileData");
                }
            } else {
                panic!("Expected Array content");
            }
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn test_user_message_text_only_stays_text_content() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hello".to_string() }],
        };
        let msg = content_to_message(&content);

        if let ChatCompletionRequestMessage::User(user_msg) = &msg {
            assert!(matches!(
                &user_msg.content,
                ChatCompletionRequestUserMessageContent::Text(t) if t == "Hello"
            ));
        } else {
            panic!("Expected User message");
        }
    }

    #[test]
    fn typed_response_normalizes_empty_tool_arguments() {
        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "chatcmpl_empty",
            "object": "chat.completion",
            "created": 0,
            "model": "compatible-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_empty",
                        "type": "function",
                        "function": {"name": "no_args", "arguments": "null"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("typed response should deserialize");

        let converted = from_openai_response(&response);
        let parts = converted.content.expect("tool content").parts;
        assert!(matches!(
            &parts[0],
            Part::FunctionCall { args, .. } if args == &serde_json::json!({})
        ));
        assert!(converted.error_code.is_none());
        assert!(!converted.turn_complete);
    }

    #[test]
    fn typed_response_surfaces_malformed_tool_arguments() {
        let response: CreateChatCompletionResponse = serde_json::from_value(serde_json::json!({
            "id": "chatcmpl_invalid",
            "object": "chat.completion",
            "created": 0,
            "model": "compatible-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_invalid",
                        "type": "function",
                        "function": {"name": "bash", "arguments": "{\"command\":\""}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("typed response should deserialize");

        let converted = from_openai_response(&response);
        assert_eq!(converted.error_code.as_deref(), Some("model.openai.invalid_tool_arguments"));
        assert!(converted.content.is_none());
        assert!(converted.turn_complete);
    }

    #[test]
    fn test_raw_response_extracts_reasoning_content() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Let me think about this...",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 50,
                "total_tokens": 60,
                "completion_tokens_details": { "reasoning_tokens": 40 }
            }
        });

        let resp = from_raw_openai_response(&json).expect("response should parse");
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 2);
        assert!(
            matches!(&content.parts[0], Part::Thinking { thinking, .. } if thinking == "Let me think about this...")
        );
        assert!(matches!(&content.parts[1], Part::Text { text } if text == "Hello!"));
        assert_eq!(resp.usage_metadata.unwrap().thinking_token_count, Some(40));
    }

    #[test]
    fn test_raw_response_skips_empty_content() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": ""
                },
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 64,
                "total_tokens": 74,
                "completion_tokens_details": { "reasoning_tokens": 64 }
            }
        });

        let resp = from_raw_openai_response(&json).expect("response should parse");
        let content = resp.content.unwrap();
        assert!(content.parts.is_empty(), "empty text should be filtered out");
        assert_eq!(resp.finish_reason, Some(FinishReason::MaxTokens));
    }

    #[test]
    fn test_raw_response_extracts_tool_calls() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Paris\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
        });

        let resp = from_raw_openai_response(&json).expect("response should parse");
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        if let Part::FunctionCall { name, args, id, .. } = &content.parts[0] {
            assert_eq!(name, "get_weather");
            assert_eq!(args["city"], "Paris");
            assert_eq!(id.as_deref(), Some("call_abc123"));
        } else {
            panic!("Expected FunctionCall part");
        }
        assert!(!resp.turn_complete, "tool-call turns must remain open for execution");
    }

    #[test]
    fn test_raw_response_accepts_structured_and_empty_tool_arguments() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_structured",
                            "type": "function",
                            "function": {
                                "name": "bash",
                                "arguments": {"command": "pwd"}
                            }
                        },
                        {
                            "id": "call_empty",
                            "type": "function",
                            "function": {
                                "name": "no_args",
                                "arguments": "   "
                            }
                        },
                        {
                            "id": "call_empty_array",
                            "type": "function",
                            "function": {
                                "name": "no_args_array",
                                "arguments": []
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let response = from_raw_openai_response(&json).expect("compatible arguments should parse");
        let parts = response.content.expect("tool content").parts;
        assert!(matches!(
            &parts[0],
            Part::FunctionCall { name, args, .. }
                if name == "bash" && args == &serde_json::json!({"command": "pwd"})
        ));
        assert!(matches!(
            &parts[1],
            Part::FunctionCall { name, args, .. }
                if name == "no_args" && args == &serde_json::json!({})
        ));
        assert!(matches!(
            &parts[2],
            Part::FunctionCall { name, args, .. }
                if name == "no_args_array" && args == &serde_json::json!({})
        ));
    }

    #[test]
    fn test_raw_response_rejects_truncated_tool_arguments() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_invalid",
                        "type": "function",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\""
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let error = from_raw_openai_response(&json)
            .expect_err("truncated tool arguments must remain invalid");
        assert!(error.contains("bash"));
    }

    #[test]
    fn test_raw_response_rejects_non_empty_array_tool_arguments() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_invalid",
                        "type": "function",
                        "function": {
                            "name": "bash",
                            "arguments": ["pwd"]
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let error = from_raw_openai_response(&json)
            .expect_err("non-empty array arguments must remain invalid");
        assert!(error.contains("bash"));
        assert!(error.contains("array"));
    }

    #[test]
    fn test_raw_response_standard_text() {
        let json = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "Hello there!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
        });

        let resp = from_raw_openai_response(&json).expect("response should parse");
        let content = resp.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        assert!(matches!(&content.parts[0], Part::Text { text } if text == "Hello there!"));
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        let usage = resp.usage_metadata.unwrap();
        assert_eq!(usage.prompt_token_count, 5);
        assert_eq!(usage.candidates_token_count, 3);
    }

    #[test]
    fn raw_response_preserves_complete_provider_usage() {
        let provider_usage = serde_json::json!({
            "prompt_tokens": 120,
            "completion_tokens": 30,
            "total_tokens": 150,
            "prompt_tokens_details": {
                "cached_tokens": 80,
                "cache_write_tokens": 12,
                "audio_tokens": 4
            },
            "completion_tokens_details": {
                "reasoning_tokens": 18,
                "audio_tokens": 2
            },
            "provider_meter": {"units": 9}
        });
        let json = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "done"},
                "finish_reason": "stop"
            }],
            "usage": provider_usage.clone()
        });

        let response = from_raw_openai_response(&json).expect("response should parse");
        let usage = response.usage_metadata.expect("usage should be parsed");

        assert_eq!(usage.prompt_token_count, 120);
        assert_eq!(usage.candidates_token_count, 30);
        assert_eq!(usage.total_token_count, 150);
        assert_eq!(usage.cache_read_input_token_count, Some(80));
        assert_eq!(usage.cache_creation_input_token_count, Some(12));
        assert_eq!(usage.thinking_token_count, Some(18));
        assert_eq!(usage.audio_input_token_count, Some(4));
        assert_eq!(usage.audio_output_token_count, Some(2));
        assert_eq!(usage.provider_usage, Some(provider_usage));
    }

    #[test]
    fn test_convert_tools() {
        use super::super::schema_adapter::OpenAiSchemaAdapter;

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

        let adapter = OpenAiSchemaAdapter;
        let cache = SchemaCache::for_adapter(std::sync::Arc::new(OpenAiSchemaAdapter));
        let openai_tools = convert_tools(&tools, &adapter, &cache);
        assert_eq!(openai_tools.len(), 1);
        if let ChatCompletionTools::Function(tool) = &openai_tools[0] {
            assert_eq!(tool.function.name, "get_weather");
        } else {
            panic!("Expected Function variant");
        }
    }

    #[test]
    fn convert_tools_has_deterministic_name_order() {
        use super::super::schema_adapter::OpenAiSchemaAdapter;

        let adapter = OpenAiSchemaAdapter;
        let cache = SchemaCache::for_adapter(std::sync::Arc::new(OpenAiSchemaAdapter));
        for _ in 0..32 {
            let tools = HashMap::from([
                ("zeta".to_string(), serde_json::json!({})),
                ("alpha".to_string(), serde_json::json!({})),
                ("middle".to_string(), serde_json::json!({})),
            ]);
            let names = convert_tools(&tools, &adapter, &cache)
                .into_iter()
                .map(|tool| match tool {
                    ChatCompletionTools::Function(tool) => tool.function.name,
                    ChatCompletionTools::Custom(_) => panic!("expected a function tool"),
                })
                .collect::<Vec<_>>();

            assert_eq!(names, ["alpha", "middle", "zeta"]);
        }
    }

    /// Regression test for issue #395.
    ///
    /// A serialized image request must never carry `"detail": null` — strict
    /// OpenAI-compatible gateways validate `detail` against the literal set
    /// `{auto, low, high}` and reject `null` with HTTP 400. We emit the API
    /// default `"auto"` instead. This test serializes both the inline-data and
    /// file-URI image paths and asserts the wire format is valid.
    #[test]
    fn test_image_detail_serializes_as_auto_not_null() {
        for part in [
            Part::inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]),
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "https://example.com/photo.jpg".to_string(),
                annotations: None,
            },
        ] {
            let content = Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "describe".to_string() }, part],
            };
            let msg = content_to_message(&content);
            let json = serde_json::to_string(&msg).expect("message serializes");

            assert!(
                !json.contains("\"detail\":null"),
                "serialized request must not contain `\"detail\":null`: {json}"
            );
            assert!(
                json.contains("\"detail\":\"auto\""),
                "serialized request should carry `\"detail\":\"auto\"`: {json}"
            );
        }
    }
}
