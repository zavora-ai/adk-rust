//! Conversion helpers for mapping ADK request/response types to OpenRouter chat types.

use super::chat::{
    OpenRouterChatContentPart, OpenRouterChatMessage, OpenRouterChatMessageContent,
    OpenRouterChatRequest, OpenRouterChatToolCall, OpenRouterChatToolFunction, OpenRouterPlugin,
    OpenRouterReasoningReplay,
};
use adk_core::{AdkError, Content, Part};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::json;

const FILE_PARSER_PLUGIN_ID: &str = "file-parser";

/// Convert ADK contents into OpenRouter chat messages with multimodal parts preserved.
pub fn adk_contents_to_chat_messages(
    contents: &[Content],
) -> Result<Vec<OpenRouterChatMessage>, AdkError> {
    contents.iter().map(adk_content_to_chat_message).collect()
}

/// Ensure the `file-parser` plugin is present when chat content includes file inputs.
pub fn augment_chat_plugins_for_contents(
    contents: &[Content],
    mut plugins: Vec<OpenRouterPlugin>,
) -> Vec<OpenRouterPlugin> {
    if contents.iter().any(content_uses_file_inputs)
        && !plugins.iter().any(|plugin| plugin.id == FILE_PARSER_PLUGIN_ID)
    {
        plugins.push(OpenRouterPlugin {
            id: FILE_PARSER_PLUGIN_ID.to_string(),
            enabled: Some(true),
            ..Default::default()
        });
    }

    plugins
}

/// Apply replayable reasoning state to a native chat request.
pub fn apply_reasoning_replay_to_chat_request(
    request: &mut OpenRouterChatRequest,
    replay: &OpenRouterReasoningReplay,
) {
    if replay.reasoning.is_some() {
        request.reasoning = replay.reasoning.clone();
    }
    if replay.reasoning_content.is_some() {
        request.reasoning_content = replay.reasoning_content.clone();
    }
    if replay.reasoning_details.is_some() {
        request.reasoning_details = replay.reasoning_details.clone();
    }
}

/// Convert native chat reasoning fields into ADK thinking parts plus provider metadata.
pub fn chat_message_reasoning_to_parts(
    message: &OpenRouterChatMessage,
) -> (Vec<Part>, Option<serde_json::Value>) {
    let mut parts = Vec::new();

    if let Some(reasoning) = message.reasoning.as_ref().filter(|reasoning| !reasoning.is_empty()) {
        parts.push(Part::Thinking { thinking: reasoning.clone(), signature: None });
    }

    let provider_metadata = message.reasoning_details.as_ref().map(|details| {
        serde_json::json!({
            "reasoning_details": details
        })
    });

    (parts, provider_metadata)
}

/// Convert replayable reasoning state into an extension-bag payload.
pub fn reasoning_replay_to_extension_value(
    replay: &OpenRouterReasoningReplay,
) -> serde_json::Value {
    serde_json::to_value(replay).unwrap_or(serde_json::Value::Null)
}

/// Recover replayable reasoning state from an extension-bag payload.
pub fn reasoning_replay_from_extension_value(
    value: &serde_json::Value,
) -> Option<OpenRouterReasoningReplay> {
    serde_json::from_value(value.clone()).ok()
}

fn adk_content_to_chat_message(content: &Content) -> Result<OpenRouterChatMessage, AdkError> {
    if matches!(content.role.as_str(), "function" | "tool") {
        return Ok(tool_response_chat_message(content));
    }

    let mut text_fragments = Vec::new();
    let mut structured_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for part in &content.parts {
        match part {
            Part::Text { text } => {
                push_text_fragment(text, &mut text_fragments, &mut structured_parts)
            }
            Part::Thinking { thinking, .. } => {
                push_text_fragment(thinking, &mut text_fragments, &mut structured_parts)
            }
            Part::InlineData { mime_type, data } => {
                flush_text_fragments(&mut text_fragments, &mut structured_parts);
                structured_parts.push(chat_part_from_inline_data(mime_type, data)?);
            }
            Part::FileData { mime_type, file_uri } => {
                flush_text_fragments(&mut text_fragments, &mut structured_parts);
                structured_parts.push(chat_part_from_file_uri(mime_type, file_uri));
            }
            Part::FunctionCall { name, args, id, .. } => {
                tool_calls.push(OpenRouterChatToolCall {
                    id: id.clone(),
                    kind: "function".to_string(),
                    function: Some(OpenRouterChatToolFunction {
                        name: Some(name.clone()),
                        arguments: Some(args.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
            }
            Part::FunctionResponse { function_response, .. } => {
                push_text_fragment(
                    &function_response.response.to_string(),
                    &mut text_fragments,
                    &mut structured_parts,
                );
            }
            // Server-side tool parts are Gemini-specific; skip for OpenRouter chat
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {}
        }
    }

    flush_text_fragments(&mut text_fragments, &mut structured_parts);

    let message_content = if structured_parts.is_empty() {
        None
    } else if structured_parts.len() == 1
        && structured_parts[0].kind == "text"
        && structured_parts[0].image_url.is_none()
        && structured_parts[0].input_audio.is_none()
        && structured_parts[0].video_url.is_none()
        && structured_parts[0].file.is_none()
    {
        structured_parts.pop().and_then(|part| part.text).map(OpenRouterChatMessageContent::Text)
    } else {
        Some(OpenRouterChatMessageContent::Parts(structured_parts))
    };

    Ok(OpenRouterChatMessage {
        role: normalize_role(&content.role).to_string(),
        content: message_content,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        ..Default::default()
    })
}

fn tool_response_chat_message(content: &Content) -> OpenRouterChatMessage {
    if let Some(Part::FunctionResponse { function_response, id }) = content.parts.first() {
        return OpenRouterChatMessage {
            role: "tool".to_string(),
            tool_call_id: id.clone().or_else(|| Some(format!("call_{}", function_response.name))),
            content: Some(OpenRouterChatMessageContent::Text(
                serde_json::to_string(&function_response.response).unwrap_or_default(),
            )),
            ..Default::default()
        };
    }

    OpenRouterChatMessage {
        role: "tool".to_string(),
        content: Some(OpenRouterChatMessageContent::Text(String::new())),
        ..Default::default()
    }
}

fn push_text_fragment(
    text: &str,
    text_fragments: &mut Vec<String>,
    structured_parts: &mut Vec<OpenRouterChatContentPart>,
) {
    if structured_parts.is_empty() {
        text_fragments.push(text.to_string());
    } else {
        structured_parts.push(text_chat_part(text));
    }
}

fn flush_text_fragments(
    text_fragments: &mut Vec<String>,
    structured_parts: &mut Vec<OpenRouterChatContentPart>,
) {
    if !text_fragments.is_empty() {
        structured_parts.push(text_chat_part(&text_fragments.join("\n\n")));
        text_fragments.clear();
    }
}

fn text_chat_part(text: &str) -> OpenRouterChatContentPart {
    OpenRouterChatContentPart {
        kind: "text".to_string(),
        text: Some(text.to_string()),
        ..Default::default()
    }
}

fn chat_part_from_inline_data(
    mime_type: &str,
    data: &[u8],
) -> Result<OpenRouterChatContentPart, AdkError> {
    let data_url = data_url(mime_type, data);

    if mime_type.starts_with("image/") {
        return Ok(OpenRouterChatContentPart {
            kind: "image_url".to_string(),
            image_url: Some(json!({
                "url": data_url,
                "detail": "auto"
            })),
            ..Default::default()
        });
    }

    if mime_type.starts_with("audio/") {
        return Ok(OpenRouterChatContentPart {
            kind: "input_audio".to_string(),
            input_audio: Some(json!({
                "data": BASE64_STANDARD.encode(data),
                "format": audio_format(mime_type)
            })),
            ..Default::default()
        });
    }

    if mime_type.starts_with("video/") {
        return Ok(OpenRouterChatContentPart {
            kind: "video_url".to_string(),
            video_url: Some(json!({ "url": data_url })),
            ..Default::default()
        });
    }

    Ok(OpenRouterChatContentPart {
        kind: "file".to_string(),
        file: Some(json!({
            "filename": default_filename(mime_type, None),
            "file_data": data_url
        })),
        ..Default::default()
    })
}

fn chat_part_from_file_uri(mime_type: &str, file_uri: &str) -> OpenRouterChatContentPart {
    if mime_type.starts_with("image/") {
        return OpenRouterChatContentPart {
            kind: "image_url".to_string(),
            image_url: Some(json!({
                "url": file_uri,
                "detail": "auto"
            })),
            ..Default::default()
        };
    }

    if mime_type.starts_with("video/") {
        return OpenRouterChatContentPart {
            kind: "video_url".to_string(),
            video_url: Some(json!({ "url": file_uri })),
            ..Default::default()
        };
    }

    if mime_type.starts_with("audio/") && file_uri.starts_with("data:") {
        return OpenRouterChatContentPart {
            kind: "input_audio".to_string(),
            input_audio: Some(json!({
                "data": data_url_payload(file_uri),
                "format": audio_format(mime_type)
            })),
            ..Default::default()
        };
    }

    OpenRouterChatContentPart {
        kind: "file".to_string(),
        file: Some(json!({
            "filename": default_filename(mime_type, Some(file_uri)),
            "file_data": file_uri
        })),
        ..Default::default()
    }
}

fn content_uses_file_inputs(content: &Content) -> bool {
    content.parts.iter().any(|part| match part {
        Part::InlineData { mime_type, .. } | Part::FileData { mime_type, .. } => {
            !mime_type.starts_with("image/")
                && !mime_type.starts_with("audio/")
                && !mime_type.starts_with("video/")
        }
        _ => false,
    })
}

fn normalize_role(role: &str) -> &str {
    match role {
        "model" => "assistant",
        other => other,
    }
}

fn data_url(mime_type: &str, data: &[u8]) -> String {
    format!("data:{mime_type};base64,{}", BASE64_STANDARD.encode(data))
}

fn data_url_payload(uri: &str) -> String {
    uri.split_once(',').map(|(_, payload)| payload.to_string()).unwrap_or_default()
}

fn audio_format(mime_type: &str) -> String {
    match mime_type.split('/').nth(1).unwrap_or("wav").trim().to_ascii_lowercase().as_str() {
        "mpeg" => "mp3".to_string(),
        "x-wav" => "wav".to_string(),
        other => other.to_string(),
    }
}

fn default_filename(mime_type: &str, file_uri: Option<&str>) -> String {
    if let Some(file_uri) = file_uri {
        if let Some(candidate) = file_uri.rsplit('/').next() {
            if !candidate.is_empty() && !candidate.contains(':') {
                return candidate.to_string();
            }
        }
    }

    let extension = match mime_type {
        "application/pdf" => "pdf",
        mime if mime.starts_with("image/") => mime.split('/').nth(1).unwrap_or("bin"),
        mime if mime.starts_with("audio/") => mime.split('/').nth(1).unwrap_or("bin"),
        mime if mime.starts_with("video/") => mime.split('/').nth(1).unwrap_or("bin"),
        _ => "bin",
    };

    format!("attachment.{extension}")
}

#[cfg(test)]
mod tests {
    use super::{adk_contents_to_chat_messages, augment_chat_plugins_for_contents};
    use crate::openrouter::chat::{
        OpenRouterChatMessage, OpenRouterChatMessageContent, OpenRouterChatRequest,
        OpenRouterImageConfig, OpenRouterReasoningConfig, OpenRouterReasoningReplay,
    };
    use adk_core::{Content, FunctionResponseData, Part};

    #[test]
    fn inline_image_maps_to_image_url_data_uri() {
        let messages = adk_contents_to_chat_messages(&[Content::new("user")
            .with_text("describe this")
            .with_inline_data("image/png", vec![0x89, 0x50, 0x4e, 0x47])])
        .expect("messages should convert");

        let parts = match messages[0].content.as_ref() {
            Some(OpenRouterChatMessageContent::Parts(parts)) => parts,
            other => panic!("expected structured parts, got {other:?}"),
        };

        assert_eq!(parts[0].kind, "text");
        assert_eq!(parts[1].kind, "image_url");
        assert_eq!(
            parts[1].image_url.as_ref().and_then(|value| value.get("url")),
            Some(&serde_json::json!("data:image/png;base64,iVBORw=="))
        );
    }

    #[test]
    fn pdf_inputs_add_file_parser_plugin_and_map_to_file_parts() {
        let contents = vec![
            Content::new("user")
                .with_text("summarize this")
                .with_file_uri("application/pdf", "https://example.com/report.pdf"),
        ];
        let messages = adk_contents_to_chat_messages(&contents).expect("messages should convert");
        let plugins = augment_chat_plugins_for_contents(&contents, Vec::new());

        let parts = match messages[0].content.as_ref() {
            Some(OpenRouterChatMessageContent::Parts(parts)) => parts,
            other => panic!("expected structured parts, got {other:?}"),
        };

        assert_eq!(parts[1].kind, "file");
        assert_eq!(
            parts[1].file.as_ref().and_then(|value| value.get("file_data")),
            Some(&serde_json::json!("https://example.com/report.pdf"))
        );
        assert!(plugins.iter().any(|plugin| plugin.id == "file-parser"));
    }

    #[test]
    fn inline_audio_and_video_uri_map_to_native_chat_parts() {
        let messages = adk_contents_to_chat_messages(&[Content::new("user")
            .with_inline_data("audio/mpeg", vec![1, 2, 3])
            .with_file_uri("video/mp4", "https://example.com/demo.mp4")])
        .expect("messages should convert");

        let parts = match messages[0].content.as_ref() {
            Some(OpenRouterChatMessageContent::Parts(parts)) => parts,
            other => panic!("expected structured parts, got {other:?}"),
        };

        assert_eq!(parts[0].kind, "input_audio");
        assert_eq!(
            parts[0].input_audio.as_ref().and_then(|value| value.get("format")),
            Some(&serde_json::json!("mp3"))
        );
        assert_eq!(parts[1].kind, "video_url");
        assert_eq!(
            parts[1].video_url.as_ref().and_then(|value| value.get("url")),
            Some(&serde_json::json!("https://example.com/demo.mp4"))
        );
    }

    #[test]
    fn tool_response_maps_to_tool_role_with_tool_call_id() {
        let messages = adk_contents_to_chat_messages(&[Content {
            role: "tool".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "get_weather".to_string(),
                    response: serde_json::json!({ "temperature": "22C" }),
                },
                id: Some("call_weather".to_string()),
            }],
        }])
        .expect("messages should convert");

        assert_eq!(messages[0].role, "tool");
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("call_weather"));
        assert_eq!(
            messages[0].content,
            Some(OpenRouterChatMessageContent::Text("{\"temperature\":\"22C\"}".to_string()))
        );
    }

    #[test]
    fn chat_request_serializes_modalities_and_image_config() {
        let request = OpenRouterChatRequest {
            model: "openai/gpt-5.2".to_string(),
            modalities: Some(vec!["text".to_string(), "image".to_string()]),
            image_config: Some(OpenRouterImageConfig {
                size: Some("1024x1024".to_string()),
                quality: Some("high".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let json = serde_json::to_value(&request).expect("request should serialize");

        assert_eq!(json["modalities"][1], "image");
        assert_eq!(json["image_config"]["size"], "1024x1024");
    }

    #[test]
    fn reasoning_replay_helpers_round_trip_and_apply_to_chat_request() {
        let replay = OpenRouterReasoningReplay {
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("high".to_string()),
                summary: Some("auto".to_string()),
                ..Default::default()
            }),
            reasoning_content: Some("Previous chain of thought".to_string()),
            reasoning_details: Some(serde_json::json!({
                "type": "summary",
                "text": "Previous chain of thought"
            })),
        };

        let extension = super::reasoning_replay_to_extension_value(&replay);
        let round_trip =
            super::reasoning_replay_from_extension_value(&extension).expect("replay should parse");
        let mut request = OpenRouterChatRequest::default();

        super::apply_reasoning_replay_to_chat_request(&mut request, &round_trip);

        assert_eq!(request.reasoning_content.as_deref(), Some("Previous chain of thought"));
        assert_eq!(
            request.reasoning.as_ref().and_then(|reasoning| reasoning.effort.as_deref()),
            Some("high")
        );
        assert_eq!(
            request.reasoning_details.as_ref().and_then(|details| details.get("type")),
            Some(&serde_json::json!("summary"))
        );
    }

    #[test]
    fn chat_message_reasoning_maps_to_thinking_and_provider_metadata() {
        let message = OpenRouterChatMessage {
            role: "assistant".to_string(),
            reasoning: Some("Step one, step two".to_string()),
            reasoning_details: Some(serde_json::json!({
                "encrypted_content": "abc123"
            })),
            ..Default::default()
        };

        let (parts, provider_metadata) = super::chat_message_reasoning_to_parts(&message);

        assert_eq!(
            parts,
            vec![adk_core::Part::Thinking {
                thinking: "Step one, step two".to_string(),
                signature: None
            }]
        );
        assert_eq!(
            provider_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("reasoning_details"))
                .and_then(|details| details.get("encrypted_content")),
            Some(&serde_json::json!("abc123"))
        );
    }
}
