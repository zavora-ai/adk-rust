//! SSE stream implementation for Managed Agents session events.
//!
//! This module provides the [`process_managed_agents_sse`] function that converts
//! a raw byte stream (from an HTTP response) into a typed async stream of
//! [`SessionEvent`] values.
//!
//! The SSE format expected is:
//! ```text
//! event: <type>
//! data: <json>
//!
//! ```
//!
//! Key behaviors:
//! - Buffers incoming bytes and extracts complete SSE events delimited by `\n\n`
//! - Parses `event:` and `data:` lines from each SSE block
//! - Deserializes JSON data into [`SessionEvent`] variants
//! - Unknown event types produce [`SessionEvent::Unknown`] (via `#[serde(other)]`)
//! - Invalid JSON for a single event logs a warning and skips the event
//! - Invalid UTF-8 yields an encoding error, stream continues
//! - Timeout if no data received within the configured duration

use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::Stream;

use super::events::SessionEvent;
use crate::{Error, Result};

/// Default SSE stream timeout in seconds.
const DEFAULT_SSE_TIMEOUT_SECS: u64 = 300;

/// Returns the default SSE stream timeout duration (300 seconds).
pub fn default_sse_timeout() -> Duration {
    Duration::from_secs(DEFAULT_SSE_TIMEOUT_SECS)
}

/// Process an SSE byte stream into typed [`SessionEvent`] values.
///
/// Converts a raw byte stream (typically from an HTTP response body) into an
/// async stream of parsed session events. The function handles:
///
/// - Buffering incoming bytes and extracting complete SSE events (delimited by `\n\n`)
/// - Parsing `event: <type>` and `data: <json>` lines
/// - Deserializing JSON into [`SessionEvent`] variants
/// - Unknown event types → [`SessionEvent::Unknown`] (forward compatibility)
/// - Invalid JSON → warning logged, event skipped, stream continues
/// - Invalid UTF-8 → encoding error yielded, stream continues
/// - Timeout → timeout error yielded if no data received within `timeout` duration
///
/// # Arguments
///
/// * `byte_stream` - The raw byte stream from the HTTP response
/// * `timeout` - Maximum duration to wait for new data before yielding a timeout error
///
/// # Example
///
/// ```rust,ignore
/// use std::time::Duration;
/// use adk_anthropic::managed_agents::process_managed_agents_sse;
///
/// let event_stream = process_managed_agents_sse(byte_stream, Duration::from_secs(300));
/// ```
pub fn process_managed_agents_sse<S>(
    byte_stream: S,
    timeout: Duration,
) -> Pin<Box<dyn Stream<Item = Result<SessionEvent>> + Send>>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    let state = SseState {
        byte_stream: Box::pin(byte_stream),
        buffer: String::new(),
        timeout,
        done: false,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        if state.done {
            return None;
        }

        loop {
            // Try to extract a complete SSE event from the buffer
            if let Some(event_result) = state.try_parse_next_event() {
                return Some((event_result, state));
            }

            // Need more data from the byte stream
            let read_result = tokio::time::timeout(state.timeout, state.byte_stream.next()).await;

            match read_result {
                Ok(Some(Ok(bytes))) => {
                    // Append new bytes to buffer, handling UTF-8 errors
                    match std::str::from_utf8(&bytes) {
                        Ok(text) => {
                            state.buffer.push_str(text);
                        }
                        Err(e) => {
                            // Yield encoding error but continue the stream
                            let err = Error::Encoding {
                                message: format!("invalid UTF-8 in SSE stream: {e}"),
                                source: None,
                            };
                            return Some((Err(err), state));
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    // reqwest stream error — yield connection error, end stream
                    state.done = true;
                    let err = Error::Connection {
                        message: format!("SSE stream error: {e}"),
                        source: None,
                    };
                    return Some((Err(err), state));
                }
                Ok(None) => {
                    // Stream ended — try to parse any remaining buffered event
                    if let Some(event_result) = state.try_parse_next_event() {
                        state.done = true;
                        return Some((event_result, state));
                    }
                    // No more data and no complete event
                    return None;
                }
                Err(_elapsed) => {
                    // Timeout — yield timeout error, stream continues
                    let err = Error::Timeout {
                        message: format!(
                            "no data received on SSE stream within {} seconds",
                            state.timeout.as_secs()
                        ),
                        duration: Some(state.timeout.as_secs_f64()),
                    };
                    return Some((Err(err), state));
                }
            }
        }
    }))
}

/// Internal state for the SSE stream unfold.
struct SseState {
    byte_stream: Pin<Box<dyn Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send>>,
    buffer: String,
    timeout: Duration,
    done: bool,
}

impl SseState {
    /// Try to extract and parse the next complete SSE event from the buffer.
    ///
    /// An SSE event is delimited by a double newline (`\n\n`). Returns `None`
    /// if no complete event is available in the buffer.
    fn try_parse_next_event(&mut self) -> Option<Result<SessionEvent>> {
        loop {
            // Look for a complete SSE event (delimited by \n\n)
            let delimiter_pos = self.buffer.find("\n\n")?;

            // Extract the complete event block
            let event_block = self.buffer[..delimiter_pos].to_string();
            self.buffer = self.buffer[delimiter_pos + 2..].to_string();

            // Skip empty blocks
            if event_block.trim().is_empty() {
                continue;
            }

            // Parse the SSE event block
            match parse_sse_event(&event_block) {
                Some(Ok(session_event)) => return Some(Ok(session_event)),
                Some(Err(err)) => {
                    // Invalid JSON — log warning, skip event, continue
                    tracing::warn!(
                        error = %err,
                        event_block = %event_block,
                        "skipping SSE event with invalid JSON"
                    );
                    continue;
                }
                None => {
                    // No data line found — skip this block
                    continue;
                }
            }
        }
    }
}

/// Parse a single SSE event block into a [`SessionEvent`].
///
/// Expected format:
/// ```text
/// event: <type>
/// data: <json>
/// ```
///
/// Returns:
/// - `Some(Ok(event))` if parsing and deserialization succeed
/// - `Some(Err(error))` if the data line contains invalid JSON
/// - `None` if no `data:` line is found in the block
fn parse_sse_event(block: &str) -> Option<Result<SessionEvent>> {
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        }
        // We don't need to explicitly parse the `event:` line because
        // SessionEvent uses `#[serde(tag = "type")]` — the `type` field
        // in the JSON data determines the variant.
    }

    if data_lines.is_empty() {
        return None;
    }

    // Join multiple data lines (SSE spec allows multi-line data)
    let data = data_lines.join("\n");

    if data.is_empty() {
        return None;
    }

    // Deserialize the JSON data into SessionEvent
    match serde_json::from_str::<SessionEvent>(&data) {
        Ok(event) => Some(Ok(event)),
        Err(e) => Some(Err(Error::Serialization {
            message: format!("failed to deserialize SSE event data: {e}"),
            source: None,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream;

    /// Helper to create a byte stream from string chunks.
    fn byte_stream_from_chunks(
        chunks: Vec<&str>,
    ) -> impl Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Unpin {
        stream::iter(chunks.into_iter().map(|s| Ok(Bytes::from(s.to_string()))).collect::<Vec<_>>())
    }

    #[tokio::test]
    async fn test_parse_single_agent_message_event() {
        let chunks = vec![
            "event: agent.message\ndata: {\"type\":\"agent.message\",\"content\":\"Hello!\"}\n\n",
        ];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("Hello!") }
        );

        // Stream should end
        assert!(event_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_parse_multiple_events() {
        let chunks = vec![
            "event: session.status_running\ndata: {\"type\":\"session.status_running\"}\n\n",
            "event: agent.message\ndata: {\"type\":\"agent.message\",\"content\":\"Hi\"}\n\n",
            "event: session.status_idle\ndata: {\"type\":\"session.status_idle\"}\n\n",
        ];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event1 = event_stream.next().await.unwrap().unwrap();
        assert_eq!(event1, SessionEvent::StatusRunning {});

        let event2 = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event2,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("Hi") }
        );

        let event3 = event_stream.next().await.unwrap().unwrap();
        assert_eq!(event3, SessionEvent::StatusIdle { stop_reason: None });

        assert!(event_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_unknown_event_type_produces_unknown_variant() {
        let chunks = vec![
            "event: some.future.event\ndata: {\"type\":\"some.future.event\",\"foo\":\"bar\"}\n\n",
        ];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(event, SessionEvent::Unknown);
    }

    #[tokio::test]
    async fn test_invalid_json_skips_event_and_continues() {
        let chunks = vec![
            "event: agent.message\ndata: {invalid json}\n\nevent: agent.message\ndata: {\"type\":\"agent.message\",\"content\":\"valid\"}\n\n",
        ];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        // The invalid JSON event is skipped, we get the valid one
        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("valid") }
        );

        assert!(event_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_invalid_utf8_yields_encoding_error() {
        // Create a stream with invalid UTF-8 bytes followed by valid data
        let invalid_bytes: Vec<u8> = vec![0xFF, 0xFE, 0xFD];
        let valid_chunk = "event: agent.message\ndata: {\"type\":\"agent.message\",\"content\":\"after error\"}\n\n";

        let items: Vec<std::result::Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::from(invalid_bytes)), Ok(Bytes::from(valid_chunk.to_string()))];
        let stream = stream::iter(items);
        let mut event_stream = process_managed_agents_sse(Box::pin(stream), Duration::from_secs(5));

        // First item should be an encoding error
        let result = event_stream.next().await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::Encoding { .. }));

        // Stream continues — next item should be the valid event
        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("after error") }
        );
    }

    #[tokio::test]
    async fn test_timeout_yields_timeout_error() {
        // Create a stream that never produces data
        let stream = stream::pending::<std::result::Result<Bytes, reqwest::Error>>();
        let mut event_stream =
            process_managed_agents_sse(Box::pin(stream), Duration::from_millis(50));

        let result = event_stream.next().await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::Timeout { .. }));
    }

    #[tokio::test]
    async fn test_chunked_data_across_multiple_reads() {
        // Event split across multiple chunks
        let chunks = vec![
            "event: agent.message\n",
            "data: {\"type\":\"agent.message\",",
            "\"content\":\"split\"}\n\n",
        ];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("split") }
        );
    }

    #[tokio::test]
    async fn test_tool_use_event_parsing() {
        let data =
            r#"{"type":"agent.tool_use","id":"tu_123","name":"bash","input":{"command":"ls"}}"#;
        let chunk = format!("event: agent.tool_use\ndata: {data}\n\n");
        let items: Vec<std::result::Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from(chunk))];
        let stream = stream::iter(items);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentToolUse {
                id: Some("tu_123".to_string()),
                name: Some("bash".to_string()),
                input: Some(serde_json::json!({"command": "ls"})),
            }
        );
    }

    #[tokio::test]
    async fn test_custom_tool_use_event_parsing() {
        let data = r#"{"type":"agent.custom_tool_use","id":"ctu_456","name":"my_tool","input":{"key":"value"}}"#;
        let items: Vec<std::result::Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::from(format!("event: agent.custom_tool_use\ndata: {data}\n\n")))];
        let stream = stream::iter(items);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentCustomToolUse {
                id: Some("ctu_456".to_string()),
                name: Some("my_tool".to_string()),
                input: Some(serde_json::json!({"key": "value"})),
            }
        );
    }

    #[tokio::test]
    async fn test_error_event_parsing() {
        let data =
            r#"{"type":"session.error","message":"something went wrong","code":"internal_error"}"#;
        let items: Vec<std::result::Result<Bytes, reqwest::Error>> =
            vec![Ok(Bytes::from(format!("event: session.error\ndata: {data}\n\n")))];
        let stream = stream::iter(items);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::Error { error: None, message: Some("something went wrong".to_string()) }
        );
    }

    #[tokio::test]
    async fn test_empty_stream_produces_no_events() {
        let stream = stream::empty::<std::result::Result<Bytes, reqwest::Error>>();
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        assert!(event_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_data_only_without_event_line() {
        // SSE spec allows data-only events (no event: line)
        let chunks = vec!["data: {\"type\":\"agent.message\",\"content\":\"no event line\"}\n\n"];
        let stream = byte_stream_from_chunks(chunks);
        let mut event_stream = process_managed_agents_sse(stream, Duration::from_secs(5));

        let event = event_stream.next().await.unwrap().unwrap();
        assert_eq!(
            event,
            SessionEvent::AgentMessage { id: None, content: serde_json::json!("no event line") }
        );
    }
}
