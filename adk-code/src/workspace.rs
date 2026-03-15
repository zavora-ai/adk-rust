//! Workspace and collaboration types for collaborative project builds.
//!
//! This module provides the [`Workspace`] abstraction for multi-agent code generation
//! and project-building flows, along with typed [`CollaborationEvent`]s that support
//! ownership, correlation, and wait/resume semantics.
//!
//! ## Overview
//!
//! A [`Workspace`] represents a shared project context where specialist agents
//! coordinate through typed collaboration events rather than raw pub/sub.
//! The collaboration model preserves ownership, correlation, and completion
//! semantics so agents can request dependencies and resume only when matching
//! work is published.
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_code::Workspace;
//!
//! let workspace = Workspace::new("./demo-site")
//!     .project_name("demo-site")
//!     .session_id("session-123")
//!     .build();
//! assert_eq!(workspace.metadata().project_name, "demo-site");
//! ```
//!
//! ## Collaboration Events
//!
//! ```rust
//! use adk_code::{CollaborationEvent, CollaborationEventKind};
//!
//! let event = CollaborationEvent::new(
//!     "corr-001",
//!     "backend-api",
//!     "backend_engineer",
//!     CollaborationEventKind::WorkPublished,
//! );
//! assert_eq!(event.kind, CollaborationEventKind::WorkPublished);
//! ```
//!
//! ## Publish, Subscribe, and Wait/Resume
//!
//! ```rust,no_run
//! # async fn example() {
//! use adk_code::{CollaborationEvent, CollaborationEventKind, Workspace};
//! use std::time::Duration;
//!
//! let workspace = Workspace::new("./project").build();
//!
//! // Subscribe to all events
//! let mut rx = workspace.subscribe();
//!
//! // Publish an event
//! workspace.publish(CollaborationEvent::new(
//!     "corr-1", "api", "backend", CollaborationEventKind::WorkPublished,
//! ));
//!
//! // Wait for a correlated response
//! let result = workspace.wait_for("corr-1", Duration::from_secs(5)).await;
//! # }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::broadcast;

/// Default capacity for the internal broadcast channel.
const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Internal shared state behind [`Workspace`].
///
/// This struct is wrapped in `Arc` so that `Workspace` can be cheaply cloned
/// and shared across agents and async tasks. The broadcast channel provides
/// the in-process collaboration transport.
#[derive(Debug)]
struct WorkspaceInner {
    /// The shared project root directory.
    root: PathBuf,
    /// Project and session metadata.
    metadata: WorkspaceMetadata,
    /// Sender side of the broadcast channel for collaboration events.
    tx: broadcast::Sender<CollaborationEvent>,
    /// Append-only event log so [`Workspace::events`] can return history
    /// regardless of when subscribers were created.
    event_log: RwLock<Vec<CollaborationEvent>>,
}

/// A shared project context for collaborative code generation and execution.
///
/// `Workspace` is the public anchor for multi-agent project-building flows.
/// It represents a shared project root, metadata, and collaboration state.
/// Specialist agents attached to the same workspace can publish and consume
/// typed [`CollaborationEvent`]s without configuring raw pub/sub directly.
///
/// Internally, `Workspace` uses `Arc<WorkspaceInner>` so it can be cheaply
/// cloned and shared across agents and async boundaries. The collaboration
/// transport is an in-process broadcast channel — transport details are hidden
/// from the public API.
///
/// Use [`Workspace::new`] to get a [`WorkspaceBuilder`] for ergonomic construction.
///
/// # Example
///
/// ```rust
/// use adk_code::Workspace;
///
/// let workspace = Workspace::new("./my-project")
///     .project_name("my-project")
///     .session_id("sess-abc")
///     .build();
///
/// assert_eq!(workspace.root(), &std::path::PathBuf::from("./my-project"));
/// assert_eq!(workspace.metadata().project_name, "my-project");
/// assert_eq!(workspace.metadata().session_id.as_deref(), Some("sess-abc"));
/// ```
#[derive(Debug, Clone)]
pub struct Workspace {
    inner: Arc<WorkspaceInner>,
}

impl Workspace {
    /// Start building a new workspace rooted at the given path.
    ///
    /// Returns a [`WorkspaceBuilder`] for fluent configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("/tmp/project").build();
    /// assert_eq!(ws.root(), &std::path::PathBuf::from("/tmp/project"));
    /// ```
    // Intentional: `new` returns a builder per the design doc's fluent API
    // (`Workspace::new("./path").project_name("demo").build()`).
    #[allow(clippy::new_ret_no_self)]
    pub fn new(root: impl Into<PathBuf>) -> WorkspaceBuilder {
        WorkspaceBuilder {
            root: root.into(),
            project_name: None,
            session_id: None,
            created_at: None,
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    /// The shared project root directory.
    pub fn root(&self) -> &PathBuf {
        &self.inner.root
    }

    /// Project and session metadata.
    pub fn metadata(&self) -> &WorkspaceMetadata {
        &self.inner.metadata
    }

    /// Publish a collaboration event to all subscribers.
    ///
    /// This is a non-blocking operation. If there are no active subscribers,
    /// the event is silently dropped. Returns the number of receivers that
    /// received the event.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::{CollaborationEvent, CollaborationEventKind, Workspace};
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let mut rx = ws.subscribe();
    ///
    /// ws.publish(CollaborationEvent::new(
    ///     "c1", "api", "backend", CollaborationEventKind::WorkPublished,
    /// ));
    /// ```
    pub fn publish(&self, event: CollaborationEvent) -> usize {
        // Append to the persistent event log before broadcasting.
        if let Ok(mut log) = self.inner.event_log.write() {
            log.push(event.clone());
        }
        // If no receivers are listening, send returns Err — that is fine.
        self.inner.tx.send(event).unwrap_or(0)
    }

    /// Subscribe to collaboration events on this workspace.
    ///
    /// Returns a [`broadcast::Receiver`] that yields every event published
    /// after the subscription is created. Each subscriber gets its own
    /// independent stream of events.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::{CollaborationEvent, CollaborationEventKind, Workspace};
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let mut rx = ws.subscribe();
    ///
    /// ws.publish(CollaborationEvent::new(
    ///     "c1", "topic", "producer", CollaborationEventKind::NeedWork,
    /// ));
    /// ```
    pub fn subscribe(&self) -> broadcast::Receiver<CollaborationEvent> {
        self.inner.tx.subscribe()
    }

    /// Wait for a collaboration event matching the given `correlation_id`.
    ///
    /// Subscribes to the workspace event stream and returns the first event
    /// whose `correlation_id` matches. If no matching event arrives within
    /// `timeout`, returns `None`.
    ///
    /// This implements the wait/resume pattern: an agent can publish a
    /// [`CollaborationEventKind::NeedWork`] event and then call `wait_for`
    /// to suspend until the matching [`CollaborationEventKind::WorkPublished`]
    /// (or other correlated response) arrives.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() {
    /// use adk_code::{CollaborationEvent, CollaborationEventKind, Workspace};
    /// use std::time::Duration;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// // In practice another agent would publish the matching event.
    /// let result = ws.wait_for("corr-42", Duration::from_millis(100)).await;
    /// assert!(result.is_none()); // timed out — no publisher
    /// # }
    /// ```
    pub async fn wait_for(
        &self,
        correlation_id: &str,
        timeout: Duration,
    ) -> Option<CollaborationEvent> {
        let mut rx = self.subscribe();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(event) if event.correlation_id == correlation_id => {
                            return Some(event);
                        }
                        Ok(_) => {
                            // Not our correlation — keep waiting.
                            continue;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                skipped,
                                "workspace subscriber lagged, {skipped} events dropped"
                            );
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return None;
                        }
                    }
                }
                () = &mut deadline => {
                    return None;
                }
            }
        }
    }

    /// Get a snapshot of all events published to this workspace.
    ///
    /// Returns a clone of the internal event log. Unlike the broadcast
    /// channel (which has a fixed capacity and drops old events for slow
    /// subscribers), the event log retains every event published since
    /// workspace creation.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::{CollaborationEvent, CollaborationEventKind, Workspace};
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.publish(CollaborationEvent::new(
    ///     "c1", "topic", "producer", CollaborationEventKind::Completed,
    /// ));
    /// let events = ws.events();
    /// // events may contain the published event if still in the buffer
    /// ```
    pub fn events(&self) -> Vec<CollaborationEvent> {
        self.inner.event_log.read().map(|log| log.clone()).unwrap_or_default()
    }

    // ── Agent-facing workspace integration helpers ──────────────────────
    //
    // These thin wrappers make the common collaborative patterns easy
    // without exposing event construction details. Each method constructs
    // the appropriate `CollaborationEvent` and publishes it.

    /// Request work from another specialist or coordinator.
    ///
    /// Publishes a [`CollaborationEventKind::NeedWork`] event and returns
    /// the event that was published. The caller can then use
    /// [`Workspace::wait_for_work`] to suspend until the matching
    /// [`CollaborationEventKind::WorkPublished`] event arrives.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let event = ws.request_work("corr-1", "api-routes", "frontend_engineer");
    /// assert_eq!(event.kind, adk_code::CollaborationEventKind::NeedWork);
    /// ```
    pub fn request_work(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
    ) -> CollaborationEvent {
        let event = CollaborationEvent::new(
            correlation_id,
            topic,
            producer,
            CollaborationEventKind::NeedWork,
        );
        self.publish(event.clone());
        event
    }

    /// Claim ownership of a requested work item.
    ///
    /// Publishes a [`CollaborationEventKind::WorkClaimed`] event to signal
    /// that this agent is taking responsibility for the work.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.claim_work("corr-1", "api-routes", "backend_engineer");
    /// ```
    pub fn claim_work(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
    ) {
        self.publish(CollaborationEvent::new(
            correlation_id,
            topic,
            producer,
            CollaborationEventKind::WorkClaimed,
        ));
    }

    /// Publish completed work to the workspace.
    ///
    /// Publishes a [`CollaborationEventKind::WorkPublished`] event with the
    /// given payload. Agents waiting via [`Workspace::wait_for_work`] on the
    /// same `correlation_id` will be resumed.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.publish_work(
    ///     "corr-1",
    ///     "api-routes",
    ///     "backend_engineer",
    ///     serde_json::json!({ "routes": ["/api/users"] }),
    /// );
    /// ```
    pub fn publish_work(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
        payload: Value,
    ) {
        self.publish(
            CollaborationEvent::new(
                correlation_id,
                topic,
                producer,
                CollaborationEventKind::WorkPublished,
            )
            .payload(payload),
        );
    }

    /// Request feedback from another specialist or reviewer.
    ///
    /// Publishes a [`CollaborationEventKind::FeedbackRequested`] event.
    /// The caller can then use [`Workspace::wait_for_feedback`] to suspend
    /// until the matching [`CollaborationEventKind::FeedbackProvided`] arrives.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.request_feedback(
    ///     "corr-2",
    ///     "api-contract",
    ///     "backend_engineer",
    ///     serde_json::json!({ "schema": "v1" }),
    /// );
    /// ```
    pub fn request_feedback(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
        payload: Value,
    ) {
        self.publish(
            CollaborationEvent::new(
                correlation_id,
                topic,
                producer,
                CollaborationEventKind::FeedbackRequested,
            )
            .payload(payload),
        );
    }

    /// Provide feedback in response to a feedback request.
    ///
    /// Publishes a [`CollaborationEventKind::FeedbackProvided`] event.
    /// Agents waiting via [`Workspace::wait_for_feedback`] on the same
    /// `correlation_id` will be resumed.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.provide_feedback(
    ///     "corr-2",
    ///     "api-contract",
    ///     "reviewer",
    ///     serde_json::json!({ "approved": true }),
    /// );
    /// ```
    pub fn provide_feedback(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
        payload: Value,
    ) {
        self.publish(
            CollaborationEvent::new(
                correlation_id,
                topic,
                producer,
                CollaborationEventKind::FeedbackProvided,
            )
            .payload(payload),
        );
    }

    /// Signal that this agent is blocked and cannot continue.
    ///
    /// Publishes a [`CollaborationEventKind::Blocked`] event with a payload
    /// describing what is needed to unblock.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.signal_blocked(
    ///     "corr-3",
    ///     "database-schema",
    ///     "backend_engineer",
    ///     serde_json::json!({ "needs": "schema approval" }),
    /// );
    /// ```
    pub fn signal_blocked(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
        payload: Value,
    ) {
        self.publish(
            CollaborationEvent::new(
                correlation_id,
                topic,
                producer,
                CollaborationEventKind::Blocked,
            )
            .payload(payload),
        );
    }

    /// Signal that a work item is completed.
    ///
    /// Publishes a [`CollaborationEventKind::Completed`] event.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::Workspace;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// ws.signal_completed("corr-1", "api-routes", "backend_engineer");
    /// ```
    pub fn signal_completed(
        &self,
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
    ) {
        self.publish(CollaborationEvent::new(
            correlation_id,
            topic,
            producer,
            CollaborationEventKind::Completed,
        ));
    }

    /// Wait for a [`CollaborationEventKind::WorkPublished`] event matching
    /// the given `correlation_id`.
    ///
    /// This is a convenience wrapper over [`Workspace::wait_for_kind`] that
    /// filters for `WorkPublished` events specifically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() {
    /// use adk_code::Workspace;
    /// use std::time::Duration;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let result = ws.wait_for_work("corr-1", Duration::from_secs(5)).await;
    /// # }
    /// ```
    pub async fn wait_for_work(
        &self,
        correlation_id: &str,
        timeout: Duration,
    ) -> Option<CollaborationEvent> {
        self.wait_for_kind(correlation_id, CollaborationEventKind::WorkPublished, timeout).await
    }

    /// Wait for a [`CollaborationEventKind::FeedbackProvided`] event matching
    /// the given `correlation_id`.
    ///
    /// This is a convenience wrapper over [`Workspace::wait_for_kind`] that
    /// filters for `FeedbackProvided` events specifically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() {
    /// use adk_code::Workspace;
    /// use std::time::Duration;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let result = ws.wait_for_feedback("corr-2", Duration::from_secs(5)).await;
    /// # }
    /// ```
    pub async fn wait_for_feedback(
        &self,
        correlation_id: &str,
        timeout: Duration,
    ) -> Option<CollaborationEvent> {
        self.wait_for_kind(correlation_id, CollaborationEventKind::FeedbackProvided, timeout).await
    }

    /// Wait for a collaboration event matching both `correlation_id` and `kind`.
    ///
    /// Subscribes to the workspace event stream and returns the first event
    /// whose `correlation_id` and `kind` both match. If no matching event
    /// arrives within `timeout`, returns `None`.
    ///
    /// This is the most precise wait primitive — use it when you need to
    /// filter on a specific event kind rather than any correlated event.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() {
    /// use adk_code::{CollaborationEventKind, Workspace};
    /// use std::time::Duration;
    ///
    /// let ws = Workspace::new("./proj").build();
    /// let result = ws
    ///     .wait_for_kind("corr-1", CollaborationEventKind::WorkClaimed, Duration::from_secs(5))
    ///     .await;
    /// # }
    /// ```
    pub async fn wait_for_kind(
        &self,
        correlation_id: &str,
        kind: CollaborationEventKind,
        timeout: Duration,
    ) -> Option<CollaborationEvent> {
        let mut rx = self.subscribe();
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(event)
                            if event.correlation_id == correlation_id
                                && event.kind == kind =>
                        {
                            return Some(event);
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                skipped,
                                "workspace subscriber lagged, {skipped} events dropped"
                            );
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return None;
                        }
                    }
                }
                () = &mut deadline => {
                    return None;
                }
            }
        }
    }
}

/// Builder for constructing a [`Workspace`] with fluent configuration.
///
/// # Example
///
/// ```rust
/// use adk_code::Workspace;
///
/// let workspace = Workspace::new("./app")
///     .project_name("my-app")
///     .session_id("session-42")
///     .created_at(1719000000)
///     .build();
///
/// assert_eq!(workspace.metadata().project_name, "my-app");
/// assert_eq!(workspace.metadata().created_at, Some(1719000000));
/// ```
#[derive(Debug, Clone)]
pub struct WorkspaceBuilder {
    root: PathBuf,
    project_name: Option<String>,
    session_id: Option<String>,
    created_at: Option<u64>,
    channel_capacity: usize,
}

impl WorkspaceBuilder {
    /// Set the project name.
    ///
    /// If not set, defaults to the root directory's file name or `"unnamed"`.
    pub fn project_name(mut self, name: impl Into<String>) -> Self {
        self.project_name = Some(name.into());
        self
    }

    /// Set the session ID for execution correlation.
    pub fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = Some(id.into());
        self
    }

    /// Set the workspace creation timestamp (Unix epoch seconds).
    pub fn created_at(mut self, timestamp: u64) -> Self {
        self.created_at = Some(timestamp);
        self
    }

    /// Set the broadcast channel capacity for collaboration events.
    ///
    /// Defaults to 256. Larger values retain more event history at the cost
    /// of memory. Events beyond the capacity are dropped for slow subscribers.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity;
        self
    }

    /// Build the [`Workspace`].
    ///
    /// If `project_name` was not set, it defaults to the root directory's
    /// file name component, or `"unnamed"` if the path has no file name.
    pub fn build(self) -> Workspace {
        let project_name = self.project_name.unwrap_or_else(|| {
            self.root.file_name().and_then(|n| n.to_str()).unwrap_or("unnamed").to_string()
        });

        let (tx, _rx) = broadcast::channel(self.channel_capacity);

        Workspace {
            inner: Arc::new(WorkspaceInner {
                root: self.root,
                metadata: WorkspaceMetadata {
                    project_name,
                    session_id: self.session_id,
                    created_at: self.created_at,
                },
                tx,
                event_log: RwLock::new(Vec::new()),
            }),
        }
    }
}

/// Metadata about a workspace project and execution session.
///
/// # Example
///
/// ```rust
/// use adk_code::WorkspaceMetadata;
///
/// let meta = WorkspaceMetadata {
///     project_name: "demo".to_string(),
///     session_id: Some("sess-1".to_string()),
///     created_at: Some(1719000000),
/// };
/// assert_eq!(meta.project_name, "demo");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMetadata {
    /// Human-readable project name.
    pub project_name: String,
    /// Optional session ID for execution and telemetry correlation.
    pub session_id: Option<String>,
    /// Optional creation timestamp (Unix epoch seconds).
    pub created_at: Option<u64>,
}

/// The kind of a collaboration event in a shared workspace.
///
/// These typed event kinds support ownership, correlation, and wait/resume
/// semantics for multi-agent project builds. They are more disciplined than
/// raw pub/sub and preserve completion semantics.
///
/// # Example
///
/// ```rust
/// use adk_code::CollaborationEventKind;
///
/// let kind = CollaborationEventKind::NeedWork;
/// assert_ne!(kind, CollaborationEventKind::Completed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollaborationEventKind {
    /// An agent requests a dependency from another specialist or coordinator.
    NeedWork,
    /// Another agent or coordinator accepts ownership of the requested work.
    WorkClaimed,
    /// The requested work product is now available in the workspace.
    WorkPublished,
    /// The producer asks for review or contract validation.
    FeedbackRequested,
    /// A specialist responds with approval or requested changes.
    FeedbackProvided,
    /// The producer cannot continue without another dependency or decision.
    Blocked,
    /// The work item is done.
    Completed,
}

impl std::fmt::Display for CollaborationEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedWork => write!(f, "NeedWork"),
            Self::WorkClaimed => write!(f, "WorkClaimed"),
            Self::WorkPublished => write!(f, "WorkPublished"),
            Self::FeedbackRequested => write!(f, "FeedbackRequested"),
            Self::FeedbackProvided => write!(f, "FeedbackProvided"),
            Self::Blocked => write!(f, "Blocked"),
            Self::Completed => write!(f, "Completed"),
        }
    }
}

/// A typed collaboration event for cross-agent coordination in a shared workspace.
///
/// Collaboration events carry correlation IDs, topic names, producer/consumer
/// identities, and structured payloads. They support the wait/resume pattern:
/// an agent can publish a [`CollaborationEventKind::NeedWork`] event and resume
/// only when a matching [`CollaborationEventKind::WorkPublished`] event arrives.
///
/// # Example
///
/// ```rust
/// use adk_code::{CollaborationEvent, CollaborationEventKind};
///
/// let event = CollaborationEvent::new(
///     "corr-42",
///     "api-routes",
///     "backend_engineer",
///     CollaborationEventKind::WorkPublished,
/// )
/// .consumer("frontend_engineer")
/// .payload(serde_json::json!({ "routes": ["/api/users"] }));
///
/// assert_eq!(event.correlation_id, "corr-42");
/// assert_eq!(event.consumer.as_deref(), Some("frontend_engineer"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationEvent {
    /// Correlation ID linking related events (e.g., request and response).
    pub correlation_id: String,
    /// Topic or work item name this event relates to.
    pub topic: String,
    /// Identity of the agent that produced this event.
    pub producer: String,
    /// Identity of the intended consumer, if targeted.
    pub consumer: Option<String>,
    /// The kind of collaboration event.
    pub kind: CollaborationEventKind,
    /// Structured payload carrying event-specific data.
    pub payload: Value,
    /// Timestamp when the event was created (Unix epoch milliseconds).
    pub timestamp: u64,
}

impl CollaborationEvent {
    /// Create a new collaboration event with the given correlation ID, topic,
    /// producer, and kind.
    ///
    /// The payload defaults to `null`, consumer defaults to `None`, and
    /// timestamp defaults to `0` (callers should set it via [`Self::timestamp`]).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::{CollaborationEvent, CollaborationEventKind};
    ///
    /// let event = CollaborationEvent::new(
    ///     "req-1",
    ///     "database-schema",
    ///     "coordinator",
    ///     CollaborationEventKind::NeedWork,
    /// );
    /// assert_eq!(event.kind, CollaborationEventKind::NeedWork);
    /// assert_eq!(event.producer, "coordinator");
    /// ```
    pub fn new(
        correlation_id: impl Into<String>,
        topic: impl Into<String>,
        producer: impl Into<String>,
        kind: CollaborationEventKind,
    ) -> Self {
        Self {
            correlation_id: correlation_id.into(),
            topic: topic.into(),
            producer: producer.into(),
            consumer: None,
            kind,
            payload: Value::Null,
            timestamp: 0,
        }
    }

    /// Set the intended consumer for this event.
    pub fn consumer(mut self, consumer: impl Into<String>) -> Self {
        self.consumer = Some(consumer.into());
        self
    }

    /// Set the structured payload for this event.
    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }

    /// Set the timestamp (Unix epoch milliseconds).
    pub fn timestamp(mut self, ts: u64) -> Self {
        self.timestamp = ts;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_builder_defaults_project_name_from_root() {
        let ws = Workspace::new("/tmp/my-project").build();
        assert_eq!(ws.root(), &PathBuf::from("/tmp/my-project"));
        assert_eq!(ws.metadata().project_name, "my-project");
        assert_eq!(ws.metadata().session_id, None);
        assert_eq!(ws.metadata().created_at, None);
    }

    #[test]
    fn workspace_builder_with_all_fields() {
        let ws = Workspace::new("./demo")
            .project_name("demo-site")
            .session_id("sess-abc")
            .created_at(1719000000)
            .build();
        assert_eq!(ws.root(), &PathBuf::from("./demo"));
        assert_eq!(ws.metadata().project_name, "demo-site");
        assert_eq!(ws.metadata().session_id.as_deref(), Some("sess-abc"));
        assert_eq!(ws.metadata().created_at, Some(1719000000));
    }

    #[test]
    fn workspace_builder_unnamed_fallback() {
        let ws = Workspace::new("/").build();
        assert_eq!(ws.metadata().project_name, "unnamed");
    }

    #[test]
    fn workspace_clone_shares_transport() {
        let ws1 = Workspace::new("./proj").build();
        let ws2 = ws1.clone();
        let mut rx = ws2.subscribe();

        ws1.publish(CollaborationEvent::new(
            "c1",
            "topic",
            "producer",
            CollaborationEventKind::WorkPublished,
        ));

        let event = rx.try_recv().expect("should receive event from clone");
        assert_eq!(event.correlation_id, "c1");
    }

    #[test]
    fn publish_with_no_subscribers_returns_zero() {
        let ws = Workspace::new("./proj").build();
        let count = ws.publish(CollaborationEvent::new(
            "c1",
            "topic",
            "producer",
            CollaborationEventKind::NeedWork,
        ));
        assert_eq!(count, 0);
    }

    #[test]
    fn publish_with_subscriber_returns_count() {
        let ws = Workspace::new("./proj").build();
        let _rx1 = ws.subscribe();
        let _rx2 = ws.subscribe();

        let count = ws.publish(CollaborationEvent::new(
            "c1",
            "topic",
            "producer",
            CollaborationEventKind::NeedWork,
        ));
        assert_eq!(count, 2);
    }

    #[test]
    fn subscribe_receives_published_events() {
        let ws = Workspace::new("./proj").build();
        let mut rx = ws.subscribe();

        ws.publish(CollaborationEvent::new(
            "c1",
            "api",
            "backend",
            CollaborationEventKind::WorkPublished,
        ));
        ws.publish(CollaborationEvent::new(
            "c2",
            "schema",
            "db",
            CollaborationEventKind::Completed,
        ));

        let e1 = rx.try_recv().unwrap();
        assert_eq!(e1.correlation_id, "c1");
        assert_eq!(e1.kind, CollaborationEventKind::WorkPublished);

        let e2 = rx.try_recv().unwrap();
        assert_eq!(e2.correlation_id, "c2");
        assert_eq!(e2.kind, CollaborationEventKind::Completed);
    }

    #[tokio::test]
    async fn wait_for_returns_matching_event() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        // Spawn a task that publishes the matching event after a short delay.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Publish a non-matching event first.
            ws_clone.publish(CollaborationEvent::new(
                "other",
                "unrelated",
                "someone",
                CollaborationEventKind::NeedWork,
            ));
            // Then publish the matching event.
            ws_clone.publish(
                CollaborationEvent::new(
                    "target",
                    "api",
                    "backend",
                    CollaborationEventKind::WorkPublished,
                )
                .payload(serde_json::json!({ "done": true })),
            );
        });

        let result = ws.wait_for("target", Duration::from_secs(1)).await;
        let event = result.expect("should receive matching event");
        assert_eq!(event.correlation_id, "target");
        assert_eq!(event.kind, CollaborationEventKind::WorkPublished);
        assert_eq!(event.payload, serde_json::json!({ "done": true }));
    }

    #[tokio::test]
    async fn wait_for_times_out_when_no_match() {
        let ws = Workspace::new("./proj").build();
        let result = ws.wait_for("nonexistent", Duration::from_millis(50)).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn wait_for_ignores_non_matching_events() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            // Publish several non-matching events.
            for i in 0..5 {
                ws_clone.publish(CollaborationEvent::new(
                    format!("wrong-{i}"),
                    "topic",
                    "producer",
                    CollaborationEventKind::NeedWork,
                ));
            }
            // Then publish the matching one.
            ws_clone.publish(CollaborationEvent::new(
                "right",
                "topic",
                "producer",
                CollaborationEventKind::WorkPublished,
            ));
        });

        let result = ws.wait_for("right", Duration::from_secs(1)).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().correlation_id, "right");
    }

    #[test]
    fn events_returns_buffered_events() {
        let ws = Workspace::new("./proj").channel_capacity(16).build();

        ws.publish(CollaborationEvent::new("c1", "t1", "p1", CollaborationEventKind::NeedWork));
        ws.publish(CollaborationEvent::new("c2", "t2", "p2", CollaborationEventKind::Completed));

        let events = ws.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].correlation_id, "c1");
        assert_eq!(events[1].correlation_id, "c2");
    }

    #[test]
    fn collaboration_event_kind_display() {
        assert_eq!(CollaborationEventKind::NeedWork.to_string(), "NeedWork");
        assert_eq!(CollaborationEventKind::WorkClaimed.to_string(), "WorkClaimed");
        assert_eq!(CollaborationEventKind::WorkPublished.to_string(), "WorkPublished");
        assert_eq!(CollaborationEventKind::FeedbackRequested.to_string(), "FeedbackRequested");
        assert_eq!(CollaborationEventKind::FeedbackProvided.to_string(), "FeedbackProvided");
        assert_eq!(CollaborationEventKind::Blocked.to_string(), "Blocked");
        assert_eq!(CollaborationEventKind::Completed.to_string(), "Completed");
    }

    #[test]
    fn collaboration_event_new_defaults() {
        let event = CollaborationEvent::new(
            "corr-1",
            "backend-api",
            "coordinator",
            CollaborationEventKind::NeedWork,
        );
        assert_eq!(event.correlation_id, "corr-1");
        assert_eq!(event.topic, "backend-api");
        assert_eq!(event.producer, "coordinator");
        assert_eq!(event.consumer, None);
        assert_eq!(event.kind, CollaborationEventKind::NeedWork);
        assert_eq!(event.payload, Value::Null);
        assert_eq!(event.timestamp, 0);
    }

    #[test]
    fn collaboration_event_builder_methods() {
        let event = CollaborationEvent::new(
            "corr-2",
            "api-routes",
            "backend_engineer",
            CollaborationEventKind::WorkPublished,
        )
        .consumer("frontend_engineer")
        .payload(serde_json::json!({ "routes": ["/api/users"] }))
        .timestamp(1719000000000);

        assert_eq!(event.consumer.as_deref(), Some("frontend_engineer"));
        assert_eq!(event.payload, serde_json::json!({ "routes": ["/api/users"] }));
        assert_eq!(event.timestamp, 1719000000000);
    }

    #[test]
    fn collaboration_event_kind_equality_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CollaborationEventKind::NeedWork);
        set.insert(CollaborationEventKind::NeedWork);
        set.insert(CollaborationEventKind::Completed);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn collaboration_event_kind_copy() {
        let kind = CollaborationEventKind::Blocked;
        let copy = kind;
        assert_eq!(kind, copy);
    }

    #[test]
    fn workspace_metadata_serialization_roundtrip() {
        let meta = WorkspaceMetadata {
            project_name: "test-proj".to_string(),
            session_id: Some("sess-1".to_string()),
            created_at: Some(1719000000),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: WorkspaceMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, deserialized);
    }

    #[test]
    fn collaboration_event_serialization_roundtrip() {
        let event = CollaborationEvent::new(
            "corr-rt",
            "schema",
            "db_engineer",
            CollaborationEventKind::FeedbackRequested,
        )
        .consumer("reviewer")
        .payload(serde_json::json!({ "tables": ["users"] }))
        .timestamp(1719000000000);

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: CollaborationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.correlation_id, "corr-rt");
        assert_eq!(deserialized.kind, CollaborationEventKind::FeedbackRequested);
        assert_eq!(deserialized.consumer.as_deref(), Some("reviewer"));
    }

    // ── Agent-facing helper tests ──────────────────────────────────────

    #[test]
    fn request_work_publishes_need_work_event() {
        let ws = Workspace::new("./proj").build();
        let event = ws.request_work("corr-rw", "api-routes", "frontend");
        assert_eq!(event.correlation_id, "corr-rw");
        assert_eq!(event.topic, "api-routes");
        assert_eq!(event.producer, "frontend");
        assert_eq!(event.kind, CollaborationEventKind::NeedWork);

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CollaborationEventKind::NeedWork);
    }

    #[test]
    fn claim_work_publishes_work_claimed_event() {
        let ws = Workspace::new("./proj").build();
        ws.claim_work("corr-cw", "api-routes", "backend");

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].correlation_id, "corr-cw");
        assert_eq!(events[0].kind, CollaborationEventKind::WorkClaimed);
    }

    #[test]
    fn publish_work_publishes_work_published_with_payload() {
        let ws = Workspace::new("./proj").build();
        ws.publish_work(
            "corr-pw",
            "api-routes",
            "backend",
            serde_json::json!({ "routes": ["/users"] }),
        );

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CollaborationEventKind::WorkPublished);
        assert_eq!(events[0].payload, serde_json::json!({ "routes": ["/users"] }));
    }

    #[test]
    fn request_feedback_publishes_feedback_requested_with_payload() {
        let ws = Workspace::new("./proj").build();
        ws.request_feedback(
            "corr-rf",
            "api-contract",
            "backend",
            serde_json::json!({ "schema": "v1" }),
        );

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CollaborationEventKind::FeedbackRequested);
        assert_eq!(events[0].payload, serde_json::json!({ "schema": "v1" }));
    }

    #[test]
    fn provide_feedback_publishes_feedback_provided_with_payload() {
        let ws = Workspace::new("./proj").build();
        ws.provide_feedback(
            "corr-pf",
            "api-contract",
            "reviewer",
            serde_json::json!({ "approved": true }),
        );

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CollaborationEventKind::FeedbackProvided);
        assert_eq!(events[0].payload, serde_json::json!({ "approved": true }));
    }

    #[test]
    fn signal_blocked_publishes_blocked_with_payload() {
        let ws = Workspace::new("./proj").build();
        ws.signal_blocked(
            "corr-sb",
            "database-schema",
            "backend",
            serde_json::json!({ "needs": "approval" }),
        );

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CollaborationEventKind::Blocked);
        assert_eq!(events[0].payload, serde_json::json!({ "needs": "approval" }));
    }

    #[test]
    fn signal_completed_publishes_completed_event() {
        let ws = Workspace::new("./proj").build();
        ws.signal_completed("corr-sc", "api-routes", "backend");

        let events = ws.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].correlation_id, "corr-sc");
        assert_eq!(events[0].kind, CollaborationEventKind::Completed);
    }

    #[tokio::test]
    async fn wait_for_work_returns_work_published_event() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Publish a non-matching kind first (same correlation).
            ws_clone.claim_work("corr-wfw", "api", "backend");
            // Then publish the matching WorkPublished.
            ws_clone.publish_work(
                "corr-wfw",
                "api",
                "backend",
                serde_json::json!({ "done": true }),
            );
        });

        let result = ws.wait_for_work("corr-wfw", Duration::from_secs(1)).await;
        let event = result.expect("should receive WorkPublished event");
        assert_eq!(event.kind, CollaborationEventKind::WorkPublished);
        assert_eq!(event.payload, serde_json::json!({ "done": true }));
    }

    #[tokio::test]
    async fn wait_for_feedback_returns_feedback_provided_event() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Publish a FeedbackRequested first (same correlation, wrong kind).
            ws_clone.request_feedback(
                "corr-wff",
                "contract",
                "backend",
                serde_json::json!({ "schema": "v1" }),
            );
            // Then publish the matching FeedbackProvided.
            ws_clone.provide_feedback(
                "corr-wff",
                "contract",
                "reviewer",
                serde_json::json!({ "approved": true }),
            );
        });

        let result = ws.wait_for_feedback("corr-wff", Duration::from_secs(1)).await;
        let event = result.expect("should receive FeedbackProvided event");
        assert_eq!(event.kind, CollaborationEventKind::FeedbackProvided);
        assert_eq!(event.payload, serde_json::json!({ "approved": true }));
    }

    #[tokio::test]
    async fn wait_for_kind_filters_by_both_correlation_and_kind() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            // Same correlation, wrong kind.
            ws_clone.request_work("corr-wfk", "topic", "agent-a");
            // Wrong correlation, right kind.
            ws_clone.claim_work("other-corr", "topic", "agent-b");
            // Both match.
            ws_clone.claim_work("corr-wfk", "topic", "agent-b");
        });

        let result = ws
            .wait_for_kind("corr-wfk", CollaborationEventKind::WorkClaimed, Duration::from_secs(1))
            .await;
        let event = result.expect("should receive matching event");
        assert_eq!(event.correlation_id, "corr-wfk");
        assert_eq!(event.kind, CollaborationEventKind::WorkClaimed);
        assert_eq!(event.producer, "agent-b");
    }

    #[tokio::test]
    async fn wait_for_kind_times_out_when_kind_does_not_match() {
        let ws = Workspace::new("./proj").build();
        let ws_clone = ws.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            // Right correlation, wrong kind.
            ws_clone.request_work("corr-to", "topic", "agent");
        });

        let result = ws
            .wait_for_kind("corr-to", CollaborationEventKind::Completed, Duration::from_millis(100))
            .await;
        assert!(result.is_none());
    }

    #[test]
    fn full_collaboration_flow_via_helpers() {
        let ws = Workspace::new("./proj").build();

        // Coordinator requests work.
        ws.request_work("flow-1", "backend-api", "coordinator");
        // Specialist claims it.
        ws.claim_work("flow-1", "backend-api", "backend_engineer");
        // Specialist publishes the result.
        ws.publish_work(
            "flow-1",
            "backend-api",
            "backend_engineer",
            serde_json::json!({ "endpoints": 3 }),
        );
        // Specialist requests feedback.
        ws.request_feedback(
            "flow-1",
            "backend-api",
            "backend_engineer",
            serde_json::json!({ "review": "please" }),
        );
        // Reviewer provides feedback.
        ws.provide_feedback(
            "flow-1",
            "backend-api",
            "reviewer",
            serde_json::json!({ "approved": true }),
        );
        // Specialist signals completion.
        ws.signal_completed("flow-1", "backend-api", "backend_engineer");

        let events = ws.events();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0].kind, CollaborationEventKind::NeedWork);
        assert_eq!(events[1].kind, CollaborationEventKind::WorkClaimed);
        assert_eq!(events[2].kind, CollaborationEventKind::WorkPublished);
        assert_eq!(events[3].kind, CollaborationEventKind::FeedbackRequested);
        assert_eq!(events[4].kind, CollaborationEventKind::FeedbackProvided);
        assert_eq!(events[5].kind, CollaborationEventKind::Completed);
    }
}
