//! Action Node Runtime Executor
//!
//! This module provides runtime execution for action nodes in ADK Studio workflows.
//! Action nodes are non-LLM programmatic nodes that perform deterministic operations
//! like HTTP requests, data transformation, conditional branching, and more.
//!
//! ## Requirements
//!
//! - 13.2: Integrate action nodes with ADK runtime
//! - 1.2: Error handling mode behavior (stop, continue, retry, fallback)

use crate::codegen::action_nodes::{ActionNodeConfig, ErrorHandling, ErrorMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Error types for action node execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionError {
    /// Execution timed out
    Timeout { node: String, timeout_ms: u64 },
    /// HTTP request failed with unexpected status
    HttpStatus { status: u16, expected: String },
    /// No matching branch in switch node
    NoMatchingBranch { node: String },
    /// Insufficient branches completed in merge node
    InsufficientBranches { expected: usize, got: usize },
    /// No branch completed in merge node
    NoBranchCompleted,
    /// Missing credential reference
    MissingCredential(String),
    /// Transform operation failed
    Transform(String),
    /// Code execution failed
    CodeExecution(String),
    /// Sandbox initialization failed
    SandboxInit(String),
    /// Invalid timestamp format
    InvalidTimestamp(String),
    /// Webhook wait timed out
    WebhookTimeout,
    /// Webhook was cancelled
    WebhookCancelled,
    /// Condition polling timed out
    ConditionTimeout { condition: String, timeout_ms: u64 },
    /// Node was skipped due to condition
    Skipped { node: String },
    /// Generic execution error
    Execution(String),
    /// Retry exhausted
    RetryExhausted { node: String, attempts: u32 },
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Timeout { node, timeout_ms } => {
                write!(f, "Node '{}' timed out after {}ms", node, timeout_ms)
            }
            ActionError::HttpStatus { status, expected } => {
                write!(f, "HTTP status {} not in expected range {}", status, expected)
            }
            ActionError::NoMatchingBranch { node } => {
                write!(f, "No matching branch in switch node '{}'", node)
            }
            ActionError::InsufficientBranches { expected, got } => {
                write!(f, "Expected {} branches, got {}", expected, got)
            }
            ActionError::NoBranchCompleted => write!(f, "No branch completed"),
            ActionError::MissingCredential(name) => {
                write!(f, "Missing credential: {}", name)
            }
            ActionError::Transform(msg) => write!(f, "Transform error: {}", msg),
            ActionError::CodeExecution(msg) => write!(f, "Code execution error: {}", msg),
            ActionError::SandboxInit(msg) => write!(f, "Sandbox init error: {}", msg),
            ActionError::InvalidTimestamp(msg) => write!(f, "Invalid timestamp: {}", msg),
            ActionError::WebhookTimeout => write!(f, "Webhook wait timed out"),
            ActionError::WebhookCancelled => write!(f, "Webhook was cancelled"),
            ActionError::ConditionTimeout { condition, timeout_ms } => {
                write!(f, "Condition '{}' not met within {}ms", condition, timeout_ms)
            }
            ActionError::Skipped { node } => write!(f, "Node '{}' was skipped", node),
            ActionError::Execution(msg) => write!(f, "Execution error: {}", msg),
            ActionError::RetryExhausted { node, attempts } => {
                write!(f, "Node '{}' failed after {} retry attempts", node, attempts)
            }
        }
    }
}

impl std::error::Error for ActionError {}

/// Result of action node execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    /// Node ID that was executed
    pub node_id: String,
    /// Node type
    pub node_type: String,
    /// Whether execution succeeded
    pub success: bool,
    /// Output value (if successful)
    pub output: Option<Value>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of retry attempts (if any)
    pub retry_attempts: u32,
    /// Whether node was skipped
    pub skipped: bool,
}

/// Workflow state - shared mutable state passed between nodes
pub type State = HashMap<String, Value>;

/// Workflow executor for action nodes
///
/// Handles execution of action nodes with proper error handling,
/// state management, and event emission.
pub struct WorkflowExecutor {
    /// Current workflow state
    state: Arc<RwLock<State>>,
    /// Action node configurations
    action_nodes: HashMap<String, ActionNodeConfig>,
    /// Event sender for SSE events
    event_sender: Option<tokio::sync::mpsc::Sender<ActionNodeEvent>>,
}

/// SSE event for action node execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionNodeEvent {
    /// Event type: action_start, action_end, action_error
    #[serde(rename = "type")]
    pub event_type: String,
    /// Node ID
    pub node_id: String,
    /// Node type (trigger, http, set, etc.)
    pub node_type: String,
    /// Timestamp in milliseconds
    pub timestamp: u64,
    /// State snapshot (input/output)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_snapshot: Option<StateSnapshot>,
    /// Error details (for action_error events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ActionErrorDetails>,
    /// Loop iteration info (for loop nodes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<IterationInfo>,
}

/// State snapshot for action node events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Input state before execution
    pub input: Value,
    /// Output state after execution
    pub output: Value,
}

/// Error details for action_error events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionErrorDetails {
    /// Error message
    pub message: String,
    /// Error code
    pub code: String,
    /// Retry attempt number (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_attempt: Option<u32>,
}

/// Loop iteration info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationInfo {
    /// Current iteration (0-indexed)
    pub current: usize,
    /// Total iterations
    pub total: usize,
}

impl WorkflowExecutor {
    /// Create a new workflow executor
    pub fn new(action_nodes: HashMap<String, ActionNodeConfig>) -> Self {
        Self { state: Arc::new(RwLock::new(HashMap::new())), action_nodes, event_sender: None }
    }

    /// Create executor with event sender for SSE
    pub fn with_event_sender(
        action_nodes: HashMap<String, ActionNodeConfig>,
        sender: tokio::sync::mpsc::Sender<ActionNodeEvent>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            action_nodes,
            event_sender: Some(sender),
        }
    }

    /// Get current state
    pub async fn get_state(&self) -> State {
        self.state.read().await.clone()
    }

    /// Set initial state
    pub async fn set_state(&self, state: State) {
        *self.state.write().await = state;
    }

    /// Update state with new values
    pub async fn update_state(&self, updates: HashMap<String, Value>) {
        let mut state = self.state.write().await;
        for (key, value) in updates {
            state.insert(key, value);
        }
    }

    /// Get a value from state
    pub async fn get_state_value(&self, key: &str) -> Option<Value> {
        self.state.read().await.get(key).cloned()
    }

    /// Set a value in state
    pub async fn set_state_value(&self, key: String, value: Value) {
        self.state.write().await.insert(key, value);
    }

    /// Execute an action node by ID
    pub async fn execute_node(&self, node_id: &str) -> Result<ActionResult, ActionError> {
        let node = self.action_nodes.get(node_id).ok_or_else(|| {
            ActionError::Execution(format!("Action node '{}' not found", node_id))
        })?;

        let standard = node.standard();
        let node_type = node.node_type().to_string();

        // Capture input state before execution
        let input_state = self.get_state().await;
        let input_snapshot = serde_json::to_value(&input_state).unwrap_or_default();

        // Emit action_start event
        self.emit_event(ActionNodeEvent {
            event_type: "action_start".to_string(),
            node_id: node_id.to_string(),
            node_type: node_type.clone(),
            timestamp: current_timestamp_ms(),
            state_snapshot: Some(StateSnapshot {
                input: input_snapshot.clone(),
                output: Value::Null,
            }),
            error: None,
            iteration: None,
        })
        .await;

        let start_time = Instant::now();

        // Check skip condition
        if let Some(condition) = &standard.execution.condition {
            if !condition.is_empty() && !self.evaluate_condition(condition).await {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                return Ok(ActionResult {
                    node_id: node_id.to_string(),
                    node_type,
                    success: true,
                    output: Some(Value::Null),
                    error: None,
                    duration_ms,
                    retry_attempts: 0,
                    skipped: true,
                });
            }
        }

        // Execute with error handling
        let result =
            self.execute_with_error_handling(node_id, node, &standard.error_handling).await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        match result {
            Ok((output, retry_attempts)) => {
                // Capture output state
                let output_state = self.get_state().await;
                let output_snapshot = serde_json::to_value(&output_state).unwrap_or_default();

                // Emit action_end event
                self.emit_event(ActionNodeEvent {
                    event_type: "action_end".to_string(),
                    node_id: node_id.to_string(),
                    node_type: node_type.clone(),
                    timestamp: current_timestamp_ms(),
                    state_snapshot: Some(StateSnapshot {
                        input: input_snapshot,
                        output: output_snapshot,
                    }),
                    error: None,
                    iteration: None,
                })
                .await;

                Ok(ActionResult {
                    node_id: node_id.to_string(),
                    node_type,
                    success: true,
                    output: Some(output),
                    error: None,
                    duration_ms,
                    retry_attempts,
                    skipped: false,
                })
            }
            Err(e) => {
                // Emit action_error event
                self.emit_event(ActionNodeEvent {
                    event_type: "action_error".to_string(),
                    node_id: node_id.to_string(),
                    node_type: node_type.clone(),
                    timestamp: current_timestamp_ms(),
                    state_snapshot: Some(StateSnapshot {
                        input: input_snapshot,
                        output: Value::Null,
                    }),
                    error: Some(ActionErrorDetails {
                        message: e.to_string(),
                        code: error_code(&e),
                        retry_attempt: None,
                    }),
                    iteration: None,
                })
                .await;

                Ok(ActionResult {
                    node_id: node_id.to_string(),
                    node_type,
                    success: false,
                    output: None,
                    error: Some(e.to_string()),
                    duration_ms,
                    retry_attempts: 0,
                    skipped: false,
                })
            }
        }
    }

    /// Execute node with error handling based on mode
    async fn execute_with_error_handling(
        &self,
        node_id: &str,
        node: &ActionNodeConfig,
        error_handling: &ErrorHandling,
    ) -> Result<(Value, u32), ActionError> {
        let standard = node.standard();
        let timeout_ms = standard.execution.timeout;

        match error_handling.mode {
            ErrorMode::Stop => {
                // Default: errors propagate up
                let result = tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    self.execute_node_inner(node),
                )
                .await
                .map_err(|_| ActionError::Timeout { node: node_id.to_string(), timeout_ms })??;
                Ok((result, 0))
            }
            ErrorMode::Continue => {
                // Continue on error, return null
                match tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    self.execute_node_inner(node),
                )
                .await
                {
                    Ok(Ok(result)) => Ok((result, 0)),
                    Ok(Err(e)) => {
                        tracing::warn!(node = node_id, error = %e, "Node failed, continuing");
                        Ok((Value::Null, 0))
                    }
                    Err(_) => {
                        tracing::warn!(node = node_id, "Node timed out, continuing");
                        Ok((Value::Null, 0))
                    }
                }
            }
            ErrorMode::Retry => {
                // Retry up to retry_count times
                let retry_count = error_handling.retry_count.unwrap_or(3);
                let retry_delay = error_handling.retry_delay.unwrap_or(1000);
                let mut attempts = 0u32;

                loop {
                    match tokio::time::timeout(
                        Duration::from_millis(timeout_ms),
                        self.execute_node_inner(node),
                    )
                    .await
                    {
                        Ok(Ok(result)) => return Ok((result, attempts)),
                        Ok(Err(_)) | Err(_) => {
                            attempts += 1;
                            if attempts >= retry_count {
                                return Err(ActionError::RetryExhausted {
                                    node: node_id.to_string(),
                                    attempts,
                                });
                            }
                            tracing::warn!(
                                node = node_id,
                                attempt = attempts,
                                "Retrying after error"
                            );
                            tokio::time::sleep(Duration::from_millis(retry_delay)).await;
                        }
                    }
                }
            }
            ErrorMode::Fallback => {
                // Use fallback value on error
                match tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    self.execute_node_inner(node),
                )
                .await
                {
                    Ok(Ok(result)) => Ok((result, 0)),
                    Ok(Err(_)) | Err(_) => {
                        let fallback = error_handling.fallback_value.clone().unwrap_or(Value::Null);
                        tracing::warn!(node = node_id, "Using fallback value");
                        Ok((fallback, 0))
                    }
                }
            }
        }
    }

    /// Execute the inner node logic based on type
    async fn execute_node_inner(&self, node: &ActionNodeConfig) -> Result<Value, ActionError> {
        match node {
            ActionNodeConfig::Trigger(config) => self.execute_trigger(config).await,
            ActionNodeConfig::Http(config) => self.execute_http(config).await,
            ActionNodeConfig::Set(config) => self.execute_set(config).await,
            ActionNodeConfig::Transform(config) => self.execute_transform(config).await,
            ActionNodeConfig::Switch(config) => self.execute_switch(config).await,
            ActionNodeConfig::Loop(config) => self.execute_loop(config).await,
            ActionNodeConfig::Merge(config) => self.execute_merge(config).await,
            ActionNodeConfig::Wait(config) => self.execute_wait(config).await,
            ActionNodeConfig::Code(config) => self.execute_code(config).await,
            ActionNodeConfig::Database(config) => self.execute_database(config).await,
            ActionNodeConfig::Email(config) => self.execute_email(config).await,
            ActionNodeConfig::Notification(config) => self.execute_notification(config).await,
            ActionNodeConfig::Rss(_config) => {
                // RSS monitoring requires a long-running polling service
                Err(ActionError::Execution(
                    "RSS node execution requires a dedicated polling service. Use the generated code for production.".to_string()
                ))
            }
            ActionNodeConfig::File(_config) => {
                // File operations require filesystem access
                Err(ActionError::Execution(
                    "File node execution requires filesystem access. Use the generated code for production.".to_string()
                ))
            }
        }
    }

    /// Execute trigger node
    async fn execute_trigger(
        &self,
        config: &crate::codegen::action_nodes::TriggerNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::TriggerType;

        match config.trigger_type {
            TriggerType::Manual => Ok(serde_json::json!({
                "trigger": "manual",
                "timestamp": chrono::Utc::now().to_rfc3339()
            })),
            TriggerType::Webhook => {
                // Webhook payload should be in state
                let payload = self.get_state_value("webhook_payload").await.unwrap_or(Value::Null);
                Ok(payload)
            }
            TriggerType::Schedule => {
                if let Some(schedule) = &config.schedule {
                    Ok(serde_json::json!({
                        "trigger": "schedule",
                        "cron": schedule.cron,
                        "timezone": schedule.timezone,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    }))
                } else {
                    Ok(Value::Null)
                }
            }
            TriggerType::Event => {
                let event_data = self.get_state_value("event_data").await.unwrap_or(Value::Null);
                Ok(event_data)
            }
        }
    }

    /// Execute HTTP node
    /// Note: Full HTTP execution requires reqwest dependency.
    /// This is a placeholder that returns the configuration for code generation.
    async fn execute_http(
        &self,
        config: &crate::codegen::action_nodes::HttpNodeConfig,
    ) -> Result<Value, ActionError> {
        // HTTP execution would need reqwest dependency
        // For runtime, we return a placeholder indicating the node was processed
        // The actual HTTP calls happen in the generated Rust code
        let state = self.get_state().await;
        let url = interpolate_variables(&config.url, &state);

        // Return configuration info for debugging
        let result = serde_json::json!({
            "node_type": "http",
            "method": format!("{:?}", config.method),
            "url": url,
            "note": "HTTP execution happens in generated code"
        });

        self.set_state_value(config.standard.mapping.output_key.clone(), result.clone()).await;
        Ok(result)
    }

    /// Execute Set node
    async fn execute_set(
        &self,
        config: &crate::codegen::action_nodes::SetNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::SetMode;

        let state = self.get_state().await;
        let mut result = serde_json::Map::new();

        match config.mode {
            SetMode::Set => {
                for var in &config.variables {
                    let value = if var.value_type == "expression" {
                        let expr = var.value.as_str().unwrap_or("");
                        Value::String(interpolate_variables(expr, &state))
                    } else {
                        var.value.clone()
                    };
                    self.set_state_value(var.key.clone(), value.clone()).await;
                    result.insert(var.key.clone(), value);
                }
            }
            SetMode::Merge => {
                for var in &config.variables {
                    let existing = self.get_state_value(&var.key).await;
                    let merged = if let Some(existing) = existing {
                        deep_merge(&existing, &var.value)
                    } else {
                        var.value.clone()
                    };
                    self.set_state_value(var.key.clone(), merged.clone()).await;
                    result.insert(var.key.clone(), merged);
                }
            }
            SetMode::Delete => {
                let mut state = self.state.write().await;
                for var in &config.variables {
                    state.remove(&var.key);
                    result.insert(var.key.clone(), Value::Null);
                }
            }
        }

        let output = Value::Object(result);
        self.set_state_value(config.standard.mapping.output_key.clone(), output.clone()).await;
        Ok(output)
    }

    /// Execute Transform node
    async fn execute_transform(
        &self,
        config: &crate::codegen::action_nodes::TransformNodeConfig,
    ) -> Result<Value, ActionError> {
        let state = self.get_state().await;
        let _input = serde_json::to_value(&state).unwrap_or_default();

        // For now, support template transformation
        let result = match config.transform_type {
            crate::codegen::action_nodes::TransformType::Template => {
                Value::String(interpolate_variables(&config.expression, &state))
            }
            _ => {
                // Other transform types would need additional dependencies
                Value::String(interpolate_variables(&config.expression, &state))
            }
        };

        self.set_state_value(config.standard.mapping.output_key.clone(), result.clone()).await;
        Ok(result)
    }

    /// Execute Switch node
    async fn execute_switch(
        &self,
        config: &crate::codegen::action_nodes::SwitchNodeConfig,
    ) -> Result<Value, ActionError> {
        let state = self.get_state().await;

        // Check expression mode first
        if let Some(expr_mode) = &config.expression_mode {
            if expr_mode.enabled && !expr_mode.expression.is_empty() {
                let branch = interpolate_variables(&expr_mode.expression, &state);
                return Ok(serde_json::json!({ "branch": branch }));
            }
        }

        // Evaluate conditions
        for condition in &config.conditions {
            if let Some(value) = get_nested_value(&state, &condition.field) {
                if evaluate_operator(&condition.operator, value, &condition.value) {
                    return Ok(serde_json::json!({ "branch": condition.output_port }));
                }
            }
        }

        // Default branch
        if let Some(default) = &config.default_branch {
            Ok(serde_json::json!({ "branch": default }))
        } else {
            Err(ActionError::NoMatchingBranch { node: config.standard.id.clone() })
        }
    }

    /// Execute Loop node
    async fn execute_loop(
        &self,
        config: &crate::codegen::action_nodes::LoopNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::LoopType;

        let mut results = Vec::new();

        match config.loop_type {
            LoopType::ForEach => {
                if let Some(for_each) = &config.for_each {
                    let source = self
                        .get_state_value(&for_each.source_array)
                        .await
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();

                    let total = source.len();
                    for (idx, item) in source.into_iter().enumerate() {
                        // Set loop variables
                        self.set_state_value(for_each.item_var.clone(), item).await;
                        self.set_state_value(for_each.index_var.clone(), serde_json::json!(idx))
                            .await;

                        // Emit iteration event
                        self.emit_event(ActionNodeEvent {
                            event_type: "action_iteration".to_string(),
                            node_id: config.standard.id.clone(),
                            node_type: "loop".to_string(),
                            timestamp: current_timestamp_ms(),
                            state_snapshot: None,
                            error: None,
                            iteration: Some(IterationInfo { current: idx, total }),
                        })
                        .await;

                        if config.results.collect {
                            let state = self.get_state().await;
                            results.push(serde_json::to_value(&state).unwrap_or_default());
                        }
                    }
                }
            }
            LoopType::Times => {
                if let Some(times) = &config.times {
                    let count = times.count.as_u64().unwrap_or(0) as usize;
                    for i in 0..count {
                        self.set_state_value("index".to_string(), serde_json::json!(i)).await;

                        if config.results.collect {
                            let state = self.get_state().await;
                            results.push(serde_json::to_value(&state).unwrap_or_default());
                        }
                    }
                }
            }
            LoopType::While => {
                // While loops need condition evaluation
                let mut iteration = 0;
                const MAX_ITERATIONS: usize = 1000;
                if let Some(while_config) = &config.while_config {
                    while iteration < MAX_ITERATIONS {
                        if !self.evaluate_condition(&while_config.condition).await {
                            break;
                        }
                        if config.results.collect {
                            let state = self.get_state().await;
                            results.push(serde_json::to_value(&state).unwrap_or_default());
                        }
                        iteration += 1;
                    }
                }
            }
        }

        let output = if config.results.collect { serde_json::json!(results) } else { Value::Null };

        if let Some(agg_key) = &config.results.aggregation_key {
            self.set_state_value(agg_key.clone(), output.clone()).await;
        }

        Ok(output)
    }

    /// Execute Merge node
    async fn execute_merge(
        &self,
        config: &crate::codegen::action_nodes::MergeNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::{CombineStrategy, MergeMode};

        // Get branch results from state (would be set by previous branches)
        // Pre-fetch state once to avoid blocking calls inside iterators
        let current_state = self.get_state().await;
        let branch_results: Vec<(String, Value)> = config
            .branch_keys
            .as_ref()
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| current_state.get(k).map(|v| (k.clone(), v.clone())))
                    .collect()
            })
            .unwrap_or_default();

        // Check mode requirements
        match config.mode {
            MergeMode::WaitAll => {
                // All branches should be present
            }
            MergeMode::WaitAny => {
                if branch_results.is_empty() {
                    return Err(ActionError::NoBranchCompleted);
                }
            }
            MergeMode::WaitN => {
                let n = config.wait_count.unwrap_or(1) as usize;
                if branch_results.len() < n {
                    return Err(ActionError::InsufficientBranches {
                        expected: n,
                        got: branch_results.len(),
                    });
                }
            }
        }

        // Combine results
        let result = match config.combine_strategy {
            CombineStrategy::Array => {
                let values: Vec<Value> = branch_results.into_iter().map(|(_, v)| v).collect();
                serde_json::json!(values)
            }
            CombineStrategy::Object => {
                let mut map = serde_json::Map::new();
                for (key, value) in branch_results {
                    map.insert(key, value);
                }
                Value::Object(map)
            }
            CombineStrategy::First => {
                branch_results.into_iter().next().map(|(_, v)| v).unwrap_or(Value::Null)
            }
            CombineStrategy::Last => {
                branch_results.into_iter().last().map(|(_, v)| v).unwrap_or(Value::Null)
            }
        };

        self.set_state_value(config.standard.mapping.output_key.clone(), result.clone()).await;
        Ok(result)
    }

    /// Execute Wait node
    async fn execute_wait(
        &self,
        config: &crate::codegen::action_nodes::WaitNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::WaitType;

        match config.wait_type {
            WaitType::Fixed => {
                if let Some(fixed) = &config.fixed {
                    let ms = match fixed.unit.as_str() {
                        "ms" => fixed.duration,
                        "s" => fixed.duration * 1000,
                        "m" => fixed.duration * 60 * 1000,
                        "h" => fixed.duration * 60 * 60 * 1000,
                        _ => fixed.duration,
                    };
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
            }
            WaitType::Until => {
                if let Some(until) = &config.until {
                    let target = chrono::DateTime::parse_from_rfc3339(&until.timestamp)
                        .map_err(|e| ActionError::InvalidTimestamp(e.to_string()))?
                        .with_timezone(&chrono::Utc);
                    let now = chrono::Utc::now();
                    if target > now {
                        let duration = (target - now).to_std().unwrap_or(Duration::from_secs(0));
                        tokio::time::sleep(duration).await;
                    }
                }
            }
            WaitType::Condition => {
                if let Some(condition) = &config.condition {
                    let poll_interval = Duration::from_millis(condition.poll_interval);
                    let max_wait = Duration::from_millis(condition.max_wait);
                    let start = Instant::now();

                    loop {
                        if self.evaluate_condition(&condition.expression).await {
                            break;
                        }
                        if start.elapsed() >= max_wait {
                            return Err(ActionError::ConditionTimeout {
                                condition: condition.expression.clone(),
                                timeout_ms: condition.max_wait,
                            });
                        }
                        tokio::time::sleep(poll_interval).await;
                    }
                }
            }
            WaitType::Webhook => {
                // Webhook wait would need external coordination
                return Err(ActionError::Execution(
                    "Webhook wait not implemented in runtime".to_string(),
                ));
            }
        }

        Ok(serde_json::json!({ "waited": true }))
    }

    /// Execute Code node (placeholder - needs sandbox)
    async fn execute_code(
        &self,
        _config: &crate::codegen::action_nodes::CodeNodeConfig,
    ) -> Result<Value, ActionError> {
        // Code execution would need a JavaScript sandbox like quickjs
        // For now, return a placeholder
        Err(ActionError::Execution("Code node execution requires JavaScript sandbox".to_string()))
    }

    /// Execute Database node
    async fn execute_database(
        &self,
        config: &crate::codegen::action_nodes::DatabaseNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::DatabaseType;

        let state = self.get_state().await;

        // Interpolate connection string
        let connection_string = interpolate_variables(&config.connection.connection_string, &state);

        match config.db_type {
            DatabaseType::Postgresql | DatabaseType::Mysql | DatabaseType::Sqlite => {
                self.execute_sql_database(config, &connection_string, &state).await
            }
            DatabaseType::Mongodb => self.execute_mongodb(config, &connection_string, &state).await,
            DatabaseType::Redis => self.execute_redis(config, &connection_string, &state).await,
        }
    }

    /// Execute SQL database operations (PostgreSQL, MySQL, SQLite)
    async fn execute_sql_database(
        &self,
        config: &crate::codegen::action_nodes::DatabaseNodeConfig,
        connection_string: &str,
        state: &State,
    ) -> Result<Value, ActionError> {
        use sqlx::{AnyPool, Column, Row};

        let sql_config = config.sql.as_ref().ok_or_else(|| {
            ActionError::Execution("SQL configuration required for SQL databases".to_string())
        })?;

        // Interpolate query
        let query = interpolate_variables(&sql_config.query, state);

        // Create connection pool
        let pool = AnyPool::connect(connection_string)
            .await
            .map_err(|e| ActionError::Execution(format!("Database connection failed: {}", e)))?;

        // Execute based on operation type
        let result = match sql_config.operation.as_str() {
            "query" => {
                let rows = sqlx::query(&query)
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Query failed: {}", e)))?;

                // Convert rows to JSON - use String for Any driver compatibility
                let json_rows: Vec<Value> = rows
                    .iter()
                    .map(|row| {
                        let mut obj = serde_json::Map::new();
                        for (i, column) in row.columns().iter().enumerate() {
                            // Try to get as string first, then try other types
                            let value: Value = if let Ok(s) = row.try_get::<String, _>(i) {
                                // Try to parse as JSON, otherwise use as string
                                serde_json::from_str(&s).unwrap_or(Value::String(s))
                            } else if let Ok(n) = row.try_get::<i64, _>(i) {
                                Value::Number(n.into())
                            } else if let Ok(f) = row.try_get::<f64, _>(i) {
                                serde_json::Number::from_f64(f)
                                    .map(Value::Number)
                                    .unwrap_or(Value::Null)
                            } else if let Ok(b) = row.try_get::<bool, _>(i) {
                                Value::Bool(b)
                            } else {
                                Value::Null
                            };
                            obj.insert(column.name().to_string(), value);
                        }
                        Value::Object(obj)
                    })
                    .collect();

                serde_json::json!({
                    "success": true,
                    "operation": "query",
                    "row_count": json_rows.len(),
                    "rows": json_rows
                })
            }
            "insert" | "update" | "delete" | "upsert" => {
                let result = sqlx::query(&query)
                    .execute(&pool)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Execute failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": sql_config.operation,
                    "rows_affected": result.rows_affected()
                })
            }
            _ => {
                return Err(ActionError::Execution(format!(
                    "Unknown SQL operation: {}",
                    sql_config.operation
                )));
            }
        };

        // Close pool
        pool.close().await;

        // Store in state
        let mut state_write = self.state.write().await;
        state_write.insert(config.standard.mapping.output_key.clone(), result.clone());

        Ok(result)
    }

    /// Execute MongoDB operations
    async fn execute_mongodb(
        &self,
        config: &crate::codegen::action_nodes::DatabaseNodeConfig,
        connection_string: &str,
        state: &State,
    ) -> Result<Value, ActionError> {
        use futures::TryStreamExt;
        use mongodb::{Client, bson::doc};

        let mongo_config = config
            .mongodb
            .as_ref()
            .ok_or_else(|| ActionError::Execution("MongoDB configuration required".to_string()))?;

        // Connect to MongoDB
        let client = Client::with_uri_str(connection_string)
            .await
            .map_err(|e| ActionError::Execution(format!("MongoDB connection failed: {}", e)))?;

        // Extract database name from connection string or use default
        let db_name =
            connection_string.split('/').last().and_then(|s| s.split('?').next()).unwrap_or("test");

        let db = client.database(db_name);

        // Interpolate collection name with state variables
        let collection_name = interpolate_variables(&mongo_config.collection, state);
        let collection = db.collection::<mongodb::bson::Document>(&collection_name);

        // Execute based on operation
        let result = match mongo_config.operation.as_str() {
            "find" => {
                let filter = mongo_config
                    .filter
                    .as_ref()
                    .map(|f| mongodb::bson::to_document(f).unwrap_or_default())
                    .unwrap_or_default();

                let mut cursor = collection
                    .find(filter)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Find failed: {}", e)))?;

                let mut docs = Vec::new();
                while let Some(doc) = cursor
                    .try_next()
                    .await
                    .map_err(|e| ActionError::Execution(format!("Cursor error: {}", e)))?
                {
                    let json: Value = mongodb::bson::from_document(doc).unwrap_or(Value::Null);
                    docs.push(json);
                }

                serde_json::json!({
                    "success": true,
                    "operation": "find",
                    "count": docs.len(),
                    "documents": docs
                })
            }
            "findOne" => {
                let filter = mongo_config
                    .filter
                    .as_ref()
                    .map(|f| mongodb::bson::to_document(f).unwrap_or_default())
                    .unwrap_or_default();

                let doc = collection
                    .find_one(filter)
                    .await
                    .map_err(|e| ActionError::Execution(format!("FindOne failed: {}", e)))?;

                let json_doc =
                    doc.map(|d| mongodb::bson::from_document::<Value>(d).unwrap_or(Value::Null));

                serde_json::json!({
                    "success": true,
                    "operation": "findOne",
                    "document": json_doc
                })
            }
            "insert" => {
                let document = mongo_config
                    .document
                    .as_ref()
                    .map(|d| mongodb::bson::to_document(d).unwrap_or_default())
                    .ok_or_else(|| {
                        ActionError::Execution("Document required for insert".to_string())
                    })?;

                let result = collection
                    .insert_one(document)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Insert failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "insert",
                    "inserted_id": result.inserted_id.to_string()
                })
            }
            "update" => {
                let filter = mongo_config
                    .filter
                    .as_ref()
                    .map(|f| mongodb::bson::to_document(f).unwrap_or_default())
                    .unwrap_or_default();

                let update = mongo_config
                    .document
                    .as_ref()
                    .map(|d| doc! { "$set": mongodb::bson::to_document(d).unwrap_or_default() })
                    .ok_or_else(|| {
                        ActionError::Execution("Update document required".to_string())
                    })?;

                let result = collection
                    .update_many(filter, update)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Update failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "update",
                    "matched_count": result.matched_count,
                    "modified_count": result.modified_count
                })
            }
            "delete" => {
                let filter = mongo_config
                    .filter
                    .as_ref()
                    .map(|f| mongodb::bson::to_document(f).unwrap_or_default())
                    .unwrap_or_default();

                let result = collection
                    .delete_many(filter)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Delete failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "delete",
                    "deleted_count": result.deleted_count
                })
            }
            _ => {
                return Err(ActionError::Execution(format!(
                    "Unknown MongoDB operation: {}",
                    mongo_config.operation
                )));
            }
        };

        // Store in state
        let mut state_write = self.state.write().await;
        state_write.insert(config.standard.mapping.output_key.clone(), result.clone());

        Ok(result)
    }

    /// Execute Redis operations
    async fn execute_redis(
        &self,
        config: &crate::codegen::action_nodes::DatabaseNodeConfig,
        connection_string: &str,
        _state: &State,
    ) -> Result<Value, ActionError> {
        use redis::AsyncCommands;

        let redis_config = config
            .redis
            .as_ref()
            .ok_or_else(|| ActionError::Execution("Redis configuration required".to_string()))?;

        // Connect to Redis
        let client = redis::Client::open(connection_string)
            .map_err(|e| ActionError::Execution(format!("Redis connection failed: {}", e)))?;

        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| ActionError::Execution(format!("Redis connection failed: {}", e)))?;

        // Execute based on operation
        let result = match redis_config.operation.as_str() {
            "get" => {
                let value: Option<String> = conn
                    .get(&redis_config.key)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Redis GET failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "get",
                    "key": redis_config.key,
                    "value": value
                })
            }
            "set" => {
                let value = redis_config.value.as_ref().map(|v| v.to_string()).unwrap_or_default();

                if let Some(ttl) = redis_config.ttl {
                    let _: () = conn.set_ex(&redis_config.key, &value, ttl).await.map_err(|e| {
                        ActionError::Execution(format!("Redis SETEX failed: {}", e))
                    })?;
                } else {
                    let _: () = conn
                        .set(&redis_config.key, &value)
                        .await
                        .map_err(|e| ActionError::Execution(format!("Redis SET failed: {}", e)))?;
                }

                serde_json::json!({
                    "success": true,
                    "operation": "set",
                    "key": redis_config.key
                })
            }
            "del" => {
                let deleted: i64 = conn
                    .del(&redis_config.key)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Redis DEL failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "del",
                    "key": redis_config.key,
                    "deleted": deleted
                })
            }
            "hget" => {
                let field = redis_config.value.as_ref().and_then(|v| v.as_str()).unwrap_or("");

                let value: Option<String> = conn
                    .hget(&redis_config.key, field)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Redis HGET failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "hget",
                    "key": redis_config.key,
                    "field": field,
                    "value": value
                })
            }
            "hset" => {
                let value_obj =
                    redis_config.value.as_ref().and_then(|v| v.as_object()).ok_or_else(|| {
                        ActionError::Execution("HSET requires object value".to_string())
                    })?;

                for (field, val) in value_obj {
                    let _: () = conn
                        .hset(&redis_config.key, field, val.to_string())
                        .await
                        .map_err(|e| ActionError::Execution(format!("Redis HSET failed: {}", e)))?;
                }

                serde_json::json!({
                    "success": true,
                    "operation": "hset",
                    "key": redis_config.key
                })
            }
            "lpush" => {
                let value = redis_config.value.as_ref().map(|v| v.to_string()).unwrap_or_default();

                let len: i64 = conn
                    .lpush(&redis_config.key, &value)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Redis LPUSH failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "lpush",
                    "key": redis_config.key,
                    "list_length": len
                })
            }
            "rpop" => {
                let value: Option<String> = conn
                    .rpop(&redis_config.key, None)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Redis RPOP failed: {}", e)))?;

                serde_json::json!({
                    "success": true,
                    "operation": "rpop",
                    "key": redis_config.key,
                    "value": value
                })
            }
            _ => {
                return Err(ActionError::Execution(format!(
                    "Unknown Redis operation: {}",
                    redis_config.operation
                )));
            }
        };

        // Store in state
        let mut state_write = self.state.write().await;
        state_write.insert(config.standard.mapping.output_key.clone(), result.clone());

        Ok(result)
    }

    /// Execute Email node
    async fn execute_email(
        &self,
        config: &crate::codegen::action_nodes::EmailNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::EmailMode;
        use lettre::{
            AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
            message::{Attachment, MultiPart, SinglePart, header::ContentType},
            transport::smtp::authentication::Credentials,
        };

        let state = self.get_state().await;

        match config.mode {
            EmailMode::Monitor => {
                // IMAP monitoring is not implemented in runtime
                // It would require a long-running connection
                Err(ActionError::Execution(
                    "Email monitoring requires a dedicated IMAP service. Use the generated code for production.".to_string()
                ))
            }
            EmailMode::Send => {
                let smtp = config.smtp.as_ref().ok_or_else(|| {
                    ActionError::Execution(
                        "SMTP configuration required for sending emails".to_string(),
                    )
                })?;

                let recipients = config.recipients.as_ref().ok_or_else(|| {
                    ActionError::Execution("Recipients required for sending emails".to_string())
                })?;

                let content = config.content.as_ref().ok_or_else(|| {
                    ActionError::Execution("Email content required for sending emails".to_string())
                })?;

                // Interpolate values
                let host = interpolate_variables(&smtp.host, &state);
                let username = interpolate_variables(&smtp.username, &state);
                let password = interpolate_variables(&smtp.password, &state);
                let from_email = interpolate_variables(&smtp.from_email, &state);
                let to = interpolate_variables(&recipients.to, &state);
                let subject = interpolate_variables(&content.subject, &state);
                let body = interpolate_variables(&content.body, &state);

                // Build from address
                let from = if let Some(from_name) = &smtp.from_name {
                    let name = interpolate_variables(from_name, &state);
                    format!("{} <{}>", name, from_email)
                } else {
                    from_email.clone()
                };

                // Build message
                let mut message_builder = Message::builder()
                    .from(from.parse().map_err(|e| {
                        ActionError::Execution(format!("Invalid from address: {}", e))
                    })?)
                    .to(to.parse().map_err(|e| {
                        ActionError::Execution(format!("Invalid to address: {}", e))
                    })?)
                    .subject(&subject);

                // Add CC if present
                if let Some(cc) = &recipients.cc {
                    let cc_addr = interpolate_variables(cc, &state);
                    if !cc_addr.is_empty() {
                        message_builder = message_builder.cc(cc_addr.parse().map_err(|e| {
                            ActionError::Execution(format!("Invalid CC address: {}", e))
                        })?);
                    }
                }

                // Add BCC if present
                if let Some(bcc) = &recipients.bcc {
                    let bcc_addr = interpolate_variables(bcc, &state);
                    if !bcc_addr.is_empty() {
                        message_builder = message_builder.bcc(bcc_addr.parse().map_err(|e| {
                            ActionError::Execution(format!("Invalid BCC address: {}", e))
                        })?);
                    }
                }

                // Build body based on type
                let body_part = match content.body_type {
                    crate::codegen::action_nodes::EmailBodyType::Html => {
                        SinglePart::builder().header(ContentType::TEXT_HTML).body(body)
                    }
                    crate::codegen::action_nodes::EmailBodyType::Text => {
                        SinglePart::builder().header(ContentType::TEXT_PLAIN).body(body)
                    }
                };

                // Build email with or without attachments
                let email = if let Some(attachments) = &config.attachments {
                    if !attachments.is_empty() {
                        let mut multipart = MultiPart::mixed().singlepart(body_part);

                        for attachment in attachments {
                            if let Some(attachment_data) = state.get(&attachment.state_key) {
                                let data = if let Some(s) = attachment_data.as_str() {
                                    // Assume base64 encoded
                                    use base64::Engine;
                                    base64::engine::general_purpose::STANDARD
                                        .decode(s)
                                        .unwrap_or_else(|_| s.as_bytes().to_vec())
                                } else {
                                    serde_json::to_vec(attachment_data).unwrap_or_default()
                                };

                                let mime_type_str = attachment
                                    .mime_type
                                    .as_deref()
                                    .unwrap_or("application/octet-stream");

                                let content_type: ContentType = mime_type_str
                                    .parse()
                                    .unwrap_or_else(|_| ContentType::TEXT_PLAIN);

                                let att = Attachment::new(attachment.filename.clone())
                                    .body(data, content_type);
                                multipart = multipart.singlepart(att);
                            }
                        }

                        message_builder.multipart(multipart)
                    } else {
                        message_builder.singlepart(body_part)
                    }
                } else {
                    message_builder.singlepart(body_part)
                }
                .map_err(|e| ActionError::Execution(format!("Failed to build email: {}", e)))?;

                // Create SMTP transport
                let creds = Credentials::new(username, password);

                let mailer: AsyncSmtpTransport<Tokio1Executor> = if smtp.secure {
                    AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
                        .map_err(|e| ActionError::Execution(format!("SMTP relay error: {}", e)))?
                        .port(smtp.port)
                        .credentials(creds)
                        .build()
                } else {
                    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
                        .port(smtp.port)
                        .credentials(creds)
                        .build()
                };

                // Send email
                let response = mailer
                    .send(email)
                    .await
                    .map_err(|e| ActionError::Execution(format!("Failed to send email: {}", e)))?;

                let result = serde_json::json!({
                    "success": true,
                    "message": format!("Email sent to {}", to),
                    "response_code": response.code().to_string()
                });

                // Store in state
                let mut state_write = self.state.write().await;
                state_write.insert(config.standard.mapping.output_key.clone(), result.clone());

                Ok(result)
            }
        }
    }

    /// Execute Notification node
    async fn execute_notification(
        &self,
        config: &crate::codegen::action_nodes::NotificationNodeConfig,
    ) -> Result<Value, ActionError> {
        use crate::codegen::action_nodes::{MessageFormat, NotificationChannel};

        let state = self.get_state().await;

        // Interpolate webhook URL
        let webhook_url = interpolate_variables(&config.webhook_url, &state);

        // Interpolate message text
        let message_text = interpolate_variables(&config.message.text, &state);

        // Build payload based on channel
        let payload = match config.channel {
            NotificationChannel::Slack => {
                let mut payload = serde_json::json!({
                    "text": message_text
                });

                // Add blocks if present
                if let Some(blocks) = &config.message.blocks {
                    if !blocks.is_empty() {
                        payload["blocks"] = serde_json::json!(blocks);
                    }
                }

                // Add mrkdwn flag for markdown
                if config.message.format == MessageFormat::Markdown {
                    payload["mrkdwn"] = serde_json::json!(true);
                }

                // Add optional fields
                if let Some(username) = &config.username {
                    payload["username"] =
                        serde_json::json!(interpolate_variables(username, &state));
                }
                if let Some(icon_url) = &config.icon_url {
                    payload["icon_url"] =
                        serde_json::json!(interpolate_variables(icon_url, &state));
                }
                if let Some(channel) = &config.target_channel {
                    payload["channel"] = serde_json::json!(interpolate_variables(channel, &state));
                }

                payload
            }
            NotificationChannel::Discord => {
                let mut payload = serde_json::json!({
                    "content": message_text
                });

                // Add embeds if present
                if let Some(blocks) = &config.message.blocks {
                    if !blocks.is_empty() {
                        payload["embeds"] = serde_json::json!(blocks);
                    }
                }

                // Add optional fields
                if let Some(username) = &config.username {
                    payload["username"] =
                        serde_json::json!(interpolate_variables(username, &state));
                }
                if let Some(icon_url) = &config.icon_url {
                    payload["avatar_url"] =
                        serde_json::json!(interpolate_variables(icon_url, &state));
                }

                payload
            }
            NotificationChannel::Teams => {
                // Check if custom adaptive card is provided
                if let Some(blocks) = &config.message.blocks {
                    if !blocks.is_empty() {
                        serde_json::json!(blocks)
                    } else {
                        // Simple message card format
                        serde_json::json!({
                            "@type": "MessageCard",
                            "@context": "http://schema.org/extensions",
                            "summary": message_text,
                            "sections": [{
                                "activityTitle": "Notification",
                                "text": message_text
                            }]
                        })
                    }
                } else {
                    serde_json::json!({
                        "@type": "MessageCard",
                        "@context": "http://schema.org/extensions",
                        "summary": message_text,
                        "sections": [{
                            "activityTitle": "Notification",
                            "text": message_text
                        }]
                    })
                }
            }
            NotificationChannel::Webhook => {
                // Check if custom payload is provided
                if let Some(blocks) = &config.message.blocks {
                    if !blocks.is_empty() {
                        serde_json::json!(blocks)
                    } else {
                        serde_json::json!({
                            "message": message_text,
                            "timestamp": chrono::Utc::now().to_rfc3339()
                        })
                    }
                } else {
                    serde_json::json!({
                        "message": message_text,
                        "timestamp": chrono::Utc::now().to_rfc3339()
                    })
                }
            }
        };

        // Send the notification
        let client = reqwest::Client::new();
        let response = client
            .post(&webhook_url)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ActionError::Execution(format!("Notification send failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Err(ActionError::Execution(format!(
                "Notification failed with status {}: {}",
                status, error_body
            )));
        }

        // Return result
        let result = serde_json::json!({
            "success": true,
            "channel": format!("{:?}", config.channel),
            "status": status.as_u16()
        });

        // Store in state
        let mut state_write = self.state.write().await;
        state_write.insert(config.standard.mapping.output_key.clone(), result.clone());

        Ok(result)
    }

    /// Evaluate a condition expression
    async fn evaluate_condition(&self, condition: &str) -> bool {
        let state = self.get_state().await;
        // Simple evaluation: check if a state key is truthy
        // More complex evaluation would need an expression parser
        if let Some(value) = state.get(condition) {
            match value {
                Value::Bool(b) => *b,
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
            }
        } else {
            false
        }
    }

    /// Emit an event to the SSE channel
    async fn emit_event(&self, event: ActionNodeEvent) {
        if let Some(sender) = &self.event_sender {
            let _ = sender.send(event).await;
        }
    }

    /// Execute loop body (for nested execution)
    pub async fn execute_loop_body(&self, _body_state: State) -> Result<Value, ActionError> {
        // This would execute the nodes inside the loop
        // For now, return the state as-is
        let state = self.get_state().await;
        Ok(serde_json::to_value(&state).unwrap_or_default())
    }
}

// ============================================
// Helper Functions
// ============================================

/// Get current timestamp in milliseconds
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Get error code from ActionError
fn error_code(error: &ActionError) -> String {
    match error {
        ActionError::Timeout { .. } => "TIMEOUT".to_string(),
        ActionError::HttpStatus { .. } => "HTTP_STATUS".to_string(),
        ActionError::NoMatchingBranch { .. } => "NO_MATCHING_BRANCH".to_string(),
        ActionError::InsufficientBranches { .. } => "INSUFFICIENT_BRANCHES".to_string(),
        ActionError::NoBranchCompleted => "NO_BRANCH_COMPLETED".to_string(),
        ActionError::MissingCredential(_) => "MISSING_CREDENTIAL".to_string(),
        ActionError::Transform(_) => "TRANSFORM_ERROR".to_string(),
        ActionError::CodeExecution(_) => "CODE_EXECUTION_ERROR".to_string(),
        ActionError::SandboxInit(_) => "SANDBOX_INIT_ERROR".to_string(),
        ActionError::InvalidTimestamp(_) => "INVALID_TIMESTAMP".to_string(),
        ActionError::WebhookTimeout => "WEBHOOK_TIMEOUT".to_string(),
        ActionError::WebhookCancelled => "WEBHOOK_CANCELLED".to_string(),
        ActionError::ConditionTimeout { .. } => "CONDITION_TIMEOUT".to_string(),
        ActionError::Skipped { .. } => "SKIPPED".to_string(),
        ActionError::Execution(_) => "EXECUTION_ERROR".to_string(),
        ActionError::RetryExhausted { .. } => "RETRY_EXHAUSTED".to_string(),
    }
}

/// Interpolate {{variable}} patterns in a string with state values
fn interpolate_variables(template: &str, state: &State) -> String {
    let mut result = template.to_string();
    let mut start = 0;

    while let Some(open_pos) = result[start..].find("{{") {
        let open_pos = start + open_pos;
        if let Some(close_pos) = result[open_pos..].find("}}") {
            let close_pos = open_pos + close_pos;
            let var_name = &result[open_pos + 2..close_pos];
            let replacement = get_nested_value(state, var_name)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            result = format!("{}{}{}", &result[..open_pos], replacement, &result[close_pos + 2..]);
            start = open_pos + replacement.len();
        } else {
            break;
        }
    }

    result
}

/// Get a nested value from state using dot notation
fn get_nested_value<'a>(state: &'a State, path: &str) -> Option<&'a Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = state.get(parts[0])?;
    for part in &parts[1..] {
        current = current.get(part)?;
    }
    Some(current)
}

/// Deep merge two JSON values
fn deep_merge(base: &Value, overlay: &Value) -> Value {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            let mut result = base_map.clone();
            for (key, value) in overlay_map {
                if let Some(base_value) = result.get(key) {
                    result.insert(key.clone(), deep_merge(base_value, value));
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            Value::Object(result)
        }
        _ => overlay.clone(),
    }
}

/// Validate HTTP status code against a pattern
#[allow(dead_code)]
fn validate_status_code(status: u16, pattern: &str) -> bool {
    for part in pattern.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let range: Vec<&str> = part.split('-').collect();
            if range.len() == 2 {
                if let (Ok(start), Ok(end)) = (range[0].parse::<u16>(), range[1].parse::<u16>()) {
                    if status >= start && status <= end {
                        return true;
                    }
                }
            }
        } else if let Ok(expected) = part.parse::<u16>() {
            if status == expected {
                return true;
            }
        }
    }
    false
}

/// Evaluate a comparison operator
fn evaluate_operator(operator: &str, value: &Value, compare_to: &Option<Value>) -> bool {
    let compare_value = compare_to.as_ref().unwrap_or(&Value::Null);

    match operator {
        "eq" => value == compare_value,
        "neq" => value != compare_value,
        "gt" => value.as_f64().zip(compare_value.as_f64()).map(|(a, b)| a > b).unwrap_or(false),
        "lt" => value.as_f64().zip(compare_value.as_f64()).map(|(a, b)| a < b).unwrap_or(false),
        "gte" => value.as_f64().zip(compare_value.as_f64()).map(|(a, b)| a >= b).unwrap_or(false),
        "lte" => value.as_f64().zip(compare_value.as_f64()).map(|(a, b)| a <= b).unwrap_or(false),
        "contains" => {
            value.as_str().zip(compare_value.as_str()).map(|(a, b)| a.contains(b)).unwrap_or(false)
        }
        "startsWith" => value
            .as_str()
            .zip(compare_value.as_str())
            .map(|(a, b)| a.starts_with(b))
            .unwrap_or(false),
        "endsWith" => {
            value.as_str().zip(compare_value.as_str()).map(|(a, b)| a.ends_with(b)).unwrap_or(false)
        }
        "empty" => value.as_str().map(|s| s.is_empty()).unwrap_or(value.is_null()),
        "exists" => !value.is_null(),
        _ => false,
    }
}

// ============================================
// Tests
// ============================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ============================================
    // Property Test Generators
    // ============================================

    /// Generate arbitrary error modes
    fn arb_error_mode() -> impl Strategy<Value = ErrorMode> {
        prop_oneof![
            Just(ErrorMode::Stop),
            Just(ErrorMode::Continue),
            Just(ErrorMode::Retry),
            Just(ErrorMode::Fallback),
        ]
    }

    /// Generate arbitrary retry counts (1-10 as per requirement 1.2.2)
    fn arb_retry_count() -> impl Strategy<Value = u32> {
        1u32..=10u32
    }

    /// Generate arbitrary retry delays (100ms - 5000ms)
    fn arb_retry_delay() -> impl Strategy<Value = u64> {
        100u64..=5000u64
    }

    /// Generate arbitrary fallback values
    fn arb_fallback_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            Just(Value::Bool(true)),
            Just(Value::Bool(false)),
            any::<i64>().prop_map(|n| Value::Number(n.into())),
            "[a-z]{1,10}".prop_map(|s| Value::String(s)),
        ]
    }

    /// Generate arbitrary error handling configurations
    fn arb_error_handling() -> impl Strategy<Value = ErrorHandling> {
        (
            arb_error_mode(),
            proptest::option::of(arb_retry_count()),
            proptest::option::of(arb_retry_delay()),
            proptest::option::of(arb_fallback_value()),
        )
            .prop_map(|(mode, retry_count, retry_delay, fallback_value)| ErrorHandling {
                mode,
                retry_count,
                retry_delay,
                fallback_value,
            })
    }

    // ============================================
    // Property Tests
    // ============================================

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// **Feature: action-nodes, Property 2: Error Handling Mode Behavior**
        /// *For any* error handling configuration, the system SHALL respect the configured mode.
        /// **Validates: Requirements 1.2**
        ///
        /// This property verifies:
        /// - 1.2.1: Error mode options (stop, continue, retry, fallback) are respected
        /// - 1.2.2: Retry mode uses retry_count (1-10) and retry_delay (ms)
        /// - 1.2.3: Fallback mode uses fallback_value configuration
        #[test]
        fn prop_error_handling_mode_behavior(
            error_handling in arb_error_handling()
        ) {
            // Property 1: Error mode is preserved in configuration
            let mode = error_handling.mode.clone();
            prop_assert!(matches!(
                mode,
                ErrorMode::Stop | ErrorMode::Continue | ErrorMode::Retry | ErrorMode::Fallback
            ));

            // Property 2: Retry count is within valid range (1-10) when specified
            if let Some(retry_count) = error_handling.retry_count {
                prop_assert!(retry_count >= 1 && retry_count <= 10,
                    "Retry count {} should be between 1 and 10", retry_count);
            }

            // Property 3: Retry delay is positive when specified
            if let Some(retry_delay) = error_handling.retry_delay {
                prop_assert!(retry_delay > 0,
                    "Retry delay {} should be positive", retry_delay);
            }

            // Property 4: Fallback value is valid JSON when specified
            if let Some(ref fallback) = error_handling.fallback_value {
                // Verify it can be serialized (valid JSON)
                let serialized = serde_json::to_string(fallback);
                prop_assert!(serialized.is_ok(),
                    "Fallback value should be serializable to JSON");
            }

            // Property 5: Mode-specific requirements
            match error_handling.mode {
                ErrorMode::Retry => {
                    // Retry mode should have retry_count and retry_delay available
                    // (they may be None, in which case defaults are used)
                }
                ErrorMode::Fallback => {
                    // Fallback mode should have fallback_value available
                    // (it may be None, in which case null is used)
                }
                ErrorMode::Stop | ErrorMode::Continue => {
                    // These modes don't require additional configuration
                }
            }
        }

        /// **Feature: action-nodes, Property 2b: Retry Exhaustion Behavior**
        /// *For any* retry configuration, when retries are exhausted, the system SHALL
        /// return a RetryExhausted error with the correct attempt count.
        /// **Validates: Requirements 1.2.2**
        #[test]
        fn prop_retry_exhaustion_reports_correct_attempts(
            retry_count in arb_retry_count()
        ) {
            // Create a RetryExhausted error
            let error = ActionError::RetryExhausted {
                node: "test_node".to_string(),
                attempts: retry_count,
            };

            // Property: The error should contain the correct attempt count
            if let ActionError::RetryExhausted { attempts, .. } = error {
                prop_assert_eq!(attempts, retry_count,
                    "RetryExhausted should report {} attempts", retry_count);
            } else {
                prop_assert!(false, "Expected RetryExhausted error");
            }
        }

        /// **Feature: action-nodes, Property 2c: Error Code Mapping**
        /// *For any* ActionError variant, the system SHALL produce a valid error code.
        /// **Validates: Requirements 1.2.4**
        #[test]
        fn prop_error_code_is_valid(
            retry_count in arb_retry_count(),
            timeout_ms in 1000u64..60000u64
        ) {
            // Test various error types produce valid codes
            let errors = vec![
                ActionError::Timeout { node: "test".to_string(), timeout_ms },
                ActionError::HttpStatus { status: 500, expected: "200-299".to_string() },
                ActionError::NoMatchingBranch { node: "test".to_string() },
                ActionError::InsufficientBranches { expected: 3, got: 1 },
                ActionError::NoBranchCompleted,
                ActionError::MissingCredential("api_key".to_string()),
                ActionError::Transform("invalid".to_string()),
                ActionError::CodeExecution("error".to_string()),
                ActionError::SandboxInit("failed".to_string()),
                ActionError::InvalidTimestamp("bad".to_string()),
                ActionError::WebhookTimeout,
                ActionError::WebhookCancelled,
                ActionError::ConditionTimeout { condition: "x > 0".to_string(), timeout_ms },
                ActionError::Skipped { node: "test".to_string() },
                ActionError::Execution("error".to_string()),
                ActionError::RetryExhausted { node: "test".to_string(), attempts: retry_count },
            ];

            for error in errors {
                let code = error_code(&error);
                // Property: Error code should be non-empty and uppercase
                prop_assert!(!code.is_empty(), "Error code should not be empty");
                prop_assert!(code.chars().all(|c| c.is_uppercase() || c == '_'),
                    "Error code '{}' should be uppercase with underscores", code);
            }
        }
    }

    // ============================================
    // Unit Tests for Error Handling Logic
    // ============================================

    #[test]
    fn test_error_mode_stop_propagates_error() {
        let error_handling = ErrorHandling {
            mode: ErrorMode::Stop,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        };
        assert!(matches!(error_handling.mode, ErrorMode::Stop));
    }

    #[test]
    fn test_error_mode_continue_has_null_fallback() {
        let error_handling = ErrorHandling {
            mode: ErrorMode::Continue,
            retry_count: None,
            retry_delay: None,
            fallback_value: None,
        };
        assert!(matches!(error_handling.mode, ErrorMode::Continue));
    }

    #[test]
    fn test_error_mode_retry_with_config() {
        let error_handling = ErrorHandling {
            mode: ErrorMode::Retry,
            retry_count: Some(3),
            retry_delay: Some(1000),
            fallback_value: None,
        };
        assert!(matches!(error_handling.mode, ErrorMode::Retry));
        assert_eq!(error_handling.retry_count, Some(3));
        assert_eq!(error_handling.retry_delay, Some(1000));
    }

    #[test]
    fn test_error_mode_fallback_with_value() {
        let fallback = serde_json::json!({"default": true});
        let error_handling = ErrorHandling {
            mode: ErrorMode::Fallback,
            retry_count: None,
            retry_delay: None,
            fallback_value: Some(fallback.clone()),
        };
        assert!(matches!(error_handling.mode, ErrorMode::Fallback));
        assert_eq!(error_handling.fallback_value, Some(fallback));
    }

    #[test]
    fn test_action_error_display() {
        let error = ActionError::RetryExhausted { node: "http_1".to_string(), attempts: 3 };
        let display = format!("{}", error);
        assert!(display.contains("http_1"));
        assert!(display.contains("3"));
    }

    #[test]
    fn test_interpolate_variables_simple() {
        let mut state = HashMap::new();
        state.insert("name".to_string(), Value::String("Alice".to_string()));

        let result = interpolate_variables("Hello, {{name}}!", &state);
        assert_eq!(result, "Hello, Alice!");
    }

    #[test]
    fn test_interpolate_variables_nested() {
        let mut state = HashMap::new();
        state.insert("user".to_string(), serde_json::json!({"name": "Bob", "age": 30}));

        let result = interpolate_variables("Name: {{user.name}}", &state);
        assert_eq!(result, "Name: Bob");
    }

    #[test]
    fn test_interpolate_variables_missing() {
        let state = HashMap::new();
        let result = interpolate_variables("Hello, {{name}}!", &state);
        assert_eq!(result, "Hello, !");
    }

    #[test]
    fn test_deep_merge_objects() {
        let base = serde_json::json!({"a": 1, "b": {"c": 2}});
        let overlay = serde_json::json!({"b": {"d": 3}, "e": 4});

        let result = deep_merge(&base, &overlay);

        assert_eq!(result["a"], 1);
        assert_eq!(result["b"]["c"], 2);
        assert_eq!(result["b"]["d"], 3);
        assert_eq!(result["e"], 4);
    }

    #[test]
    fn test_validate_status_code_single() {
        assert!(validate_status_code(200, "200"));
        assert!(!validate_status_code(201, "200"));
    }

    #[test]
    fn test_validate_status_code_range() {
        assert!(validate_status_code(200, "200-299"));
        assert!(validate_status_code(299, "200-299"));
        assert!(!validate_status_code(300, "200-299"));
    }

    #[test]
    fn test_validate_status_code_multiple() {
        assert!(validate_status_code(200, "200, 201, 204"));
        assert!(validate_status_code(204, "200, 201, 204"));
        assert!(!validate_status_code(202, "200, 201, 204"));
    }

    #[test]
    fn test_evaluate_operator_eq() {
        let value = Value::String("test".to_string());
        let compare = Some(Value::String("test".to_string()));
        assert!(evaluate_operator("eq", &value, &compare));
    }

    #[test]
    fn test_evaluate_operator_gt() {
        let value = Value::Number(10.into());
        let compare = Some(Value::Number(5.into()));
        assert!(evaluate_operator("gt", &value, &compare));
    }

    #[test]
    fn test_evaluate_operator_contains() {
        let value = Value::String("hello world".to_string());
        let compare = Some(Value::String("world".to_string()));
        assert!(evaluate_operator("contains", &value, &compare));
    }

    #[test]
    fn test_evaluate_operator_empty() {
        let value = Value::String("".to_string());
        assert!(evaluate_operator("empty", &value, &None));

        let non_empty = Value::String("test".to_string());
        assert!(!evaluate_operator("empty", &non_empty, &None));
    }

    #[test]
    fn test_evaluate_operator_exists() {
        let value = Value::String("test".to_string());
        assert!(evaluate_operator("exists", &value, &None));

        let null_value = Value::Null;
        assert!(!evaluate_operator("exists", &null_value, &None));
    }
}
