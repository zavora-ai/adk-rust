//! Type conversions between ADK and Claudius types.

use super::error::ConversionError;
use adk_core::{Content, FinishReason, LlmResponse, Part, UsageMetadata};
use base64::Engine;
use claudius::{
    Base64ImageSource, Base64PdfSource, CacheControlEphemeral, ContentBlock, DocumentBlock,
    ImageBlock, ImageMediaType, Message, MessageCreateParams, MessageParam, MessageRole, Model,
    StopReason, SystemPrompt, TextBlock, ToolParam, ToolResultBlock, ToolResultBlockContent,
    ToolUnionParam, ToolUseBlock, UrlImageSource, UrlPdfSource,
};
use serde_json::Value;
use std::collections::HashMap;

/// Map a MIME type string to the claudius `ImageMediaType` enum.
///
/// Returns `None` if the MIME type is not a supported image format.
fn mime_to_image_media_type(mime: &str) -> Option<ImageMediaType> {
    match mime {
        "image/jpeg" => Some(ImageMediaType::Jpeg),
        "image/png" => Some(ImageMediaType::Png),
        "image/gif" => Some(ImageMediaType::Gif),
        "image/webp" => Some(ImageMediaType::Webp),
        _ => None,
    }
}

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

    let mut blocks: Vec<ContentBlock> = Vec::new();

    let cache = if prompt_caching { Some(CacheControlEphemeral::new()) } else { None };

    for part in &content.parts {
        match part {
            Part::Text { text } => {
                if !text.is_empty() {
                    let mut block = TextBlock::new(text.clone());
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    blocks.push(ContentBlock::Text(block));
                }
            }
            Part::FunctionCall { name, args, id } => {
                let mut block = ToolUseBlock {
                    id: id.clone().unwrap_or_else(|| format!("call_{name}")),
                    name: name.clone(),
                    input: args.clone(),
                    cache_control: None,
                };
                if let Some(ref cc) = cache {
                    block = block.with_cache_control(cc.clone());
                }
                blocks.push(ContentBlock::ToolUse(block));
            }
            Part::FunctionResponse { function_response, id } => {
                let mut block = ToolResultBlock {
                    tool_use_id: id.clone().unwrap_or_else(|| "unknown".to_string()),
                    content: Some(ToolResultBlockContent::String(
                        serde_json::to_string(&function_response.response).unwrap_or_default(),
                    )),
                    is_error: None,
                    cache_control: None,
                };
                if let Some(ref cc) = cache {
                    block = block.with_cache_control(cc.clone());
                }
                blocks.push(ContentBlock::ToolResult(block));
            }
            Part::InlineData { mime_type, data } => {
                if let Some(media_type) = mime_to_image_media_type(mime_type) {
                    // Requirement 2.1: image/* → image block with base64 source
                    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                    let mut block =
                        ImageBlock::new_with_base64(Base64ImageSource::new(b64, media_type));
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    blocks.push(ContentBlock::Image(block));
                } else if mime_type == "application/pdf" {
                    // Requirement 2.3: application/pdf → document block with base64 source
                    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                    let mut block = DocumentBlock::new_with_base64_pdf(Base64PdfSource::new(b64));
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    blocks.push(ContentBlock::Document(block));
                } else {
                    // Requirement 2.5: unsupported MIME type → error
                    return Err(ConversionError::UnsupportedMimeType(mime_type.clone()));
                }
            }
            Part::FileData { mime_type, file_uri } => {
                if mime_type.starts_with("image/") {
                    // Requirement 2.2: image/* → image block with URL source
                    let mut block = ImageBlock::new_with_url(UrlImageSource::new(file_uri.clone()));
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    blocks.push(ContentBlock::Image(block));
                } else if mime_type == "application/pdf" {
                    // Requirement 2.4: application/pdf → document block with URL source
                    let mut block =
                        DocumentBlock::new_with_url_pdf(UrlPdfSource::new(file_uri.clone()));
                    if let Some(ref cc) = cache {
                        block = block.with_cache_control(cc.clone());
                    }
                    blocks.push(ContentBlock::Document(block));
                } else {
                    // Requirement 2.5: unsupported MIME type → error
                    return Err(ConversionError::UnsupportedMimeType(mime_type.clone()));
                }
            }
        }
    }

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
///
/// When the response includes cache usage tokens (`cache_creation_input_tokens`,
/// `cache_read_input_tokens`), they are returned as the second element of the
/// tuple as a `HashMap` of provider metadata.
pub fn from_anthropic_message(message: &Message) -> (LlmResponse, HashMap<String, String>) {
    let mut parts = Vec::new();
    let mut provider_metadata = HashMap::new();

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
                });
            }
            // Requirement 7.2: Include thinking text as a distinguishable part
            ContentBlock::Thinking(thinking_block) => {
                if !thinking_block.thinking.is_empty() {
                    parts.push(Part::Text {
                        text: format!("<thinking>{}</thinking>", thinking_block.thinking),
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
    });

    // Requirement 6.3: Extract cache usage tokens into provider metadata
    if let Some(tokens) = message.usage.cache_creation_input_tokens {
        provider_metadata
            .insert("anthropic.cache_creation_input_tokens".to_string(), tokens.to_string());
    }
    if let Some(tokens) = message.usage.cache_read_input_tokens {
        provider_metadata
            .insert("anthropic.cache_read_input_tokens".to_string(), tokens.to_string());
    }

    let finish_reason = message.stop_reason.as_ref().map(|sr| match sr {
        StopReason::EndTurn => FinishReason::Stop,
        StopReason::MaxTokens => FinishReason::MaxTokens,
        StopReason::StopSequence => FinishReason::Stop,
        StopReason::ToolUse => FinishReason::Stop,
        _ => FinishReason::Stop,
    });

    let response = LlmResponse {
        content,
        usage_metadata,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted: false,
        error_code: None,
        error_message: None,
    };

    (response, provider_metadata)
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
///
/// Wraps thinking text in `<thinking>` tags to distinguish it from regular text content.
/// Sets `partial: true` and `turn_complete: false` since thinking deltas are incremental.
pub fn from_thinking_delta(thinking_text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: format!("<thinking>{thinking_text}</thinking>") }],
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
///
/// Returns a `HashMap` with `anthropic.cache_creation_input_tokens` and
/// `anthropic.cache_read_input_tokens` entries when present in the usage data.
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
        .map(|(id, name, args)| Part::FunctionCall { name, args, id: Some(id) })
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
            // Requirement 6.2: When prompt caching is enabled, use block-based system
            // prompt with cache_control on each block.
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

    // Requirement 7.1: When thinking is enabled, add thinking parameter
    // Requirement 7.4: When thinking is not enabled, omit the thinking parameter
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
    fn test_inline_data_image_jpeg() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "image/jpeg".to_string(),
                data: vec![0xFF, 0xD8, 0xFF],
            }],
        };
        let msg = content_to_message(&content, false).unwrap();
        assert!(matches!(msg.role, MessageRole::User));
        if let claudius::MessageParamContent::Array(blocks) = &msg.content {
            assert_eq!(blocks.len(), 1);
            assert!(blocks[0].is_image());
        } else {
            panic!("Expected array content");
        }
    }

    #[test]
    fn test_inline_data_pdf() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "application/pdf".to_string(),
                data: vec![0x25, 0x50, 0x44, 0x46],
            }],
        };
        let msg = content_to_message(&content, false).unwrap();
        if let claudius::MessageParamContent::Array(blocks) = &msg.content {
            assert_eq!(blocks.len(), 1);
            assert!(blocks[0].is_document());
        } else {
            panic!("Expected array content");
        }
    }

    #[test]
    fn test_file_data_image_url() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "image/png".to_string(),
                file_uri: "https://example.com/img.png".to_string(),
            }],
        };
        let msg = content_to_message(&content, false).unwrap();
        if let claudius::MessageParamContent::Array(blocks) = &msg.content {
            assert_eq!(blocks.len(), 1);
            assert!(blocks[0].is_image());
        } else {
            panic!("Expected array content");
        }
    }

    #[test]
    fn test_file_data_pdf_url() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "application/pdf".to_string(),
                file_uri: "https://example.com/doc.pdf".to_string(),
            }],
        };
        let msg = content_to_message(&content, false).unwrap();
        if let claudius::MessageParamContent::Array(blocks) = &msg.content {
            assert_eq!(blocks.len(), 1);
            assert!(blocks[0].is_document());
        } else {
            panic!("Expected array content");
        }
    }

    #[test]
    fn test_unsupported_mime_type_inline_data() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData { mime_type: "audio/wav".to_string(), data: vec![0x00] }],
        };
        let err = content_to_message(&content, false).unwrap_err();
        assert_eq!(err, ConversionError::UnsupportedMimeType("audio/wav".to_string()));
    }

    #[test]
    fn test_unsupported_mime_type_file_data() {
        let content = Content {
            role: "user".to_string(),
            parts: vec![Part::FileData {
                mime_type: "video/mp4".to_string(),
                file_uri: "https://example.com/video.mp4".to_string(),
            }],
        };
        let err = content_to_message(&content, false).unwrap_err();
        assert_eq!(err, ConversionError::UnsupportedMimeType("video/mp4".to_string()));
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

    // ---- Property-based tests for multimodal conversion ----

    use proptest::prelude::*;

    /// Generator for supported image MIME types.
    fn arb_image_mime() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("image/jpeg".to_string()),
            Just("image/png".to_string()),
            Just("image/gif".to_string()),
            Just("image/webp".to_string()),
        ]
    }

    /// Generator for arbitrary byte data (1..256 bytes).
    fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(any::<u8>(), 1..256)
    }

    /// Generator for arbitrary URI strings.
    fn arb_uri() -> impl Strategy<Value = String> {
        "https://[a-z]{3,12}\\.[a-z]{2,4}/[a-z0-9_-]{1,20}\\.(jpg|png|gif|webp|pdf)"
            .prop_map(String::from)
    }

    /// Generator for MIME types that are NOT image/* and NOT application/pdf.
    fn arb_unsupported_mime() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("audio/wav".to_string()),
            Just("audio/mp3".to_string()),
            Just("video/mp4".to_string()),
            Just("text/plain".to_string()),
            Just("application/json".to_string()),
            Just("application/octet-stream".to_string()),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// **Feature: anthropic-deep-integration, Property 3: InlineData multimodal conversion**
        /// *For any* Part::InlineData with a supported MIME type (image/* or application/pdf)
        /// and arbitrary byte data, `content_to_message` SHALL produce a content block whose
        /// type matches the MIME category (image block for image/*, document block for
        /// application/pdf) with source type "base64" and data equal to the base64 encoding
        /// of the input bytes.
        /// **Validates: Requirements 2.1, 2.3**
        #[test]
        fn prop_inline_data_multimodal_conversion(
            mime in prop_oneof![arb_image_mime(), Just("application/pdf".to_string())],
            data in arb_bytes(),
        ) {
            let content = Content {
                role: "user".to_string(),
                parts: vec![Part::InlineData { mime_type: mime.clone(), data: data.clone() }],
            };

            let msg = content_to_message(&content, false).unwrap();
            let expected_b64 = base64::engine::general_purpose::STANDARD.encode(&data);

            if let claudius::MessageParamContent::Array(blocks) = &msg.content {
                prop_assert_eq!(blocks.len(), 1);

                let json = serde_json::to_value(&blocks[0]).unwrap();
                let source = &json["source"];

                prop_assert_eq!(source["type"].as_str().unwrap(), "base64");
                prop_assert_eq!(source["data"].as_str().unwrap(), expected_b64.as_str());

                if mime.starts_with("image/") {
                    prop_assert!(blocks[0].is_image(), "Expected image block for {mime}");
                } else {
                    prop_assert!(blocks[0].is_document(), "Expected document block for {mime}");
                }
            } else {
                prop_assert!(false, "Expected array content");
            }
        }

        /// **Feature: anthropic-deep-integration, Property 4: FileData multimodal conversion**
        /// *For any* Part::FileData with a supported MIME type (image/* or application/pdf)
        /// and an arbitrary URI string, `content_to_message` SHALL produce a content block
        /// whose type matches the MIME category (image block for image/*, document block for
        /// application/pdf) with source type "url" and url equal to the input URI.
        /// **Validates: Requirements 2.2, 2.4**
        #[test]
        fn prop_file_data_multimodal_conversion(
            mime in prop_oneof![arb_image_mime(), Just("application/pdf".to_string())],
            uri in arb_uri(),
        ) {
            let content = Content {
                role: "user".to_string(),
                parts: vec![Part::FileData { mime_type: mime.clone(), file_uri: uri.clone() }],
            };

            let msg = content_to_message(&content, false).unwrap();

            if let claudius::MessageParamContent::Array(blocks) = &msg.content {
                prop_assert_eq!(blocks.len(), 1);

                let json = serde_json::to_value(&blocks[0]).unwrap();
                let source = &json["source"];

                prop_assert_eq!(source["type"].as_str().unwrap(), "url");
                prop_assert_eq!(source["url"].as_str().unwrap(), uri.as_str());

                if mime.starts_with("image/") {
                    prop_assert!(blocks[0].is_image(), "Expected image block for {mime}");
                } else {
                    prop_assert!(blocks[0].is_document(), "Expected document block for {mime}");
                }
            } else {
                prop_assert!(false, "Expected array content");
            }
        }

        /// **Feature: anthropic-deep-integration, Property 5: Unsupported MIME type produces error**
        /// *For any* Part::InlineData or Part::FileData with a MIME type that does not start
        /// with "image/" and is not "application/pdf", `content_to_message` SHALL return an
        /// error containing the unsupported MIME type string.
        /// **Validates: Requirements 2.5**
        #[test]
        fn prop_unsupported_mime_type_produces_error(
            mime in arb_unsupported_mime(),
            use_inline in any::<bool>(),
        ) {
            let part = if use_inline {
                Part::InlineData { mime_type: mime.clone(), data: vec![0x00] }
            } else {
                Part::FileData { mime_type: mime.clone(), file_uri: "https://example.com/file".to_string() }
            };

            let content = Content {
                role: "user".to_string(),
                parts: vec![part],
            };

            let err = content_to_message(&content, false).unwrap_err();
            prop_assert_eq!(err, ConversionError::UnsupportedMimeType(mime));
        }

        // ---- Property-based tests for streaming events ----

        /// **Feature: anthropic-deep-integration, Property 6: Streaming thinking delta emission**
        /// *For any* thinking text string received as a `thinking_delta` streaming event,
        /// the emitted LlmResponse SHALL contain a Part::Text whose text includes the input
        /// thinking text, with `partial` set to true and `turn_complete` set to false.
        /// **Validates: Requirements 3.1**
        #[test]
        fn prop_streaming_thinking_delta_emission(
            thinking_text in "[A-Za-z0-9 .,!?:;'\\-]{1,200}",
        ) {
            let response = from_thinking_delta(&thinking_text);

            // partial must be true, turn_complete must be false
            prop_assert!(response.partial, "thinking delta must have partial=true");
            prop_assert!(!response.turn_complete, "thinking delta must have turn_complete=false");

            // Content must exist with role "model"
            let content = response.content.as_ref().expect("thinking delta must have content");
            prop_assert_eq!(&content.role, "model");

            // Must have exactly one Part::Text containing the thinking text
            prop_assert_eq!(content.parts.len(), 1);
            match &content.parts[0] {
                Part::Text { text } => {
                    prop_assert!(
                        text.contains(&thinking_text),
                        "Part::Text must contain the thinking text. Got: {text}"
                    );
                    // Verify wrapped in <thinking> tags
                    prop_assert!(
                        text.starts_with("<thinking>") && text.ends_with("</thinking>"),
                        "Thinking text must be wrapped in <thinking> tags. Got: {text}"
                    );
                }
                other => prop_assert!(false, "Expected Part::Text, got: {other:?}"),
            }

            // No error fields
            prop_assert!(response.error_code.is_none(), "thinking delta must not have error_code");
            prop_assert!(response.error_message.is_none(), "thinking delta must not have error_message");
        }

        /// **Feature: anthropic-deep-integration, Property 7: Streaming error event propagation**
        /// *For any* error event with an error type string and message string received during
        /// streaming, the emitted LlmResponse SHALL have `error_code` equal to the error type
        /// and `error_message` equal to the message.
        /// **Validates: Requirements 3.4**
        #[test]
        fn prop_streaming_error_event_propagation(
            error_type in prop_oneof![
                Just("invalid_request_error".to_string()),
                Just("authentication_error".to_string()),
                Just("rate_limit_error".to_string()),
                Just("api_error".to_string()),
                Just("overloaded_error".to_string()),
                "[a-z_]{3,30}".prop_map(String::from),
            ],
            message in "[A-Za-z0-9 .,!?:;'\\-]{1,200}",
        ) {
            let response = from_stream_error(&error_type, &message);

            // error_code must equal the error type
            prop_assert_eq!(
                response.error_code.as_deref(),
                Some(error_type.as_str()),
                "error_code must match error_type"
            );

            // error_message must equal the message
            prop_assert_eq!(
                response.error_message.as_deref(),
                Some(message.as_str()),
                "error_message must match message"
            );

            // turn_complete must be true for error events
            prop_assert!(response.turn_complete, "error event must have turn_complete=true");

            // partial must be false for error events
            prop_assert!(!response.partial, "error event must have partial=false");

            // No content for error events
            prop_assert!(response.content.is_none(), "error event must not have content");
        }
    }
}
