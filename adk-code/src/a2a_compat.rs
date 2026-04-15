//! A2A-compatible event mapping boundary for collaboration transport.
//!
//! This module defines the [`CollaborationTransport`] trait that abstracts the
//! transport layer for [`CollaborationEvent`]s, and provides a [`LocalTransport`]
//! implementation backed by an in-process broadcast channel.
//!
//! ## A2A Concept Mapping
//!
//! The collaboration event model is designed to map cleanly onto the ADK A2A
//! protocol (see `adk-server/src/a2a/types.rs`) for future remote specialist
//! execution. The mapping is:
//!
//! | Collaboration Concept | A2A Concept |
//! |---|---|
//! | [`CollaborationEvent`] | A2A `Message` or `TaskStatusUpdateEvent` |
//! | `correlation_id` | A2A `task_id` — links related events into one task |
//! | `producer` | A2A agent card sender — the agent originating the event |
//! | `consumer` | A2A agent card receiver — the intended recipient agent |
//! | `kind` | A2A `TaskState` or message role (see kind mapping below) |
//! | `payload` | A2A `Artifact` parts or `Message` parts (structured data) |
//! | `topic` | A2A message metadata or artifact name |
//! | `timestamp` | A2A event timestamp |
//!
//! ### Event Kind → A2A Task State Mapping
//!
//! | `CollaborationEventKind` | A2A `TaskState` | Notes |
//! |---|---|---|
//! | `NeedWork` | `Submitted` | A new task is submitted for another agent |
//! | `WorkClaimed` | `Working` | The receiving agent has accepted the task |
//! | `WorkPublished` | `Completed` + artifact | Task completed with output artifact |
//! | `FeedbackRequested` | `InputRequired` | Agent needs input before continuing |
//! | `FeedbackProvided` | `Submitted` (response) | Input provided as a new message |
//! | `Blocked` | `InputRequired` + metadata | Agent blocked, needs external decision |
//! | `Completed` | `Completed` (final) | Terminal state, no more updates |
//!
//! ### Transport Neutrality
//!
//! Phase 1 uses [`LocalTransport`] (in-process broadcast channel). The event
//! envelope is transport-neutral: the same [`CollaborationEvent`] struct works
//! whether the transport is local or remote. A future `A2aTransport` would
//! serialize events into A2A JSON-RPC messages and route them over HTTP/SSE
//! without changing the [`CollaborationTransport`] trait contract.
//!
//! ### Migration Path to A2A
//!
//! To add remote specialist execution later:
//!
//! 1. Implement `CollaborationTransport` backed by an A2A client
//! 2. Map `CollaborationEvent` → A2A `Message` on publish
//! 3. Map A2A `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` → `CollaborationEvent` on receive
//! 4. Use `correlation_id` as the A2A `task_id` for routing
//! 5. Use `producer`/`consumer` to resolve agent card URLs
//!
//! The `Workspace` API remains unchanged — only the transport implementation
//! swaps from local to remote.
//!
//! ## Example
//!
//! ```rust
//! use adk_code::a2a_compat::{CollaborationTransport, LocalTransport};
//! use adk_code::{CollaborationEvent, CollaborationEventKind};
//!
//! # async fn example() {
//! let transport = LocalTransport::new(64);
//! let mut receiver = transport.subscribe();
//!
//! let event = CollaborationEvent::new(
//!     "corr-1", "api-routes", "backend", CollaborationEventKind::WorkPublished,
//! );
//! transport.publish(event).await.unwrap();
//!
//! let received = receiver.recv().await;
//! assert!(received.is_some());
//! # }
//! ```

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::{CollaborationEvent, ExecutionError};

/// Async trait abstracting the collaboration event transport layer.
///
/// Implementations can be local (in-process broadcast) or remote (A2A over
/// HTTP/SSE). The trait is intentionally minimal — publish and subscribe —
/// so that higher-level semantics (correlation, wait/resume) stay in
/// [`Workspace`](crate::Workspace).
///
/// # Example
///
/// ```rust
/// use adk_code::a2a_compat::{CollaborationTransport, LocalTransport};
/// use adk_code::{CollaborationEvent, CollaborationEventKind};
///
/// # async fn example() {
/// let transport = LocalTransport::new(64);
/// let event = CollaborationEvent::new(
///     "c1", "topic", "agent", CollaborationEventKind::NeedWork,
/// );
/// transport.publish(event).await.unwrap();
/// # }
/// ```
#[async_trait]
pub trait CollaborationTransport: Send + Sync {
    /// Publish a collaboration event to all subscribers.
    async fn publish(&self, event: CollaborationEvent) -> Result<(), ExecutionError>;

    /// Create a new receiver for collaboration events.
    fn subscribe(&self) -> Box<dyn CollaborationReceiver>;
}

/// Async trait for receiving collaboration events from a transport.
///
/// Each receiver gets its own independent stream of events published
/// after the subscription was created.
#[async_trait]
pub trait CollaborationReceiver: Send {
    /// Receive the next collaboration event.
    ///
    /// Returns `None` if the transport is closed.
    async fn recv(&mut self) -> Option<CollaborationEvent>;
}

/// In-process collaboration transport backed by a [`broadcast`] channel.
///
/// This is the phase 1 default transport. It keeps all collaboration local
/// to the process while preserving the same [`CollaborationTransport`] trait
/// contract that a future A2A-backed transport would implement.
///
/// # Example
///
/// ```rust
/// use adk_code::a2a_compat::{CollaborationTransport, LocalTransport};
/// use adk_code::{CollaborationEvent, CollaborationEventKind};
///
/// # async fn example() {
/// let transport = LocalTransport::new(128);
/// let mut rx = transport.subscribe();
///
/// transport.publish(CollaborationEvent::new(
///     "c1", "topic", "agent", CollaborationEventKind::WorkPublished,
/// )).await.unwrap();
///
/// let event = rx.recv().await.unwrap();
/// assert_eq!(event.correlation_id, "c1");
/// # }
/// ```
#[derive(Debug)]
pub struct LocalTransport {
    tx: broadcast::Sender<CollaborationEvent>,
}

impl LocalTransport {
    /// Create a new local transport with the given channel capacity.
    ///
    /// The capacity determines how many events can be buffered before slow
    /// subscribers start lagging. A reasonable default is 256.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }
}

impl Default for LocalTransport {
    fn default() -> Self {
        Self::new(256)
    }
}

#[async_trait]
impl CollaborationTransport for LocalTransport {
    async fn publish(&self, event: CollaborationEvent) -> Result<(), ExecutionError> {
        // If no receivers are listening, send returns Err — that is fine,
        // the event is simply not delivered (same as Workspace::publish).
        let _ = self.tx.send(event);
        Ok(())
    }

    fn subscribe(&self) -> Box<dyn CollaborationReceiver> {
        Box::new(LocalReceiver { rx: self.tx.subscribe() })
    }
}

/// Receiver side of a [`LocalTransport`].
struct LocalReceiver {
    rx: broadcast::Receiver<CollaborationEvent>,
}

#[async_trait]
impl CollaborationReceiver for LocalReceiver {
    async fn recv(&mut self) -> Option<CollaborationEvent> {
        loop {
            match self.rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "local transport receiver lagged, {skipped} events dropped"
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CollaborationEventKind;

    #[tokio::test]
    async fn local_transport_publish_and_receive() {
        let transport = LocalTransport::new(16);
        let mut rx = transport.subscribe();

        let event = CollaborationEvent::new(
            "c1",
            "api-routes",
            "backend",
            CollaborationEventKind::WorkPublished,
        );
        transport.publish(event).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.correlation_id, "c1");
        assert_eq!(received.kind, CollaborationEventKind::WorkPublished);
    }

    #[tokio::test]
    async fn local_transport_multiple_subscribers() {
        let transport = LocalTransport::new(16);
        let mut rx1 = transport.subscribe();
        let mut rx2 = transport.subscribe();

        transport
            .publish(CollaborationEvent::new(
                "c1",
                "topic",
                "agent",
                CollaborationEventKind::NeedWork,
            ))
            .await
            .unwrap();

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.correlation_id, "c1");
        assert_eq!(e2.correlation_id, "c1");
    }

    #[tokio::test]
    async fn local_transport_publish_with_no_subscribers_succeeds() {
        let transport = LocalTransport::new(16);
        // No subscribers — publish should still succeed.
        let result = transport
            .publish(CollaborationEvent::new(
                "c1",
                "topic",
                "agent",
                CollaborationEventKind::Completed,
            ))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn local_transport_default_capacity() {
        let transport = LocalTransport::default();
        let mut rx = transport.subscribe();

        transport
            .publish(CollaborationEvent::new(
                "c1",
                "topic",
                "agent",
                CollaborationEventKind::WorkClaimed,
            ))
            .await
            .unwrap();

        let event = rx.recv().await.unwrap();
        assert_eq!(event.kind, CollaborationEventKind::WorkClaimed);
    }

    #[tokio::test]
    async fn local_transport_preserves_event_fields() {
        let transport = LocalTransport::new(16);
        let mut rx = transport.subscribe();

        let original = CollaborationEvent::new(
            "corr-42",
            "database-schema",
            "db_engineer",
            CollaborationEventKind::FeedbackRequested,
        )
        .consumer("reviewer")
        .payload(serde_json::json!({ "tables": ["users", "orders"] }))
        .timestamp(1719000000000);

        transport.publish(original).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.correlation_id, "corr-42");
        assert_eq!(received.topic, "database-schema");
        assert_eq!(received.producer, "db_engineer");
        assert_eq!(received.consumer.as_deref(), Some("reviewer"));
        assert_eq!(received.kind, CollaborationEventKind::FeedbackRequested);
        assert_eq!(received.payload, serde_json::json!({ "tables": ["users", "orders"] }));
        assert_eq!(received.timestamp, 1719000000000);
    }

    #[tokio::test]
    async fn local_transport_event_ordering() {
        let transport = LocalTransport::new(16);
        let mut rx = transport.subscribe();

        for i in 0..5 {
            transport
                .publish(CollaborationEvent::new(
                    format!("c{i}"),
                    "topic",
                    "agent",
                    CollaborationEventKind::NeedWork,
                ))
                .await
                .unwrap();
        }

        for i in 0..5 {
            let event = rx.recv().await.unwrap();
            assert_eq!(event.correlation_id, format!("c{i}"));
        }
    }
}
