//! Adapt native SSE events without rebuilding the SDK's message accumulator.

use super::{client::convert_anthropic_error, convert};
use adk_anthropic::{
    AccumulatingStream, ContentBlock, ContentBlockDelta, MessageStreamEvent, ToolUseBlock,
};
use adk_core::{AdkError, LlmResponseStream};
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::Span;

pub(super) fn responses<S>(events: S) -> LlmResponseStream
where
    S: Stream<Item = Result<MessageStreamEvent, adk_anthropic::Error>> + Send + 'static,
{
    Box::pin(async_stream::try_stream! {
        // The accumulator turns a mid-stream `error` event into an `Err` item, the
        // same shape as a transport failure; this flag tells the two apart.
        let saw_error_event = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&saw_error_event);
        let events = events.inspect(move |event| {
            if matches!(event, Ok(MessageStreamEvent::StreamError { .. })) {
                flag.store(true, Ordering::Relaxed);
            }
        });
        let (mut events, _) = AccumulatingStream::new(events);
        // Client tool calls by block index: id, name and the streamed argument JSON.
        let mut tool_calls: BTreeMap<usize, (String, String, String)> = BTreeMap::new();
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                // A mid-stream `error` event (for example `overloaded_error`) aborts
                // the message; it fails the stream with the category the same error
                // gets as an HTTP response, so retry policy can act on it.
                Err(error) if saw_error_event.load(Ordering::Relaxed) => {
                    Err(convert_anthropic_error(error))?
                }
                Err(error) => {
                    let error = super::client::to_anthropic_api_error(&error);
                    if let Some(request_id) = &error.request_id {
                        Span::current().record("anthropic.request_id", request_id.as_str());
                    }
                    // The message can echo the request, so only the content-free part is logged.
                    tracing::error!(
                        error.type_ = %error.error_type,
                        error.status_code = error.status_code,
                        "anthropic stream error"
                    );
                    yield convert::from_stream_error(&error.error_type, &error.message);
                    return;
                }
            };
            match event {
                MessageStreamEvent::ContentBlockStart(start) => match start.content_block {
                    ContentBlock::ToolUse(tool) => {
                        tool_calls.insert(start.index, (tool.id, tool.name, String::new()));
                    }
                    ContentBlock::Text(text) if !text.text.is_empty() => {
                        yield convert::from_text_delta(&text.text);
                    }
                    ContentBlock::Thinking(thinking) if !thinking.thinking.is_empty() => {
                        yield convert::from_thinking_delta(&thinking.thinking);
                    }
                    _ => {}
                },
                MessageStreamEvent::ContentBlockDelta(delta) => match delta.delta {
                    ContentBlockDelta::InputJsonDelta(json) => {
                        if let Some((_, _, arguments)) = tool_calls.get_mut(&delta.index) {
                            arguments.push_str(&json.partial_json);
                        }
                    }
                    ContentBlockDelta::TextDelta(text) if !text.text.is_empty() => {
                        yield convert::from_text_delta(&text.text);
                    }
                    ContentBlockDelta::ThinkingDelta(thinking) if !thinking.thinking.is_empty() => {
                        yield convert::from_thinking_delta(&thinking.thinking);
                    }
                    _ => {}
                },
                MessageStreamEvent::MessageStop(_) => {
                    let mut message = events.finalize_partial().map_err(convert_anthropic_error)?;
                    if message.stop_reason.is_none() {
                        Err(AdkError::model("Anthropic stream ended without a stop reason"))?;
                    }
                    // The accumulator keeps malformed streamed arguments as a JSON
                    // string; a truncated call must never run with substituted input, so
                    // it is marked and the agent answers it with an error.
                    for block in &mut message.content {
                        if let ContentBlock::ToolUse(tool) = block
                            && !tool.input.is_object()
                        {
                            let raw = match &tool.input {
                                Value::String(raw) => raw.clone(),
                                other => other.to_string(),
                            };
                            tool.input =
                                crate::tool_args::streamed_tool_arguments("anthropic", &tool.name, &raw);
                        }
                    }
                    // The accumulator drops a call that `max_tokens` cut off. It is kept,
                    // marked as cut off, so the model is told instead of the call vanishing.
                    for (id, name, arguments) in tool_calls.values() {
                        let kept = message.content.iter().any(
                            |block| matches!(block, ContentBlock::ToolUse(tool) if &tool.id == id),
                        );
                        if !kept {
                            tracing::warn!(tool.name = %name, "anthropic tool call was cut off before it finished");
                            message.content.push(ContentBlock::ToolUse(ToolUseBlock::new(
                                id.clone(),
                                name.clone(),
                                adk_core::invalid_tool_arguments(
                                    "the call was cut off before its arguments were complete",
                                    arguments,
                                ),
                            )));
                        }
                    }
                    let mut response = convert::from_anthropic_message(&message).0;
                    // Replace deltas with the SDK's complete, ordered blocks.
                    // This also retains original text boundaries and signatures.
                    response.provider_metadata.get_or_insert_with(|| serde_json::json!({}))
                        ["content_complete"] = serde_json::json!(true);
                    yield response;
                    return;
                }
                _ => {}
            }
        }
        Err(AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::Unavailable,
            "model.anthropic.unfinished_stream",
            "Anthropic stream ended before message_stop",
        ).with_provider("anthropic"))?;
    })
}
