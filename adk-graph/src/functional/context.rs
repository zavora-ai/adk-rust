//! TaskContext — runtime context for functional API tasks.
//!
//! Provides access to state, checkpointing, interrupts, and streaming.
//! Passed to `#[entrypoint]` and `#[task]` annotated functions.

use std::collections::HashMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::checkpoint::Checkpointer;
use crate::error::Result;
use crate::state::{State, StateSchema};
use crate::stream::StreamEvent;

use super::error::FunctionalError;
use super::execution_log::ExecutionLog;
use super::schema::StateSchemaValidator;

/// Runtime context passed to `#[entrypoint]` and `#[task]` functions.
///
/// Provides access to workflow state, checkpointing, interrupt/resume,
/// and progress streaming. Each task function receives a mutable reference
/// to `TaskContext` enabling state reads, writes, event emission, and
/// interrupt requests.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::TaskContext;
///
/// #[task]
/// async fn my_step(ctx: &mut TaskContext) -> Result<Value> {
///     // Read state
///     let count: i64 = ctx.get("counter").unwrap_or(0);
///
///     // Write state
///     ctx.set("counter", serde_json::json!(count + 1));
///
///     // Emit progress
///     ctx.emit(StreamEvent::custom("my_step", "progress", serde_json::json!({"count": count + 1})));
///
///     Ok(serde_json::json!({"new_count": count + 1}))
/// }
/// ```
pub struct TaskContext {
    /// Thread identifier for checkpoint scoping.
    thread_id: String,
    /// Current workflow state.
    state: State,
    /// Checkpointer for persistence.
    checkpointer: Arc<dyn Checkpointer>,
    /// Stream event sender.
    event_tx: tokio::sync::broadcast::Sender<StreamEvent>,
    /// Task execution tracker (for skip-on-resume).
    execution_log: Arc<RwLock<ExecutionLog>>,
    /// Cancellation token.
    cancel_token: CancellationToken,
    /// State schema for validation.
    schema: Option<StateSchema>,
    /// State schema validator for functional API validation.
    schema_validator: Option<StateSchemaValidator>,
    /// Iteration counters for loop checkpoint keying.
    /// Maps task_name -> current iteration index.
    iteration_counters: HashMap<String, usize>,
}

impl TaskContext {
    /// Create a new `TaskContext`.
    ///
    /// Typically constructed by the macro-generated entrypoint, not by user code.
    pub fn new(
        thread_id: String,
        state: State,
        checkpointer: Arc<dyn Checkpointer>,
        event_tx: tokio::sync::broadcast::Sender<StreamEvent>,
        execution_log: Arc<RwLock<ExecutionLog>>,
        cancel_token: CancellationToken,
        schema: Option<StateSchema>,
    ) -> Self {
        Self {
            thread_id,
            state,
            checkpointer,
            event_tx,
            execution_log,
            cancel_token,
            schema,
            schema_validator: None,
            iteration_counters: HashMap::new(),
        }
    }

    // ─── Public Methods ──────────────────────────────────────────────────

    /// Get the current workflow state (read-only).
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Get a typed value from state.
    ///
    /// Attempts to deserialize the value stored at `key` into type `T`.
    /// Returns `None` if the key does not exist or deserialization fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let count: Option<i64> = ctx.get("counter");
    /// ```
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Set a value in state.
    ///
    /// If a [`StateSchema`] is configured, the update is applied using the
    /// appropriate reducer for the key. Otherwise the value is set directly
    /// (overwrite semantics).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// ctx.set("counter", serde_json::json!(42));
    /// ```
    pub fn set(&mut self, key: &str, value: impl Into<Value>) {
        let value = value.into();
        if let Some(schema) = &self.schema {
            schema.apply_update(&mut self.state, key, value);
        } else {
            self.state.insert(key.to_string(), value);
        }
    }

    /// Emit a progress event to stream listeners.
    ///
    /// Events are broadcast to all registered receivers. If no listeners
    /// are active the event is silently dropped.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// ctx.emit(StreamEvent::custom("my_task", "progress", json!({"pct": 50})));
    /// ```
    pub fn emit(&self, event: StreamEvent) {
        // Ignore send errors — they indicate no active receivers.
        let _ = self.event_tx.send(event);
    }

    /// Interrupt execution and wait for external input.
    ///
    /// Persists the current state as an interrupt checkpoint, emits an
    /// interrupted event, and suspends execution. When the workflow is
    /// resumed with an interrupt value, the value is deserialized into `T`
    /// and returned.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionalError::InterruptTypeMismatch`] if the resume
    /// value cannot be deserialized into `T`.
    ///
    /// Returns [`FunctionalError::CheckpointFailed`] if persisting the
    /// interrupt checkpoint fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let approval: bool = ctx.interrupt("Please approve this action").await?;
    /// ```
    pub async fn interrupt<T: DeserializeOwned>(&self, message: &str) -> Result<T> {
        // Emit the interrupt event for stream listeners.
        self.emit(StreamEvent::interrupted("functional_task", message));

        // Persist the interrupt checkpoint.
        let checkpoint = crate::state::Checkpoint::new(
            &self.thread_id,
            self.state.clone(),
            self.current_step().await,
            vec![],
        )
        .with_metadata("interrupt_message", Value::String(message.to_string()));

        self.checkpointer.save(&checkpoint).await.map_err(|e| {
            FunctionalError::CheckpointFailed {
                task: "interrupt".to_string(),
                message: e.to_string(),
            }
        })?;

        // Mark the current task as interrupted in the execution log.
        {
            let mut log = self.execution_log.write().await;
            log.tasks.entry("__interrupt__".to_string()).or_insert(
                super::execution_log::TaskRecord {
                    status: super::execution_log::TaskStatus::Interrupted,
                    result: None,
                    error: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    completed_at: None,
                    attempt: 1,
                },
            );
        }

        // In a real runtime the workflow executor would suspend here and
        // later provide the resume value. For now we return an error
        // indicating the interrupt was requested — the macro-generated
        // wrapper handles actual suspension/resumption.
        Err(FunctionalError::InterruptTypeMismatch {
            task: "interrupt".to_string(),
            message: format!("workflow interrupted: {message}"),
        }
        .into())
    }

    /// Get the thread identifier for this context.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Get a reference to the cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }

    /// Check if the workflow has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Get the current step number from the execution log.
    pub async fn current_step(&self) -> usize {
        self.execution_log.read().await.current_step()
    }

    /// Set a [`StateSchemaValidator`] for this context.
    ///
    /// When set, the validator is used to validate initial state at
    /// workflow start and task output before applying reducers.
    pub fn with_schema_validator(mut self, validator: StateSchemaValidator) -> Self {
        self.schema_validator = Some(validator);
        self
    }

    /// Get the schema validator, if configured.
    pub fn schema_validator(&self) -> Option<&StateSchemaValidator> {
        self.schema_validator.as_ref()
    }

    /// Validate the current state against the schema validator.
    ///
    /// Called at workflow start to validate initial state.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionalError::SchemaValidation`] if validation fails.
    pub fn validate_state(&self) -> std::result::Result<(), FunctionalError> {
        if let Some(validator) = &self.schema_validator {
            validator.validate_state(&self.state)?;
        }
        Ok(())
    }

    /// Validate task output against the schema validator.
    ///
    /// Called after a task produces output, before applying reducers.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionalError::SchemaValidation`] if validation fails.
    pub fn validate_task_output(&self, output: &State) -> std::result::Result<(), FunctionalError> {
        if let Some(validator) = &self.schema_validator {
            validator.validate_task_output(output)?;
        }
        Ok(())
    }

    // ─── Loop Iteration Checkpoint Keying ────────────────────────────────

    /// Generate a unique checkpoint key for a task inside a loop.
    ///
    /// Each call to this method for the same `task_name` increments the
    /// iteration counter, producing keys like `"step_a::iter_0"`,
    /// `"step_a::iter_1"`, etc. Keys are deterministic from task name
    /// and iteration index.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// for item in items {
    ///     let key = ctx.iteration_key("process_item");
    ///     // key = "process_item::iter_0", "process_item::iter_1", ...
    /// }
    /// ```
    pub fn iteration_key(&mut self, task_name: &str) -> String {
        let counter = self.iteration_counters.entry(task_name.to_string()).or_insert(0);
        let key = format!("{task_name}::iter_{counter}");
        *counter += 1;
        key
    }

    /// Get the current iteration index for a task without incrementing.
    ///
    /// Returns `None` if the task has not been called in a loop yet.
    pub fn current_iteration(&self, task_name: &str) -> Option<usize> {
        self.iteration_counters.get(task_name).copied()
    }

    /// Reset the iteration counter for a task.
    ///
    /// Useful when re-entering a loop (e.g., nested loops or retry).
    pub fn reset_iteration(&mut self, task_name: &str) {
        self.iteration_counters.remove(task_name);
    }

    /// Reset all iteration counters.
    pub fn reset_all_iterations(&mut self) {
        self.iteration_counters.clear();
    }

    // ─── Internal Methods (pub(crate)) ───────────────────────────────────
    // These methods are used by the macro-generated task wrappers
    // (`#[entrypoint]` and `#[task]`), not directly by user code.

    /// Check if a task was already completed in a prior run (for resume-skip).
    ///
    /// Uses `try_read()` for synchronous non-blocking access. If the lock
    /// is held, conservatively returns `false` (the task will re-execute).
    /// For reliable resume-skip in async contexts, prefer [`Self::is_completed_async`].
    #[allow(dead_code)]
    #[doc(hidden)]
    pub fn is_completed(&self, task_id: &str) -> bool {
        // We need synchronous access here; use try_read to avoid blocking.
        // If the lock is held, conservatively return false (task will re-execute).
        match self.execution_log.try_read() {
            Ok(log) => log.is_completed(task_id),
            Err(_) => false,
        }
    }

    /// Async version of [`Self::is_completed`] for reliable resume-skip behavior.
    ///
    /// Awaits the read lock on the execution log to guarantee accurate
    /// completion status. Use this in async task wrappers where blocking
    /// is acceptable and correctness is required.
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn is_completed_async(&self, task_id: &str) -> bool {
        self.execution_log.read().await.is_completed(task_id)
    }

    /// Get a cached result for a completed task (async).
    ///
    /// If the task is recorded as completed, returns a clone of its
    /// result value. Used by the resume-skip logic to return cached
    /// results without re-executing the task.
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn get_cached_result(&self, task_id: &str) -> Option<Value> {
        self.execution_log.read().await.get_result(task_id).cloned()
    }

    /// Record task completion for checkpoint tracking.
    ///
    /// Marks the task as completed in the execution log, persists the
    /// current state to the checkpointer, and advances the step counter.
    /// Each task gets its own checkpoint record regardless of sibling
    /// task status (parallel task independence).
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn record_completion(&self, task_id: &str, result: &Value) -> Result<()> {
        // Update execution log — each task is recorded independently.
        {
            let mut log = self.execution_log.write().await;
            log.record_completion(task_id, result.clone());
            log.advance_step();
        }

        // Persist checkpoint with current state and full execution log.
        let step = self.execution_log.read().await.current_step();
        let checkpoint =
            crate::state::Checkpoint::new(&self.thread_id, self.state.clone(), step, vec![])
                .with_metadata("completed_task", Value::String(task_id.to_string()))
                .with_metadata(
                    "execution_log",
                    serde_json::to_value(&*self.execution_log.read().await).unwrap_or(Value::Null),
                );

        self.checkpointer.save(&checkpoint).await.map_err(|e| {
            FunctionalError::CheckpointFailed { task: task_id.to_string(), message: e.to_string() }
        })?;

        Ok(())
    }

    /// Record task failure.
    ///
    /// Marks the task as failed in the execution log and persists a
    /// failure checkpoint containing the error details. Each task failure
    /// is recorded independently (parallel task independence).
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn record_failure(&self, task_id: &str, error: &str) -> Result<()> {
        // Update execution log — each task failure is independent.
        {
            let mut log = self.execution_log.write().await;
            log.record_failure(task_id, error);
        }

        // Persist failure checkpoint with error context.
        let step = self.execution_log.read().await.current_step();
        let checkpoint =
            crate::state::Checkpoint::new(&self.thread_id, self.state.clone(), step, vec![])
                .with_metadata("failed_task", Value::String(task_id.to_string()))
                .with_metadata("error", Value::String(error.to_string()))
                .with_metadata(
                    "execution_log",
                    serde_json::to_value(&*self.execution_log.read().await).unwrap_or(Value::Null),
                );

        self.checkpointer.save(&checkpoint).await.map_err(|e| {
            FunctionalError::CheckpointFailed { task: task_id.to_string(), message: e.to_string() }
        })?;

        Ok(())
    }

    /// Record that a task has started executing.
    ///
    /// Marks the task as running in the execution log. Used by the
    /// macro-generated task wrapper before executing the task body.
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn record_start(&self, task_id: &str) {
        let mut log = self.execution_log.write().await;
        log.record_start(task_id);
    }
}
