//! Type conversions between ADK and Claudius types.

use super::error::ConversionError;
use crate::attachment;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use claudius::ImageMediaType;
use claudius::{
    Base64ImageSource, Base64PdfSource, CacheControlEphemeral, ContentBlock, DocumentBlock,
    ImageBlock, Message, MessageCreateParams, MessageParam, MessageRole, Model, PlainTextSource,
    StopReason, SystemPrompt, TextBlock, ToolParam, ToolResultBlock, ToolResultBlockContent,
    ToolUnionParam, ToolUseBlock, UrlPdfSource,
};
use serde_json::Value;
use std::collections::HashMap;

/// Convert ADK Content to Claudius MessageParam.
///
/// When `prompt_caching` is true, eligible content blocks will have
/// `cache_control: {"type": "ephemeral"}` set on them.
///
/// Returns `Err(ConversionError::UnsupportedMimeType)` if any part contains
/// an unsupported MIME type for `InlineData` or `FileData`.
pub fn content_to_message(
    content: &Content,
    prompt_caching: bool,
) -> Result<MessageParam, ConversionError> {
    let role = match content.role.as_str() {
        "user" | "function" | "tool" => MessageRole::User,
        "model" | "assistant" => MessageRole::Assistant,
        _ => MessageRole::User,
    };

    let cache = if prompt_caching { Some(CacheControlEphemeral::new()) } else { None };

    let blocks: Vec<ContentBlock> = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => {
                if text.is_empty() {
                    None
                } else {
                    let mut block = TextBlock::new(text.clone());
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    Some(ContentBlock::Text(block))
                }
            }
            Part::FunctionCall { name, args, id, .. } => {
                let mut block = ToolUseBlock {
                    id: id.clone().unwrap_or_else(|| format!("call_{name}")),
                    name: name.clone(),
                    input: args.clone(),
                    cache_control: None,
                };
                if let Some(ref cc) = cache {
                    block = block.with_cache_control(cc.clone());
                }
                Some(ContentBlock::ToolUse(block))
            }
            Part::FunctionResponse { function_response, id } => {
                Some(ContentBlock::ToolResult(ToolResultBlock {
                    tool_use_id: id.clone().unwrap_or_else(|| "unknown".to_string()),
                    content: Some(ToolResultBlockContent::String(
                        serde_json::to_string(&function_response.response).unwrap_or_default(),
                    )),
                    is_error: None,
                    cache_control: None,
                }))
            }
            Part::InlineData { mime_type, data } => {
                let media_type = match mime_type.as_str() {
                    "image/jpeg" => Some(ImageMediaType::Jpeg),
                    "image/png" => Some(ImageMediaType::Png),
                    "image/gif" => Some(ImageMediaType::Gif),
                    "image/webp" => Some(ImageMediaType::Webp),
                    _ => None,
                };
                if let Some(media_type) = media_type {
                    let encoded = attachment::encode_base64(data);
                    Some(ContentBlock::Image(ImageBlock::new_with_base64(Base64ImageSource::new(
                        encoded, media_type,
                    ))))
                } else if mime_type == "application/pdf" {
                    let encoded = attachment::encode_base64(data);
                    Some(ContentBlock::Document(DocumentBlock::new_with_base64_pdf(
                        Base64PdfSource::new(encoded),
                    )))
                } else if mime_type.starts_with("text/") {
                    match String::from_utf8(data.clone()) {
                        Ok(text) => Some(ContentBlock::Document(
                            DocumentBlock::new_with_plain_text(PlainTextSource::new(text)),
                        )),
                        Err(_) => Some(ContentBlock::Text(TextBlock::new(
                            attachment::inline_attachment_to_text(mime_type, data),
                        ))),
                    }
                } else {
                    Some(ContentBlock::Text(TextBlock::new(attachment::inline_attachment_to_text(
                        mime_type, data,
                    ))))
                }
            }
            Part::FileData { mime_type, file_uri } => {
                if mime_type == "application/pdf" {
                    Some(ContentBlock::Document(DocumentBlock::new_with_url_pdf(
                        UrlPdfSource::new(file_uri.clone()),
                    )))
                } else {
                    Some(ContentBlock::Text(TextBlock::new(attachment::file_attachment_to_text(
                        mime_type, file_uri,
                    ))))
                }
            }
            Part::Thinking { thinking, .. } => {
                if thinking.is_empty() {
                    None
                } else {
                    Some(ContentBlock::Text(TextBlock::new(thinking.clone())))
                }
            }
        })
        .collect();

    // If no blocks, add a placeholder for assistant messages
    let blocks = if blocks.is_empty() && role == MessageRole::Assistant {
        vec![ContentBlock::Text(TextBlock::new(" ".to_string()))]
    } else if blocks.is_empty() {
        vec![ContentBlock::Text(TextBlock::new("".to_string()))]
    } else {
        blocks
    };

    Ok(MessageParam::new_with_blocks(blocks, role))
}

/// Convert ADK tools to Claudius ToolUnionParam format.
pub fn convert_tools(tools: &HashMap<String, Value>) -> Vec<ToolUnionParam> {
    tools
        .iter()
        .map(|(name, decl)| {
            let description = decl.get("description").and_then(|d| d.as_str()).map(String::from);

            let input_schema = decl.get("parameters").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));

            let mut tool_param = ToolParam::new(name.clone(), input_schema);
            if let Some(desc) = description {
                tool_param = tool_param.with_description(desc);
            }

            ToolUnionParam::CustomTool(tool_param)
        })
        .collect()
}

/// Convert Claudius Message to ADK LlmResponse.
pub fn from_anthropic_message(message: &Message) -> (LlmResponse, HashMap<String, String>) {
    let mut parts = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text(text_block) => {
                if !text_block.text.is_empty() {
                    parts.push(Part::Text { text: text_block.text.clone() });
                }
            }
            ContentBlock::ToolUse(tool_use) => {
                parts.push(Part::FunctionCall {
                    name: tool_use.name.clone(),
                    args: tool_use.input.clone(),
                    id: Some(tool_use.id.clone()),
                    thought_signature: None,
                });
            }
            ContentBlock::Thinking(thinking_block) => {
                if !thinking_block.thinking.is_empty() {
                    parts.push(Part::Thinking {
                        thinking: thinking_block.thinking.clone(),
                        signature: if thinking_block.signature.is_empty() {
                            None
                        } else {
                            Some(thinking_block.signature.clone())
                        },
                    });
                }
            }
            _ => {}
        }
    }

    let content =
        if parts.is_empty() { None } else { Some(Content { role: "model".to_string(), parts }) };

    let usage_metadata = Some(UsageMetadata {
        prompt_token_count: message.usage.input_tokens,
        candidates_token_count: message.usage.output_tokens,
        total_token_count: (message.usage.input_tokens + message.usage.output_tokens),
        cache_read_input_token_count: message.usage.cache_read_input_tokens,
        cache_creation_input_token_count: message.usage.cache_creation_input_tokens,
        ..Default::default()
    });

    let finish_reason = message.stop_reason.as_ref().map(|sr| match sr {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::StopSequence => FinishReason::Stop,
        StopReason::ToolUse => FinishReason::Stop,
        _ => FinishReason::Stop,
    });

    let cache_meta = extract_cache_usage(&message.usage);

    (
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
        },
        cache_meta,
    )
}

/// Convert streaming text delta to ADK LlmResponse.
pub fn from_text_delta(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        }),
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: true,
        turn_complete: false,
        interrupted: false,
        error_code: None,
        error_message: None,
    }
}

/// Convert streaming thinking delta to ADK LlmResponse.
pub fn from_thinking_delta(thinking_text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Thinking { thinking: thinking_text.to_string(), signature: None }],
        }),
        partial: true,
        turn_complete: false,
        ..Default::default()
    }
}

/// Create an LlmResponse representing a streaming error event.
pub fn from_stream_error(error_type: &str, message: &str) -> LlmResponse {
    LlmResponse {
        content: None,
        usage_metadata: None,
        finish_reason: None,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: Some(error_type.to_string()),
        error_message: Some(message.to_string()),
    }
}

/// Extract cache usage tokens from a claudius `Usage` into provider metadata.
pub fn extract_cache_usage(usage: &claudius::Usage) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    if let Some(tokens) = usage.cache_creation_input_tokens {
        metadata.insert("anthropic.cache_creation_input_tokens".to_string(), tokens.to_string());
    }
    if let Some(tokens) = usage.cache_read_input_tokens {
        metadata.insert("anthropic.cache_read_input_tokens".to_string(), tokens.to_string());
    }
    metadata
}

/// Create final response with tool calls.
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

/// Build MessageCreateParams from LlmRequest.
#[allow(clippy::too_many_arguments)]
pub fn build_message_params(
    model: &str,
    max_tokens: u32,
    messages: Vec<MessageParam>,
    tools: Vec<ToolUnionParam>,
    system_prompt: Option<String>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    top_k: Option<i32>,
    prompt_caching: bool,
    thinking: Option<&super::config::ThinkingConfig>,
) -> MessageCreateParams {
    let mut params =
        MessageCreateParams::new(max_tokens, messages, Model::Custom(model.to_string()));

    if !tools.is_empty() {
        params.tools = Some(tools);
    }

    if let Some(sys) = system_prompt {
        if prompt_caching {
            let block = TextBlock::new(sys).with_cache_control(CacheControlEphemeral::new());
            params.system = Some(SystemPrompt::from_blocks(vec![block]));
        } else {
            params.system = Some(SystemPrompt::from_string(sys));
        }
    }

    if let Some(temp) = temperature {
        params.temperature = Some(temp);
    }

    if let Some(p) = top_p {
        params.top_p = Some(p);
    }

    if let Some(k) = top_k {
        params.top_k = Some(k as u32);
    }

    if let Some(tc) = thinking {
        params.thinking = Some(claudius::ThinkingConfig::enabled(tc.budget_tokens));
    }

    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_to_message_user() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Hello".to_string() }],
        };
        let msg = content_to_message(&content, false).unwrap();
        assert!(matches!(msg.role, MessageRole::User));
    }

    #[test]
    fn test_content_to_message_assistant() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hi there".to_string() }],
        };
        let msg = content_to_message(&content, false).unwrap();
        assert!(matches!(msg.role, MessageRole::Assistant));
    }

    #[test]
    fn test_content_to_message_with_inline_image() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "What is in this image?".to_string() },
                Part::InlineData {
                    mime_type: "image/png".to_string(),
                    data: vec![0x89, 0x50, 0x4E, 0x47],
                },
            ],
        };
        let msg = content_to_message(&content, false).unwrap();
        assert!(matches!(msg.role, MessageRole::User));

        // Should have 2 blocks: text + image
        let json = serde_json::to_value(&msg).unwrap();
        let content_blocks = json["content"].as_array().unwrap();
        assert_eq!(content_blocks.len(), 2);
        assert_eq!(content_blocks[0]["type"], "text");
        assert_eq!(content_blocks[0]["text"], "What is in this image?");
        assert_eq!(content_blocks[1]["type"], "image");
        assert_eq!(content_blocks[1]["source"]["type"], "base64");
        assert_eq!(content_blocks[1]["source"]["media_type"], "image/png");
        // Verify base64 data is present and non-empty
        assert!(!content_blocks[1]["source"]["data"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_content_to_message_unsupported_mime_type_falls_back_to_text() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Check this".to_string() },
                Part::InlineData {
                    mime_type: "audio/wav".to_string(), // Not supported by Anthropic images
                    data: vec![0x52, 0x49, 0x46, 0x46],
                },
            ],
        };
        let msg = content_to_message(&content, false).unwrap();

        // Audio part should be preserved as textual attachment fallback.
        let json = serde_json::to_value(&msg).unwrap();
        let content_blocks = json["content"].as_array().unwrap();
        assert_eq!(content_blocks.len(), 2);
        assert_eq!(content_blocks[0]["type"], "text");
        assert_eq!(content_blocks[1]["type"], "text");
        assert!(content_blocks[1]["text"].as_str().unwrap_or_default().contains("audio/wav"));
    }

    #[test]
    fn test_content_to_message_multiple_images() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![
                Part::Text { text: "Compare".to_string() },
                Part::InlineData { mime_type: "image/jpeg".to_string(), data: vec![0xFF, 0xD8] },
                Part::InlineData { mime_type: "image/webp".to_string(), data: vec![0x52, 0x49] },
            ],
        };
        let msg = content_to_message(&content, false).unwrap();

        let json = serde_json::to_value(&msg).unwrap();
        let content_blocks = json["content"].as_array().unwrap();
        assert_eq!(content_blocks.len(), 3); // 1 text + 2 images
        assert_eq!(content_blocks[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content_blocks[2]["source"]["media_type"], "image/webp");
    }

    #[test]
    fn test_content_to_message_pdf_inline_data_maps_to_document_block() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "application/pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            }],
        };
        let msg = content_to_message(&content, false).unwrap();

        let json = serde_json::to_value(&msg).unwrap();
        let content_blocks = json["content"].as_array().unwrap();
        assert_eq!(content_blocks.len(), 1);
        assert_eq!(content_blocks[0]["type"], "document");
        assert_eq!(content_blocks[0]["source"]["type"], "base64");
        assert_eq!(content_blocks[0]["source"]["media_type"], "application/pdf");
    }

    #[test]
    fn test_content_to_message_pdf_file_uri_maps_to_document_block() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "application/pdf".to_string(),
                file_uri: "https://example.com/test.pdf".to_string(),
            }],
        };
        let msg = content_to_message(&content, false).unwrap();

        let json = serde_json::to_value(&msg).unwrap();
        let content_blocks = json["content"].as_array().unwrap();
        assert_eq!(content_blocks.len(), 1);
        assert_eq!(content_blocks[0]["type"], "document");
        assert_eq!(content_blocks[0]["source"]["type"], "url");
        assert_eq!(content_blocks[0]["source"]["url"], "https://example.com/test.pdf");
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

        let claude_tools = convert_tools(&tools);
        assert_eq!(claude_tools.len(), 1);
    }

    #[test]
    fn test_from_anthropic_message_with_thinking_block() {
        use claudius::{ThinkingBlock, Usage};

        let message = Message {
            id: "msg_123".to_string(),
            model: Model::Custom("claude-3-5-sonnet-20241022".to_string()),
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Thinking(ThinkingBlock::new(
                    "Let me reason through this step by step...",
                    "sig_abc123",
                )),
                ContentBlock::Text(TextBlock::new("The answer is 42.")),
            ],
            stop_reason: Some(StopReason::EndTurn),
            stop_sequence: None,
            r#type: "message".to_string(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                server_tool_use: None,
            },
        };

        let (response, _cache_meta) = from_anthropic_message(&message);
        let content = response.content.expect("should have content");
        assert_eq!(content.parts.len(), 2);

        // First part should be Thinking
        assert!(content.parts[0].is_thinking());
        assert_eq!(
            content.parts[0].thinking_text(),
            Some("Let me reason through this step by step...")
        );

        // Second part should be Text
        assert!(!content.parts[1].is_thinking());
        assert_eq!(content.parts[1].text(), Some("The answer is 42."));
    }

    #[test]
    fn test_from_anthropic_message_empty_thinking_block_skipped() {
        use claudius::{ThinkingBlock, Usage};

        let message = Message {
            id: "msg_456".to_string(),
            model: Model::Custom("claude-3-5-sonnet-20241022".to_string()),
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Thinking(ThinkingBlock::new("", "sig_empty")),
                ContentBlock::Text(TextBlock::new("Just text.")),
            ],
            stop_reason: Some(StopReason::EndTurn),
            stop_sequence: None,
            r#type: "message".to_string(),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 10,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
                server_tool_use: None,
            },
        };

        let (response, _) = from_anthropic_message(&message);
        let content = response.content.expect("should have content");
        // Empty thinking block should be skipped
        assert_eq!(content.parts.len(), 1);
        assert_eq!(content.parts[0].text(), Some("Just text."));
    }
}
