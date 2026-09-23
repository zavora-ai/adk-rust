//! Adapt native SSE events without rebuilding the SDK's message accumulator.

use super::{client::convert_anthropic_error, convert};
use adk_anthropic::{AccumulatingStream, ContentBlock, ContentBlockDelta, MessageStreamEvent};
use adk_core::{AdkError, LlmResponseStream};
use futures::{Stream, StreamExt};

pub(super) fn responses<S>(events: S) -> LlmResponseStream
where
    S: Stream<Item = Result<MessageStreamEvent, adk_anthropic::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        let (mut events, _) = AccumulatingStream::new(events);
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    let error = super::client::to_anthropic_api_error(&error);
                    yield convert::from_stream_error(&error.error_type, &error.message);
                    return;
                }
            };
            match event {
                MessageStreamEvent::ContentBlockStart(start) => match start.content_block {
                    ContentBlock::Text(text) if !text.text.is_empty() => {
                        yield convert::from_text_delta(&text.text);
                    }
                    ContentBlock::Thinking(thinking) if !thinking.thinking.is_empty() => {
                        yield convert::from_thinking_delta(&thinking.thinking);
                    }
                    _ => {}
                },
                MessageStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                    ContentBlockDelta::TextDelta(text) if !text.text.is_empty() => {
                        yield convert::from_text_delta(&text.text);
                    }
                    ContentBlockDelta::ThinkingDelta(thinking) if !thinking.thinking.is_empty() => {
                        yield convert::from_thinking_delta(&thinking.thinking);
                    }
                    _ => {}
                },
                MessageStreamEvent::MessageStop(_) => {
                    let message = events.finalize_partial().map_err(convert_anthropic_error)?;
                    if message.stop_reason.is_none() {
                        Err(AdkError::model("Anthropic stream ended without a stop reason"))?;
                    }
                    let mut response = convert::from_anthropic_message(&message).0;
                    // Replace deltas with the SDK's complete, ordered blocks.
                    // This also retains original text boundaries and signatures.
                    response.provider_metadata.get_or_insert_with(|| serde_json::json!({}))
                        ["content_complete"] = serde_json::json!(true);
                    yield response;
                    return;
                }
                MessageStreamEvent::StreamError { .. } => {
                    Err(AdkError::model("Anthropic stream reported an error"))?;
                }
                _ => {}
            }
        }
        Err(AdkError::model("Anthropic stream ended before message_stop"))?;
    })
}
