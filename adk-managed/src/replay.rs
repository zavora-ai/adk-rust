//! Event replay for SSE `Last-Event-ID` reconnection.
//!
//! Provides [`create_event_stream`] which creates a unified stream combining
//! historical events (from checkpoint) and live events (from broadcast).
//!
//! # Usage
//!
//! - `from_seq = None` → live tail only (subscribe to broadcast)
//! - `from_seq = Some(k)` → replay all events with `seq > k`, then live tail
//!
//! This enables SSE reconnection: the client provides the last `seq` it received
//! via `Last-Event-ID`, and the stream starts from there without gaps or duplicates.

use futures::stream::{self, BoxStream, StreamExt};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::checkpoint::CheckpointManager;
use crate::types::SessionEvent;

/// Extract the `seq` field from any [`SessionEvent`] variant.
///
/// Every `SessionEvent` variant carries a monotonic `seq` field.
/// This helper provides uniform access regardless of variant.
///
/// # Example
///
/// ```rust
/// use adk_managed::replay::get_seq;
/// use adk_managed::types::SessionEvent;
///
/// let event = SessionEvent::StatusRunning { seq: 42 };
/// assert_eq!(get_seq(&event), 42);
/// ```
pub fn get_seq(event: &SessionEvent) -> u64 {
    match event {
        SessionEvent::Message { seq, .. }
        | SessionEvent::ToolUse { seq, .. }
        | SessionEvent::CustomToolUse { seq, .. }
        | SessionEvent::McpToolUse { seq, .. }
        | SessionEvent::StatusRunning { seq, .. }
        | SessionEvent::StatusIdle { seq, .. }
        | SessionEvent::Error { seq, .. } => *seq,
    }
}

/// Create an event stream that replays historical events then attaches to live broadcast.
///
/// - `from_seq = None` → live tail only (subscribe to broadcast)
/// - `from_seq = Some(k)` → replay all events with `seq > k`, then live tail
///
/// The returned stream yields events in order: first any historical events from the
/// checkpoint log that have `seq > k`, then live events from the broadcast channel.
///
/// # Arguments
///
/// * `checkpoint` - The checkpoint manager holding the event log
/// * `broadcast_rx` - A broadcast receiver for live events
/// * `from_seq` - Optional sequence number; if provided, replay events with seq > this value
///
/// # Example
///
/// ```rust,ignore
/// use adk_managed::replay::create_event_stream;
/// use adk_managed::checkpoint::CheckpointManager;
/// use tokio::sync::broadcast;
///
/// let checkpoint = CheckpointManager::new("session_1".to_string());
/// let (tx, rx) = broadcast::channel(128);
///
/// // Live tail only
/// let stream = create_event_stream(&checkpoint, rx, None);
///
/// // Replay from seq 5 onward
/// let (_, rx2) = (tx.clone(), tx.subscribe());
/// let stream = create_event_stream(&checkpoint, rx2, Some(5));
/// ```
pub fn create_event_stream(
    checkpoint: &CheckpointManager,
    broadcast_rx: broadcast::Receiver<SessionEvent>,
    from_seq: Option<u64>,
) -> BoxStream<'static, SessionEvent> {
    // Convert broadcast receiver to a stream, filtering out lagged errors
    let live_stream = BroadcastStream::new(broadcast_rx).filter_map(|result| async move {
        result.ok() // Skip lagged messages
    });

    match from_seq {
        None => {
            // Live tail only
            Box::pin(live_stream)
        }
        Some(k) => {
            // Replay historical events with seq > k, then chain with live
            let historical: Vec<SessionEvent> = checkpoint
                .events()
                .iter()
                .filter(|event| get_seq(event) > k)
                .cloned()
                .collect();

            let replay_stream = stream::iter(historical);
            Box::pin(replay_stream.chain(live_stream))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::RunState;
    use crate::types::{ContentBlock, SessionStatus};
    use futures::StreamExt;
    use serde_json::json;

    /// Helper to create a simple message event with a given seq.
    fn message_event(seq: u64) -> SessionEvent {
        SessionEvent::Message {
            content: vec![ContentBlock::Text {
                text: format!("msg_{seq}"),
            }],
            seq,
        }
    }

    #[test]
    fn test_get_seq_message() {
        let event = SessionEvent::Message {
            content: vec![],
            seq: 10,
        };
        assert_eq!(get_seq(&event), 10);
    }

    #[test]
    fn test_get_seq_tool_use() {
        let event = SessionEvent::ToolUse {
            tool_use_id: "tu_1".to_string(),
            name: "search".to_string(),
            input: json!({}),
            seq: 5,
        };
        assert_eq!(get_seq(&event), 5);
    }

    #[test]
    fn test_get_seq_custom_tool_use() {
        let event = SessionEvent::CustomToolUse {
            custom_tool_use_id: "ctu_1".to_string(),
            name: "deploy".to_string(),
            input: json!({}),
            seq: 7,
        };
        assert_eq!(get_seq(&event), 7);
    }

    #[test]
    fn test_get_seq_mcp_tool_use() {
        let event = SessionEvent::McpToolUse {
            tool_use_id: "mcp_1".to_string(),
            name: "read".to_string(),
            input: json!({}),
            seq: 3,
        };
        assert_eq!(get_seq(&event), 3);
    }

    #[test]
    fn test_get_seq_status_running() {
        let event = SessionEvent::StatusRunning { seq: 0 };
        assert_eq!(get_seq(&event), 0);
    }

    #[test]
    fn test_get_seq_status_idle() {
        let event = SessionEvent::StatusIdle {
            seq: 99,
            stop_reason: None,
            usage: None,
        };
        assert_eq!(get_seq(&event), 99);
    }

    #[test]
    fn test_get_seq_error() {
        let event = SessionEvent::Error {
            code: "err".to_string(),
            message: "oops".to_string(),
            seq: 42,
        };
        assert_eq!(get_seq(&event), 42);
    }

    #[tokio::test]
    async fn test_replay_with_from_seq_filters_correctly() {
        let mut checkpoint = CheckpointManager::new("sess_1".to_string());

        // Store events with seq 1..5
        for seq in 1..=5 {
            let event = message_event(seq);
            let state = RunState {
                seq,
                pending_tool_ids: vec![],
                status: SessionStatus::Running,
            };
            checkpoint.checkpoint(event, state);
        }

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);
        drop(tx); // Close the sender so live stream ends

        // Replay from seq 3 → should get events with seq 4, 5
        let stream = create_event_stream(&checkpoint, rx, Some(3));
        let events: Vec<SessionEvent> = stream.collect().await;

        assert_eq!(events.len(), 2);
        assert_eq!(get_seq(&events[0]), 4);
        assert_eq!(get_seq(&events[1]), 5);
    }

    #[tokio::test]
    async fn test_replay_with_from_seq_zero_returns_all() {
        let mut checkpoint = CheckpointManager::new("sess_2".to_string());

        for seq in 1..=3 {
            let event = message_event(seq);
            let state = RunState {
                seq,
                pending_tool_ids: vec![],
                status: SessionStatus::Running,
            };
            checkpoint.checkpoint(event, state);
        }

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);
        drop(tx);

        // from_seq=0 → all events (seq > 0)
        let stream = create_event_stream(&checkpoint, rx, Some(0));
        let events: Vec<SessionEvent> = stream.collect().await;

        assert_eq!(events.len(), 3);
        assert_eq!(get_seq(&events[0]), 1);
        assert_eq!(get_seq(&events[1]), 2);
        assert_eq!(get_seq(&events[2]), 3);
    }

    #[tokio::test]
    async fn test_live_only_mode() {
        let checkpoint = CheckpointManager::new("sess_3".to_string());

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);

        // Send a live event before creating the stream won't be received
        // (broadcast only delivers after subscription)

        let stream = create_event_stream(&checkpoint, rx, None);

        // Send live events after stream is created
        tx.send(message_event(10)).unwrap();
        tx.send(message_event(11)).unwrap();
        drop(tx); // End the stream

        let events: Vec<SessionEvent> = stream.collect().await;

        assert_eq!(events.len(), 2);
        assert_eq!(get_seq(&events[0]), 10);
        assert_eq!(get_seq(&events[1]), 11);
    }

    #[tokio::test]
    async fn test_combined_replay_plus_live() {
        let mut checkpoint = CheckpointManager::new("sess_4".to_string());

        // Historical events: seq 1, 2, 3
        for seq in 1..=3 {
            let event = message_event(seq);
            let state = RunState {
                seq,
                pending_tool_ids: vec![],
                status: SessionStatus::Running,
            };
            checkpoint.checkpoint(event, state);
        }

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);

        // Create stream with replay from seq 2 → historical seq 3
        let stream = create_event_stream(&checkpoint, rx, Some(2));

        // Send live events
        tx.send(message_event(4)).unwrap();
        tx.send(message_event(5)).unwrap();
        drop(tx); // End live stream

        let events: Vec<SessionEvent> = stream.collect().await;

        // Should get: historical seq=3, then live seq=4, seq=5
        assert_eq!(events.len(), 3);
        assert_eq!(get_seq(&events[0]), 3);
        assert_eq!(get_seq(&events[1]), 4);
        assert_eq!(get_seq(&events[2]), 5);
    }

    #[tokio::test]
    async fn test_replay_with_from_seq_beyond_all_events() {
        let mut checkpoint = CheckpointManager::new("sess_5".to_string());

        for seq in 1..=3 {
            let event = message_event(seq);
            let state = RunState {
                seq,
                pending_tool_ids: vec![],
                status: SessionStatus::Running,
            };
            checkpoint.checkpoint(event, state);
        }

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);
        drop(tx);

        // from_seq=100 → no historical events match (all seq <= 100)
        let stream = create_event_stream(&checkpoint, rx, Some(100));
        let events: Vec<SessionEvent> = stream.collect().await;

        assert_eq!(events.len(), 0);
    }

    #[tokio::test]
    async fn test_replay_empty_checkpoint_with_live() {
        let checkpoint = CheckpointManager::new("sess_6".to_string());

        let (tx, rx) = broadcast::channel::<SessionEvent>(16);

        // from_seq=0 with empty checkpoint → just live events
        let stream = create_event_stream(&checkpoint, rx, Some(0));

        tx.send(message_event(1)).unwrap();
        drop(tx);

        let events: Vec<SessionEvent> = stream.collect().await;

        assert_eq!(events.len(), 1);
        assert_eq!(get_seq(&events[0]), 1);
    }
}
