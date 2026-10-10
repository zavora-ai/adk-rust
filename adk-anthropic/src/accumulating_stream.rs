//! Accumulates streaming events into a complete message while passing events through.

use std::pin::Pin;

use futures::Stream;
use serde_json::Value;

use crate::{
    CacheControlEphemeral, Citation, ContentBlock, ContentBlockDelta, Error, Message,
    MessageStreamEvent, ServerToolUseBlock, StopReason, TextBlock, TextCitation, ThinkingBlock,
    ToolUseBlock,
};

/// A stream wrapper that accumulates `MessageStreamEvent`s into a complete `Message`.
///
/// This allows streaming tokens to the user while simultaneously building the final message
/// without buffering. When the stream is fully drained, the accumulated message is sent via
/// the oneshot channel returned by `new()`.
///
/// A mid-stream SSE `error` event (such as `overloaded_error`) is yielded as an `Err` item
/// mapped through `From<ApiError> for Error`, and the same error is sent through the
/// channel instead of the partial message.
pub struct AccumulatingStream {
    inner: Pin<Box<dyn Stream<Item = Result<MessageStreamEvent, Error>> + Send>>,
    message_tx: Option<tokio::sync::oneshot::Sender<Result<Message, Error>>>,
    message: Option<Message>,
    content_blocks: Vec<ContentBlockBuilder>,
}

impl AccumulatingStream {
    /// Wraps a `MessageStreamEvent` stream to accumulate events into a `Message`.
    ///
    /// Returns the stream and a receiver that will contain the accumulated `Message` once the
    /// stream is fully drained.
    pub fn new<S>(stream: S) -> (Self, tokio::sync::oneshot::Receiver<Result<Message, Error>>)
    where
        S: Stream<Item = Result<MessageStreamEvent, Error>> + Send + 'static,
    {
        Self::new_with_message(stream, None)
    }

    /// Wraps a `MessageStreamEvent` stream and seeds accumulation with a fallback message.
    pub fn new_with_message<S>(
        stream: S,
        message: impl Into<Option<Message>>,
    ) -> (Self, tokio::sync::oneshot::Receiver<Result<Message, Error>>)
    where
        S: Stream<Item = Result<MessageStreamEvent, Error>> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let this = Self {
            inner: Box::pin(stream),
            message_tx: Some(tx),
            message: message.into(),
            content_blocks: Vec::new(),
        };
        (this, rx)
    }

    fn accumulate_event(&mut self, event: &MessageStreamEvent) {
        match event {
            MessageStreamEvent::MessageStart(start) => {
                self.message = Some(start.message.clone());
            }
            MessageStreamEvent::ContentBlockStart(start) => {
                let idx = start.index;
                while self.content_blocks.len() <= idx {
                    self.content_blocks.push(ContentBlockBuilder::Empty);
                }
                self.content_blocks[idx] =
                    ContentBlockBuilder::from_content_block(start.content_block.clone());
            }
            MessageStreamEvent::ContentBlockDelta(delta_event) => {
                let idx = delta_event.index;
                if idx < self.content_blocks.len() {
                    self.content_blocks[idx].apply_delta(delta_event.delta.clone());
                }
            }
            MessageStreamEvent::ContentBlockStop(_) => {}
            MessageStreamEvent::MessageDelta(delta_event) => {
                if let Some(ref mut msg) = self.message {
                    if delta_event.delta.stop_reason.is_some() {
                        msg.stop_reason = delta_event.delta.stop_reason;
                    }
                    if delta_event.delta.stop_sequence.is_some() {
                        msg.stop_sequence = delta_event.delta.stop_sequence.clone();
                    }
                    if let Some(input_tokens) = delta_event.usage.input_tokens {
                        msg.usage.input_tokens = input_tokens;
                    }
                    msg.usage.output_tokens = delta_event.usage.output_tokens;
                    if let Some(cache) = delta_event.usage.cache_creation_input_tokens {
                        msg.usage.cache_creation_input_tokens = Some(cache);
                    }
                    if let Some(cache_read) = delta_event.usage.cache_read_input_tokens {
                        msg.usage.cache_read_input_tokens = Some(cache_read);
                    }
                    if let Some(server_tool) = delta_event.usage.server_tool_use {
                        msg.usage.server_tool_use = Some(server_tool);
                    }
                }
            }
            MessageStreamEvent::MessageStop(_) => {}
            MessageStreamEvent::Ping => {}
            // New event types that don't affect message accumulation
            MessageStreamEvent::ToolInputStart { .. } => {}
            MessageStreamEvent::ToolInputDelta { .. } => {}
            MessageStreamEvent::CompactionEvent(_) => {}
            // Handled in `poll_next`, which turns it into an `Err` item.
            MessageStreamEvent::StreamError { .. } => {}
        }
    }

    fn finalize(&mut self) -> Result<Message, Error> {
        let mut msg = self
            .message
            .take()
            .ok_or_else(|| Error::streaming("stream ended without a message start event", None))?;
        let mut blocks = Vec::new();
        for builder in std::mem::take(&mut self.content_blocks) {
            if let Some(block) = builder.build(msg.stop_reason)? {
                blocks.push(block);
            }
        }
        msg.content = blocks;
        Ok(msg)
    }

    /// Finalizes the currently accumulated message without draining the stream.
    pub fn finalize_partial(&mut self) -> Result<Message, Error> {
        self.message_tx.take();
        self.finalize()
    }
}

impl Stream for AccumulatingStream {
    type Item = Result<MessageStreamEvent, Error>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            // A mid-stream `error` event (for example `overloaded_error`) aborts the
            // message, so the accumulated partial message must not be reported as success.
            std::task::Poll::Ready(Some(Ok(MessageStreamEvent::StreamError { error }))) => {
                let error = Error::from(error);
                if let Some(tx) = self.message_tx.take() {
                    let _ = tx.send(Err(error.clone()));
                }
                std::task::Poll::Ready(Some(Err(error)))
            }
            std::task::Poll::Ready(Some(Ok(event))) => {
                self.accumulate_event(&event);
                std::task::Poll::Ready(Some(Ok(event)))
            }
            std::task::Poll::Ready(Some(Err(e))) => std::task::Poll::Ready(Some(Err(e))),
            std::task::Poll::Ready(None) => {
                if let Some(tx) = self.message_tx.take() {
                    let _ = tx.send(self.finalize());
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

enum ContentBlockBuilder {
    Empty,
    Text {
        text: String,
        citations: Option<Vec<TextCitation>>,
        cache_control: Option<CacheControlEphemeral>,
    },
    ToolUse {
        id: String,
        name: String,
        input: ToolInput,
        cache_control: Option<CacheControlEphemeral>,
    },
    ServerToolUse {
        id: String,
        name: String,
        input: ToolInput,
        cache_control: Option<CacheControlEphemeral>,
        caller: Option<Value>,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    Complete(ContentBlock),
}

/// Input of a client or server tool block: the value from `content_block_start`,
/// replaced by the concatenated `input_json_delta` fragments once any arrive.
struct ToolInput {
    initial: Value,
    json: String,
    saw_delta: bool,
}

impl ToolInput {
    fn new(initial: Value) -> Self {
        Self { initial, json: String::new(), saw_delta: false }
    }

    /// Returns `None` when `max_tokens` cut the streamed JSON off, so the
    /// incomplete block is dropped rather than replayed.
    fn resolve(self, stop_reason: Option<StopReason>) -> Option<Value> {
        if !self.saw_delta {
            return Some(self.initial);
        }
        if self.json.trim().is_empty() {
            return Some(Value::Object(serde_json::Map::new()));
        }
        match serde_json::from_str::<Value>(&self.json) {
            Ok(value) => Some(value),
            Err(_) if stop_reason == Some(StopReason::MaxTokens) => None,
            Err(_) => Some(Value::String(self.json)),
        }
    }
}

impl ContentBlockBuilder {
    fn from_content_block(block: ContentBlock) -> Self {
        match block {
            ContentBlock::Text(text_block) => ContentBlockBuilder::Text {
                text: text_block.text,
                citations: text_block.citations,
                cache_control: text_block.cache_control,
            },
            ContentBlock::ToolUse(tool_use) => ContentBlockBuilder::ToolUse {
                id: tool_use.id,
                name: tool_use.name,
                input: ToolInput::new(tool_use.input),
                cache_control: tool_use.cache_control,
            },
            ContentBlock::ServerToolUse(server_tool_use) => ContentBlockBuilder::ServerToolUse {
                id: server_tool_use.id,
                name: server_tool_use.name,
                input: ToolInput::new(server_tool_use.input),
                cache_control: server_tool_use.cache_control,
                caller: server_tool_use.caller,
            },
            ContentBlock::Thinking(thinking) => ContentBlockBuilder::Thinking {
                thinking: thinking.thinking,
                signature: thinking.signature,
            },
            other => ContentBlockBuilder::Complete(other),
        }
    }

    fn apply_delta(&mut self, delta: ContentBlockDelta) {
        match (self, delta) {
            (ContentBlockBuilder::Text { text, .. }, ContentBlockDelta::TextDelta(text_delta)) => {
                text.push_str(&text_delta.text);
            }
            (
                ContentBlockBuilder::Text { citations, .. },
                ContentBlockDelta::CitationsDelta(citations_delta),
            ) => {
                let citation = match citations_delta.citation {
                    Citation::CharLocation(loc) => TextCitation::CharLocation(loc),
                    Citation::PageLocation(loc) => TextCitation::PageLocation(loc),
                    Citation::ContentBlockLocation(loc) => TextCitation::ContentBlockLocation(loc),
                    Citation::WebSearchResultLocation(loc) => {
                        TextCitation::WebSearchResultLocation(loc)
                    }
                };
                citations.get_or_insert_with(Vec::new).push(citation);
            }
            (
                ContentBlockBuilder::ToolUse { input, .. }
                | ContentBlockBuilder::ServerToolUse { input, .. },
                ContentBlockDelta::InputJsonDelta(json_delta),
            ) => {
                input.saw_delta = true;
                input.json.push_str(&json_delta.partial_json);
            }
            (
                ContentBlockBuilder::Thinking { thinking, .. },
                ContentBlockDelta::ThinkingDelta(thinking_delta),
            ) => {
                thinking.push_str(&thinking_delta.thinking);
            }
            (
                ContentBlockBuilder::Thinking { signature, .. },
                ContentBlockDelta::SignatureDelta(sig_delta),
            ) => {
                signature.push_str(&sig_delta.signature);
            }
            _ => {}
        }
    }

    fn build(self, stop_reason: Option<StopReason>) -> Result<Option<ContentBlock>, Error> {
        match self {
            ContentBlockBuilder::Empty => Ok(None),
            ContentBlockBuilder::Text { text, citations, cache_control } => {
                Ok(Some(ContentBlock::Text(TextBlock { text, citations, cache_control })))
            }
            ContentBlockBuilder::ToolUse { id, name, input, cache_control } => {
                Ok(input.resolve(stop_reason).map(|input| {
                    ContentBlock::ToolUse(ToolUseBlock { id, name, input, cache_control })
                }))
            }
            ContentBlockBuilder::ServerToolUse { id, name, input, cache_control, caller } => {
                Ok(input.resolve(stop_reason).map(|input| {
                    ContentBlock::ServerToolUse(ServerToolUseBlock {
                        id,
                        name,
                        input,
                        cache_control,
                        caller,
                    })
                }))
            }
            ContentBlockBuilder::Thinking { thinking, signature } => {
                Ok(Some(ContentBlock::Thinking(ThinkingBlock { thinking, signature })))
            }
            ContentBlockBuilder::Complete(block) => Ok(Some(block)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent, InputJsonDelta,
        KnownModel, MessageDelta, MessageDeltaEvent, MessageDeltaUsage, MessageStartEvent, Model,
        TextDelta, Usage,
    };
    use futures::stream;

    #[tokio::test]
    async fn server_inputs_preserve_initial_values_and_indexed_deltas() {
        use futures::StreamExt;
        use serde_json::json;
        for fragments in [false, true] {
            let mut values = vec![
                json!({"type":"message_start","message":{"id":"fixture","type":"message",
                    "role":"assistant","model":"fixture","content":[],"stop_reason":null,
                    "stop_sequence":null,"usage":{"input_tokens":8,"output_tokens":0}}}),
                json!({"type":"content_block_start","index":0,"content_block":{
                    "type":"server_tool_use","id":"search","name":"web_search",
                    "input":if fragments { json!({}) } else { json!({"query":"source 🙂"}) }}}),
                json!({"type":"content_block_start","index":1,"content_block":{
                    "type":"tool_use","id":"read","name":"read_file","input":{}}}),
            ];
            if fragments {
                values.push(json!({"type":"content_block_delta","index":0,
                    "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"source "}}));
            }
            values.push(json!({"type":"content_block_delta","index":1,
                "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"file.txt\"}"}}));
            if fragments {
                values.push(json!({"type":"content_block_delta","index":0,
                    "delta":{"type":"input_json_delta","partial_json":"🙂\"}"}}));
            }
            values.push(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
                "usage":{"output_tokens":4}}));
            values.push(json!({"type":"message_stop"}));
            let events: Vec<_> = values
                .into_iter()
                .map(|value| Ok(serde_json::from_value(value).unwrap()))
                .collect();
            let (mut stream, result) = AccumulatingStream::new(stream::iter(events));
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            let message = result.await.unwrap().unwrap();
            assert!(matches!(&message.content[0], ContentBlock::ServerToolUse(tool)
                if tool.id == "search" && tool.input == json!({"query":"source 🙂"})));
            assert!(matches!(&message.content[1], ContentBlock::ToolUse(tool)
                if tool.id == "read" && tool.input == json!({"path":"file.txt"})));
        }
    }

    /// A streamed `server_tool_use` keeps the `caller` from `content_block_start`.
    #[tokio::test]
    async fn streamed_server_tool_use_keeps_caller() {
        use futures::StreamExt;
        use serde_json::json;
        let caller = json!({"type": "code_execution_20260120", "tool_id": "srvtoolu_exec"});
        let events: Vec<_> = [
            json!({"type":"message_start","message":{"id":"fixture","type":"message",
                "role":"assistant","model":"fixture","content":[],"stop_reason":null,
                "stop_sequence":null,"usage":{"input_tokens":8,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{
                "type":"server_tool_use","id":"srvtoolu_search","name":"web_search",
                "input":{},"caller":caller}}),
            json!({"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                "usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ]
        .into_iter()
        .map(|value| Ok(serde_json::from_value(value).unwrap()))
        .collect();
        let (mut stream, result) = AccumulatingStream::new(stream::iter(events));
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let message = result.await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&message.content).unwrap(),
            json!([{"type":"server_tool_use","id":"srvtoolu_search","name":"web_search",
                "input":{"query":"rust"},"caller":caller}])
        );
    }

    /// Verifies that cache tokens from message_start are preserved through streaming.
    #[tokio::test]
    async fn cache_tokens_from_message_start_preserved() {
        // Build a message_start event with cache tokens in the usage
        let usage_with_cache = Usage::new(100, 0)
            .with_cache_creation_input_tokens(50)
            .with_cache_read_input_tokens(25);

        let start_message = Message::new(
            "msg_test".to_string(),
            Vec::new(),
            Model::Known(KnownModel::ClaudeSonnet46),
            usage_with_cache,
        );
        let start_event = MessageStreamEvent::MessageStart(MessageStartEvent::new(start_message));

        // Build content_block_start for text
        let text_block = ContentBlock::Text(TextBlock::new(String::new()));
        let content_start =
            MessageStreamEvent::ContentBlockStart(ContentBlockStartEvent::new(text_block, 0));

        // Build content_block_delta with text
        let text_delta = TextDelta::new("Hello".to_string());
        let content_delta = MessageStreamEvent::ContentBlockDelta(
            crate::ContentBlockDeltaEvent::new(ContentBlockDelta::TextDelta(text_delta), 0),
        );

        // Build message_delta with final output tokens (no cache tokens - they were in start)
        let delta_usage = MessageDeltaUsage::new(10);
        let message_delta = MessageDelta::new().with_stop_reason(StopReason::EndTurn);
        let delta_event =
            MessageStreamEvent::MessageDelta(MessageDeltaEvent::new(message_delta, delta_usage));

        // Create the stream
        let events = vec![Ok(start_event), Ok(content_start), Ok(content_delta), Ok(delta_event)];
        let event_stream = stream::iter(events);

        let (mut acc_stream, rx) = AccumulatingStream::new(event_stream);

        // Drain the stream
        use futures::StreamExt;
        while acc_stream.next().await.is_some() {}

        // Get the accumulated message
        let message = rx.await.expect("channel closed").expect("accumulation failed");

        // Verify cache tokens were preserved from message_start
        assert_eq!(
            message.usage.cache_creation_input_tokens,
            Some(50),
            "cache_creation_input_tokens should be preserved from message_start"
        );
        assert_eq!(
            message.usage.cache_read_input_tokens,
            Some(25),
            "cache_read_input_tokens should be preserved from message_start"
        );
        assert_eq!(message.usage.output_tokens, 10, "output_tokens should be from message_delta");
    }

    /// Verifies that tool use with empty input JSON becomes an empty object, not null.
    #[tokio::test]
    async fn empty_tool_input_becomes_empty_object() {
        let usage = Usage::new(100, 0);
        let start_message = Message::new(
            "msg_test".to_string(),
            Vec::new(),
            Model::Known(KnownModel::ClaudeSonnet46),
            usage,
        );
        let start_event = MessageStreamEvent::MessageStart(MessageStartEvent::new(start_message));

        // Build content_block_start for tool_use with initial empty input
        let tool_use_block =
            ContentBlock::ToolUse(ToolUseBlock::new("tool_123", "get_document", Value::Null));
        let content_start =
            MessageStreamEvent::ContentBlockStart(ContentBlockStartEvent::new(tool_use_block, 0));

        // Build content_block_delta with empty JSON (simulating no input parameters)
        let json_delta = InputJsonDelta::new(String::new());
        let content_delta = MessageStreamEvent::ContentBlockDelta(ContentBlockDeltaEvent::new(
            ContentBlockDelta::InputJsonDelta(json_delta),
            0,
        ));

        // Build content_block_stop
        let content_stop = MessageStreamEvent::ContentBlockStop(ContentBlockStopEvent::new(0));

        // Build message_delta
        let delta_usage = MessageDeltaUsage::new(10);
        let message_delta = MessageDelta::new().with_stop_reason(StopReason::ToolUse);
        let delta_event =
            MessageStreamEvent::MessageDelta(MessageDeltaEvent::new(message_delta, delta_usage));

        let events = vec![
            Ok(start_event),
            Ok(content_start),
            Ok(content_delta),
            Ok(content_stop),
            Ok(delta_event),
        ];
        let event_stream = stream::iter(events);

        let (mut acc_stream, rx) = AccumulatingStream::new(event_stream);

        use futures::StreamExt;
        while acc_stream.next().await.is_some() {}

        let message = rx.await.expect("channel closed").expect("accumulation failed");

        assert_eq!(message.content.len(), 1, "Should have one content block");
        let tool_use = message.content[0].as_tool_use().expect("Expected ToolUseBlock");

        // Empty input should be an empty object, not null
        assert!(
            tool_use.input.is_object(),
            "Empty tool input should be an object, not null. Got: {:?}",
            tool_use.input
        );
        assert!(
            tool_use.input.as_object().expect("input should be object").is_empty(),
            "Empty tool input should be an empty object"
        );
    }

    /// Verifies that tool use with no delta events uses initial input_value.
    #[tokio::test]
    async fn tool_input_without_delta_uses_initial_value() {
        let usage = Usage::new(100, 0);
        let start_message = Message::new(
            "msg_test".to_string(),
            Vec::new(),
            Model::Known(KnownModel::ClaudeSonnet46),
            usage,
        );
        let start_event = MessageStreamEvent::MessageStart(MessageStartEvent::new(start_message));

        // Build content_block_start for tool_use with an actual input value
        let input = serde_json::json!({"key": "value"});
        let tool_use_block =
            ContentBlock::ToolUse(ToolUseBlock::new("tool_123", "get_document", input.clone()));
        let content_start =
            MessageStreamEvent::ContentBlockStart(ContentBlockStartEvent::new(tool_use_block, 0));

        // No delta events - the input should come from the initial value

        let content_stop = MessageStreamEvent::ContentBlockStop(ContentBlockStopEvent::new(0));

        let delta_usage = MessageDeltaUsage::new(10);
        let message_delta = MessageDelta::new().with_stop_reason(StopReason::ToolUse);
        let delta_event =
            MessageStreamEvent::MessageDelta(MessageDeltaEvent::new(message_delta, delta_usage));

        let events = vec![Ok(start_event), Ok(content_start), Ok(content_stop), Ok(delta_event)];
        let event_stream = stream::iter(events);

        let (mut acc_stream, rx) = AccumulatingStream::new(event_stream);

        use futures::StreamExt;
        while acc_stream.next().await.is_some() {}

        let message = rx.await.expect("channel closed").expect("accumulation failed");

        let tool_use = message.content[0].as_tool_use().expect("Expected ToolUseBlock");

        assert_eq!(tool_use.input, input, "Tool input should match initial value");
    }

    fn start_event() -> MessageStreamEvent {
        MessageStreamEvent::MessageStart(MessageStartEvent::new(Message::new(
            "msg_test".to_string(),
            Vec::new(),
            Model::Known(KnownModel::ClaudeSonnet46),
            Usage::new(100, 0),
        )))
    }

    fn input_json_delta(index: usize, partial_json: &str) -> MessageStreamEvent {
        MessageStreamEvent::ContentBlockDelta(ContentBlockDeltaEvent::new(
            ContentBlockDelta::InputJsonDelta(InputJsonDelta::new(partial_json.to_string())),
            index,
        ))
    }

    /// Server tool input arrives through `input_json_delta` exactly like client tool input.
    #[tokio::test]
    async fn server_tool_use_applies_input_json_deltas() {
        use futures::StreamExt;

        let server_tool_use = ContentBlock::ServerToolUse(ServerToolUseBlock::new(
            "srvtoolu_1",
            serde_json::json!({}),
        ));
        let events = vec![
            Ok(start_event()),
            Ok(MessageStreamEvent::ContentBlockStart(ContentBlockStartEvent::new(
                server_tool_use,
                0,
            ))),
            Ok(input_json_delta(0, r#"{"query": "rust "#)),
            Ok(input_json_delta(0, r#"async"}"#)),
            Ok(MessageStreamEvent::ContentBlockStop(ContentBlockStopEvent::new(0))),
            Ok(MessageStreamEvent::MessageDelta(MessageDeltaEvent::new(
                MessageDelta::new().with_stop_reason(StopReason::EndTurn),
                MessageDeltaUsage::new(10),
            ))),
        ];

        let (mut acc_stream, rx) = AccumulatingStream::new(stream::iter(events));
        while acc_stream.next().await.is_some() {}
        let message = rx.await.expect("channel closed").expect("accumulation failed");

        assert_eq!(
            message.content,
            vec![ContentBlock::ServerToolUse(ServerToolUseBlock::new(
                "srvtoolu_1",
                serde_json::json!({"query": "rust async"}),
            ))]
        );
    }

    /// A mid-stream `error` event fails the stream and the accumulated message.
    #[tokio::test]
    async fn stream_error_event_is_returned_as_error_not_partial_message() {
        use futures::StreamExt;

        let events = vec![
            Ok(start_event()),
            Ok(MessageStreamEvent::ContentBlockStart(ContentBlockStartEvent::new(
                ContentBlock::Text(TextBlock::new(String::new())),
                0,
            ))),
            Ok(MessageStreamEvent::ContentBlockDelta(ContentBlockDeltaEvent::new(
                ContentBlockDelta::TextDelta(TextDelta::new("partial".to_string())),
                0,
            ))),
            Ok(MessageStreamEvent::StreamError {
                error: crate::ApiError {
                    error_type: "overloaded_error".to_string(),
                    message: "Overloaded".to_string(),
                },
            }),
        ];

        let (acc_stream, rx) = AccumulatingStream::new(stream::iter(events));
        let items: Vec<_> = acc_stream.collect().await;

        let stream_error = items.last().expect("stream yields items").as_ref().unwrap_err();
        assert!(
            matches!(stream_error, Error::ServiceUnavailable { message, retry_after: None } if message == "Overloaded"),
            "unexpected stream error: {stream_error:?}"
        );
        assert!(stream_error.is_retryable());

        let message_error = rx.await.expect("channel closed").unwrap_err();
        assert!(
            matches!(message_error, Error::ServiceUnavailable { ref message, .. } if message == "Overloaded"),
            "unexpected message error: {message_error:?}"
        );
    }
}
