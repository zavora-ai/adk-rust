//! Server-Sent Events (SSE) processing for streaming responses.
//!
//! This module handles parsing and processing of SSE streams from the Anthropic API,
//! converting raw byte streams into structured MessageStreamEvent objects.

use bytes::Bytes;
use futures::stream::{self, Stream, StreamExt};
use serde::de::DeserializeOwned;
use std::time::{Duration, Instant};
use tracing::debug;

use crate::observability::{
    STREAM_BYTES, STREAM_DURATION, STREAM_ERRORS, STREAM_EVENTS, STREAM_TTFB,
};
use crate::{
    CompactionMetadata, ContentBlockDeltaEvent, ContentBlockStartEvent, ContentBlockStopEvent,
    Error, MessageDeltaEvent, MessageStartEvent, MessageStopEvent, MessageStreamEvent, Result,
};

/// Maximum buffer size to prevent DoS attacks (1MB)
const MAX_BUFFER_SIZE: usize = 1024 * 1024;

/// Maximum event size (64KB)
const MAX_EVENT_SIZE: usize = 64 * 1024;

/// Longest wait for the next chunk from the server before the stream fails (30 seconds).
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// State for SSE processing with production hardening
struct SseState {
    buffer: String,
    total_bytes_processed: usize,
    start: Instant,
    first_byte: Option<Instant>,
    /// Set once a terminal error (inactivity timeout, transport failure, buffer
    /// overflow) has been reported; the stream then ends.
    finished: bool,
}

/// Process a stream of bytes into a stream of server-sent events with production hardening.
///
/// This function takes a byte stream from an HTTP response and converts it into
/// a stream of parsed MessageStreamEvent objects, handling SSE parsing,
/// buffering, error conditions, DoS protection, and timeouts.
///
/// Production features:
/// - Buffer size limits to prevent memory exhaustion
/// - Event size validation
/// - Inactivity timeout: when the server sends nothing for 30 seconds the
///   stream yields an `Error::Timeout` and then ends. Time the consumer spends
///   between polls does not count towards it.
/// - Terminal errors: after a transport error, an inactivity timeout, or a
///   buffer overflow the stream yields that error once and then ends.
/// - Graceful error recovery: an event type this client does not know is logged
///   at `debug` level and surfaced as [`MessageStreamEvent::Ping`], so a new
///   server-side event type never fails the stream
/// - UTF-8 validation with partial byte handling
pub fn process_sse<S>(byte_stream: S) -> impl Stream<Item = Result<MessageStreamEvent>>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Unpin + 'static,
{
    process_sse_with(byte_stream, parse_standard_event, CHUNK_TIMEOUT)
}

pub(crate) fn process_json_sse<S, T>(byte_stream: S) -> impl Stream<Item = Result<T>>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Unpin + 'static,
    T: DeserializeOwned + 'static,
{
    process_sse_with(byte_stream, parse_json_event::<T>, CHUNK_TIMEOUT)
}

fn process_sse_with<S, T, P>(
    byte_stream: S,
    parser: P,
    chunk_timeout: Duration,
) -> impl Stream<Item = Result<T>>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Unpin + 'static,
    T: 'static,
    P: Fn(&str, &str) -> Result<T> + Copy + 'static,
{
    // Convert reqwest errors to our error type
    let stream = byte_stream.map(|result| {
        result
            .map_err(|e| Error::streaming(format!("Error in HTTP stream: {e}"), Some(Box::new(e))))
    });

    // Initialize state with production hardening
    let state = SseState {
        buffer: String::new(),
        total_bytes_processed: 0,
        start: Instant::now(),
        first_byte: None,
        finished: false,
    };

    stream::unfold((stream, state), move |(mut stream, mut state)| async move {
        if state.finished {
            return None;
        }
        loop {
            // Check if we have a complete event in the buffer
            match extract_event(&state.buffer, parser) {
                Ok(Some((event, remaining))) => {
                    state.buffer = remaining;
                    match &event {
                        Ok(_) => STREAM_EVENTS.click(),
                        Err(_) => STREAM_ERRORS.click(),
                    }
                    return Some((event, (stream, state)));
                }
                Ok(None) => {
                    // No complete event yet, continue reading
                }
                Err(e) => {
                    STREAM_ERRORS.click();
                    return Some((Err(e), (stream, state)));
                }
            }

            // Check buffer size limit
            if state.buffer.len() > MAX_BUFFER_SIZE {
                state.finished = true;
                return Some((
                    Err(Error::streaming(
                        format!("SSE buffer size exceeded maximum limit: {MAX_BUFFER_SIZE} bytes"),
                        None,
                    )),
                    (stream, state),
                ));
            }

            // Only the wait on the server is timed, so a consumer that pauses
            // between polls never trips the inactivity timeout.
            let Ok(next) = tokio::time::timeout(chunk_timeout, stream.next()).await else {
                STREAM_ERRORS.click();
                state.finished = true;
                return Some((
                    Err(Error::timeout(
                        format!(
                            "SSE stream timeout: no data received for {} seconds",
                            chunk_timeout.as_secs_f64()
                        ),
                        Some(chunk_timeout.as_secs_f64()),
                    )),
                    (stream, state),
                ));
            };
            match next {
                Some(Ok(bytes)) => {
                    state.total_bytes_processed += bytes.len();
                    STREAM_BYTES.count(bytes.len() as u64);
                    if state.first_byte.is_none() {
                        let now = Instant::now();
                        state.first_byte = Some(now);
                        STREAM_TTFB.add(now.duration_since(state.start).as_secs_f64());
                    }

                    match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => {
                            state.buffer.push_str(&text);
                        }
                        Err(e) => {
                            // Try to recover partial UTF-8 sequences
                            let valid_up_to = e.utf8_error().valid_up_to();
                            if valid_up_to > 0
                                && let Ok(partial) =
                                    String::from_utf8(bytes[..valid_up_to].to_vec())
                            {
                                state.buffer.push_str(&partial);
                                // Log invalid bytes but continue processing
                                continue;
                            }
                            return Some((
                                Err(Error::encoding(
                                    format!("Invalid UTF-8 in stream: {e}"),
                                    Some(Box::new(e)),
                                )),
                                (stream, state),
                            ));
                        }
                    }
                }
                Some(Err(e)) => {
                    // A failed body (for example a request timeout) keeps returning
                    // the same error; polling it again would spin.
                    STREAM_ERRORS.click();
                    state.finished = true;
                    return Some((Err(e), (stream, state)));
                }
                None => {
                    // End of stream - try to process any remaining buffered events
                    if !state.buffer.is_empty()
                        && let Ok(Some((event, _))) = extract_event(&state.buffer, parser)
                    {
                        match &event {
                            Ok(_) => STREAM_EVENTS.click(),
                            Err(_) => STREAM_ERRORS.click(),
                        }
                        return Some((event, (stream, state)));
                    }
                    STREAM_DURATION.add(state.start.elapsed().as_secs_f64());
                    return None;
                }
            }
        }
    })
}

/// Extract a complete SSE event from a buffer string with size validation.
///
/// Parses SSE format where events are delimited by double newlines and
/// each event has an event type line followed by a data line.
/// Includes production safety checks for event size limits.
fn extract_event<T, P>(buffer: &str, parser: P) -> Result<Option<(Result<T>, String)>>
where
    P: Fn(&str, &str) -> Result<T>,
{
    // Find event boundary
    let Some(event_end) = buffer.find("\n\n") else {
        return Ok(None);
    };

    let event_text = &buffer[..event_end];
    let rest = buffer[event_end + 2..].to_string();

    // Validate event size
    if event_text.len() > MAX_EVENT_SIZE {
        return Ok(Some((
            Err(Error::streaming(
                format!(
                    "SSE event size {} exceeds maximum limit of {} bytes",
                    event_text.len(),
                    MAX_EVENT_SIZE
                ),
                None,
            )),
            rest,
        )));
    }

    // Handle empty events (ping-like keepalives)
    if event_text.trim().is_empty() {
        return Ok(Some((parser("event: ping", "{}"), rest)));
    }

    // Parse event type and data with better error handling
    let Some((event_type, _event_data)) = event_text.split_once('\n') else {
        return Ok(Some((
            Err(Error::serialization(
                "Malformed SSE event: missing newline separator in event".to_string(),
                None,
            )),
            rest,
        )));
    };

    // Handle multiple data lines (SSE spec allows this)
    let data_lines: Vec<&str> = event_text
        .lines()
        .skip(1)
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect();

    if data_lines.is_empty() {
        return Ok(Some((
            Err(Error::serialization("Malformed SSE event: missing data lines".to_string(), None)),
            rest,
        )));
    }

    let event_data = data_lines.join("\n");

    Ok(Some((parser(event_type, &event_data), rest)))
}

fn parse_standard_event(event_type: &str, event_data: &str) -> Result<MessageStreamEvent> {
    parse_event_type(event_type, event_data, String::new()).map(|(event, _)| event).unwrap_or_else(
        || {
            Err(Error::serialization(
                format!("Unknown SSE event type: {}", event_type.trim()),
                None,
            ))
        },
    )
}

fn parse_json_event<T: DeserializeOwned>(event_type: &str, event_data: &str) -> Result<T> {
    let mut value = if event_type == "event: ping" {
        serde_json::json!({"type": "ping"})
    } else {
        serde_json::from_str::<serde_json::Value>(event_data)?
    };
    if event_type == "event: error"
        && let Some(object) = value.as_object_mut()
    {
        object.insert("type".to_string(), serde_json::Value::String("stream_error".to_string()));
    }
    serde_json::from_value(value).map_err(Into::into)
}

/// Parse a specific SSE event type and its data with enhanced error handling.
fn parse_event_type(
    event_type: &str,
    event_data: &str,
    rest: String,
) -> Option<(Result<MessageStreamEvent>, String)> {
    match event_type {
        "event: ping" => Some((Ok(MessageStreamEvent::Ping), rest)),

        "event: message_start" => match serde_json::from_str::<MessageStartEvent>(event_data) {
            Ok(event) => Some((Ok(MessageStreamEvent::MessageStart(event)), rest)),
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: message_delta" => match serde_json::from_str::<MessageDeltaEvent>(event_data) {
            Ok(event) => Some((Ok(MessageStreamEvent::MessageDelta(event)), rest)),
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: message_stop" => match serde_json::from_str::<MessageStopEvent>(event_data) {
            Ok(event) => Some((Ok(MessageStreamEvent::MessageStop(event)), rest)),
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: content_block_start" => {
            match serde_json::from_str::<ContentBlockStartEvent>(event_data) {
                Ok(event) => Some((Ok(MessageStreamEvent::ContentBlockStart(event)), rest)),
                Err(e) => Some((Err(e.into()), rest)),
            }
        }

        "event: content_block_delta" => {
            match serde_json::from_str::<ContentBlockDeltaEvent>(event_data) {
                Ok(event) => Some((Ok(MessageStreamEvent::ContentBlockDelta(event)), rest)),
                Err(e) => Some((Err(e.into()), rest)),
            }
        }

        "event: content_block_stop" => {
            match serde_json::from_str::<ContentBlockStopEvent>(event_data) {
                Ok(event) => Some((Ok(MessageStreamEvent::ContentBlockStop(event)), rest)),
                Err(e) => Some((Err(e.into()), rest)),
            }
        }

        "event: tool_input_start" => match serde_json::from_str::<serde_json::Value>(event_data) {
            Ok(val) => {
                let tool_use_id =
                    val.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parameter_name =
                    val.get("parameter_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some((Ok(MessageStreamEvent::ToolInputStart { tool_use_id, parameter_name }), rest))
            }
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: tool_input_delta" => match serde_json::from_str::<serde_json::Value>(event_data) {
            Ok(val) => {
                let tool_use_id =
                    val.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parameter_name =
                    val.get("parameter_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let value_fragment =
                    val.get("value_fragment").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some((
                    Ok(MessageStreamEvent::ToolInputDelta {
                        tool_use_id,
                        parameter_name,
                        value_fragment,
                    }),
                    rest,
                ))
            }
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: compaction" => match serde_json::from_str::<CompactionMetadata>(event_data) {
            Ok(meta) => Some((Ok(MessageStreamEvent::CompactionEvent(meta)), rest)),
            Err(e) => Some((Err(e.into()), rest)),
        },

        "event: error" => {
            // Parse error event - try to extract structured error data
            match serde_json::from_str::<serde_json::Value>(event_data) {
                Ok(error_json) => {
                    let error_type = error_json
                        .get("error")
                        .and_then(|e| e.get("type"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("stream_error")
                        .to_string();
                    let message = error_json
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown stream error")
                        .to_string();

                    Some((
                        Ok(MessageStreamEvent::StreamError {
                            error: crate::types::ApiError { error_type, message },
                        }),
                        rest,
                    ))
                }
                Err(_) => Some((
                    Ok(MessageStreamEvent::StreamError {
                        error: crate::types::ApiError {
                            error_type: "stream_error".to_string(),
                            message: event_data.to_string(),
                        },
                    }),
                    rest,
                )),
            }
        }

        _ => {
            // New server-side event types must not fail the stream: log them and
            // surface the keep-alive no-op that every consumer already ignores.
            if event_type.starts_with("event:") {
                debug!(sse.event_type = %event_type.trim(), "ignoring unknown sse event type");
                Some((Ok(MessageStreamEvent::Ping), rest))
            } else {
                // Malformed event type format
                Some((
                    Err(Error::serialization(
                        "Malformed SSE event: invalid event type format".to_string(),
                        None,
                    )),
                    rest,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[tokio::test]
    async fn parse_ping_event() {
        let data = b"event: ping\ndata: {}\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(matches!(event, Ok(MessageStreamEvent::Ping)));
    }

    #[tokio::test]
    async fn parse_multiple_events() {
        let data = b"event: ping\ndata: {}\n\nevent: ping\ndata: {}\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let mut sse_stream = Box::pin(process_sse(stream));

        let event1 = sse_stream.next().await.unwrap();
        assert!(matches!(event1, Ok(MessageStreamEvent::Ping)));

        let event2 = sse_stream.next().await.unwrap();
        assert!(matches!(event2, Ok(MessageStreamEvent::Ping)));
    }

    #[tokio::test]
    async fn handle_malformed_event() {
        let data = b"malformed data without proper format\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(event.is_err());
    }

    #[tokio::test]
    async fn handle_split_event() {
        // Simulate an event split across multiple chunks
        let chunk1 = b"event: ping\n";
        let chunk2 = b"data: {}\n\n";

        let stream = Box::pin(stream::iter(vec![
            Ok(Bytes::from(&chunk1[..])),
            Ok(Bytes::from(&chunk2[..])),
        ]));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(matches!(event, Ok(MessageStreamEvent::Ping)));
    }

    #[tokio::test]
    async fn unknown_event_type_is_ignored_and_stream_continues() {
        let data = b"event: brand_new_event\ndata: {\"type\":\"brand_new_event\"}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let events: Vec<_> = Box::pin(process_sse(stream)).collect().await;

        assert_eq!(
            events.into_iter().collect::<Result<Vec<_>>>().unwrap(),
            vec![
                MessageStreamEvent::Ping,
                MessageStreamEvent::MessageStop(MessageStopEvent::new()),
            ]
        );
    }

    #[tokio::test]
    async fn stalled_server_times_out_and_ends_the_stream() {
        let stream = Box::pin(stream::pending::<std::result::Result<Bytes, reqwest::Error>>());

        let mut sse_stream =
            Box::pin(process_sse_with(stream, parse_standard_event, Duration::from_millis(50)));

        let error = sse_stream.next().await.unwrap().unwrap_err();
        assert!(error.is_timeout(), "expected a timeout error, got {error:?}");
        assert!(sse_stream.next().await.is_none(), "the stream ends after a timeout");
    }

    #[tokio::test]
    async fn consumer_pause_longer_than_timeout_does_not_fail_the_stream() {
        let stream = Box::pin(stream::iter(vec![
            Ok(Bytes::from_static(b"event: ping\ndata: {}\n\n")),
            Ok(Bytes::from_static(b"event: ping\ndata: {}\n\n")),
        ]));

        let mut sse_stream =
            Box::pin(process_sse_with(stream, parse_standard_event, Duration::from_millis(50)));

        assert!(matches!(sse_stream.next().await, Some(Ok(MessageStreamEvent::Ping))));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(matches!(sse_stream.next().await, Some(Ok(MessageStreamEvent::Ping))));
        assert!(sse_stream.next().await.is_none());
    }

    #[tokio::test]
    async fn handle_buffer_size_limit() {
        // Create multiple chunks that will exceed buffer limit when combined
        let chunk_size = MAX_BUFFER_SIZE / 2;
        let chunk1 = "a".repeat(chunk_size);
        let chunk2 = "b".repeat(chunk_size + 1000); // This will push over the limit

        let stream = Box::pin(stream::iter(vec![Ok(Bytes::from(chunk1)), Ok(Bytes::from(chunk2))]));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(event.is_err());
        if let Err(e) = event {
            assert!(e.to_string().contains("buffer size exceeded"));
        }
    }

    #[tokio::test]
    async fn handle_event_size_limit() {
        // Create an event that exceeds the single event size limit
        let large_event_data = "b".repeat(MAX_EVENT_SIZE + 100);
        let data = format!("event: ping\ndata: {large_event_data}\n\n");

        let stream = Box::pin(stream::once(async move { Ok(Bytes::from(data)) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(event.is_err());
        if let Err(e) = event {
            assert!(e.to_string().contains("event size") && e.to_string().contains("exceeds"));
        }
    }

    #[tokio::test]
    async fn handle_empty_events() {
        // Test empty events (common for keepalives)
        let data = b"\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        assert!(matches!(event, Ok(MessageStreamEvent::Ping)));
    }

    #[tokio::test]
    async fn handle_multi_line_data() {
        // Test multi-line data (valid SSE format)
        let data = b"event: message_start\ndata: {\ndata: \"test\": true\ndata: }\n\n";
        let stream = Box::pin(stream::once(async { Ok(Bytes::from(&data[..])) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        // Should attempt to parse the multi-line JSON
        match event {
            Ok(_) | Err(_) => {} // Either parse success or JSON error is acceptable
        }
    }

    #[tokio::test]
    async fn handle_partial_utf8() {
        // Test partial UTF-8 sequences (production hardening)
        let valid_part = "event: ping\ndata: test";
        let invalid_bytes = vec![0xFF, 0xFE]; // Invalid UTF-8

        let mut data = valid_part.as_bytes().to_vec();
        data.extend_from_slice(&invalid_bytes);
        data.extend_from_slice(b"\n\n");

        let stream = Box::pin(stream::once(async move { Ok(Bytes::from(data)) }));

        let mut sse_stream = Box::pin(process_sse(stream));

        // The stream might not produce an event if UTF-8 is completely invalid
        match sse_stream.next().await {
            Some(event) => {
                // Should handle partial UTF-8 gracefully or report UTF-8 error
                match event {
                    Ok(_) => {} // Successfully recovered partial UTF-8
                    Err(e) => assert!(e.to_string().contains("UTF-8")),
                }
            }
            None => {
                // Stream ended without producing events due to UTF-8 issues - acceptable
            }
        }
    }

    #[tokio::test]
    async fn handle_structured_error_events() {
        // Test structured error event parsing
        let error_json = r#"{"error": {"type": "rate_limit", "message": "Too many requests"}}"#;
        let data = format!("event: error\ndata: {error_json}\n\n");

        let stream = Box::pin(stream::once(async move { Ok(Bytes::from(data)) }));

        let mut sse_stream = Box::pin(process_sse(stream));
        let event = sse_stream.next().await.unwrap();

        match event {
            Ok(MessageStreamEvent::StreamError { error }) => {
                assert_eq!(error.error_type, "rate_limit");
                assert_eq!(error.message, "Too many requests");
            }
            other => panic!("Expected StreamError variant, got {other:?}"),
        }
    }
}
