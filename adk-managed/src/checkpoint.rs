//! Checkpoint management for resumable sessions.
//!
//! A [`CheckpointManager`] holds a session's event log, run state, pending tool calls and
//! session record, and writes them to its [`ManagedStateStore`] on
//! [`flush`](CheckpointManager::flush). The session loop flushes after every event, before
//! broadcasting it. Whether a flushed snapshot survives process loss is the store's
//! [`Durability`](crate::Durability).
//!
//! # Responsibilities
//!
//! 1. **Atomicity**: an event and the run state after it are recorded in one call
//! 2. **Load/resume**: [`restore`](CheckpointManager::restore) rebuilds a manager from a store
//! 3. **Event log**: an ordered log for replay

use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::state_store::{ManagedSessionState, ManagedStateStore, PendingAction, SessionRecord};
use crate::types::{RuntimeError, SessionEvent, SessionStatus};

/// Run-state persisted with each checkpoint.
///
/// Contains everything needed to resume a session after a crash:
/// the current sequence counter value, which tool calls are parked,
/// and the session's lifecycle status.
///
/// # Example
///
/// ```rust
/// use adk_managed::checkpoint::RunState;
/// use adk_managed::types::SessionStatus;
///
/// let state = RunState {
///     seq: 5,
///     pending_tool_ids: vec!["ctu_001".to_string()],
///     status: SessionStatus::Running,
/// };
/// assert_eq!(state.seq, 5);
/// assert!(!state.pending_tool_ids.is_empty());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunState {
    /// Current sequence counter value.
    pub seq: u64,
    /// IDs of custom tool calls that are currently parked (awaiting client response).
    pub pending_tool_ids: Vec<String>,
    /// Current session status.
    pub status: SessionStatus,
}

impl RunState {
    /// Create a new initial run state (seq=0, no pending tools, queued status).
    pub fn initial() -> Self {
        Self { seq: 0, pending_tool_ids: Vec::new(), status: SessionStatus::Queued }
    }
}

/// Manages in-process checkpoint state for resumable sessions.
///
/// Each checkpoint atomically stores an event and the updated run-state so that
/// a crash cannot leave an event emitted but un-checkpointed (or vice versa).
///
/// # Example
///
/// ```rust
/// use adk_managed::checkpoint::{CheckpointManager, RunState};
/// use adk_managed::types::{SessionEvent, SessionStatus, ContentBlock};
///
/// let mut mgr = CheckpointManager::new("session_001".to_string());
///
/// let event = SessionEvent::StatusRunning { seq: 0 };
/// let state = RunState { seq: 1, pending_tool_ids: vec![], status: SessionStatus::Running };
/// mgr.checkpoint(event, state.clone());
///
/// assert_eq!(mgr.events().len(), 1);
/// assert_eq!(mgr.run_state(), &state);
/// ```
pub struct CheckpointManager {
    /// The session ID this manager is checkpointing for.
    session_id: String,
    /// The event log held by this manager.
    events: Vec<SessionEvent>,
    /// Current run state.
    run_state: RunState,
    /// Where [`CheckpointManager::flush`] writes, when a store is configured.
    store: Option<Arc<dyn ManagedStateStore>>,
    /// The owner and agent definition the session is rebuilt from.
    record: Option<SessionRecord>,
    /// Tool calls waiting on the client.
    pending: Vec<PendingAction>,
    /// The durable run parked on `pending`.
    durable_run_id: Option<String>,
}

impl CheckpointManager {
    /// Create a new checkpoint manager for the given session.
    ///
    /// Initializes with an empty event log and the initial run state
    /// (seq=0, no pending tools, queued status).
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            events: Vec::new(),
            run_state: RunState::initial(),
            store: None,
            record: None,
            pending: Vec::new(),
            durable_run_id: None,
        }
    }

    /// Writes flushed checkpoints to `store`.
    ///
    /// Check [`ManagedStateStore::durability`] to learn whether those writes survive process
    /// loss. With the shipped [`InMemoryManagedStateStore`](crate::InMemoryManagedStateStore)
    /// they do not.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_managed::{CheckpointManager, InMemoryManagedStateStore};
    /// use std::sync::Arc;
    ///
    /// let manager = CheckpointManager::new("session-1".to_string())
    ///     .with_store(Arc::new(InMemoryManagedStateStore::new()));
    /// assert!(manager.store().is_some());
    /// ```
    pub fn with_store(mut self, store: Arc<dyn ManagedStateStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// The configured store, if any.
    pub fn store(&self) -> Option<&Arc<dyn ManagedStateStore>> {
        self.store.as_ref()
    }

    /// Writes the current snapshot to the configured store.
    ///
    /// A no-op without a store. Separate from [`CheckpointManager::checkpoint`] because that
    /// method is synchronous and a store write is not; a caller that needs the snapshot
    /// externally visible must flush.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the store rejects the write.
    pub async fn flush(&self) -> Result<(), RuntimeError> {
        let Some(store) = &self.store else {
            return Ok(());
        };

        store.save(&self.session_id, self.snapshot()).await
    }

    /// Rebuilds a manager for `session_id` from `store`.
    ///
    /// Returns a manager with the stored snapshot when one exists, and an empty one otherwise.
    /// Whether anything is found across a restart depends entirely on the store's durability —
    /// with the in-memory backend a new process finds nothing.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the store cannot be read.
    pub async fn restore(
        session_id: String,
        store: Arc<dyn ManagedStateStore>,
    ) -> Result<Self, RuntimeError> {
        let restored = store
            .load(&session_id)
            .await?
            .unwrap_or_else(|| ManagedSessionState::new(Vec::new(), RunState::initial()));

        Ok(Self {
            session_id,
            events: restored.events,
            run_state: restored.run_state,
            store: Some(store),
            record: restored.record,
            pending: restored.pending,
            durable_run_id: restored.durable_run_id,
        })
    }

    /// Records an event and the updated run state together.
    ///
    /// The pair is applied in one call, so replay never sees an event without its state. This
    /// is a write to this manager's own fields, **not** a transaction with a persistent store:
    /// it says nothing about surviving a crash. Call [`CheckpointManager::flush`] to write the
    /// snapshot out, and check the store's durability to learn what that write guarantees.
    pub fn checkpoint(&mut self, event: SessionEvent, run_state: RunState) {
        self.events.push(event);
        self.run_state = run_state;
    }

    /// The events and run state this manager holds, for resume within the process.
    ///
    /// Reconstructing a session in a *different* process requires a crash-durable
    /// [`ManagedStateStore`] and [`CheckpointManager::restore`]; this method reads local
    /// fields only.
    pub fn load_checkpoint(&self) -> (Vec<SessionEvent>, RunState) {
        (self.events.clone(), self.run_state.clone())
    }

    /// Get all events stored in the checkpoint log.
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// Get current run state.
    pub fn run_state(&self) -> &RunState {
        &self.run_state
    }

    /// Get the session ID this manager is checkpointing for.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The whole state this manager holds, as [`flush`](Self::flush) writes it.
    pub fn snapshot(&self) -> ManagedSessionState {
        ManagedSessionState {
            events: self.events.clone(),
            run_state: self.run_state.clone(),
            record: self.record.clone(),
            pending: self.pending.clone(),
            durable_run_id: self.durable_run_id.clone(),
        }
    }

    /// Replaces everything this manager holds with `state`, keeping its store.
    pub fn replace(&mut self, state: ManagedSessionState) {
        self.events = state.events;
        self.run_state = state.run_state;
        self.record = state.record;
        self.pending = state.pending;
        self.durable_run_id = state.durable_run_id;
    }

    /// The owner and agent definition the session is rebuilt from.
    pub fn record(&self) -> Option<&SessionRecord> {
        self.record.as_ref()
    }

    /// Sets the owner and agent definition the session is rebuilt from.
    pub fn set_record(&mut self, record: SessionRecord) {
        self.record = Some(record);
    }

    /// Tool calls waiting on the client.
    pub fn pending(&self) -> &[PendingAction] {
        &self.pending
    }

    /// The durable run parked on the pending calls, if any.
    pub fn durable_run_id(&self) -> Option<&str> {
        self.durable_run_id.as_deref()
    }

    /// Replaces the pending calls and the run parked on them.
    ///
    /// The run state's `pending_tool_ids` follow the pending calls.
    pub fn set_pending(&mut self, pending: Vec<PendingAction>, durable_run_id: Option<String>) {
        self.run_state.pending_tool_ids = pending.iter().map(|action| action.id.clone()).collect();
        self.pending = pending;
        self.durable_run_id = durable_run_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentBlock;
    use serde_json::json;

    #[test]
    fn test_run_state_initial() {
        let state = RunState::initial();
        assert_eq!(state.seq, 0);
        assert!(state.pending_tool_ids.is_empty());
        assert_eq!(state.status, SessionStatus::Queued);
    }

    #[test]
    fn test_run_state_serialization_round_trip() {
        let state = RunState {
            seq: 42,
            pending_tool_ids: vec!["ctu_001".to_string(), "ctu_002".to_string()],
            status: SessionStatus::Running,
        };
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, deserialized);
    }

    #[test]
    fn test_checkpoint_manager_new() {
        let mgr = CheckpointManager::new("sess_123".to_string());
        assert_eq!(mgr.session_id(), "sess_123");
        assert!(mgr.events().is_empty());
        assert_eq!(mgr.run_state(), &RunState::initial());
    }

    #[test]
    fn test_checkpoint_stores_event_and_state_atomically() {
        let mut mgr = CheckpointManager::new("sess_001".to_string());

        let event = SessionEvent::StatusRunning { seq: 0 };
        let state = RunState { seq: 1, pending_tool_ids: vec![], status: SessionStatus::Running };

        mgr.checkpoint(event, state.clone());

        // Both event and state should be stored together
        assert_eq!(mgr.events().len(), 1);
        assert_eq!(mgr.run_state(), &state);
    }

    #[test]
    fn test_checkpoint_multiple_events() {
        let mut mgr = CheckpointManager::new("sess_002".to_string());

        // First checkpoint
        let event1 = SessionEvent::StatusRunning { seq: 0 };
        let state1 = RunState { seq: 1, pending_tool_ids: vec![], status: SessionStatus::Running };
        mgr.checkpoint(event1, state1);

        // Second checkpoint
        let event2 = SessionEvent::Message {
            content: vec![ContentBlock::Text { text: "Hello".to_string() }],
            seq: 1,
        };
        let state2 = RunState { seq: 2, pending_tool_ids: vec![], status: SessionStatus::Running };
        mgr.checkpoint(event2, state2.clone());

        // Third checkpoint — idle with pending tool
        let event3 = SessionEvent::CustomToolUse {
            custom_tool_use_id: "ctu_001".to_string(),
            name: "deploy".to_string(),
            input: json!({"target": "staging"}),
            seq: 2,
        };
        let state3 = RunState {
            seq: 3,
            pending_tool_ids: vec!["ctu_001".to_string()],
            status: SessionStatus::Idle,
        };
        mgr.checkpoint(event3, state3.clone());

        assert_eq!(mgr.events().len(), 3);
        // Run state should reflect the LAST checkpoint
        assert_eq!(mgr.run_state(), &state3);
    }

    #[test]
    fn test_load_checkpoint_returns_all_events_and_current_state() {
        let mut mgr = CheckpointManager::new("sess_003".to_string());

        let event1 = SessionEvent::StatusRunning { seq: 0 };
        let state1 = RunState { seq: 1, pending_tool_ids: vec![], status: SessionStatus::Running };
        mgr.checkpoint(event1, state1);

        let event2 = SessionEvent::StatusIdle { seq: 1, stop_reason: None, usage: None };
        let state2 = RunState { seq: 2, pending_tool_ids: vec![], status: SessionStatus::Idle };
        mgr.checkpoint(event2, state2.clone());

        let (events, run_state) = mgr.load_checkpoint();
        assert_eq!(events.len(), 2);
        assert_eq!(run_state, state2);
    }

    #[test]
    fn test_load_checkpoint_empty_manager() {
        let mgr = CheckpointManager::new("sess_empty".to_string());
        let (events, run_state) = mgr.load_checkpoint();
        assert!(events.is_empty());
        assert_eq!(run_state, RunState::initial());
    }

    #[test]
    fn test_run_state_updates_atomically_with_event() {
        let mut mgr = CheckpointManager::new("sess_atomic".to_string());

        // Simulate a custom tool use that parks
        let event = SessionEvent::CustomToolUse {
            custom_tool_use_id: "ctu_park".to_string(),
            name: "user_action".to_string(),
            input: json!({}),
            seq: 0,
        };
        let state = RunState {
            seq: 1,
            pending_tool_ids: vec!["ctu_park".to_string()],
            status: SessionStatus::Idle,
        };
        mgr.checkpoint(event, state.clone());

        // Verify the state reflects the parked tool
        assert_eq!(mgr.run_state().pending_tool_ids, vec!["ctu_park"]);
        assert_eq!(mgr.run_state().status, SessionStatus::Idle);

        // Simulate the tool result arriving and session resuming
        let event2 = SessionEvent::StatusRunning { seq: 1 };
        let state2 = RunState { seq: 2, pending_tool_ids: vec![], status: SessionStatus::Running };
        mgr.checkpoint(event2, state2.clone());

        // Pending tools should be cleared
        assert!(mgr.run_state().pending_tool_ids.is_empty());
        assert_eq!(mgr.run_state().status, SessionStatus::Running);
    }

    #[test]
    fn test_run_state_with_multiple_pending_tools() {
        let state = RunState {
            seq: 10,
            pending_tool_ids: vec![
                "ctu_001".to_string(),
                "ctu_002".to_string(),
                "ctu_003".to_string(),
            ],
            status: SessionStatus::Idle,
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.pending_tool_ids.len(), 3);
        assert_eq!(deserialized, state);
    }
}
