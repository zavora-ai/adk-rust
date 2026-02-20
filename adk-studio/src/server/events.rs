//! SSE Event Schema v2.0
//!
//! This module defines the enhanced SSE event types for ADK Studio v2.0,
//! which adds support for state snapshots, data flow overlays, and HITL interrupts.
//!
//! ## Features
//! - State snapshots: Input/output state at each agent execution step
//! - State keys: List of state keys for data flow overlay visualization
//! - HITL Interrupts: Events for human-in-the-loop workflow interactions
//!
//! ## Requirements Traceability
//! - Requirement 5.8: State snapshot capture at each node during execution
//! - Requirement 3.3: State keys sourced from runtime execution events
//! - Requirement 5.1: SSE event types for trigger/HITL flow

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// State snapshot captured at agent start/end events.
/// Contains input and output state for timeline debugging and state inspection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateSnapshot {
    /// Input state before node execution
    pub input: serde_json::Value,
    /// Output state after node execution
    pub output: serde_json::Value,
}

impl StateSnapshot {
    /// Create a new state snapshot with the given input and output state.
    pub fn new(input: serde_json::Value, output: serde_json::Value) -> Self {
        Self { input, output }
    }

    /// Create a snapshot with only input state (for node_start events).
    pub fn with_input(input: serde_json::Value) -> Self {
        Self { input, output: serde_json::Value::Object(Default::default()) }
    }

    /// Extract top-level keys from the output state for data flow overlays.
    pub fn extract_state_keys(&self) -> Vec<String> {
        match &self.output {
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }
}

// ============================================
// HITL (Human-in-the-Loop) Event Types
// ============================================
// These event types support the trigger/HITL flow as defined in Requirement 5.1:
// - `trigger_input`: When trigger receives user input
// - `interrupt`: When workflow requests human intervention
// - `interrupt_response`: When user responds to interrupt
// - `resume`: When workflow resumes after interrupt

/// Event emitted when a trigger node receives user input.
/// This marks the start of workflow execution from a manual trigger.
///
/// ## SSE Event Format
/// ```json
/// {
///   "type": "trigger_input",
///   "trigger_id": "manual_trigger_1",
///   "input": "User's input message",
///   "timestamp": 1706400000000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerInputEvent {
    /// Event type identifier (always "trigger_input")
    #[serde(rename = "type")]
    pub event_type: String,
    /// ID of the trigger node that received input
    pub trigger_id: String,
    /// The user's input text
    pub input: String,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: u64,
}

impl TriggerInputEvent {
    /// Create a new trigger input event.
    pub fn new(trigger_id: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            event_type: "trigger_input".to_string(),
            trigger_id: trigger_id.into(),
            input: input.into(),
            timestamp: current_timestamp_ms(),
        }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Event emitted when a workflow requests human intervention (HITL interrupt).
/// The workflow pauses at this point until the user responds.
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
pub struct InterruptEvent {
    /// Event type identifier (always "interrupt")
    #[serde(rename = "type")]
    pub event_type: String,
    /// ID of the node that triggered the interrupt
    pub node_id: String,
    /// Human-readable message explaining what input is needed
    pub message: String,
    /// Additional data for the interrupt (plan details, options, etc.)
    pub data: serde_json::Value,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: u64,
}

impl InterruptEvent {
    /// Create a new interrupt event.
    pub fn new(
        node_id: impl Into<String>,
        message: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            event_type: "interrupt".to_string(),
            node_id: node_id.into(),
            message: message.into(),
            data,
            timestamp: current_timestamp_ms(),
        }
    }

    /// Create an interrupt event with empty data.
    pub fn simple(node_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(node_id, message, serde_json::Value::Object(Default::default()))
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Event emitted when a user responds to an interrupt.
/// This is sent before the workflow resumes to acknowledge the response.
///
/// ## SSE Event Format
/// ```json
/// {
///   "type": "interrupt_response",
///   "node_id": "review",
///   "response": { "approved": true },
///   "timestamp": 1706400000000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptResponseEvent {
    /// Event type identifier (always "interrupt_response")
    #[serde(rename = "type")]
    pub event_type: String,
    /// ID of the node that was interrupted
    pub node_id: String,
    /// The user's response data
    pub response: serde_json::Value,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: u64,
}

impl InterruptResponseEvent {
    /// Create a new interrupt response event.
    pub fn new(node_id: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            event_type: "interrupt_response".to_string(),
            node_id: node_id.into(),
            response,
            timestamp: current_timestamp_ms(),
        }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Event emitted when a workflow resumes after an interrupt.
/// This signals that execution is continuing from the checkpoint.
///
/// ## SSE Event Format
/// ```json
/// {
///   "type": "resume",
///   "node_id": "review",
///   "timestamp": 1706400000000
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeEvent {
    /// Event type identifier (always "resume")
    #[serde(rename = "type")]
    pub event_type: String,
    /// ID of the node that is resuming
    pub node_id: String,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: u64,
}

impl ResumeEvent {
    /// Create a new resume event.
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            event_type: "resume".to_string(),
            node_id: node_id.into(),
            timestamp: current_timestamp_ms(),
        }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ============================================
// SSE Emitter for HITL Events
// ============================================

/// SSE emitter for HITL (Human-in-the-Loop) events.
/// Provides convenient methods for emitting interrupt-related events.
///
/// ## Usage
/// ```rust,ignore
/// let emitter = HitlEventEmitter::new(sender);
/// emitter.emit_trigger_input("trigger_1", "Hello").await;
/// emitter.emit_interrupt("review", "Approval needed", data).await;
/// emitter.emit_resume("review").await;
/// ```
pub struct HitlEventEmitter {
    /// Channel sender for SSE events
    sender: tokio::sync::mpsc::Sender<String>,
}

impl HitlEventEmitter {
    /// Create a new HITL event emitter with the given channel sender.
    pub fn new(sender: tokio::sync::mpsc::Sender<String>) -> Self {
        Self { sender }
    }

    /// Emit a trigger_input event when a manual trigger receives user input.
    ///
    /// ## Arguments
    /// * `trigger_id` - ID of the trigger node
    /// * `input` - The user's input text
    ///
    /// ## Requirements
    /// Validates: Requirement 5.1 - `trigger_input` event type
    pub async fn emit_trigger_input(
        &self,
        trigger_id: impl Into<String>,
        input: impl Into<String>,
    ) {
        let event = TriggerInputEvent::new(trigger_id, input);
        let _ = self.sender.send(event.to_json()).await;
    }

    /// Emit an interrupt event when the workflow requests human intervention.
    ///
    /// ## Arguments
    /// * `node_id` - ID of the node requesting intervention
    /// * `message` - Human-readable message explaining what input is needed
    /// * `data` - Additional context data (plan details, options, etc.)
    ///
    /// ## Requirements
    /// Validates: Requirement 5.1 - `interrupt` event type
    pub async fn emit_interrupt(
        &self,
        node_id: impl Into<String>,
        message: impl Into<String>,
        data: serde_json::Value,
    ) {
        let event = InterruptEvent::new(node_id, message, data);
        let _ = self.sender.send(event.to_json()).await;
    }

    /// Emit an interrupt_response event when the user responds to an interrupt.
    ///
    /// ## Arguments
    /// * `node_id` - ID of the node that was interrupted
    /// * `response` - The user's response data
    ///
    /// ## Requirements
    /// Validates: Requirement 5.1 - `interrupt_response` event type
    pub async fn emit_interrupt_response(
        &self,
        node_id: impl Into<String>,
        response: serde_json::Value,
    ) {
        let event = InterruptResponseEvent::new(node_id, response);
        let _ = self.sender.send(event.to_json()).await;
    }

    /// Emit a resume event when the workflow resumes after an interrupt.
    ///
    /// ## Arguments
    /// * `node_id` - ID of the node that is resuming
    ///
    /// ## Requirements
    /// Validates: Requirement 5.1 - `resume` event type
    pub async fn emit_resume(&self, node_id: impl Into<String>) {
        let event = ResumeEvent::new(node_id);
        let _ = self.sender.send(event.to_json()).await;
    }
}

/// Get current timestamp in milliseconds since Unix epoch.
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ============================================
// Debug Console Event Types
// ============================================
// These event types support the Debug Console Tab (Requirement 9.1):
// - `debug`: Enriched debug trace events with level, category, and detail payload

/// Debug event emitted via SSE for the Debug Console Tab.
/// Carries enriched trace data including log level, category, agent name,
/// summary text, and a full detail payload.
///
/// ## SSE Event Format
/// ```json
/// {
///   "level": "debug",
///   "category": "request",
///   "agent": "weather_agent",
///   "summary": "LLM request to gemini-2.0-flash",
///   "detail": { "model": "gemini-2.0-flash", "messages": [] },
///   "timestamp": 1706400000000
/// }
/// ```
///
/// ## Requirements
/// Validates: Requirement 9.1 - Backend debug SSE event structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEvent {
    /// Log level: "error", "warn", "info", "debug", "trace"
    pub level: String,
    /// Trace category: "request", "response", "error", "state_change", "tool_call", "tool_result", "lifecycle"
    pub category: String,
    /// Agent or node name that produced this event
    pub agent: String,
    /// Human-readable summary of the event
    pub summary: String,
    /// Full detail payload (request body, response body, state diff, etc.)
    pub detail: serde_json::Value,
    /// Timestamp in milliseconds since Unix epoch
    pub timestamp: u64,
}

impl DebugEvent {
    /// Create a new debug event with the current timestamp.
    ///
    /// ## Arguments
    /// * `level` - Log level ("error", "warn", "info", "debug", "trace")
    /// * `category` - Trace category ("request", "response", "error", "state_change", "tool_call", "tool_result", "lifecycle")
    /// * `agent` - Agent or node name
    /// * `summary` - Human-readable summary
    /// * `detail` - Full detail payload as JSON value
    pub fn new(
        level: &str,
        category: &str,
        agent: &str,
        summary: impl Into<String>,
        detail: serde_json::Value,
    ) -> Self {
        Self {
            level: level.to_string(),
            category: category.to_string(),
            agent: agent.to_string(),
            summary: summary.into(),
            detail,
            timestamp: current_timestamp_ms(),
        }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Enhanced trace event for SSE v2.0.
/// Extends the existing trace event format with state snapshot support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEventV2 {
    /// Event type: node_start, node_end, state, done
    #[serde(rename = "type")]
    pub event_type: String,

    /// Agent/node name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,

    /// Execution step number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,

    /// Duration in milliseconds (for node_end events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,

    /// Total steps (for done events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,

    /// v2.0: State snapshot for timeline/inspector
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_snapshot: Option<StateSnapshot>,

    /// v2.0: State keys for data flow overlays
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_keys: Option<Vec<String>>,

    /// Legacy state field for backward compatibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

impl TraceEventV2 {
    /// Create a node_start event with state snapshot.
    pub fn node_start(node: &str, step: u32, input_state: serde_json::Value) -> Self {
        let snapshot = StateSnapshot::with_input(input_state);
        let state_keys = snapshot.extract_state_keys();
        Self {
            event_type: "node_start".to_string(),
            node: Some(node.to_string()),
            step: Some(step),
            duration_ms: None,
            total_steps: None,
            state_snapshot: Some(snapshot),
            state_keys: if state_keys.is_empty() { None } else { Some(state_keys) },
            state: None,
        }
    }

    /// Create a node_end event with state snapshot.
    pub fn node_end(
        node: &str,
        step: u32,
        duration_ms: u64,
        input_state: serde_json::Value,
        output_state: serde_json::Value,
    ) -> Self {
        let snapshot = StateSnapshot::new(input_state, output_state);
        let state_keys = snapshot.extract_state_keys();
        Self {
            event_type: "node_end".to_string(),
            node: Some(node.to_string()),
            step: Some(step),
            duration_ms: Some(duration_ms),
            total_steps: None,
            state_snapshot: Some(snapshot),
            state_keys: if state_keys.is_empty() { None } else { Some(state_keys) },
            state: None,
        }
    }

    /// Create a done event with final state snapshot.
    pub fn done(
        total_steps: u32,
        input_state: serde_json::Value,
        output_state: serde_json::Value,
    ) -> Self {
        let snapshot = StateSnapshot::new(input_state, output_state);
        let state_keys = snapshot.extract_state_keys();
        Self {
            event_type: "done".to_string(),
            node: None,
            step: None,
            duration_ms: None,
            total_steps: Some(total_steps),
            state_snapshot: Some(snapshot),
            state_keys: if state_keys.is_empty() { None } else { Some(state_keys) },
            state: None,
        }
    }

    /// Create a state update event.
    pub fn state_update(output_state: serde_json::Value) -> Self {
        let snapshot =
            StateSnapshot::new(serde_json::Value::Object(Default::default()), output_state);
        let state_keys = snapshot.extract_state_keys();
        Self {
            event_type: "state".to_string(),
            node: None,
            step: None,
            duration_ms: None,
            total_steps: None,
            state_snapshot: Some(snapshot),
            state_keys: if state_keys.is_empty() { None } else { Some(state_keys) },
            state: None,
        }
    }

    /// Convert to JSON string for SSE emission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Execution state tracker for capturing state snapshots.
/// Used by the SSE handler to track state across agent executions.
#[derive(Debug, Clone, Default)]
pub struct ExecutionStateTracker {
    /// Current execution state
    current_state: HashMap<String, serde_json::Value>,
    /// Step counter
    step: u32,
    /// Node start times for duration calculation
    node_start_times: HashMap<String, std::time::Instant>,
}

impl ExecutionStateTracker {
    /// Create a new execution state tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record node start and return the trace event.
    pub fn node_start(&mut self, node: &str) -> TraceEventV2 {
        self.step += 1;
        self.node_start_times.insert(node.to_string(), std::time::Instant::now());
        let input_state = serde_json::to_value(&self.current_state).unwrap_or_default();
        TraceEventV2::node_start(node, self.step, input_state)
    }

    /// Record node end and return the trace event.
    pub fn node_end(&mut self, node: &str, output_state: serde_json::Value) -> TraceEventV2 {
        let duration_ms = self
            .node_start_times
            .remove(node)
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);

        let input_state = serde_json::to_value(&self.current_state).unwrap_or_default();

        // Merge output state into current state
        if let serde_json::Value::Object(map) = &output_state {
            for (k, v) in map {
                self.current_state.insert(k.clone(), v.clone());
            }
        }

        TraceEventV2::node_end(node, self.step, duration_ms, input_state, output_state)
    }

    /// Record execution complete and return the done event.
    pub fn done(&self) -> TraceEventV2 {
        let output_state = serde_json::to_value(&self.current_state).unwrap_or_default();
        TraceEventV2::done(self.step, serde_json::Value::Object(Default::default()), output_state)
    }

    /// Update current state with new values.
    pub fn update_state(&mut self, key: &str, value: serde_json::Value) {
        self.current_state.insert(key.to_string(), value);
    }

    /// Get current step count.
    pub fn current_step(&self) -> u32 {
        self.step
    }

    /// Get current state as JSON value.
    pub fn current_state_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.current_state).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_snapshot_extract_keys() {
        let snapshot = StateSnapshot::new(
            serde_json::json!({"input_key": "value"}),
            serde_json::json!({"output_key1": "value1", "output_key2": "value2"}),
        );
        let keys = snapshot.extract_state_keys();
        assert!(keys.contains(&"output_key1".to_string()));
        assert!(keys.contains(&"output_key2".to_string()));
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_trace_event_node_start() {
        let event = TraceEventV2::node_start("test_agent", 1, serde_json::json!({"query": "test"}));
        assert_eq!(event.event_type, "node_start");
        assert_eq!(event.node, Some("test_agent".to_string()));
        assert_eq!(event.step, Some(1));
        assert!(event.state_snapshot.is_some());
    }

    #[test]
    fn test_trace_event_node_end() {
        let event = TraceEventV2::node_end(
            "test_agent",
            1,
            1500,
            serde_json::json!({"query": "test"}),
            serde_json::json!({"query": "test", "result": "answer"}),
        );
        assert_eq!(event.event_type, "node_end");
        assert_eq!(event.duration_ms, Some(1500));
        assert!(event.state_keys.is_some());
        let keys = event.state_keys.unwrap();
        assert!(keys.contains(&"query".to_string()));
        assert!(keys.contains(&"result".to_string()));
    }

    #[test]
    fn test_execution_state_tracker() {
        let mut tracker = ExecutionStateTracker::new();

        // Start node
        let start_event = tracker.node_start("agent1");
        assert_eq!(start_event.event_type, "node_start");
        assert_eq!(tracker.current_step(), 1);

        // End node with output
        let end_event = tracker.node_end("agent1", serde_json::json!({"result": "done"}));
        assert_eq!(end_event.event_type, "node_end");
        assert!(end_event.duration_ms.is_some());

        // Done
        let done_event = tracker.done();
        assert_eq!(done_event.event_type, "done");
        assert_eq!(done_event.total_steps, Some(1));
    }

    // ============================================
    // HITL Event Tests
    // ============================================

    #[test]
    fn test_trigger_input_event() {
        let event = TriggerInputEvent::new("manual_trigger_1", "Hello, world!");
        assert_eq!(event.event_type, "trigger_input");
        assert_eq!(event.trigger_id, "manual_trigger_1");
        assert_eq!(event.input, "Hello, world!");
        assert!(event.timestamp > 0);

        // Test JSON serialization
        let json = event.to_json();
        assert!(json.contains("\"type\":\"trigger_input\""));
        assert!(json.contains("\"trigger_id\":\"manual_trigger_1\""));
        assert!(json.contains("\"input\":\"Hello, world!\""));
    }

    #[test]
    fn test_interrupt_event() {
        let data = serde_json::json!({
            "plan": "Delete all backup files",
            "risk_level": "high",
            "action": "Set 'approved' to true to continue"
        });
        let event =
            InterruptEvent::new("review", "HIGH RISK: Human approval required", data.clone());

        assert_eq!(event.event_type, "interrupt");
        assert_eq!(event.node_id, "review");
        assert_eq!(event.message, "HIGH RISK: Human approval required");
        assert_eq!(event.data, data);
        assert!(event.timestamp > 0);

        // Test JSON serialization
        let json = event.to_json();
        assert!(json.contains("\"type\":\"interrupt\""));
        assert!(json.contains("\"node_id\":\"review\""));
        assert!(json.contains("\"risk_level\":\"high\""));
    }

    #[test]
    fn test_interrupt_event_simple() {
        let event = InterruptEvent::simple("approval_node", "Please approve this action");
        assert_eq!(event.event_type, "interrupt");
        assert_eq!(event.node_id, "approval_node");
        assert_eq!(event.message, "Please approve this action");
        assert_eq!(event.data, serde_json::Value::Object(Default::default()));
    }

    #[test]
    fn test_interrupt_response_event() {
        let response = serde_json::json!({ "approved": true, "comment": "Looks good" });
        let event = InterruptResponseEvent::new("review", response.clone());

        assert_eq!(event.event_type, "interrupt_response");
        assert_eq!(event.node_id, "review");
        assert_eq!(event.response, response);
        assert!(event.timestamp > 0);

        // Test JSON serialization
        let json = event.to_json();
        assert!(json.contains("\"type\":\"interrupt_response\""));
        assert!(json.contains("\"approved\":true"));
    }

    #[test]
    fn test_resume_event() {
        let event = ResumeEvent::new("review");

        assert_eq!(event.event_type, "resume");
        assert_eq!(event.node_id, "review");
        assert!(event.timestamp > 0);

        // Test JSON serialization
        let json = event.to_json();
        assert!(json.contains("\"type\":\"resume\""));
        assert!(json.contains("\"node_id\":\"review\""));
    }

    #[test]
    fn test_hitl_event_json_format() {
        // Verify the JSON format matches the design document schema
        let interrupt = InterruptEvent::new(
            "review",
            "HIGH RISK: Human approval required",
            serde_json::json!({
                "plan": "...",
                "risk_level": "high",
                "action": "Set 'approved' to true to continue"
            }),
        );

        let parsed: serde_json::Value = serde_json::from_str(&interrupt.to_json()).unwrap();

        // Verify required fields exist
        assert_eq!(parsed["type"], "interrupt");
        assert_eq!(parsed["node_id"], "review");
        assert!(parsed["message"].is_string());
        assert!(parsed["data"].is_object());
        assert!(parsed["timestamp"].is_number());
    }

    // ============================================
    // Debug Event Tests
    // ============================================

    #[test]
    fn test_debug_event_new() {
        let detail = serde_json::json!({
            "model": "gemini-2.0-flash",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let event = DebugEvent::new(
            "debug",
            "request",
            "weather_agent",
            "LLM request to gemini-2.0-flash",
            detail.clone(),
        );

        assert_eq!(event.level, "debug");
        assert_eq!(event.category, "request");
        assert_eq!(event.agent, "weather_agent");
        assert_eq!(event.summary, "LLM request to gemini-2.0-flash");
        assert_eq!(event.detail, detail);
        assert!(event.timestamp > 0);
    }

    #[test]
    fn test_debug_event_to_json() {
        let event = DebugEvent::new(
            "error",
            "error",
            "tool_agent",
            "Tool execution failed",
            serde_json::json!({"error": "timeout"}),
        );

        let json = event.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["category"], "error");
        assert_eq!(parsed["agent"], "tool_agent");
        assert_eq!(parsed["summary"], "Tool execution failed");
        assert_eq!(parsed["detail"]["error"], "timeout");
        assert!(parsed["timestamp"].is_number());
    }

    #[test]
    fn test_debug_event_all_categories() {
        let categories = [
            "request",
            "response",
            "error",
            "state_change",
            "tool_call",
            "tool_result",
            "lifecycle",
        ];
        for category in categories {
            let event = DebugEvent::new("info", category, "agent", "test", serde_json::json!({}));
            assert_eq!(event.category, category);
            let json = event.to_json();
            assert!(json.contains(&format!("\"category\":\"{category}\"")));
        }
    }

    #[test]
    fn test_debug_event_all_levels() {
        let levels = ["error", "warn", "info", "debug", "trace"];
        for level in levels {
            let event = DebugEvent::new(level, "lifecycle", "agent", "test", serde_json::json!({}));
            assert_eq!(event.level, level);
            let json = event.to_json();
            assert!(json.contains(&format!("\"level\":\"{level}\"")));
        }
    }
}
