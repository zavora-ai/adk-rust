//! Type conversions between ADK and async-openai types.

use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageArgs,
    ChatCompletionRequestMessage, ChatCompletionRequestMessageContentPartAudio,
    ChatCompletionRequestMessageContentPartImage, ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestToolMessageArgs,
    ChatCompletionRequestUserMessageArgs, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ChatCompletionTool, ChatCompletionToolType,
    CreateChatCompletionResponse, CreateChatCompletionStreamResponse, FunctionCall, FunctionObject,
    ImageUrl, InputAudio, InputAudioFormat,
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
                        Part::InlineData { mime_type, data } => {
                            Some(inline_data_part_to_openai(mime_type, data))
                        }
                        Part::FileData { mime_type, file_uri } => {
                            Some(ChatCompletionRequestUserMessageContentPart::Text(
                                ChatCompletionRequestMessageContentPartText {
                                    text: attachment::file_attachment_to_text(mime_type, file_uri),
                                },
                            ))
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
            if let Some(Part::FunctionResponse { function_response, id }) = content.parts.first() {
                let tool_call_id = id.clone().unwrap_or_else(|| "unknown".to_string());
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tool_call_id)
                    .content(serde_json::to_string(&function_response.response).unwrap_or_default())
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
                image_url: ImageUrl { url: data_uri, detail: None },
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
fn extract_tool_calls(parts: &[Part]) -> Vec<ChatCompletionMessageToolCall> {
    parts
        .iter()
        .filter_map(|part| {
            if let Part::FunctionCall { name, args, id, .. } = part {
                Some(ChatCompletionMessageToolCall {
                    id: id.clone().unwrap_or_else(|| format!("call_{}", name)),
                    r#type: ChatCompletionToolType::Function,
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(args).unwrap_or_default(),
                    },
                })
            } else {
                None
            }
        })
        .collect()
}

/// Convert ADK tools to OpenAI ChatCompletionTool.
pub fn convert_tools(tools: &HashMap<String, serde_json::Value>) -> Vec<ChatCompletionTool> {
    tools
        .iter()
        .map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);

            let parameters = decl.get("parameters").cloned();

            ChatCompletionTool {
                r#type: ChatCompletionToolType::Function,
                function: FunctionObject {
                    name: name.clone(),
                    description,
                    parameters,
                    strict: None,
                },
            }
        })
        .collect()
}

/// Convert OpenAI response to ADK LlmResponse (for non-streaming use).
#[allow(dead_code)]
pub fn from_openai_response(resp: &CreateChatCompletionResponse) -> LlmResponse {
    let content = resp.choices.first().map(|choice| {
        let mut parts = Vec::new();

        // Add text content
        if let Some(text) = &choice.message.content {
            parts.push(Part::Text { text: text.clone() });
        }

        // Add tool calls with IDs
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                let args: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));
                parts.push(Part::FunctionCall {
                    name: tc.function.name.clone(),
                    args,
                    id: Some(tc.id.clone()),
                    thought_signature: None,
                });
            }
        }

        Content { role: "model".to_string(), parts }
    });

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
        async_openai::types::FinishReason::Stop => FinishReason::Stop,
        async_openai::types::FinishReason::Length => FinishReason::MaxTokens,
        async_openai::types::FinishReason::ToolCalls => FinishReason::Stop,
        async_openai::types::FinishReason::ContentFilter => FinishReason::Safety,
        async_openai::types::FinishReason::FunctionCall => FinishReason::Stop,
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

/// Convert OpenAI stream chunk to ADK LlmResponse.
pub fn from_openai_chunk(chunk: &CreateChatCompletionStreamResponse) -> LlmResponse {
    let content = chunk.choices.first().and_then(|choice| {
        let mut parts = Vec::new();

        // Add text content from delta
        if let Some(text) = &choice.delta.content {
            if !text.is_empty() {
                parts.push(Part::Text { text: text.clone() });
            }
        }

        // Add tool calls from delta
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                if let Some(func) = &tc.function {
                    if let Some(name) = &func.name {
                        if !name.is_empty() {
                            let args: serde_json::Value = func
                                .arguments
                                .as_ref()
                                .and_then(|a| serde_json::from_str(a).ok())
                                .unwrap_or(serde_json::json!({}));
                            parts.push(Part::FunctionCall {
                                name: name.clone(),
                                args,
                                id: tc.id.clone(),
                                thought_signature: None,
                            });
                        }
                    }
                }
            }
        }

        // Only return content if there are actual parts
        // This prevents empty Content from being accumulated in conversation history
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) }
    });

    let finish_reason = chunk.choices.first().and_then(|c| c.finish_reason).map(|fr| match fr {
        async_openai::types::FinishReason::Stop => FinishReason::Stop,
        async_openai::types::FinishReason::Length => FinishReason::MaxTokens,
        async_openai::types::FinishReason::ToolCalls => FinishReason::Stop,
        async_openai::types::FinishReason::ContentFilter => FinishReason::Safety,
        async_openai::types::FinishReason::FunctionCall => FinishReason::Stop,
    });

    let is_final = chunk.choices.first().map(|c| c.finish_reason.is_some()).unwrap_or(false);

    LlmResponse {
        content,
        usage_metadata: None, // Streaming chunks don't have usage info
        finish_reason,
        citation_metadata: None,
        partial: !is_final,
        turn_complete: is_final,
        interrupted: false,
        error_code: None,
        error_message: None,
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
                Part::InlineData {
                    mime_type: "image/png".to_string(),
                    data: vec![0x89, 0x50, 0x4E, 0x47], // PNG magic bytes
                },
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
                Part::InlineData { mime_type: "image/jpeg".to_string(), data: vec![0xFF, 0xD8] },
                Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x89, 0x50] },
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
                Part::InlineData {
                    mime_type: "audio/wav".to_string(),
                    data: vec![0x52, 0x49, 0x46, 0x46],
                },
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
            parts: vec![Part::InlineData {
                mime_type: "application/pdf".to_string(),
                data: b"%PDF".to_vec(),
            }],
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
    fn test_convert_tools() {
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

        let openai_tools = convert_tools(&tools);
        assert_eq!(openai_tools.len(), 1);
        assert_eq!(openai_tools[0].function.name, "get_weather");
    }
}
