//! Shared SSE frame decoding for OpenRouter-native streaming APIs.

use super::chat::OpenRouterChatResponse;
use super::responses::{OpenRouterResponse, OpenRouterResponseOutputItem};
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::pin::Pin;

type JsonMap = BTreeMap<String, serde_json::Value>;

/// One parsed SSE frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterSseFrame {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

/// Stream type returned by `send_chat_stream`.
pub type OpenRouterChatStream =
    Pin<Box<dyn Stream<Item = Result<OpenRouterChatStreamItem, AdkError>> + Send>>;

/// Stream type returned by `create_response_stream`.
pub type OpenRouterResponsesStream =
    Pin<Box<dyn Stream<Item = Result<OpenRouterResponsesStreamItem, AdkError>> + Send>>;

/// One chat-stream item emitted by the OpenRouter streaming client.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenRouterChatStreamItem {
    Chunk(Box<OpenRouterChatResponse>),
    Error(Box<OpenRouterStreamError>),
    Done,
}

/// One Responses-stream item emitted by the OpenRouter streaming client.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenRouterResponsesStreamItem {
    Event(Box<OpenRouterResponsesStreamEvent>),
    Error(Box<OpenRouterStreamError>),
    Done,
}

/// Stream-level error payload emitted during SSE decoding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterStreamError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<u64>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Generic typed Responses SSE event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OpenRouterResponsesStreamEvent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<OpenRouterResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<OpenRouterResponseOutputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation: Option<serde_json::Value>,
    #[serde(default, flatten)]
    pub extra: JsonMap,
}

/// Parse one complete SSE frame block into a typed frame.
pub fn parse_sse_frame_block(block: &str) -> Option<OpenRouterSseFrame> {
    let mut event = None;
    let mut data_lines = Vec::new();
    let mut id = None;
    let mut retry = None;

    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        let (field, raw_value) = line.split_once(':').unwrap_or((line, ""));
        let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);

        match field {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value.to_string()),
            "id" => id = Some(value.to_string()),
            "retry" => retry = value.parse::<u64>().ok(),
            _ => {}
        }
    }

    if event.is_none() && data_lines.is_empty() && id.is_none() && retry.is_none() {
        return None;
    }

    Some(OpenRouterSseFrame { event, data: data_lines.join("\n"), id, retry })
}

/// Parse a chat streaming frame into a typed stream item.
pub fn parse_chat_stream_frame(
    frame: &OpenRouterSseFrame,
) -> Result<Option<OpenRouterChatStreamItem>, AdkError> {
    if frame.data.trim().is_empty() {
        return Ok(None);
    }
    if frame.data.trim() == "[DONE]" {
        return Ok(Some(OpenRouterChatStreamItem::Done));
    }

    let value = parse_frame_json(
        &frame.data,
        "model.openrouter.chat_stream_invalid_json",
        "OpenRouter chat stream emitted invalid JSON",
    )?;

    if value.get("error").is_some() {
        return Ok(Some(OpenRouterChatStreamItem::Error(Box::new(parse_stream_error(&value)))));
    }

    let chunk = serde_json::from_value::<OpenRouterChatResponse>(value).map_err(|err| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.openrouter.chat_stream_invalid_chunk",
            "OpenRouter chat stream emitted a chunk that could not be parsed",
        )
        .with_provider("openrouter")
        .with_source(err)
    })?;

    Ok(Some(OpenRouterChatStreamItem::Chunk(Box::new(chunk))))
}

/// Parse a Responses streaming frame into a typed stream item.
pub fn parse_responses_stream_frame(
    frame: &OpenRouterSseFrame,
) -> Result<Option<OpenRouterResponsesStreamItem>, AdkError> {
    if frame.data.trim().is_empty() {
        return Ok(None);
    }
    if frame.data.trim() == "[DONE]" {
        return Ok(Some(OpenRouterResponsesStreamItem::Done));
    }

    let value = parse_frame_json(
        &frame.data,
        "model.openrouter.responses_stream_invalid_json",
        "OpenRouter responses stream emitted invalid JSON",
    )?;

    if value.get("type").and_then(|item| item.as_str()) == Some("error") {
        return Ok(Some(OpenRouterResponsesStreamItem::Error(Box::new(parse_stream_error(
            &value,
        )))));
    }

    let event = serde_json::from_value::<OpenRouterResponsesStreamEvent>(value).map_err(|err| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.openrouter.responses_stream_invalid_event",
            "OpenRouter responses stream emitted an event that could not be parsed",
        )
        .with_provider("openrouter")
        .with_source(err)
    })?;

    Ok(Some(OpenRouterResponsesStreamItem::Event(Box::new(event))))
}

/// Stateful SSE decoder that accepts arbitrarily chunked text input.
#[derive(Debug, Default)]
pub struct OpenRouterSseDecoder {
    buffer: String,
}

impl OpenRouterSseDecoder {
    /// Create a new decoder with an empty internal buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a UTF-8 text chunk into the decoder and return any completed SSE frames.
    pub fn push(&mut self, chunk: &str) -> Vec<OpenRouterSseFrame> {
        self.buffer.push_str(&chunk.replace("\r\n", "\n"));

        let mut frames = Vec::new();

        while let Some(delimiter_index) = self.buffer.find("\n\n") {
            let block = self.buffer[..delimiter_index].to_string();
            self.buffer.drain(..delimiter_index + 2);

            if let Some(frame) = parse_sse_frame_block(&block) {
                frames.push(frame);
            }
        }

        frames
    }

    /// Drain any remaining complete frame buffered in the decoder.
    pub fn finish(self) -> Vec<OpenRouterSseFrame> {
        if self.buffer.trim().is_empty() {
            return Vec::new();
        }

        parse_sse_frame_block(self.buffer.trim_end()).into_iter().collect()
    }
}

fn parse_frame_json(
    data: &str,
    code: &'static str,
    message: &'static str,
) -> Result<serde_json::Value, AdkError> {
    serde_json::from_str(data).map_err(|err| {
        AdkError::new(ErrorComponent::Model, ErrorCategory::Internal, code, message)
            .with_provider("openrouter")
            .with_source(err)
    })
}

fn parse_stream_error(value: &serde_json::Value) -> OpenRouterStreamError {
    let error = value.get("error").unwrap_or(value);
    let mut stream_error = OpenRouterStreamError::default();

    if let Some(message) = error.get("message").and_then(|item| item.as_str()) {
        stream_error.message = message.to_string();
    }
    if let Some(code) = error.get("code") {
        stream_error.code = Some(code.clone());
    }
    if let Some(param) = error.get("param").and_then(|item| item.as_str()) {
        stream_error.param = Some(param.to_string());
    }
    if let Some(error_type) = error.get("type").and_then(|item| item.as_str()) {
        stream_error.error_type = Some(error_type.to_string());
    }
    if let Some(provider_name) = error.get("provider_name").and_then(|item| item.as_str()) {
        stream_error.provider_name = Some(provider_name.to_string());
    }
    if let Some(metadata) = error.get("metadata") {
        stream_error.metadata = Some(metadata.clone());
    }
    if let Some(sequence_number) = value.get("sequence_number").and_then(|item| item.as_u64()) {
        stream_error.sequence_number = Some(sequence_number);
    }

    if let Some(object) = value.as_object() {
        stream_error.extra = object
            .iter()
            .filter(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "error"
                        | "message"
                        | "code"
                        | "param"
                        | "type"
                        | "provider_name"
                        | "metadata"
                        | "sequence_number"
                )
            })
            .map(|(key, item)| (key.clone(), item.clone()))
            .collect();
    }

    stream_error
}

#[cfg(test)]
mod tests {
    use super::{
        OpenRouterChatStreamItem, OpenRouterResponsesStreamItem, OpenRouterSseDecoder,
        OpenRouterSseFrame, parse_chat_stream_frame, parse_responses_stream_frame,
        parse_sse_frame_block,
    };
    use serde_json::json;

    #[test]
    fn sse_decoder_reassembles_chunked_frames() {
        let mut decoder = OpenRouterSseDecoder::new();
        let first = decoder.push("data: {\"type\":\"response.output_text.delta\"");
        let second = decoder.push(",\"delta\":\"hel\"}\n\n");

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].data, "{\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}");
    }

    #[test]
    fn parse_sse_frame_block_collects_multiline_data() {
        let frame = parse_sse_frame_block("event: message\ndata: first\ndata: second\nid: 1")
            .expect("frame should parse");

        assert_eq!(frame.event.as_deref(), Some("message"));
        assert_eq!(frame.data, "first\nsecond");
        assert_eq!(frame.id.as_deref(), Some("1"));
    }

    #[test]
    fn chat_stream_parser_handles_done_sentinel() {
        let item = parse_chat_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: "[DONE]".to_string(),
            id: None,
            retry: None,
        })
        .expect("frame should parse")
        .expect("done item should be emitted");

        assert!(matches!(item, OpenRouterChatStreamItem::Done));
    }

    #[test]
    fn chat_stream_parser_handles_error_frames() {
        let item = parse_chat_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: json!({
                "id": "chatcmpl-1",
                "created": 1,
                "model": "openai/gpt-5.2",
                "object": "chat.completion.chunk",
                "choices": [],
                "error": {
                    "message": "Rate limit exceeded",
                    "code": 429
                }
            })
            .to_string(),
            id: None,
            retry: None,
        })
        .expect("frame should parse")
        .expect("error item should be emitted");

        match item {
            OpenRouterChatStreamItem::Error(error) => {
                assert_eq!(error.message, "Rate limit exceeded");
                assert_eq!(error.code, Some(json!(429)));
                assert_eq!(error.error_type.as_deref(), None);
            }
            other => panic!("expected error item, got {other:?}"),
        }
    }

    #[test]
    fn chat_stream_parser_handles_partial_text_and_function_call_arguments() {
        let item = parse_chat_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: json!({
                "id": "chatcmpl-1",
                "created": 1,
                "model": "openai/gpt-5.2",
                "object": "chat.completion.chunk",
                "choices": [
                    {
                        "index": 0,
                        "delta": {
                            "role": "assistant",
                            "content": "hel",
                            "audio": {
                                "id": "aud_1",
                                "delta": "AAAA"
                            },
                            "tool_calls": [
                                {
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "lookup_weather",
                                        "arguments": "{\"city\":\"San"
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
            .to_string(),
            id: None,
            retry: None,
        })
        .expect("frame should parse")
        .expect("chunk should be emitted");

        match item {
            OpenRouterChatStreamItem::Chunk(chunk) => {
                let delta = chunk
                    .choices
                    .first()
                    .and_then(|choice| choice.delta.as_ref())
                    .expect("delta should exist");
                assert_eq!(delta.role, "assistant");
                assert_eq!(
                    delta.content.as_ref(),
                    Some(&crate::openrouter::chat::OpenRouterChatMessageContent::Text(
                        "hel".to_string(),
                    ))
                );
                assert_eq!(
                    delta
                        .tool_calls
                        .as_ref()
                        .and_then(|calls| calls.first())
                        .and_then(|call| call.function.as_ref())
                        .and_then(|function| function.arguments.as_deref()),
                    Some("{\"city\":\"San")
                );
                assert_eq!(
                    delta.extra.get("audio").and_then(|audio| audio.get("delta")),
                    Some(&json!("AAAA"))
                );
            }
            other => panic!("expected chunk item, got {other:?}"),
        }
    }

    #[test]
    fn chat_stream_parser_allows_partial_tool_call_chunks_without_name() {
        let item = parse_chat_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: json!({
                "id": "chatcmpl-1",
                "created": 1,
                "model": "openai/gpt-5.2",
                "object": "chat.completion.chunk",
                "choices": [
                    {
                        "index": 0,
                        "delta": {
                            "tool_calls": [
                                {
                                    "id": "call_1",
                                    "function": {
                                        "arguments": "{\"city\":\"Nai"
                                    }
                                }
                            ]
                        }
                    }
                ]
            })
            .to_string(),
            id: None,
            retry: None,
        })
        .expect("frame should parse")
        .expect("chunk should be emitted");

        match item {
            OpenRouterChatStreamItem::Chunk(chunk) => {
                let function = chunk
                    .choices
                    .first()
                    .and_then(|choice| choice.delta.as_ref())
                    .and_then(|delta| delta.tool_calls.as_ref())
                    .and_then(|calls| calls.first())
                    .and_then(|call| call.function.as_ref())
                    .expect("function should exist");

                assert_eq!(function.name.as_deref(), None);
                assert_eq!(function.arguments.as_deref(), Some("{\"city\":\"Nai"));
            }
            other => panic!("expected chunk item, got {other:?}"),
        }
    }

    #[test]
    fn responses_stream_parser_handles_reasoning_and_annotation_events() {
        let reasoning = parse_responses_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: json!({
                "type": "response.reasoning_text.delta",
                "output_index": 0,
                "item_id": "item-1",
                "content_index": 0,
                "delta": "Thinking...",
                "sequence_number": 4
            })
            .to_string(),
            id: None,
            retry: None,
        })
        .expect("reasoning frame should parse")
        .expect("reasoning event should be emitted");

        match reasoning {
            OpenRouterResponsesStreamItem::Event(event) => {
                assert_eq!(event.kind, "response.reasoning_text.delta");
                assert_eq!(event.delta.as_deref(), Some("Thinking..."));
            }
            other => panic!("expected reasoning event, got {other:?}"),
        }

        let annotation = parse_responses_stream_frame(&OpenRouterSseFrame {
            event: None,
            data: json!({
                "type": "response.output_text.annotation.added",
                "output_index": 0,
                "item_id": "item-1",
                "content_index": 0,
                "annotation_index": 0,
                "annotation": {
                    "type": "url_citation",
                    "url": "https://example.com",
                    "title": "Example"
                },
                "sequence_number": 5
            })
            .to_string(),
            id: None,
            retry: None,
        })
        .expect("annotation frame should parse")
        .expect("annotation event should be emitted");

        match annotation {
            OpenRouterResponsesStreamItem::Event(event) => {
                assert_eq!(event.kind, "response.output_text.annotation.added");
                assert_eq!(event.annotation_index, Some(0));
                assert_eq!(
                    event.annotation.as_ref().and_then(|annotation| annotation.get("type")),
                    Some(&json!("url_citation"))
                );
            }
            other => panic!("expected annotation event, got {other:?}"),
        }
    }
}
