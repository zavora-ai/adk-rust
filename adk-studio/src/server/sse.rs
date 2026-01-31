use crate::server::events::TraceEventV2;
use crate::server::state::AppState;
use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, Sse},
};
use futures::Stream;
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

lazy_static::lazy_static! {
    static ref SESSIONS: Arc<Mutex<HashMap<String, SessionProcess>>> = Arc::new(Mutex::new(HashMap::new()));
}

struct SessionProcess {
    stdin: BufWriter<tokio::process::ChildStdin>,
    stdout_rx: tokio::sync::mpsc::Receiver<String>,
    stderr_rx: tokio::sync::mpsc::Receiver<String>,
    _child: Child,
}

/// Pending agent info - tracks agents that have started but not yet ended
#[derive(Debug, Clone)]
struct PendingAgent {
    name: String,
    step: u32,
    start_time: std::time::Instant,
    input_state: serde_json::Value,
}

/// Tracks execution state for SSE v2.0 state snapshots.
/// Uses a deferred emission strategy: we track agent starts but only emit
/// node_end events when we have the actual output state from TRACE events.
struct ExecutionContext {
    /// Current execution state (accumulated from agent outputs)
    current_state: HashMap<String, serde_json::Value>,
    /// Step counter for tracking execution progress
    step: u32,
    /// Stack of pending agents (started but not yet ended)
    pending_agents: Vec<PendingAgent>,
    /// Completed agent outputs (from TRACE events with state)
    completed_outputs: HashMap<String, serde_json::Value>,
}

impl ExecutionContext {
    fn new() -> Self {
        Self {
            current_state: HashMap::new(),
            step: 0,
            pending_agents: Vec::new(),
            completed_outputs: HashMap::new(),
        }
    }

    /// Record node start - captures input state but doesn't emit event yet.
    /// Returns the trace event JSON for immediate emission.
    fn node_start(&mut self, node: &str) -> String {
        self.step += 1;
        
        // Capture current state as input state for this node
        let input_state = serde_json::to_value(&self.current_state).unwrap_or_default();
        
        // Push to pending stack
        self.pending_agents.push(PendingAgent {
            name: node.to_string(),
            step: self.step,
            start_time: std::time::Instant::now(),
            input_state: input_state.clone(),
        });
        
        let event = TraceEventV2::node_start(node, self.step, input_state);
        event.to_json()
    }

    /// Record node end with output state from graph execution.
    /// This is called when we parse a TRACE event with state information.
    fn node_end_with_state(&mut self, node: &str, output_state: serde_json::Value) -> Option<String> {
        // Find and remove the pending agent
        let pending_idx = self.pending_agents.iter().position(|p| p.name == node)?;
        let pending = self.pending_agents.remove(pending_idx);
        
        let duration_ms = pending.start_time.elapsed().as_millis() as u64;
        
        // Merge output state into current state for subsequent nodes
        if let serde_json::Value::Object(map) = &output_state {
            for (k, v) in map {
                self.current_state.insert(k.clone(), v.clone());
            }
        }
        
        let event = TraceEventV2::node_end(
            node,
            pending.step,
            duration_ms,
            pending.input_state,
            output_state,
        );
        Some(event.to_json())
    }

    /// Record node end without explicit output state (fallback).
    /// Uses current accumulated state as output.
    fn node_end_fallback(&mut self, node: &str) -> Option<String> {
        // Find and remove the pending agent
        let pending_idx = self.pending_agents.iter().position(|p| p.name == node)?;
        let pending = self.pending_agents.remove(pending_idx);
        
        let duration_ms = pending.start_time.elapsed().as_millis() as u64;
        let output_state = serde_json::to_value(&self.current_state).unwrap_or_default();
        
        let event = TraceEventV2::node_end(
            node,
            pending.step,
            duration_ms,
            pending.input_state,
            output_state,
        );
        Some(event.to_json())
    }

    /// Process a StreamEvent from TRACE output and extract state updates.
    /// Returns (node_end_event, should_emit_done) if applicable.
    fn process_stream_event(&mut self, trace_json: &str) -> (Option<String>, bool) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(trace_json) else {
            return (None, false);
        };
        
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        
        match event_type {
            "node_start" => {
                // NodeStart from adk-graph - we already handle this via stderr
                (None, false)
            }
            "node_end" => {
                // NodeEnd from adk-graph doesn't include state, just duration
                // Use node_end_fallback to emit our own node_end with accumulated state
                // if we haven't already emitted one for this node
                let node = event.get("node").and_then(|v| v.as_str()).unwrap_or("");
                if !node.is_empty() {
                    // Check if this node is still pending (hasn't been emitted via message event)
                    if self.pending_agents.iter().any(|p| p.name == node) {
                        if let Some(event_json) = self.node_end_fallback(node) {
                            return (Some(event_json), false);
                        }
                    }
                }
                (None, false)
            }
            "message" => {
                // Message event contains agent output text
                // Capture this as the agent's response for state tracking
                let node = event.get("node").and_then(|v| v.as_str()).unwrap_or("");
                let content = event.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let is_final = event.get("is_final").and_then(|v| v.as_bool()).unwrap_or(false);
                
                if !node.is_empty() && !content.is_empty() {
                    // Store the agent's response in completed_outputs
                    // This will be used when emitting node_end events
                    let agent_response = serde_json::json!({
                        "response": content,
                        "input": self.current_state.get("input").cloned().unwrap_or(serde_json::Value::Null)
                    });
                    self.completed_outputs.insert(node.to_string(), agent_response.clone());
                    
                    // If this is a final message, also update current state
                    if is_final {
                        self.current_state.insert("response".to_string(), serde_json::Value::String(content.to_string()));
                        
                        // Emit node_end immediately for this agent since we have the final output
                        // This provides more granular timeline updates
                        if let Some(event_json) = self.node_end_with_state(node, agent_response) {
                            return (Some(event_json), false);
                        }
                    }
                }
                (None, false)
            }
            "state" => {
                // State snapshot - update current state
                if let Some(state) = event.get("state") {
                    if let serde_json::Value::Object(map) = state {
                        for (k, v) in map {
                            self.current_state.insert(k.clone(), v.clone());
                        }
                    }
                }
                (None, false)
            }
            "updates" => {
                // State updates from a node
                let node = event.get("node").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(updates) = event.get("updates") {
                    if let serde_json::Value::Object(map) = updates {
                        // Store as completed output for this node
                        self.completed_outputs.insert(node.to_string(), updates.clone());
                        // Also update current state
                        for (k, v) in map {
                            self.current_state.insert(k.clone(), v.clone());
                        }
                    }
                }
                (None, false)
            }
            "done" => {
                // Done event contains final state - emit node_end for all pending agents
                if let Some(state) = event.get("state") {
                    if let serde_json::Value::Object(map) = state {
                        for (k, v) in map {
                            self.current_state.insert(k.clone(), v.clone());
                        }
                    }
                }
                (None, true)
            }
            _ => (None, false),
        }
    }

    /// Emit node_end events for all pending agents using their captured output state.
    /// Called when we receive the Done event with final state.
    /// 
    /// For multi-agent workflows, each agent's output is captured from Message events
    /// in the TRACE output. This ensures each agent shows its own response, not the
    /// final accumulated state.
    fn emit_pending_node_ends(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        
        // Process in order (first started, first ended)
        // We need to emit in the correct order for the timeline
        let pending: Vec<_> = self.pending_agents.drain(..).collect();
        
        for pending_agent in pending {
            let duration_ms = pending_agent.start_time.elapsed().as_millis() as u64;
            
            // Use the agent's captured output if available
            // This comes from Message events in the TRACE output
            let output_state = self.completed_outputs
                .remove(&pending_agent.name)
                .unwrap_or_else(|| {
                    // Fallback: use current state but include the input
                    let mut state = serde_json::Map::new();
                    state.insert("input".to_string(), 
                        pending_agent.input_state.get("input")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null));
                    if let Some(response) = self.current_state.get("response") {
                        state.insert("response".to_string(), response.clone());
                    }
                    serde_json::Value::Object(state)
                });
            
            let event = TraceEventV2::node_end(
                &pending_agent.name,
                pending_agent.step,
                duration_ms,
                pending_agent.input_state,
                output_state,
            );
            events.push(event.to_json());
        }
        
        events
    }

    /// Record execution complete and return the done event JSON.
    fn done(&self) -> String {
        let output_state = serde_json::to_value(&self.current_state).unwrap_or_default();
        let event = TraceEventV2::done(
            self.step,
            serde_json::Value::Object(Default::default()),
            output_state,
        );
        event.to_json()
    }

    /// Update current state with a new key-value pair.
    fn update_state(&mut self, key: &str, value: serde_json::Value) {
        self.current_state.insert(key.to_string(), value);
    }
    
    /// Check if there are pending agents
    fn has_pending_agents(&self) -> bool {
        !self.pending_agents.is_empty()
    }
}

#[derive(Deserialize)]
pub struct StreamQuery {
    input: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    binary_path: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

async fn get_or_create_session(
    session_id: &str,
    binary_path: &str,
    api_key: &str,
) -> Result<(), String> {
    let mut sessions = SESSIONS.lock().await;
    if sessions.contains_key(session_id) {
        return Ok(());
    }

    let mut child = Command::new(binary_path)
        .arg(session_id)
        .env("GOOGLE_API_KEY", api_key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start binary: {}", e))?;

    let stdin = BufWriter::new(child.stdin.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let (stdout_tx, stdout_rx) = tokio::sync::mpsc::channel(100);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if stdout_tx.send(line).await.is_err() {
                break;
            }
        }
    });

    let (stderr_tx, stderr_rx) = tokio::sync::mpsc::channel(100);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if stderr_tx.send(line).await.is_err() {
                break;
            }
        }
    });

    sessions.insert(
        session_id.to_string(),
        SessionProcess { stdin, stdout_rx, stderr_rx, _child: child },
    );
    Ok(())
}

pub async fn stream_handler(
    Path(_id): Path<String>,
    Query(query): Query<StreamQuery>,
    State(_state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let api_key =
        query.api_key.or_else(|| std::env::var("GOOGLE_API_KEY").ok()).unwrap_or_default();
    let input = query.input.clone();
    let binary_path = query.binary_path;
    let session_id = query.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let stream = async_stream::stream! {
        let Some(bin_path) = binary_path else {
            yield Ok(Event::default().event("error").data("No binary available. Click 'Build' first."));
            return;
        };

        if let Err(e) = get_or_create_session(&session_id, &bin_path, &api_key).await {
            yield Ok(Event::default().event("error").data(e));
            return;
        }

        yield Ok(Event::default().event("session").data(session_id.clone()));

        // Initialize execution context for state snapshot tracking (v2.0)
        // Uses deferred emission: node_start events are emitted immediately,
        // but node_end events are deferred until we have the actual output state
        // from TRACE events (specifically StreamEvent::Done which has final state)
        let mut exec_ctx = ExecutionContext::new();
        // Store the initial input in the execution state
        exec_ctx.update_state("input", serde_json::Value::String(input.clone()));

        // Send input
        {
            let mut sessions = SESSIONS.lock().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                if session.stdin.write_all(format!("{}\n", input).as_bytes()).await.is_err()
                    || session.stdin.flush().await.is_err() {
                    yield Ok(Event::default().event("error").data("Failed to send input"));
                    return;
                }
            }
        }

        let timeout = tokio::time::Duration::from_secs(60);
        let start = tokio::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                yield Ok(Event::default().event("error").data("Timeout"));
                break;
            }

            let (stdout_msg, stderr_msg) = {
                let mut sessions = SESSIONS.lock().await;
                match sessions.get_mut(&session_id) {
                    Some(s) => (s.stdout_rx.try_recv().ok(), s.stderr_rx.try_recv().ok()),
                    None => {
                        yield Ok(Event::default().event("error").data("Session lost"));
                        break;
                    }
                }
            };

            let mut got_data = false;

            if let Some(line) = stdout_msg {
                got_data = true;
                let line = line.trim_start_matches("> ");
                if let Some(sid) = line.strip_prefix("SESSION:") {
                    yield Ok(Event::default().event("session").data(sid));
                } else if let Some(trace) = line.strip_prefix("TRACE:") {
                    // Process TRACE events from adk-graph to extract state information
                    // This is where we get the actual output state for each agent
                    let (node_end_event, is_done) = exec_ctx.process_stream_event(trace);
                    
                    // If we got a node_end event with state, emit it
                    if let Some(event_json) = node_end_event {
                        yield Ok(Event::default().event("trace").data(event_json));
                    }
                    
                    // If this is a Done event, emit all pending node_end events
                    // with the correct output state (now that we have it)
                    if is_done {
                        for event_json in exec_ctx.emit_pending_node_ends() {
                            yield Ok(Event::default().event("trace").data(event_json));
                        }
                    }
                    
                    // Also pass through the original trace for backward compatibility
                    yield Ok(Event::default().event("trace").data(trace));
                } else if let Some(chunk) = line.strip_prefix("CHUNK:") {
                    // Streaming chunk - emit immediately
                    let decoded = serde_json::from_str::<String>(chunk).unwrap_or_else(|_| chunk.to_string());
                    yield Ok(Event::default().event("chunk").data(decoded));
                } else if let Some(response) = line.strip_prefix("RESPONSE:") {
                    let decoded = serde_json::from_str::<String>(response).unwrap_or_else(|_| response.to_string());
                    // Update execution state with the response
                    exec_ctx.update_state("response", serde_json::Value::String(decoded.clone()));
                    yield Ok(Event::default().event("chunk").data(decoded));
                    
                    // Emit any remaining pending node_end events with final state
                    // This handles the case where RESPONSE comes before/without Done event
                    if exec_ctx.has_pending_agents() {
                        for event_json in exec_ctx.emit_pending_node_ends() {
                            yield Ok(Event::default().event("trace").data(event_json));
                        }
                    }
                    
                    // Emit done event with final state snapshot (v2.0)
                    yield Ok(Event::default().event("trace").data(exec_ctx.done()));
                    yield Ok(Event::default().event("end").data(""));
                    break;
                }
            }

            if let Some(line) = stderr_msg {
                got_data = true;
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    let fields = json.get("fields");
                    let msg = fields.and_then(|f| f.get("message")).and_then(|m| m.as_str()).unwrap_or("");

                    if msg == "tool_call" {
                        let name = fields.and_then(|f| f.get("tool.name")).and_then(|v| v.as_str()).unwrap_or("");
                        let args = fields.and_then(|f| f.get("tool.args")).and_then(|v| v.as_str()).unwrap_or("{}");
                        yield Ok(Event::default().event("tool_call").data(serde_json::json!({"name": name, "args": args}).to_string()));
                    } else if msg == "tool_result" {
                        let name = fields.and_then(|f| f.get("tool.name")).and_then(|v| v.as_str()).unwrap_or("");
                        let result = fields.and_then(|f| f.get("tool.result")).and_then(|v| v.as_str()).unwrap_or("");
                        // Update execution state with tool result (v2.0)
                        exec_ctx.update_state(&format!("tool_{}", name), serde_json::Value::String(result.to_string()));
                        yield Ok(Event::default().event("tool_result").data(serde_json::json!({"name": name, "result": result}).to_string()));
                    } else if msg == "Starting agent execution" {
                        // Emit node_start for sub-agent with state snapshot (v2.0)
                        // This is emitted immediately so the UI can show the agent is running
                        let agent = json.get("span").and_then(|s| s.get("agent.name")).and_then(|v| v.as_str()).unwrap_or("");
                        let trace_data = exec_ctx.node_start(agent);
                        yield Ok(Event::default().event("trace").data(trace_data));
                    } else if msg == "Agent execution complete" {
                        // Don't emit node_end here - we defer until we have actual output state
                        // The node_end will be emitted when we process the Done TRACE event
                        // or when RESPONSE is received (whichever comes first)
                        // This fixes the timing issue where node_end was emitted before
                        // the agent's response was captured in state
                    } else if msg == "Generating content" {
                        // Model call - extract details
                        let span = json.get("span");
                        let model = span.and_then(|s| s.get("model.name")).and_then(|v| v.as_str()).unwrap_or("");
                        let tools = span.and_then(|s| s.get("request.tools_count")).and_then(|v| v.as_str()).unwrap_or("0");
                        yield Ok(Event::default().event("log").data(serde_json::json!({"message": format!("Calling {} (tools: {})", model, tools)}).to_string()));
                    }
                }
            }

            if !got_data {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        }
    };

    Sse::new(stream)
}

pub async fn kill_session(Path(session_id): Path<String>) -> &'static str {
    let mut sessions = SESSIONS.lock().await;
    if let Some(mut session) = sessions.remove(&session_id) {
        // Kill the child process explicitly
        if let Err(e) = session._child.kill().await {
            tracing::warn!("Failed to kill session {}: {}", session_id, e);
        }
    }
    "ok"
}
