//! Checkpoint management for durable sessions.
//!
//! The [`CheckpointManager`] provides atomic checkpoint persistence so that
//! a crash cannot leave an event emitted but un-checkpointed (or vice versa).
//! For the initial implementation, storage is in-memory (`Vec<SessionEvent>`).
//! The real integration with `SessionService` for persistent storage is a
//! platform concern.
//!
//! # Responsibilities
//!
//! 1. **Atomicity guarantee**: event + state saved together in one operation
//! 2. **Load/resume interface**: retrieve all events and last run state
//! 3. **Event log maintenance**: ordered log for replay

use serde::{Deserialize, Serialize};

use crate::types::{SessionEvent, SessionStatus};

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
        Self {
            seq: 0,
            pending_tool_ids: Vec::new(),
            status: SessionStatus::Queued,
        }
    }
}

/// Manages atomic checkpoint persistence for durable sessions.
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
    /// The event log (in-memory implementation).
    events: Vec<SessionEvent>,
    /// Current run state.
    run_state: RunState,
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
        }
    }

    /// Atomically persist an event and updated run-state.
    ///
    /// Both the event and the new state are stored together in one operation,
    /// guaranteeing that replay will see a consistent view after any crash.
    pub fn checkpoint(&mut self, event: SessionEvent, run_state: RunState) {
        self.events.push(event);
        self.run_state = run_state;
    }

    /// Load the last checkpoint for resume.
    ///
    /// Returns all stored events and the current run state, providing
    /// everything needed to reconstruct a session after a restart.
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
        let state = RunState {
            seq: 1,
            pending_tool_ids: vec![],
            status: SessionStatus::Running,
        };

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
        let state1 = RunState {
            seq: 1,
            pending_tool_ids: vec![],
            status: SessionStatus::Running,
        };
        mgr.checkpoint(event1, state1);

        // Second checkpoint
        let event2 = SessionEvent::Message {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
            seq: 1,
        };
        let state2 = RunState {
            seq: 2,
            pending_tool_ids: vec![],
            status: SessionStatus::Running,
        };
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
        let state1 = RunState {
            seq: 1,
            pending_tool_ids: vec![],
            status: SessionStatus::Running,
        };
        mgr.checkpoint(event1, state1);

        let event2 = SessionEvent::StatusIdle {
            seq: 1,
            stop_reason: None,
            usage: None,
        };
        let state2 = RunState {
            seq: 2,
            pending_tool_ids: vec![],
            status: SessionStatus::Idle,
        };
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
        let state2 = RunState {
            seq: 2,
            pending_tool_ids: vec![],
            status: SessionStatus::Running,
        };
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
