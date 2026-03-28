//! Helpers for mapping OpenRouter-native metadata into ADK response metadata types.

use super::chat::{
    OpenRouterChatMessage, OpenRouterChatMessageContent, OpenRouterChatResponse,
    OpenRouterChatUsage,
};
use super::responses::{
    OpenRouterResponse, OpenRouterResponseOutputItem, OpenRouterResponsesUsage,
};
use adk_core::{CitationMetadata, CitationSource, UsageMetadata};
use serde_json::{Map, Value, json};

/// Convert OpenRouter chat usage into ADK usage metadata while preserving raw details.
pub fn chat_usage_to_metadata(usage: &OpenRouterChatUsage) -> UsageMetadata {
    let mut provider_usage = Map::new();

    if let Some(details) = usage.prompt_tokens_details.clone() {
        provider_usage.insert("prompt_tokens_details".to_string(), details);
    }
    if let Some(details) = usage.completion_tokens_details.clone() {
        provider_usage.insert("completion_tokens_details".to_string(), details);
    }
    if let Some(details) = usage.cost_details.clone() {
        provider_usage.insert("cost_details".to_string(), details);
    }
    if !usage.extra.is_empty() {
        provider_usage.insert("extra".to_string(), json!(usage.extra));
    }

    UsageMetadata {
        prompt_token_count: usage.prompt_tokens.unwrap_or(0) as i32,
        candidates_token_count: usage.completion_tokens.unwrap_or(0) as i32,
        total_token_count: usage.total_tokens.unwrap_or(0) as i32,
        cache_read_input_token_count: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("cached_tokens"))),
        cache_creation_input_token_count: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("cache_write_tokens"))),
        thinking_token_count: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("reasoning_tokens"))),
        audio_input_token_count: usage
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        audio_output_token_count: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        cost: usage.cost,
        is_byok: usage.is_byok,
        provider_usage: (!provider_usage.is_empty()).then_some(Value::Object(provider_usage)),
    }
}

/// Convert OpenRouter Responses API usage into ADK usage metadata while preserving raw details.
pub fn responses_usage_to_metadata(usage: &OpenRouterResponsesUsage) -> UsageMetadata {
    let mut provider_usage = Map::new();

    if let Some(details) = usage.input_tokens_details.clone() {
        provider_usage.insert("input_tokens_details".to_string(), details);
    }
    if let Some(details) = usage.output_tokens_details.clone() {
        provider_usage.insert("output_tokens_details".to_string(), details);
    }
    if let Some(details) = usage.cost_details.clone() {
        provider_usage.insert("cost_details".to_string(), details);
    }
    if !usage.extra.is_empty() {
        provider_usage.insert("extra".to_string(), json!(usage.extra));
    }

    UsageMetadata {
        prompt_token_count: usage.input_tokens.unwrap_or(0) as i32,
        candidates_token_count: usage.output_tokens.unwrap_or(0) as i32,
        total_token_count: usage.total_tokens.unwrap_or(0) as i32,
        cache_read_input_token_count: usage
            .input_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("cached_tokens"))),
        cache_creation_input_token_count: usage
            .input_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("cache_write_tokens"))),
        thinking_token_count: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("reasoning_tokens"))),
        audio_input_token_count: usage
            .input_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        audio_output_token_count: usage
            .output_tokens_details
            .as_ref()
            .and_then(|details| json_i32(details.get("audio_tokens"))),
        cost: usage.cost,
        is_byok: usage.is_byok,
        provider_usage: (!provider_usage.is_empty()).then_some(Value::Object(provider_usage)),
    }
}

/// Extract citation metadata from a chat response when annotations are present.
pub fn chat_response_citation_metadata(
    response: &OpenRouterChatResponse,
) -> Option<CitationMetadata> {
    let mut sources = Vec::new();

    for choice in &response.choices {
        for message in [choice.message.as_ref(), choice.delta.as_ref()].into_iter().flatten() {
            for annotation in chat_message_annotations(message) {
                if let Some(source) = annotation_to_citation_source(&annotation) {
                    push_unique_citation(&mut sources, source);
                }
            }
        }
    }

    (!sources.is_empty()).then_some(CitationMetadata { citation_sources: sources })
}

/// Extract citation metadata from a Responses API payload when annotations are present.
pub fn responses_citation_metadata(response: &OpenRouterResponse) -> Option<CitationMetadata> {
    let mut sources = Vec::new();

    for item in &response.output {
        for annotation in response_item_annotations(item) {
            if let Some(source) = annotation_to_citation_source(&annotation) {
                push_unique_citation(&mut sources, source);
            }
        }
    }

    (!sources.is_empty()).then_some(CitationMetadata { citation_sources: sources })
}

/// Preserve chat annotations and other unmappable metadata in provider metadata.
pub fn chat_response_provider_metadata(response: &OpenRouterChatResponse) -> Option<Value> {
    let mut metadata = Map::new();
    let annotations = response
        .choices
        .iter()
        .flat_map(|choice| {
            [choice.message.as_ref(), choice.delta.as_ref()]
                .into_iter()
                .flatten()
                .enumerate()
                .flat_map(move |(message_variant_index, message)| {
                    chat_message_annotations(message).into_iter().enumerate().map(
                        move |(annotation_index, annotation)| {
                            json!({
                                "choice_index": choice.index,
                                "message_variant_index": message_variant_index,
                                "annotation_index": annotation_index,
                                "annotation": annotation
                            })
                        },
                    )
                })
        })
        .collect::<Vec<_>>();

    if !annotations.is_empty() {
        metadata.insert("annotations".to_string(), Value::Array(annotations));
    }

    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

/// Preserve response annotations and server-tool usage in provider metadata.
pub fn responses_provider_metadata(response: &OpenRouterResponse) -> Option<Value> {
    let mut metadata = Map::new();
    let annotations =
        response.output.iter().flat_map(response_item_annotations_with_context).collect::<Vec<_>>();
    let server_tool_usage = response
        .output
        .iter()
        .filter(|item| {
            matches!(
                item.kind.as_str(),
                "web_search_call" | "file_search_call" | "image_generation_call"
            )
        })
        .map(|item| {
            json!({
                "item_id": item.id,
                "type": item.kind,
                "status": item.status,
                "name": item.name,
                "raw_item": serde_json::to_value(item).unwrap_or(Value::Null)
            })
        })
        .collect::<Vec<_>>();

    if !annotations.is_empty() {
        metadata.insert("annotations".to_string(), Value::Array(annotations));
    }
    if !server_tool_usage.is_empty() {
        metadata.insert("server_tool_usage".to_string(), Value::Array(server_tool_usage));
    }

    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

fn chat_message_annotations(message: &OpenRouterChatMessage) -> Vec<Value> {
    match message.content.as_ref() {
        Some(OpenRouterChatMessageContent::Parts(parts)) => {
            parts.iter().flat_map(|part| part.annotations.clone().unwrap_or_default()).collect()
        }
        _ => Vec::new(),
    }
}

fn response_item_annotations(item: &OpenRouterResponseOutputItem) -> Vec<Value> {
    let mut annotations = item.annotations.clone().unwrap_or_default();

    if let Some(Value::Array(content_parts)) = item.content.as_ref() {
        for part in content_parts {
            if let Some(Value::Array(part_annotations)) = part.get("annotations") {
                annotations.extend(part_annotations.iter().cloned());
            }
        }
    }

    annotations
}

fn response_item_annotations_with_context(item: &OpenRouterResponseOutputItem) -> Vec<Value> {
    let mut annotations = item
        .annotations
        .as_ref()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(annotation_index, annotation)| {
            json!({
                "item_id": item.id,
                "annotation_index": annotation_index,
                "annotation": annotation
            })
        })
        .collect::<Vec<_>>();

    if let Some(Value::Array(content_parts)) = item.content.as_ref() {
        for (content_index, part) in content_parts.iter().enumerate() {
            if let Some(Value::Array(part_annotations)) = part.get("annotations") {
                annotations.extend(part_annotations.iter().enumerate().map(
                    |(annotation_index, annotation)| {
                        json!({
                            "item_id": item.id,
                            "content_index": content_index,
                            "annotation_index": annotation_index,
                            "annotation": annotation
                        })
                    },
                ));
            }
        }
    }

    annotations
}

fn annotation_to_citation_source(annotation: &Value) -> Option<CitationSource> {
    match annotation.get("type").and_then(Value::as_str) {
        Some("url_citation") => Some(CitationSource {
            uri: annotation.get("url").and_then(Value::as_str).map(ToString::to_string),
            title: annotation.get("title").and_then(Value::as_str).map(ToString::to_string),
            start_index: json_i32(annotation.get("start_index")),
            end_index: json_i32(annotation.get("end_index")),
            license: None,
            publication_date: None,
        }),
        Some("file_citation") => Some(CitationSource {
            uri: annotation
                .get("file_id")
                .and_then(Value::as_str)
                .map(|file_id| format!("openrouter://file/{file_id}")),
            title: annotation.get("filename").and_then(Value::as_str).map(ToString::to_string),
            start_index: None,
            end_index: None,
            license: None,
            publication_date: None,
        }),
        _ => None,
    }
}

fn push_unique_citation(sources: &mut Vec<CitationSource>, source: CitationSource) {
    if !sources.contains(&source) {
        sources.push(source);
    }
}

fn json_i32(value: Option<&Value>) -> Option<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
        .or_else(|| value.and_then(Value::as_u64).and_then(|number| i32::try_from(number).ok()))
}

#[cfg(test)]
mod tests {
    use super::{
        chat_response_citation_metadata, chat_response_provider_metadata, chat_usage_to_metadata,
        responses_citation_metadata, responses_provider_metadata, responses_usage_to_metadata,
    };
    use crate::openrouter::chat::{
        OpenRouterChatChoice, OpenRouterChatContentPart, OpenRouterChatMessage,
        OpenRouterChatMessageContent, OpenRouterChatResponse, OpenRouterChatUsage,
    };
    use crate::openrouter::responses::{
        OpenRouterResponse, OpenRouterResponseOutputItem, OpenRouterResponsesUsage,
    };
    use serde_json::json;

    #[test]
    fn chat_usage_maps_cost_audio_video_reasoning_and_provider_usage() {
        let usage = OpenRouterChatUsage {
            prompt_tokens: Some(120),
            completion_tokens: Some(80),
            total_tokens: Some(200),
            prompt_tokens_details: Some(json!({
                "cached_tokens": 16,
                "cache_write_tokens": 8,
                "audio_tokens": 4,
                "video_tokens": 12
            })),
            completion_tokens_details: Some(json!({
                "reasoning_tokens": 22,
                "audio_tokens": 6
            })),
            cost: Some(0.0042),
            cost_details: Some(json!({
                "upstream_inference_input_cost": 0.0015,
                "upstream_inference_output_cost": 0.0027
            })),
            is_byok: Some(true),
            ..Default::default()
        };

        let mapped = chat_usage_to_metadata(&usage);

        assert_eq!(mapped.prompt_token_count, 120);
        assert_eq!(mapped.candidates_token_count, 80);
        assert_eq!(mapped.total_token_count, 200);
        assert_eq!(mapped.cache_read_input_token_count, Some(16));
        assert_eq!(mapped.cache_creation_input_token_count, Some(8));
        assert_eq!(mapped.audio_input_token_count, Some(4));
        assert_eq!(mapped.audio_output_token_count, Some(6));
        assert_eq!(mapped.thinking_token_count, Some(22));
        assert_eq!(mapped.cost, Some(0.0042));
        assert_eq!(mapped.is_byok, Some(true));
        assert_eq!(
            mapped
                .provider_usage
                .as_ref()
                .and_then(|usage| usage.get("prompt_tokens_details"))
                .and_then(|details| details.get("video_tokens")),
            Some(&json!(12))
        );
    }

    #[test]
    fn responses_usage_maps_cost_and_preserves_raw_usage_details() {
        let usage = OpenRouterResponsesUsage {
            input_tokens: Some(140),
            output_tokens: Some(60),
            total_tokens: Some(200),
            input_tokens_details: Some(json!({
                "cached_tokens": 18,
                "cache_write_tokens": 5,
                "audio_tokens": 3,
                "video_tokens": 7
            })),
            output_tokens_details: Some(json!({
                "reasoning_tokens": 14,
                "audio_tokens": 2
            })),
            cost: Some(0.0031),
            cost_details: Some(json!({
                "upstream_inference_cost": 0.0024
            })),
            is_byok: Some(false),
            ..Default::default()
        };

        let mapped = responses_usage_to_metadata(&usage);

        assert_eq!(mapped.prompt_token_count, 140);
        assert_eq!(mapped.candidates_token_count, 60);
        assert_eq!(mapped.total_token_count, 200);
        assert_eq!(mapped.cache_read_input_token_count, Some(18));
        assert_eq!(mapped.cache_creation_input_token_count, Some(5));
        assert_eq!(mapped.audio_input_token_count, Some(3));
        assert_eq!(mapped.audio_output_token_count, Some(2));
        assert_eq!(mapped.thinking_token_count, Some(14));
        assert_eq!(mapped.cost, Some(0.0031));
        assert_eq!(mapped.is_byok, Some(false));
        assert_eq!(
            mapped
                .provider_usage
                .as_ref()
                .and_then(|usage| usage.get("input_tokens_details"))
                .and_then(|details| details.get("video_tokens")),
            Some(&json!(7))
        );
    }

    #[test]
    fn chat_response_maps_url_and_file_annotations_into_citations_and_provider_metadata() {
        let response = OpenRouterChatResponse {
            choices: vec![OpenRouterChatChoice {
                index: Some(0),
                message: Some(OpenRouterChatMessage {
                    role: "assistant".to_string(),
                    content: Some(OpenRouterChatMessageContent::Parts(vec![
                        OpenRouterChatContentPart {
                            kind: "text".to_string(),
                            text: Some("Result".to_string()),
                            annotations: Some(vec![
                                json!({
                                    "type": "url_citation",
                                    "url": "https://openrouter.ai/docs",
                                    "title": "Docs",
                                    "start_index": 0,
                                    "end_index": 6
                                }),
                                json!({
                                    "type": "file_citation",
                                    "file_id": "file-123",
                                    "filename": "brief.pdf",
                                    "index": 0
                                }),
                            ]),
                            ..Default::default()
                        },
                    ])),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let citations =
            chat_response_citation_metadata(&response).expect("citations should be present");
        let provider_metadata =
            chat_response_provider_metadata(&response).expect("provider metadata should exist");

        assert_eq!(citations.citation_sources.len(), 2);
        assert_eq!(
            citations.citation_sources[0].uri.as_deref(),
            Some("https://openrouter.ai/docs")
        );
        assert_eq!(
            citations.citation_sources[1].uri.as_deref(),
            Some("openrouter://file/file-123")
        );
        assert_eq!(
            provider_metadata["annotations"][0]["annotation"]["type"],
            json!("url_citation")
        );
    }

    #[test]
    fn responses_metadata_maps_citations_and_preserves_server_tool_usage() {
        let response = OpenRouterResponse {
            output: vec![
                OpenRouterResponseOutputItem {
                    kind: "message".to_string(),
                    id: Some("msg-1".to_string()),
                    role: Some("assistant".to_string()),
                    content: Some(json!([
                        {
                            "type": "output_text",
                            "text": "OpenRouter docs",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://openrouter.ai/docs",
                                    "title": "OpenRouter Docs",
                                    "start_index": 0,
                                    "end_index": 14
                                },
                                {
                                    "type": "file_citation",
                                    "file_id": "file-abc",
                                    "filename": "report.pdf",
                                    "index": 0
                                },
                                {
                                    "type": "file_path",
                                    "file_id": "file-path",
                                    "index": 1
                                }
                            ]
                        }
                    ])),
                    ..Default::default()
                },
                OpenRouterResponseOutputItem {
                    kind: "web_search_call".to_string(),
                    id: Some("search-1".to_string()),
                    status: Some("completed".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let citations =
            responses_citation_metadata(&response).expect("citations should be present");
        let provider_metadata =
            responses_provider_metadata(&response).expect("provider metadata should exist");

        assert_eq!(citations.citation_sources.len(), 2);
        assert_eq!(citations.citation_sources[0].title.as_deref(), Some("OpenRouter Docs"));
        assert_eq!(
            citations.citation_sources[1].uri.as_deref(),
            Some("openrouter://file/file-abc")
        );
        assert_eq!(provider_metadata["server_tool_usage"][0]["type"], json!("web_search_call"));
        assert_eq!(provider_metadata["annotations"][2]["annotation"]["type"], json!("file_path"));
    }
}
