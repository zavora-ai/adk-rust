//! Conversion helpers for mapping ADK request/response types to OpenRouter Responses API types.

use super::responses::{
    OpenRouterResponseInput, OpenRouterResponseInputContent, OpenRouterResponseInputContentPart,
    OpenRouterResponseInputItem,
};
use super::{OpenRouterReasoningReplay, OpenRouterResponseOutputItem, OpenRouterResponsesRequest};
use adk_core::{AdkError, Content, Part};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

/// Convert ADK contents into OpenRouter Responses API input items.
pub fn adk_contents_to_response_input(
    contents: &[Content],
) -> Result<OpenRouterResponseInput, AdkError> {
    let items = contents.iter().map(adk_content_to_response_item).collect::<Result<Vec<_>, _>>()?;
    Ok(OpenRouterResponseInput::Items(items))
}

/// Apply replayable reasoning state to a native Responses API request.
pub fn apply_reasoning_replay_to_responses_request(
    request: &mut OpenRouterResponsesRequest,
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

/// Convert native Responses API reasoning items into ADK thinking parts plus provider metadata.
pub fn responses_reasoning_items_to_parts(
    items: &[OpenRouterResponseOutputItem],
) -> (Vec<Part>, Option<serde_json::Value>) {
    let mut parts = Vec::new();
    let mut metadata = Vec::new();

    for item in items.iter().filter(|item| item.kind == "reasoning") {
        let summary_text = item.summary.as_ref().map(|summary| {
            summary
                .iter()
                .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        });

        if let Some(summary_text) = summary_text.filter(|summary| !summary.is_empty()) {
            parts.push(Part::Thinking { thinking: summary_text, signature: None });
        }

        if let Some(reasoning_details) = item.reasoning_details.as_ref() {
            metadata.push(serde_json::json!({
                "item_id": item.id,
                "reasoning_details": reasoning_details
            }));
        }
    }

    let provider_metadata = (!metadata.is_empty()).then_some(serde_json::json!({
        "reasoning_items": metadata
    }));

    (parts, provider_metadata)
}

fn adk_content_to_response_item(
    content: &Content,
) -> Result<OpenRouterResponseInputItem, AdkError> {
    let mut text_fragments = Vec::new();
    let mut structured_parts = Vec::new();

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
                structured_parts.push(response_part_from_inline_data(mime_type, data));
            }
            Part::FileData { mime_type, file_uri } => {
                flush_text_fragments(&mut text_fragments, &mut structured_parts);
                structured_parts.push(response_part_from_file_uri(mime_type, file_uri));
            }
            Part::FunctionCall { name, args, id, .. } => {
                return Ok(OpenRouterResponseInputItem {
                    kind: "function_call".to_string(),
                    id: id.clone(),
                    call_id: id.clone().or_else(|| Some(format!("call_{name}"))),
                    name: Some(name.clone()),
                    arguments: Some(args.to_string()),
                    ..Default::default()
                });
            }
            Part::FunctionResponse { function_response, id } => {
                return Ok(OpenRouterResponseInputItem {
                    kind: "function_call_output".to_string(),
                    id: id.clone(),
                    call_id: id
                        .clone()
                        .or_else(|| Some(format!("call_{}", function_response.name))),
                    output: Some(function_response.response.to_string()),
                    ..Default::default()
                });
            }
            // Server-side tool parts are Gemini-specific; skip for OpenRouter responses
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {}
        }
    }

    flush_text_fragments(&mut text_fragments, &mut structured_parts);

    let message_content = if structured_parts.len() == 1 && structured_parts[0].kind == "input_text"
    {
        structured_parts.pop().and_then(|part| part.text).map(OpenRouterResponseInputContent::Text)
    } else {
        Some(OpenRouterResponseInputContent::Parts(structured_parts))
    };

    Ok(OpenRouterResponseInputItem {
        kind: "message".to_string(),
        role: Some(normalize_role(&content.role).to_string()),
        content: message_content,
        ..Default::default()
    })
}

fn push_text_fragment(
    text: &str,
    text_fragments: &mut Vec<String>,
    structured_parts: &mut Vec<OpenRouterResponseInputContentPart>,
) {
    if structured_parts.is_empty() {
        text_fragments.push(text.to_string());
    } else {
        structured_parts.push(text_response_part(text));
    }
}

fn flush_text_fragments(
    text_fragments: &mut Vec<String>,
    structured_parts: &mut Vec<OpenRouterResponseInputContentPart>,
) {
    if !text_fragments.is_empty() {
        structured_parts.push(text_response_part(&text_fragments.join("\n\n")));
        text_fragments.clear();
    }
}

fn text_response_part(text: &str) -> OpenRouterResponseInputContentPart {
    OpenRouterResponseInputContentPart {
        kind: "input_text".to_string(),
        text: Some(text.to_string()),
        ..Default::default()
    }
}

fn response_part_from_inline_data(
    mime_type: &str,
    data: &[u8],
) -> OpenRouterResponseInputContentPart {
    let data_url = data_url(mime_type, data);

    if mime_type.starts_with("image/") {
        return OpenRouterResponseInputContentPart {
            kind: "input_image".to_string(),
            image_url: Some(data_url),
            detail: Some("auto".to_string()),
            ..Default::default()
        };
    }

    if mime_type.starts_with("audio/") {
        return OpenRouterResponseInputContentPart {
            kind: "input_audio".to_string(),
            input_audio: Some(serde_json::json!({
                "data": BASE64_STANDARD.encode(data),
                "format": audio_format(mime_type)
            })),
            ..Default::default()
        };
    }

    OpenRouterResponseInputContentPart {
        kind: "input_file".to_string(),
        filename: Some(default_filename(mime_type, None)),
        file_data: Some(data_url),
        ..Default::default()
    }
}

fn response_part_from_file_uri(
    mime_type: &str,
    file_uri: &str,
) -> OpenRouterResponseInputContentPart {
    if mime_type.starts_with("image/") {
        return OpenRouterResponseInputContentPart {
            kind: "input_image".to_string(),
            image_url: Some(file_uri.to_string()),
            detail: Some("auto".to_string()),
            ..Default::default()
        };
    }

    if mime_type.starts_with("audio/") && file_uri.starts_with("data:") {
        return OpenRouterResponseInputContentPart {
            kind: "input_audio".to_string(),
            input_audio: Some(serde_json::json!({
                "data": data_url_payload(file_uri),
                "format": audio_format(mime_type)
            })),
            ..Default::default()
        };
    }

    OpenRouterResponseInputContentPart {
        kind: "input_file".to_string(),
        filename: Some(default_filename(mime_type, Some(file_uri))),
        file_url: Some(file_uri.to_string()),
        ..Default::default()
    }
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
    use super::adk_contents_to_response_input;
    use crate::openrouter::responses::{
        OpenRouterResponseInput, OpenRouterResponseOutputItem, OpenRouterResponsesRequest,
    };
    use crate::openrouter::{OpenRouterReasoningConfig, OpenRouterReasoningReplay};
    use adk_core::Content;

    #[test]
    fn responses_input_maps_image_pdf_audio_and_video_parts() {
        let input = adk_contents_to_response_input(&[Content::new("user")
            .with_inline_data("image/png", vec![0x89, 0x50, 0x4e, 0x47])
            .with_file_uri("application/pdf", "https://example.com/report.pdf")
            .with_inline_data("audio/wav", vec![1, 2, 3])
            .with_file_uri("video/mp4", "https://example.com/demo.mp4")])
        .expect("input should convert");

        let items = match input {
            OpenRouterResponseInput::Items(items) => items,
            other => panic!("expected item input, got {other:?}"),
        };
        let parts = match items[0].content.as_ref() {
            Some(crate::openrouter::responses::OpenRouterResponseInputContent::Parts(parts)) => {
                parts
            }
            other => panic!("expected structured parts, got {other:?}"),
        };

        assert_eq!(parts[0].kind, "input_image");
        assert_eq!(parts[1].kind, "input_file");
        assert_eq!(parts[2].kind, "input_audio");
        assert_eq!(parts[3].kind, "input_file");
        assert_eq!(parts[3].file_url.as_deref(), Some("https://example.com/demo.mp4"));
    }

    #[test]
    fn responses_request_serializes_modalities_and_image_config() {
        let request = OpenRouterResponsesRequest {
            modalities: Some(vec!["text".to_string(), "image".to_string()]),
            image_config: Some(crate::openrouter::chat::OpenRouterImageConfig {
                size: Some("1024x1024".to_string()),
                quality: Some("high".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let json = serde_json::to_value(&request).expect("request should serialize");

        assert_eq!(json["modalities"][1], "image");
        assert_eq!(json["image_config"]["quality"], "high");
    }

    #[test]
    fn reasoning_replay_applies_to_responses_request() {
        let replay = OpenRouterReasoningReplay {
            reasoning: Some(OpenRouterReasoningConfig {
                effort: Some("medium".to_string()),
                summary: Some("auto".to_string()),
                ..Default::default()
            }),
            reasoning_content: Some("Previous reasoning".to_string()),
            reasoning_details: Some(serde_json::json!({
                "compressed": true
            })),
        };
        let mut request = OpenRouterResponsesRequest::default();

        super::apply_reasoning_replay_to_responses_request(&mut request, &replay);

        assert_eq!(request.reasoning_content.as_deref(), Some("Previous reasoning"));
        assert_eq!(
            request.reasoning.as_ref().and_then(|reasoning| reasoning.effort.as_deref()),
            Some("medium")
        );
        assert_eq!(
            request.reasoning_details.as_ref().and_then(|details| details.get("compressed")),
            Some(&serde_json::json!(true))
        );
    }

    #[test]
    fn reasoning_items_map_to_thinking_parts_and_provider_metadata() {
        let items = vec![OpenRouterResponseOutputItem {
            kind: "reasoning".to_string(),
            id: Some("reasoning_1".to_string()),
            summary: Some(vec![
                serde_json::json!({
                    "type": "summary_text",
                    "text": "Investigated the source material."
                }),
                serde_json::json!({
                    "type": "summary_text",
                    "text": "Validated the conclusion."
                }),
            ]),
            reasoning_details: Some(serde_json::json!({
                "encrypted_content": "sealed"
            })),
            ..Default::default()
        }];

        let (parts, provider_metadata) = super::responses_reasoning_items_to_parts(&items);

        assert_eq!(
            parts,
            vec![adk_core::Part::Thinking {
                thinking: "Investigated the source material.\nValidated the conclusion."
                    .to_string(),
                signature: None
            }]
        );
        assert_eq!(
            provider_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("reasoning_items"))
                .and_then(|items| items.get(0))
                .and_then(|item| item.get("reasoning_details"))
                .and_then(|details| details.get("encrypted_content")),
            Some(&serde_json::json!("sealed"))
        );
    }
}
