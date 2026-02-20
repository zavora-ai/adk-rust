use crate::keystore::{KNOWN_PROVIDER_KEYS, Keystore};
use crate::server::events::{DebugEvent, TraceEventV2};
use crate::server::graph_runner::{GraphInterruptHandler, INTERRUPTED_SESSIONS};
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

/// Tuple of (action node output keys, project env vars, action node types, storage base dir, project uuid)
/// extracted from a project for use in the SSE stream handler.
type ProjectStreamContext = (
    HashMap<String, Vec<String>>,
    HashMap<String, String>,
    HashMap<String, String>,
    Option<std::path::PathBuf>,
    Option<uuid::Uuid>,
);

lazy_static::lazy_static! {
    static ref SESSIONS: Arc<Mutex<HashMap<String, SessionProcess>>> = Arc::new(Mutex::new(HashMap::new()));
}

struct SessionProcess {
    stdin: BufWriter<tokio::process::ChildStdin>,
    stdout_rx: tokio::sync::mpsc::Receiver<String>,
    stderr_rx: tokio::sync::mpsc::Receiver<String>,
    _child: Child,
    created_at: std::time::Instant,
}

/// Remove stale sessions older than the given max age.
/// Called periodically to prevent unbounded memory growth.
pub async fn cleanup_stale_sessions(max_age: std::time::Duration) {
    let mut sessions = SESSIONS.lock().await;
    let before = sessions.len();
    sessions.retain(|id, session| {
        let stale = session.created_at.elapsed() > max_age;
        if stale {
            tracing::info!(session_id = %id, "Cleaning up stale session");
        }
        !stale
    });
    let removed = before - sessions.len();
    if removed > 0 {
        tracing::info!(
            removed = removed,
            remaining = sessions.len(),
            "Stale session cleanup complete"
        );
    }
}

/// Information about an interrupt extracted from TRACE output.
#[derive(Debug, Clone)]
struct InterruptInfo {
    node_id: String,
    message: String,
    data: serde_json::Value,
    thread_id: String,
    checkpoint_id: String,
    step: usize,
    state: HashMap<String, serde_json::Value>,
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
    /// Durations recorded from raw TRACE node_end events (node → ms)
    recorded_durations: HashMap<String, u64>,
    /// Expected output keys per action node (node_id → keys).
    /// Populated from the project's action node configuration so we can
    /// reconstruct per-node output snapshots from the final state.
    action_node_output_keys: HashMap<String, Vec<String>>,
    /// Action node types (node_id → type string like "trigger", "set", "transform").
    /// Used to emit debug events with the correct category (trigger vs action).
    action_node_types: HashMap<String, String>,
}

impl ExecutionContext {
    fn new() -> Self {
        Self {
            current_state: HashMap::new(),
            step: 0,
            pending_agents: Vec::new(),
            completed_outputs: HashMap::new(),
            recorded_durations: HashMap::new(),
            action_node_output_keys: HashMap::new(),
            action_node_types: HashMap::new(),
        }
    }

    /// Record node start - captures input state and emits node_start event.
    /// Returns a list of events to emit.
    ///
    /// When a new node_start arrives, we close ALL previously pending nodes:
    /// - Same-name nodes (loop iterations): closed with current state diff
    /// - Different-name nodes: closed with their expected output keys or diff
    ///
    /// Action nodes whose expected output keys are NOT yet in `current_state`
    /// are kept pending — they'll be resolved in `emit_pending_node_ends()`
    /// when the Done event arrives with the full final state. This is necessary
    /// because `adk-graph` v0.2.1 doesn't emit `updates` events for
    /// FunctionNodes in Messages mode.
    fn node_start(&mut self, node: &str) -> Vec<String> {
        let mut events = Vec::new();
        let mut still_pending = Vec::new();

        // Close ALL pending nodes before starting the new one.
        // Sort by step so they emit in chronological order.
        let mut pending: Vec<_> = self.pending_agents.drain(..).collect();
        pending.sort_by_key(|p| p.step);
        let pending_count = pending.len();

        for prev in pending {
            // If this pending node hasn't produced output yet, check if we should defer it.
            // Deferral is ONLY for parallel branches — when multiple nodes are pending
            // simultaneously. In sequential flow (single pending node), the arrival of
            // a new node_start proves the previous node finished, so close it immediately.
            let has_captured = self.completed_outputs.contains_key(&prev.name);
            let is_parallel = pending_count > 1;
            if !has_captured && is_parallel {
                let is_action_node = self.action_node_output_keys.contains_key(&prev.name);
                if is_action_node {
                    if let Some(expected_keys) = self.action_node_output_keys.get(&prev.name) {
                        let any_key_present =
                            expected_keys.iter().any(|k| self.current_state.contains_key(k));
                        if !any_key_present && !expected_keys.is_empty() {
                            still_pending.push(prev);
                            continue;
                        }
                    }
                } else {
                    // LLM agent with no completed output — it's still running
                    // in a parallel branch. Defer until done event.
                    still_pending.push(prev);
                    continue;
                }
            }

            let duration_ms = self
                .recorded_durations
                .remove(&prev.name)
                .unwrap_or_else(|| prev.start_time.elapsed().as_millis() as u64);

            let output_state = if let Some(captured) = self.completed_outputs.remove(&prev.name) {
                captured
            } else if let Some(expected_keys) = self.action_node_output_keys.get(&prev.name) {
                let mut output = serde_json::Map::new();
                for key in expected_keys {
                    if let Some(value) = self.current_state.get(key) {
                        output.insert(key.clone(), value.clone());
                    }
                }
                if output.is_empty() {
                    self.diff_against_input(&prev.input_state, &self.current_state)
                } else {
                    serde_json::Value::Object(output)
                }
            } else {
                self.diff_against_input(&prev.input_state, &self.current_state)
            };

            let end_event = TraceEventV2::node_end(
                &prev.name,
                prev.step,
                duration_ms,
                prev.input_state,
                output_state,
            );
            events.push(end_event.to_json());
        }

        // Re-add deferred action nodes back to pending
        self.pending_agents.extend(still_pending);

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

        let start_event = TraceEventV2::node_start(node, self.step, input_state);
        events.push(start_event.to_json());

        events
    }

    /// Process a StreamEvent from TRACE output and extract state updates.
    /// Returns (enriched_events, should_emit_done, suppress_raw_passthrough).
    fn process_stream_event(&mut self, trace_json: &str) -> (Vec<String>, bool, bool) {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(trace_json) else {
            return (Vec::new(), false, false);
        };

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "node_start" => {
                // NodeStart from adk-graph — track action nodes that aren't
                // already tracked via stderr ("Starting agent execution").
                let node = event.get("node").and_then(|v| v.as_str()).unwrap_or("");
                if !node.is_empty() {
                    let events = self.node_start(node);
                    if !events.is_empty() {
                        // suppress_raw=true: we emit our enriched version instead
                        return (events, false, true);
                    }
                }
                (Vec::new(), false, false)
            }
            "node_end" => {
                // NodeEnd from adk-graph doesn't include state, just duration.
                // For action nodes (Set, Transform, etc.) the published adk-graph
                // doesn't emit Updates events in Messages mode, so we don't know
                // what state they produced yet. Keep them pending until the Done
                // event arrives with the full final state, then reconstruct each
                // node's output by diffing input states.
                //
                // For LLM agents, node_end_with_state is called from the "message"
                // handler when is_final=true, so they won't be pending here.
                let node = event.get("node").and_then(|v| v.as_str()).unwrap_or("");
                if !node.is_empty() {
                    let is_pending = self.pending_agents.iter().any(|p| p.name == node);
                    if is_pending {
                        // Record the raw duration from the TRACE event so we can
                        // use it later when we emit the reconstructed node_end.
                        let duration =
                            event.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                        self.recorded_durations.insert(node.to_string(), duration);
                    }
                }
                // Suppress the raw node_end — we'll emit enriched versions later
                (Vec::new(), false, true)
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

                    // If this is a final message, update current state but DON'T
                    // emit node_end yet. We defer ALL node_ends to emit_pending_node_ends()
                    // so they arrive in correct step order (action nodes before LLM agents
                    // that run after them). The node_end will be emitted when the Done
                    // event arrives.
                    if is_final {
                        self.current_state.insert(
                            "response".to_string(),
                            serde_json::Value::String(content.to_string()),
                        );
                    }
                }
                (Vec::new(), false, false)
            }
            "state" => {
                // State snapshot - update current state
                if let Some(serde_json::Value::Object(map)) = event.get("state") {
                    for (k, v) in map {
                        self.current_state.insert(k.clone(), v.clone());
                    }
                }
                (Vec::new(), false, false)
            }
            "updates" => {
                // State updates from a node (emitted by local adk-graph builds,
                // but NOT by the published crate used in generated binaries).
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
                (Vec::new(), false, false)
            }
            "done" => {
                // Done event contains final state - emit node_end for all pending agents
                if let Some(serde_json::Value::Object(map)) = event.get("state") {
                    for (k, v) in map {
                        self.current_state.insert(k.clone(), v.clone());
                    }
                }
                (Vec::new(), true, false)
            }
            "interrupted" => {
                // Interrupt event from ADK-Graph - workflow is paused
                // This is handled separately via the interrupt SSE event
                // Just update state if provided
                if let Some(serde_json::Value::Object(map)) = event.get("state") {
                    for (k, v) in map {
                        self.current_state.insert(k.clone(), v.clone());
                    }
                }
                (Vec::new(), false, false)
            }
            _ => (Vec::new(), false, false),
        }
    }

    /// Process an interrupt event from TRACE output.
    /// Returns the interrupt data if this is an interrupt event.
    fn process_interrupt_event(&self, trace_json: &str) -> Option<InterruptInfo> {
        let event: serde_json::Value = serde_json::from_str(trace_json).ok()?;
        let event_type = event.get("type").and_then(|v| v.as_str())?;

        if event_type == "interrupted" {
            let node_id =
                event.get("node").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
            let message = event
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Workflow interrupted")
                .to_string();
            let data = event.get("data").cloned().unwrap_or(serde_json::Value::Null);
            let thread_id =
                event.get("thread_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let checkpoint_id =
                event.get("checkpoint_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let step = event.get("step").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            Some(InterruptInfo {
                node_id,
                message,
                data,
                thread_id,
                checkpoint_id,
                step,
                state: self.current_state.clone(),
            })
        } else {
            None
        }
    }

    /// Emit node_end events for all pending agents, using the final state
    /// from the Done event to populate output snapshots.
    ///
    /// The published adk-graph crate (used by generated binaries) does NOT emit
    /// Updates events for action nodes in Messages mode. So when Done arrives
    /// with the full final state, we use it to reconstruct meaningful output
    /// for each pending action node.
    ///
    /// Strategy: if we know the expected output keys for a node (from the
    /// project's action node config), we extract exactly those keys from the
    /// final state. Otherwise we fall back to showing all new keys.
    ///
    /// LLM agents that already emitted via Message events won't be pending here.
    fn emit_pending_node_ends(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        let mut pending: Vec<_> = self.pending_agents.drain(..).collect();

        if pending.is_empty() {
            return events;
        }

        // Sort by step order so snapshots arrive chronologically
        pending.sort_by_key(|a| a.step);

        let final_state = &self.current_state;

        // Progressive state: accumulates each node's output so the next node's
        // input_state reflects the full pipeline state up to that point.
        // Start from the first node's captured input_state (typically just {input}).
        let mut running_state: HashMap<String, serde_json::Value> =
            if let Some(first) = pending.first() {
                match &first.input_state {
                    serde_json::Value::Object(map) => {
                        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    }
                    _ => HashMap::new(),
                }
            } else {
                HashMap::new()
            };

        for agent in &pending {
            // Use recorded TRACE duration if available, otherwise wall-clock
            let duration_ms = self
                .recorded_durations
                .remove(&agent.name)
                .unwrap_or_else(|| agent.start_time.elapsed().as_millis() as u64);

            // The input state for this node is the accumulated running state
            let input_state = serde_json::to_value(&running_state).unwrap_or_default();

            // Determine the output state for this node
            let output_state = if let Some(captured) = self.completed_outputs.remove(&agent.name) {
                captured
            } else if let Some(expected_keys) = self.action_node_output_keys.get(&agent.name) {
                // We know exactly which keys this action node produces.
                // Extract those keys from the final state.
                let mut output = serde_json::Map::new();
                for key in expected_keys {
                    if let Some(value) = final_state.get(key) {
                        output.insert(key.clone(), value.clone());
                    }
                }
                if output.is_empty() {
                    self.diff_against_input(&input_state, final_state)
                } else {
                    serde_json::Value::Object(output)
                }
            } else {
                // Unknown node type — show all keys that weren't in the input
                self.diff_against_input(&input_state, final_state)
            };

            // Merge this node's output into the running state for the next node
            if let serde_json::Value::Object(map) = &output_state {
                for (k, v) in map {
                    running_state.insert(k.clone(), v.clone());
                }
            }

            let event = TraceEventV2::node_end(
                &agent.name,
                agent.step,
                duration_ms,
                input_state,
                output_state,
            );
            events.push(event.to_json());
        }

        events
    }

    /// Compute the diff between a node's input state and the final state.
    /// Returns keys present in final_state but not in input_state, excluding
    /// the "response" key (which belongs to LLM agents).
    fn diff_against_input(
        &self,
        input_state: &serde_json::Value,
        final_state: &HashMap<String, serde_json::Value>,
    ) -> serde_json::Value {
        let input_keys: std::collections::HashSet<String> = match input_state {
            serde_json::Value::Object(map) => map.keys().cloned().collect(),
            _ => std::collections::HashSet::new(),
        };

        let mut output = serde_json::Map::new();
        for (key, value) in final_state {
            if !input_keys.contains(key) && key != "response" {
                output.insert(key.clone(), value.clone());
            }
        }

        if output.is_empty() {
            serde_json::to_value(final_state).unwrap_or_default()
        } else {
            serde_json::Value::Object(output)
        }
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

    /// Return the name of the most recently started agent, if any.
    fn current_agent(&self) -> Option<&str> {
        self.pending_agents.last().map(|pa| pa.name.as_str())
    }

    /// Return the debug event category for a node.
    /// Trigger nodes → "trigger", other action nodes → "action", LLM agents → "lifecycle".
    fn debug_category_for_node(&self, node: &str) -> &'static str {
        match self.action_node_types.get(node).map(|s| s.as_str()) {
            Some("trigger") => "trigger",
            Some(_) => "action",
            None => "lifecycle",
        }
    }

    /// Return the action node type string for a node (e.g. "set", "transform"), or None.
    fn action_type_for_node(&self, node: &str) -> Option<&str> {
        self.action_node_types.get(node).map(|s| s.as_str())
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
    merged_env: &HashMap<String, String>,
) -> Result<(), String> {
    let mut sessions = SESSIONS.lock().await;
    if sessions.contains_key(session_id) {
        return Ok(());
    }

    // Launch the child binary with the pre-merged environment variables.
    // The caller (stream_handler) has already merged keys from process env,
    // project env_vars, and the encrypted keystore in priority order.
    let mut cmd = Command::new(binary_path);
    cmd.arg(session_id)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    for (key, val) in merged_env {
        cmd.env(key, val);
    }

    let mut child = cmd.spawn().map_err(|e| format!("Failed to start binary: {}", e))?;

    let stdin = BufWriter::new(
        child.stdin.take().ok_or_else(|| "Failed to capture child stdin".to_string())?,
    );
    let stdout = child.stdout.take().ok_or_else(|| "Failed to capture child stdout".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "Failed to capture child stderr".to_string())?;

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
        SessionProcess {
            stdin,
            stdout_rx,
            stderr_rx,
            _child: child,
            created_at: std::time::Instant::now(),
        },
    );
    Ok(())
}

pub async fn stream_handler(
    Path(_id): Path<String>,
    Query(query): Query<StreamQuery>,
    State(app_state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let input = query.input.clone();
    let binary_path = query.binary_path;
    let session_id = query.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Load the project's action node output key mappings so we can
    // reconstruct per-node output snapshots from the final state.
    // Also load project-level env_vars to pass to the subprocess.
    // Additionally load action node types for debug event categorization.
    let (
        action_node_output_keys,
        project_env_vars,
        action_node_types,
        storage_base_dir,
        project_uuid,
    ): ProjectStreamContext = {
        if let Ok(project_id) = uuid::Uuid::parse_str(&_id) {
            let storage = app_state.storage.read().await;
            if let Ok(project) = storage.get(project_id).await {
                let output_keys = project
                    .action_nodes
                    .iter()
                    .map(|(id, cfg)| (id.clone(), cfg.expected_output_keys()))
                    .collect();
                let node_types = project
                    .action_nodes
                    .iter()
                    .map(|(id, cfg)| (id.clone(), cfg.node_type().to_string()))
                    .collect();
                let env_vars = project.settings.env_vars.clone();
                let base_dir = storage.base_dir().to_path_buf();
                (output_keys, env_vars, node_types, Some(base_dir), Some(project_id))
            } else {
                (HashMap::new(), HashMap::new(), HashMap::new(), None, None)
            }
        } else {
            (HashMap::new(), HashMap::new(), HashMap::new(), None, None)
        }
    };

    // Merge API keys from three sources in priority order:
    // 1. Process environment variables (lowest priority)
    // 2. Project env_vars from settings
    // 3. Encrypted keystore keys (highest priority)
    let merged_env = {
        let mut keys = HashMap::new();

        // Layer 1 (lowest priority): process environment
        for key in KNOWN_PROVIDER_KEYS {
            if let Ok(val) = std::env::var(key) {
                keys.insert(key.to_string(), val);
            }
        }

        // Layer 2: project env_vars (non-sensitive settings + any remaining keys)
        for (key, val) in &project_env_vars {
            keys.insert(key.clone(), val.clone());
        }

        // Layer 3 (highest priority): encrypted keystore
        if let (Some(base_dir), Some(pid)) = (&storage_base_dir, project_uuid) {
            if let Ok(keystore) = Keystore::new(base_dir, pid) {
                if let Ok(stored_keys) = keystore.load().await {
                    for (key, val) in stored_keys {
                        keys.insert(key, val);
                    }
                }
            }
        }

        keys
    };

    let stream = async_stream::stream! {
        let Some(bin_path) = binary_path else {
            let err_msg = "No binary available. Click 'Build' first.";
            yield Ok(Event::default().event("error").data(err_msg));
            let debug_evt = DebugEvent::new("error", "error", "system", err_msg, serde_json::json!({ "error": err_msg }));
            yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
            return;
        };

        // Check if any provider API key is available from the merged sources
        let has_api_key = query.api_key.is_some()
            || KNOWN_PROVIDER_KEYS
                .iter()
                .any(|k| merged_env.contains_key(*k));

        if !has_api_key {
            let err_msg = "No API key found. Set one of: GOOGLE_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY, \
                 DEEPSEEK_API_KEY, GROQ_API_KEY, or OLLAMA_HOST for local models.";
            yield Ok(Event::default().event("error").data(err_msg));
            let debug_evt = DebugEvent::new("error", "error", "system", err_msg, serde_json::json!({ "error": err_msg }));
            yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
            return;
        }

        if let Err(e) = get_or_create_session(&session_id, &bin_path, &merged_env).await {
            let debug_evt = DebugEvent::new("error", "error", "system", &e, serde_json::json!({ "error": &e }));
            yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
            yield Ok(Event::default().event("error").data(e));
            return;
        }

        yield Ok(Event::default().event("session").data(session_id.clone()));

        // Initialize execution context for state snapshot tracking (v2.0)
        // Uses deferred emission: node_start events are emitted immediately,
        // but node_end events are deferred until we have the actual output state
        // from TRACE events (specifically StreamEvent::Done which has final state)
        let mut exec_ctx = ExecutionContext::new();
        exec_ctx.action_node_output_keys = action_node_output_keys;
        exec_ctx.action_node_types = action_node_types;

        // Check if this is a webhook trigger (special input marker)
        let actual_input = if input == "__webhook__" {
            // Retrieve the webhook payload stored by the webhook trigger endpoint
            if let Some(webhook_payload) = crate::server::handlers::get_webhook_payload(&session_id).await {
                // Store webhook payload in execution state
                exec_ctx.update_state("webhook_payload", webhook_payload.payload.clone());
                exec_ctx.update_state("webhook_path", serde_json::Value::String(webhook_payload.path.clone()));
                exec_ctx.update_state("webhook_method", serde_json::Value::String(webhook_payload.method.clone()));

                // Use the webhook payload as the input (serialized as JSON)
                serde_json::to_string(&webhook_payload.payload).unwrap_or_else(|_| "{}".to_string())
            } else {
                // No webhook payload found, use empty object
                tracing::warn!(session_id = %session_id, "Webhook input requested but no payload found");
                "{}".to_string()
            }
        } else {
            // Regular input
            exec_ctx.update_state("input", serde_json::Value::String(input.clone()));
            input.clone()
        };

        // Send input
        {
            let mut sessions = SESSIONS.lock().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                if session.stdin.write_all(format!("{}\n", actual_input).as_bytes()).await.is_err()
                    || session.stdin.flush().await.is_err() {
                    let err_msg = "Failed to send input";
                    yield Ok(Event::default().event("error").data(err_msg));
                    let debug_evt = DebugEvent::new("error", "error", "system", err_msg, serde_json::json!({ "error": err_msg }));
                    yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                    return;
                }
            }
        }

        // Drain stale output from previous execution.
        // When the session process is reused across turns, the previous execution's
        // trailing output (e.g. Done TRACE event after RESPONSE) may still be in the
        // stdout/stderr channels. We must discard it before reading the new execution's
        // output, otherwise the handler would see the old Done event and break immediately.
        {
            let drain_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(200);
            loop {
                if tokio::time::Instant::now() > drain_deadline {
                    break;
                }
                let (stdout_msg, stderr_msg) = {
                    let mut sessions = SESSIONS.lock().await;
                    match sessions.get_mut(&session_id) {
                        Some(s) => (s.stdout_rx.try_recv().ok(), s.stderr_rx.try_recv().ok()),
                        None => (None, None),
                    }
                };
                // Drain stderr silently
                let _ = stderr_msg;

                match stdout_msg {
                    Some(line) => {
                        let trimmed = line.trim_start_matches("> ");
                        // If we see a node_start for step 0, this is the NEW execution starting.
                        if trimmed.starts_with("TRACE:") {
                            if let Ok(ev) = serde_json::from_str::<serde_json::Value>(trimmed.trim_start_matches("TRACE:")) {
                                let is_new_start = ev.get("type").and_then(|v| v.as_str()) == Some("node_start")
                                    && ev.get("step").and_then(|v| v.as_u64()) == Some(0);
                                if is_new_start {
                                    // This belongs to the new execution — process it and continue to main loop
                                    let trace = trimmed.trim_start_matches("TRACE:");
                                    let (enriched_events, _is_done, suppress_raw) = exec_ctx.process_stream_event(trace);
                                    for event_json in &enriched_events {
                                        // Emit debug events for the first node_start (drain loop)
                                        if let Ok(ev) = serde_json::from_str::<serde_json::Value>(event_json) {
                                            let ev_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                            let node = ev.get("node").and_then(|v| v.as_str()).unwrap_or("");
                                            let step = ev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
                                            let category = exec_ctx.debug_category_for_node(node);
                                            let action_type = exec_ctx.action_type_for_node(node);
                                            if ev_type == "node_start" {
                                                let summary = match action_type {
                                                    Some("trigger") => format!("Trigger fired: {node}, step {step}"),
                                                    Some(atype) => format!("{} node started: {node}, step {step}", capitalize(atype)),
                                                    None => format!("Agent started: {node}, step {step}"),
                                                };
                                                let debug_evt = DebugEvent::new("debug", category, node, summary, ev.clone());
                                                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                                            }
                                        }
                                        yield Ok(Event::default().event("trace").data(event_json.clone()));
                                    }
                                    if !suppress_raw {
                                        yield Ok(Event::default().event("trace").data(trace));
                                    }
                                    break;
                                }
                            }
                        }
                        // Otherwise it's stale output from previous turn — discard it
                    }
                    None => {
                        // No more buffered output — wait a bit for new data
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }

        // Timeout is configurable via ADK_STUDIO_STREAM_TIMEOUT_SECS env var (default: 300s / 5 min)
        let timeout_secs: u64 = std::env::var("ADK_STUDIO_STREAM_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let timeout = tokio::time::Duration::from_secs(timeout_secs);
        let start = tokio::time::Instant::now();

        loop {
            if start.elapsed() > timeout {
                let err_msg = "Timeout";
                yield Ok(Event::default().event("error").data(err_msg));
                let debug_evt = DebugEvent::new("error", "error", "system", err_msg, serde_json::json!({ "error": err_msg, "timeout_secs": timeout_secs }));
                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                break;
            }

            let (stdout_msg, stderr_msg) = {
                let mut sessions = SESSIONS.lock().await;
                match sessions.get_mut(&session_id) {
                    Some(s) => (s.stdout_rx.try_recv().ok(), s.stderr_rx.try_recv().ok()),
                    None => {
                        let err_msg = "Session lost";
                        yield Ok(Event::default().event("error").data(err_msg));
                        let debug_evt = DebugEvent::new("error", "error", "system", err_msg, serde_json::json!({ "error": err_msg, "session_id": &session_id }));
                        yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
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
                    let (enriched_events, is_done, suppress_raw) = exec_ctx.process_stream_event(trace);

                    // Check for interrupt events (Task 9: Handle ADK-Graph Interrupts)
                    // Requirements: 3.1, 5.2
                    if let Some(interrupt_info) = exec_ctx.process_interrupt_event(trace) {
                        // Create interrupt handler and store the interrupted state
                        let handler = GraphInterruptHandler::new(INTERRUPTED_SESSIONS.clone());
                        handler.handle_interrupt(
                            &session_id,
                            interrupt_info.thread_id.clone(),
                            interrupt_info.checkpoint_id.clone(),
                            interrupt_info.node_id.clone(),
                            interrupt_info.message.clone(),
                            interrupt_info.data.clone(),
                            interrupt_info.state.clone(),
                            interrupt_info.step,
                        ).await;

                        // Emit interrupt event to frontend
                        let interrupt_event = serde_json::json!({
                            "type": "interrupt",
                            "node_id": interrupt_info.node_id,
                            "message": interrupt_info.message,
                            "data": interrupt_info.data,
                            "thread_id": interrupt_info.thread_id,
                            "checkpoint_id": interrupt_info.checkpoint_id,
                            "timestamp": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0)
                        });
                        yield Ok(Event::default().event("interrupt").data(interrupt_event.to_string()));

                        // Don't break - the workflow is paused, waiting for user input
                        // The frontend will call the resume endpoint when ready
                    }

                    // Emit the enriched events (node_start/node_end with state_snapshot)
                    // In loop iterations, this may include a node_end for the previous
                    // iteration followed by a node_start for the new iteration.
                    for event_json in &enriched_events {
                        // Emit debug events alongside trace events with category
                        // based on node type: trigger → "trigger", action → "action", agent → "lifecycle"
                        if let Ok(ev) = serde_json::from_str::<serde_json::Value>(event_json) {
                            let ev_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            let node = ev.get("node").and_then(|v| v.as_str()).unwrap_or("");
                            let step = ev.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
                            let category = exec_ctx.debug_category_for_node(node);
                            let action_type = exec_ctx.action_type_for_node(node);
                            if ev_type == "node_start" {
                                let summary = match action_type {
                                    Some("trigger") => format!("Trigger fired: {node}, step {step}"),
                                    Some(atype) => format!("{} node started: {node}, step {step}", capitalize(atype)),
                                    None => format!("Agent started: {node}, step {step}"),
                                };
                                let mut detail = ev.clone();
                                if let Some(atype) = action_type {
                                    detail.as_object_mut().map(|m| m.insert("action_type".to_string(), serde_json::Value::String(atype.to_string())));
                                }
                                let debug_evt = DebugEvent::new("debug", category, node, summary, detail);
                                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                            } else if ev_type == "node_end" {
                                let duration = ev.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                                let summary = match action_type {
                                    Some("trigger") => format!("Trigger completed: {node}, {duration}ms"),
                                    Some(atype) => format!("{} node completed: {node}, {duration}ms", capitalize(atype)),
                                    None => format!("Agent completed: {node}, {duration}ms"),
                                };
                                let mut detail = ev.clone();
                                if let Some(atype) = action_type {
                                    detail.as_object_mut().map(|m| m.insert("action_type".to_string(), serde_json::Value::String(atype.to_string())));
                                }
                                let debug_evt = DebugEvent::new("debug", category, node, summary, detail);
                                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                            }
                        }
                        yield Ok(Event::default().event("trace").data(event_json.clone()));
                    }

                    // If this is a Done event, emit all pending node_end events
                    // with the correct output state (now that we have it)
                    if is_done {
                        for event_json in exec_ctx.emit_pending_node_ends() {
                            // Emit debug event for deferred node_end with correct category (Requirement 9.7)
                            if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&event_json) {
                                let node = ev.get("node").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let duration = ev.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                                let category = exec_ctx.debug_category_for_node(&node);
                                let action_type = exec_ctx.action_type_for_node(&node).map(|s| s.to_string());
                                let summary = match action_type.as_deref() {
                                    Some("trigger") => format!("Trigger completed: {node}, {duration}ms"),
                                    Some(atype) => format!("{} node completed: {node}, {duration}ms", capitalize(atype)),
                                    None => format!("Agent completed: {node}, {duration}ms"),
                                };
                                let mut detail = ev.clone();
                                if let Some(atype) = &action_type {
                                    detail.as_object_mut().map(|m| m.insert("action_type".to_string(), serde_json::Value::String(atype.clone())));
                                }
                                let debug_evt = DebugEvent::new("debug", category, &node, summary, detail);
                                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                            }
                            yield Ok(Event::default().event("trace").data(event_json));
                        }

                        // Emit our enriched done event (with state_snapshot)
                        yield Ok(Event::default().event("trace").data(exec_ctx.done()));
                        yield Ok(Event::default().event("end").data(""));
                        break;
                    }

                    // Pass through the raw trace for backward compatibility,
                    // unless we already emitted an enriched version (suppress_raw)
                    if !suppress_raw {
                        // Emit debug state_change events for state updates (Requirement 9.7)
                        if let Ok(ev) = serde_json::from_str::<serde_json::Value>(trace) {
                            let ev_type = ev.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if ev_type == "state" || ev_type == "updates" {
                                let node = ev.get("node").and_then(|v| v.as_str()).unwrap_or("system").to_string();
                                let keys: Vec<String> = if ev_type == "state" {
                                    ev.get("state").and_then(|v| v.as_object())
                                        .map(|m| m.keys().cloned().collect())
                                        .unwrap_or_default()
                                } else {
                                    ev.get("updates").and_then(|v| v.as_object())
                                        .map(|m| m.keys().cloned().collect())
                                        .unwrap_or_default()
                                };
                                if !keys.is_empty() {
                                    let debug_evt = DebugEvent::new(
                                        "debug", "state_change", &node,
                                        format!("State updated: {}", keys.join(", ")),
                                        ev,
                                    );
                                    yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                                }
                            }
                        }
                        yield Ok(Event::default().event("trace").data(trace));
                    }
                } else if let Some(err_msg) = line.strip_prefix("Error: ").or_else(|| line.strip_prefix("Error:")) {
                    // Binary reported an execution error (e.g. model failure, node error)
                    // Emit as error + debug event and terminate the stream
                    let err_text = err_msg.trim().to_string();
                    let debug_evt = DebugEvent::new(
                        "error", "error",
                        exec_ctx.current_agent().unwrap_or("system"),
                        &err_text,
                        serde_json::json!({ "error": &err_text }),
                    );
                    yield Ok(Event::default().event("debug").data(debug_evt.to_json()));

                    // Emit pending node_ends so the frontend sees completion
                    for event_json in exec_ctx.emit_pending_node_ends() {
                        yield Ok(Event::default().event("trace").data(event_json));
                    }

                    yield Ok(Event::default().event("error").data(&err_text));
                    yield Ok(Event::default().event("trace").data(exec_ctx.done()));
                    yield Ok(Event::default().event("end").data(""));
                    break;
                } else if let Some(chunk) = line.strip_prefix("CHUNK:") {
                    // Streaming chunk - emit immediately
                    let decoded = serde_json::from_str::<String>(chunk).unwrap_or_else(|_| chunk.to_string());
                    yield Ok(Event::default().event("chunk").data(decoded));
                } else if let Some(response) = line.strip_prefix("RESPONSE:") {
                    let decoded = serde_json::from_str::<String>(response).unwrap_or_else(|_| response.to_string());
                    // Update execution state with the response
                    exec_ctx.update_state("response", serde_json::Value::String(decoded.clone()));
                    yield Ok(Event::default().event("chunk").data(decoded.clone()));

                    // Emit debug response event (Requirement 9.3)
                    let debug_evt = DebugEvent::new(
                        "debug", "response", "system",
                        "LLM response received",
                        serde_json::json!({ "response": decoded }),
                    );
                    yield Ok(Event::default().event("debug").data(debug_evt.to_json()));

                    // Emit any remaining pending node_end events with final state
                    // This handles the case where RESPONSE comes before/without Done event
                    if exec_ctx.has_pending_agents() {
                        for event_json in exec_ctx.emit_pending_node_ends() {
                            // Emit debug event for deferred node_end with correct category (Requirement 9.7)
                            if let Ok(ev) = serde_json::from_str::<serde_json::Value>(&event_json) {
                                let node = ev.get("node").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let duration = ev.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                                let category = exec_ctx.debug_category_for_node(&node);
                                let action_type = exec_ctx.action_type_for_node(&node).map(|s| s.to_string());
                                let summary = match action_type.as_deref() {
                                    Some("trigger") => format!("Trigger completed: {node}, {duration}ms"),
                                    Some(atype) => format!("{} node completed: {node}, {duration}ms", capitalize(atype)),
                                    None => format!("Agent completed: {node}, {duration}ms"),
                                };
                                let mut detail = ev.clone();
                                if let Some(atype) = &action_type {
                                    detail.as_object_mut().map(|m| m.insert("action_type".to_string(), serde_json::Value::String(atype.clone())));
                                }
                                let debug_evt = DebugEvent::new("debug", category, &node, summary, detail);
                                yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                            }
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
                        let tool_data = serde_json::json!({"name": name, "args": args});
                        yield Ok(Event::default().event("tool_call").data(tool_data.to_string()));
                        // Emit debug tool_call event (Requirement 9.4)
                        let debug_evt = DebugEvent::new(
                            "debug", "tool_call", name,
                            format!("Tool call: {name}"),
                            tool_data,
                        );
                        yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                    } else if msg == "tool_result" {
                        let name = fields.and_then(|f| f.get("tool.name")).and_then(|v| v.as_str()).unwrap_or("");
                        let result = fields.and_then(|f| f.get("tool.result")).and_then(|v| v.as_str()).unwrap_or("");
                        // Update execution state with tool result (v2.0)
                        exec_ctx.update_state(&format!("tool_{name}"), serde_json::Value::String(result.to_string()));
                        let result_data = serde_json::json!({"name": name, "result": result});
                        yield Ok(Event::default().event("tool_result").data(result_data.to_string()));
                        // Emit debug tool_result event (Requirement 9.5)
                        let debug_evt = DebugEvent::new(
                            "debug", "tool_result", name,
                            format!("Tool result: {name}"),
                            result_data,
                        );
                        yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                    } else if msg == "Starting agent execution" {
                        // Don't emit node_start here — the TRACE node_start event
                        // from stdout will handle it in the correct execution order.
                        // Emitting from stderr causes a race condition where the
                        // chat_agent gets tracked before action nodes that execute
                        // before it (e.g., set, transform).
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
                        yield Ok(Event::default().event("log").data(serde_json::json!({"message": format!("Calling {model} (tools: {tools})")}).to_string()));
                        // Emit debug request event for model interaction (Requirement 9.2)
                        let debug_evt = DebugEvent::new(
                            "debug", "request", model,
                            format!("LLM request to {model} (tools: {tools})"),
                            serde_json::json!({ "model": model, "tools_count": tools }),
                        );
                        yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                    } else {
                        // Capture all other structured log messages as debug entries.
                        // This catches ERROR/WARN level logs from the runtime (model
                        // failures, connection errors, etc.) that were previously dropped.
                        let level_str = json.get("level").and_then(|v| v.as_str()).unwrap_or("DEBUG");
                        let target = json.get("target").and_then(|v| v.as_str()).unwrap_or("");
                        let error_field = fields.and_then(|f| f.get("error")).and_then(|v| v.as_str()).unwrap_or("");
                        let span_info = json.get("span").cloned().unwrap_or(serde_json::Value::Null);
                        let agent_name = span_info.as_object()
                            .and_then(|s| s.get("agent.name"))
                            .and_then(|v| v.as_str())
                            .or_else(|| exec_ctx.current_agent())
                            .unwrap_or("system");

                        let (debug_level, debug_category) = match level_str {
                            "ERROR" => ("error", "error"),
                            "WARN" => ("warn", "error"),
                            _ => ("debug", "lifecycle"),
                        };

                        // Build a meaningful summary from the message and error fields
                        let summary = if !error_field.is_empty() {
                            if msg.is_empty() {
                                format!("{target}: {error_field}")
                            } else {
                                format!("{msg}: {error_field}")
                            }
                        } else if !msg.is_empty() {
                            msg.to_string()
                        } else {
                            format!("{target} log")
                        };

                        // Only emit ERROR and WARN level, skip noisy DEBUG/INFO/TRACE
                        if debug_level == "error" || debug_level == "warn" {
                            let debug_evt = DebugEvent::new(
                                debug_level, debug_category, agent_name,
                                &summary,
                                json.clone(),
                            );
                            yield Ok(Event::default().event("debug").data(debug_evt.to_json()));
                        }
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
/// Capitalize the first character of a string.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
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

// ============================================
// Task 10: Resume Workflow Functions
// ============================================
// These functions support resuming interrupted workflows by sending
// the user's response to the subprocess via stdin.
// Requirements: 3.2, 5.2

/// Send a resume response to an interrupted workflow session.
///
/// This function sends the user's response to the subprocess stdin,
/// which triggers the workflow to resume from its checkpoint.
///
/// ## Arguments
/// * `session_id` - The session ID of the interrupted workflow
/// * `response` - The user's response as a JSON value
///
/// ## Returns
/// * `Ok(())` if the response was sent successfully
/// * `Err(String)` if the session was not found or the send failed
///
/// ## Requirements
/// - Requirement 3.2: After user response, workflow resumes
/// - Requirement 5.2: State persistence - workflow resumes from checkpoint
pub async fn send_resume_response(
    session_id: &str,
    response: serde_json::Value,
) -> Result<(), String> {
    let mut sessions = SESSIONS.lock().await;

    let session = sessions
        .get_mut(session_id)
        .ok_or_else(|| format!("Session '{}' not found", session_id))?;

    // Format the response as a JSON string to send to the subprocess
    let response_str = serde_json::to_string(&response)
        .map_err(|e| format!("Failed to serialize response: {}", e))?;

    // Send the response to the subprocess stdin
    session
        .stdin
        .write_all(format!("{}\n", response_str).as_bytes())
        .await
        .map_err(|e| format!("Failed to write to stdin: {}", e))?;

    session.stdin.flush().await.map_err(|e| format!("Failed to flush stdin: {}", e))?;

    tracing::info!(
        session_id = %session_id,
        response = %response_str,
        "Sent resume response to subprocess"
    );

    Ok(())
}

/// Check if a session exists and is active.
pub async fn session_exists(session_id: &str) -> bool {
    let sessions = SESSIONS.lock().await;
    sessions.contains_key(session_id)
}

/// Get the list of active session IDs.
pub async fn list_active_sessions() -> Vec<String> {
    let sessions = SESSIONS.lock().await;
    sessions.keys().cloned().collect()
}

// ============================================
// Webhook Notification SSE Endpoint
// ============================================
// SSE endpoint for UI clients to receive webhook notifications.

/// SSE endpoint for webhook notifications.
///
/// The UI subscribes to this endpoint to receive real-time notifications
/// when webhooks are triggered. This allows the UI to automatically
/// start streaming the workflow execution.
///
/// ## Endpoint
/// `GET /api/projects/{id}/webhook-events`
///
/// ## Events
/// - `webhook`: A webhook was received, contains session_id, path, payload, binary_path
pub async fn webhook_events_handler(
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        // Subscribe to webhook notifications for this project
        let mut receiver = crate::server::handlers::subscribe_webhook_notifications(&id).await;

        // Send initial connection event
        yield Ok(Event::default().event("connected").data(format!("{{\"project_id\":\"{}\"}}", id)));

        // Listen for webhook notifications
        loop {
            match receiver.recv().await {
                Ok(notification) => {
                    // Serialize the notification to JSON
                    if let Ok(json) = serde_json::to_string(&notification) {
                        yield Ok(Event::default().event("webhook").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Some messages were dropped due to slow consumer
                    tracing::warn!(project_id = %id, dropped = n, "Webhook notification consumer lagged");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    // Channel closed, end the stream
                    break;
                }
            }
        }
    };

    Sse::new(stream)
}
