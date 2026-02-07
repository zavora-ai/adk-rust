//! ADK-Graph Workflow Runner with HITL Support
//!
//! This module provides workflow execution for ADK-Graph based workflows with
//! Human-in-the-Loop (HITL) interrupt support. It handles:
//! - Catching `GraphError::Interrupted` from ADK-Graph
//! - Emitting interrupt events via SSE
//! - Storing interrupted session state for resumption
//! - Serializing interrupt data for frontend consumption
//!
//! ## Requirements Traceability
//! - Requirement 3.1: Interrupt Detection
//! - Requirement 5.2: State Persistence

use crate::server::events::HitlEventEmitter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// ============================================
// Interrupt Data Types
// ============================================

/// Serialized interrupt data from ADK-Graph.
/// This is the data structure sent to the frontend via SSE.
///
/// ## SSE Event Format
/// ```json
/// {
///   "type": "interrupt",
///   "node_id": "review",
///   "message": "HIGH RISK: Human approval required",
///   "data": {
///     "plan": "...",
///     "risk_level": "high",
///     "action": "Set 'approved' to true to continue"
///   },
///   "timestamp": 1706400000000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptData {
    /// ID of the node that triggered the interrupt
    pub node_id: String,
    /// Human-readable message explaining what input is needed
    pub message: String,
    /// Additional data for the interrupt (plan details, options, etc.)
    pub data: Value,
    /// Thread ID for resumption
    pub thread_id: String,
    /// Checkpoint ID for resumption
    pub checkpoint_id: String,
    /// Step number when interrupted
    pub step: usize,
}

impl InterruptData {
    /// Create a new interrupt data from ADK-Graph InterruptedExecution.
    ///
    /// This handles the conversion from ADK-Graph's internal interrupt types
    /// to the serializable format used by the frontend.
    pub fn from_interrupted(
        thread_id: String,
        checkpoint_id: String,
        node_id: String,
        message: String,
        data: Value,
        step: usize,
    ) -> Self {
        Self { node_id, message, data, thread_id, checkpoint_id, step }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ============================================
// Session State Storage
// ============================================

/// State of an interrupted session.
/// Stores all information needed to resume execution after user response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptedSessionState {
    /// Session ID
    pub session_id: String,
    /// Thread ID for ADK-Graph resumption
    pub thread_id: String,
    /// Checkpoint ID for ADK-Graph resumption
    pub checkpoint_id: String,
    /// Node that triggered the interrupt
    pub node_id: String,
    /// Interrupt message
    pub message: String,
    /// Interrupt data
    pub data: Value,
    /// Current workflow state at interruption
    pub state: HashMap<String, Value>,
    /// Step number when interrupted
    pub step: usize,
    /// Timestamp when interrupted
    pub interrupted_at: u64,
}

impl InterruptedSessionState {
    /// Create a new interrupted session state.
    pub fn new(
        session_id: String,
        interrupt_data: InterruptData,
        state: HashMap<String, Value>,
    ) -> Self {
        Self {
            session_id,
            thread_id: interrupt_data.thread_id,
            checkpoint_id: interrupt_data.checkpoint_id,
            node_id: interrupt_data.node_id,
            message: interrupt_data.message,
            data: interrupt_data.data,
            state,
            step: interrupt_data.step,
            interrupted_at: current_timestamp_ms(),
        }
    }
}

/// Storage for interrupted session states.
/// Allows sessions to be resumed after user provides input.
///
/// ## Thread Safety
/// Uses `RwLock` for concurrent access from multiple SSE handlers.
#[derive(Debug, Default)]
pub struct InterruptedSessionStore {
    /// Map of session_id -> interrupted state
    sessions: RwLock<HashMap<String, InterruptedSessionState>>,
}

impl InterruptedSessionStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self { sessions: RwLock::new(HashMap::new()) }
    }

    /// Store an interrupted session state.
    ///
    /// ## Arguments
    /// * `session_id` - Unique session identifier
    /// * `state` - The interrupted session state to store
    ///
    /// ## Requirements
    /// Validates: Requirement 5.2 - State Persistence
    pub async fn store(&self, session_id: &str, state: InterruptedSessionState) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(session_id.to_string(), state);
    }

    /// Get an interrupted session state.
    ///
    /// ## Arguments
    /// * `session_id` - Unique session identifier
    ///
    /// ## Returns
    /// The interrupted session state if found, None otherwise.
    pub async fn get(&self, session_id: &str) -> Option<InterruptedSessionState> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// Remove an interrupted session state (after resumption).
    ///
    /// ## Arguments
    /// * `session_id` - Unique session identifier
    ///
    /// ## Returns
    /// The removed session state if found, None otherwise.
    pub async fn remove(&self, session_id: &str) -> Option<InterruptedSessionState> {
        let mut sessions = self.sessions.write().await;
        sessions.remove(session_id)
    }

    /// Check if a session is interrupted.
    pub async fn is_interrupted(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        sessions.contains_key(session_id)
    }

    /// Get all interrupted session IDs.
    pub async fn list_interrupted(&self) -> Vec<String> {
        let sessions = self.sessions.read().await;
        sessions.keys().cloned().collect()
    }

    /// Clear old interrupted sessions (cleanup).
    /// Removes sessions older than the specified age in milliseconds.
    pub async fn cleanup_old(&self, max_age_ms: u64) {
        let now = current_timestamp_ms();
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, state| now - state.interrupted_at < max_age_ms);
    }
}

// ============================================
// Graph Interrupt Handler
// ============================================

/// Handler for ADK-Graph interrupts.
/// Coordinates between ADK-Graph execution, SSE emission, and session storage.
///
/// ## Usage
/// ```rust,ignore
/// let handler = GraphInterruptHandler::new(store, emitter);
///
/// // When graph execution returns Interrupted error:
/// match graph.invoke(input, config).await {
///     Err(GraphError::Interrupted(interrupted)) => {
///         handler.handle_interrupt(&session_id, &interrupted).await;
///     }
///     // ...
/// }
/// ```
pub struct GraphInterruptHandler {
    /// Storage for interrupted session states
    store: Arc<InterruptedSessionStore>,
    /// SSE emitter for HITL events
    emitter: Option<HitlEventEmitter>,
}

impl GraphInterruptHandler {
    /// Create a new interrupt handler.
    pub fn new(store: Arc<InterruptedSessionStore>) -> Self {
        Self { store, emitter: None }
    }

    /// Create a new interrupt handler with SSE emitter.
    pub fn with_emitter(store: Arc<InterruptedSessionStore>, emitter: HitlEventEmitter) -> Self {
        Self { store, emitter: Some(emitter) }
    }

    /// Handle an interrupt from ADK-Graph.
    ///
    /// This method:
    /// 1. Extracts interrupt data from the InterruptedExecution
    /// 2. Emits an interrupt SSE event
    /// 3. Stores the interrupted state for later resumption
    ///
    /// ## Arguments
    /// * `session_id` - Unique session identifier
    /// * `thread_id` - ADK-Graph thread ID
    /// * `checkpoint_id` - ADK-Graph checkpoint ID
    /// * `node_id` - ID of the node that triggered the interrupt
    /// * `message` - Human-readable interrupt message
    /// * `data` - Additional interrupt data
    /// * `state` - Current workflow state at interruption
    /// * `step` - Step number when interrupted
    ///
    /// ## Requirements
    /// Validates: Requirements 3.1, 5.2
    pub async fn handle_interrupt(
        &self,
        session_id: &str,
        thread_id: String,
        checkpoint_id: String,
        node_id: String,
        message: String,
        data: Value,
        state: HashMap<String, Value>,
        step: usize,
    ) {
        // Create interrupt data
        let interrupt_data = InterruptData::from_interrupted(
            thread_id,
            checkpoint_id,
            node_id.clone(),
            message.clone(),
            data.clone(),
            step,
        );

        // Emit interrupt event via SSE
        if let Some(emitter) = &self.emitter {
            emitter.emit_interrupt(&node_id, &message, data.clone()).await;
        }

        // Store interrupted state for resumption
        let session_state =
            InterruptedSessionState::new(session_id.to_string(), interrupt_data, state);
        self.store.store(session_id, session_state).await;
    }

    /// Handle an interrupt from ADK-Graph InterruptedExecution.
    ///
    /// This is a convenience method that extracts data from the ADK-Graph
    /// InterruptedExecution struct and calls handle_interrupt.
    ///
    /// ## Arguments
    /// * `session_id` - Unique session identifier
    /// * `interrupted` - The InterruptedExecution from ADK-Graph
    ///
    /// ## Requirements
    /// Validates: Requirements 3.1, 5.2
    ///
    /// Note: This method is only available when using adk-graph directly.
    /// For SSE-based interrupt handling, use handle_interrupt() instead.
    #[allow(dead_code)]
    pub async fn handle_graph_interrupt_direct(
        &self,
        session_id: &str,
        thread_id: String,
        checkpoint_id: String,
        interrupt_type: &str,
        interrupt_message: String,
        interrupt_data: Option<Value>,
        state: HashMap<String, Value>,
        step: usize,
    ) {
        // Extract node_id and message from the interrupt type
        let (node_id, message, data) = match interrupt_type {
            "before" => {
                let node = interrupt_message.clone();
                (node.clone(), format!("Interrupt before '{}'", node), Value::Null)
            }
            "after" => {
                let node = interrupt_message.clone();
                (node.clone(), format!("Interrupt after '{}'", node), Value::Null)
            }
            "dynamic" | _ => {
                // For dynamic interrupts, we need to determine the node_id
                // from the checkpoint or use a default
                let node_id = "dynamic".to_string();
                (node_id, interrupt_message, interrupt_data.unwrap_or(Value::Null))
            }
        };

        self.handle_interrupt(
            session_id,
            thread_id,
            checkpoint_id,
            node_id,
            message,
            data,
            state,
            step,
        )
        .await;
    }

    /// Get the interrupted state for a session.
    pub async fn get_interrupted_state(&self, session_id: &str) -> Option<InterruptedSessionState> {
        self.store.get(session_id).await
    }

    /// Check if a session is interrupted.
    pub async fn is_interrupted(&self, session_id: &str) -> bool {
        self.store.is_interrupted(session_id).await
    }

    /// Clear the interrupted state for a session (after resumption).
    pub async fn clear_interrupted_state(
        &self,
        session_id: &str,
    ) -> Option<InterruptedSessionState> {
        self.store.remove(session_id).await
    }
}

// ============================================
// Interrupt Data Serialization
// ============================================

/// Serialize interrupt data for SSE transmission.
///
/// This function handles the conversion of various interrupt data types
/// to a consistent JSON format for the frontend.
///
/// ## Arguments
/// * `node_id` - ID of the node that triggered the interrupt
/// * `message` - Human-readable interrupt message
/// * `data` - Additional interrupt data (can be any JSON value)
///
/// ## Returns
/// A JSON object with the interrupt data in the expected format.
///
/// ## Requirements
/// Validates: Requirement 5.1 - SSE Event Types
pub fn serialize_interrupt_data(node_id: &str, message: &str, data: Value) -> Value {
    serde_json::json!({
        "nodeId": node_id,
        "message": message,
        "data": data,
        "timestamp": current_timestamp_ms()
    })
}

/// Deserialize user response for interrupt resumption.
///
/// This function parses the user's response to an interrupt and
/// prepares it for use with `graph.update_state()`.
///
/// ## Arguments
/// * `response` - The user's response as a JSON value
///
/// ## Returns
/// A HashMap of state updates to apply before resuming.
pub fn deserialize_interrupt_response(response: Value) -> HashMap<String, Value> {
    match response {
        Value::Object(map) => map.into_iter().collect(),
        _ => {
            // If response is not an object, wrap it in a "response" key
            let mut updates = HashMap::new();
            updates.insert("response".to_string(), response);
            updates
        }
    }
}

// ============================================
// Helper Functions
// ============================================

/// Get current timestamp in milliseconds since Unix epoch.
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ============================================
// Global Session Store
// ============================================

lazy_static::lazy_static! {
    /// Global store for interrupted session states.
    /// This is shared across all SSE handlers.
    pub static ref INTERRUPTED_SESSIONS: Arc<InterruptedSessionStore> =
        Arc::new(InterruptedSessionStore::new());
}

// ============================================
// Tests
// ============================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interrupt_data_creation() {
        let data = InterruptData::from_interrupted(
            "thread-123".to_string(),
            "checkpoint-456".to_string(),
            "review".to_string(),
            "Human approval required".to_string(),
            serde_json::json!({"risk_level": "high"}),
            5,
        );

        assert_eq!(data.node_id, "review");
        assert_eq!(data.message, "Human approval required");
        assert_eq!(data.thread_id, "thread-123");
        assert_eq!(data.checkpoint_id, "checkpoint-456");
        assert_eq!(data.step, 5);
    }

    #[test]
    fn test_interrupt_data_serialization() {
        let data = InterruptData::from_interrupted(
            "thread-123".to_string(),
            "checkpoint-456".to_string(),
            "review".to_string(),
            "Human approval required".to_string(),
            serde_json::json!({"risk_level": "high"}),
            5,
        );

        let json = data.to_json();
        assert!(json.contains("\"node_id\":\"review\""));
        assert!(json.contains("\"message\":\"Human approval required\""));
        assert!(json.contains("\"risk_level\":\"high\""));
    }

    #[tokio::test]
    async fn test_interrupted_session_store() {
        let store = InterruptedSessionStore::new();

        // Create test interrupt data
        let interrupt_data = InterruptData::from_interrupted(
            "thread-123".to_string(),
            "checkpoint-456".to_string(),
            "review".to_string(),
            "Human approval required".to_string(),
            serde_json::json!({"risk_level": "high"}),
            5,
        );

        let mut state = HashMap::new();
        state.insert("task".to_string(), serde_json::json!("Delete files"));

        let session_state =
            InterruptedSessionState::new("session-789".to_string(), interrupt_data, state);

        // Store the session
        store.store("session-789", session_state.clone()).await;

        // Verify it's stored
        assert!(store.is_interrupted("session-789").await);

        // Get the session
        let retrieved = store.get("session-789").await;
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.node_id, "review");
        assert_eq!(retrieved.thread_id, "thread-123");

        // Remove the session
        let removed = store.remove("session-789").await;
        assert!(removed.is_some());
        assert!(!store.is_interrupted("session-789").await);
    }

    #[test]
    fn test_serialize_interrupt_data() {
        let data = serialize_interrupt_data(
            "review",
            "Human approval required",
            serde_json::json!({"risk_level": "high"}),
        );

        assert_eq!(data["nodeId"], "review");
        assert_eq!(data["message"], "Human approval required");
        assert_eq!(data["data"]["risk_level"], "high");
        assert!(data["timestamp"].is_number());
    }

    #[test]
    fn test_deserialize_interrupt_response_object() {
        let response = serde_json::json!({
            "approved": true,
            "comment": "Looks good"
        });

        let updates = deserialize_interrupt_response(response);
        assert_eq!(updates.get("approved"), Some(&serde_json::json!(true)));
        assert_eq!(updates.get("comment"), Some(&serde_json::json!("Looks good")));
    }

    #[test]
    fn test_deserialize_interrupt_response_non_object() {
        let response = serde_json::json!("approve");

        let updates = deserialize_interrupt_response(response);
        assert_eq!(updates.get("response"), Some(&serde_json::json!("approve")));
    }

    #[tokio::test]
    async fn test_graph_interrupt_handler() {
        let store = Arc::new(InterruptedSessionStore::new());
        let handler = GraphInterruptHandler::new(store.clone());

        let mut state = HashMap::new();
        state.insert("task".to_string(), serde_json::json!("Delete files"));

        // Handle an interrupt
        handler
            .handle_interrupt(
                "session-123",
                "thread-456".to_string(),
                "checkpoint-789".to_string(),
                "review".to_string(),
                "Human approval required".to_string(),
                serde_json::json!({"risk_level": "high"}),
                state,
                5,
            )
            .await;

        // Verify the interrupt was stored
        assert!(handler.is_interrupted("session-123").await);

        let interrupted_state = handler.get_interrupted_state("session-123").await;
        assert!(interrupted_state.is_some());
        let interrupted_state = interrupted_state.unwrap();
        assert_eq!(interrupted_state.node_id, "review");
        assert_eq!(interrupted_state.thread_id, "thread-456");
        assert_eq!(interrupted_state.checkpoint_id, "checkpoint-789");

        // Clear the interrupt
        let cleared = handler.clear_interrupted_state("session-123").await;
        assert!(cleared.is_some());
        assert!(!handler.is_interrupted("session-123").await);
    }

    #[tokio::test]
    async fn test_cleanup_old_sessions() {
        let store = InterruptedSessionStore::new();

        // Create an old session (manually set timestamp)
        let interrupt_data = InterruptData::from_interrupted(
            "thread-123".to_string(),
            "checkpoint-456".to_string(),
            "review".to_string(),
            "Old interrupt".to_string(),
            Value::Null,
            1,
        );

        let mut session_state =
            InterruptedSessionState::new("old-session".to_string(), interrupt_data, HashMap::new());
        // Set timestamp to 2 hours ago
        session_state.interrupted_at = current_timestamp_ms() - (2 * 60 * 60 * 1000);

        store.store("old-session", session_state).await;

        // Create a recent session
        let interrupt_data = InterruptData::from_interrupted(
            "thread-789".to_string(),
            "checkpoint-012".to_string(),
            "review".to_string(),
            "Recent interrupt".to_string(),
            Value::Null,
            1,
        );

        let session_state = InterruptedSessionState::new(
            "recent-session".to_string(),
            interrupt_data,
            HashMap::new(),
        );

        store.store("recent-session", session_state).await;

        // Cleanup sessions older than 1 hour
        store.cleanup_old(60 * 60 * 1000).await;

        // Old session should be removed
        assert!(!store.is_interrupted("old-session").await);
        // Recent session should still exist
        assert!(store.is_interrupted("recent-session").await);
    }
}
