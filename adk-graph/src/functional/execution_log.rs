//! Execution log for tracking task completion status within a workflow run.
//!
//! The [`ExecutionLog`] is stored as part of checkpoint metadata and enables
//! the resume-skip behavior: when a workflow resumes from a checkpoint, tasks
//! that were already completed are skipped automatically.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tracks task completion status within a workflow run.
/// Stored as part of the checkpoint metadata.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::ExecutionLog;
///
/// let mut log = ExecutionLog::new();
/// log.record_start("step_a");
/// log.record_completion("step_a", serde_json::json!({"result": 42}));
///
/// assert!(log.is_completed("step_a"));
/// assert_eq!(log.get_result("step_a"), Some(&serde_json::json!({"result": 42})));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionLog {
    /// Map of task_id -> completion record
    pub tasks: HashMap<String, TaskRecord>,
    /// Current workflow step counter
    pub current_step: usize,
}

/// A record of a single task's execution state.
///
/// Contains status, optional result/error, timestamps, and retry attempt count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Current status of the task
    pub status: TaskStatus,
    /// The result value if the task completed successfully
    pub result: Option<Value>,
    /// Error message if the task failed
    pub error: Option<String>,
    /// ISO 8601 timestamp when the task started
    pub started_at: String,
    /// ISO 8601 timestamp when the task completed (success or failure)
    pub completed_at: Option<String>,
    /// Number of execution attempts (starts at 1)
    pub attempt: u32,
}

/// The lifecycle status of a task within a workflow execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task is currently executing
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed after exhausting retries
    Failed,
    /// Task was interrupted (waiting for external input)
    Interrupted,
}

impl ExecutionLog {
    /// Create a new empty execution log.
    pub fn new() -> Self {
        Self { tasks: HashMap::new(), current_step: 0 }
    }

    /// Get the cached result for a completed task.
    ///
    /// Returns `Some(&Value)` if the task is completed and has a result,
    /// `None` otherwise.
    pub fn get_result(&self, task_id: &str) -> Option<&Value> {
        self.tasks.get(task_id).and_then(|record| {
            if record.status == TaskStatus::Completed { record.result.as_ref() } else { None }
        })
    }

    /// Check if a task was already completed in a prior run.
    pub fn is_completed(&self, task_id: &str) -> bool {
        self.tasks.get(task_id).is_some_and(|record| record.status == TaskStatus::Completed)
    }

    /// Mark a task as running, recording the start timestamp.
    pub fn record_start(&mut self, task_id: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        let record = self.tasks.entry(task_id.to_string()).or_insert(TaskRecord {
            status: TaskStatus::Running,
            result: None,
            error: None,
            started_at: now.clone(),
            completed_at: None,
            attempt: 0,
        });
        record.status = TaskStatus::Running;
        record.attempt += 1;
        // Update started_at only on first attempt
        if record.attempt == 1 {
            record.started_at = now;
        }
    }

    /// Mark a task as completed with a result value.
    pub fn record_completion(&mut self, task_id: &str, result: Value) {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.status = TaskStatus::Completed;
            record.result = Some(result);
            record.error = None;
            record.completed_at = Some(now);
        } else {
            // Task was completed without a prior record_start (edge case)
            self.tasks.insert(
                task_id.to_string(),
                TaskRecord {
                    status: TaskStatus::Completed,
                    result: Some(result),
                    error: None,
                    started_at: now.clone(),
                    completed_at: Some(now),
                    attempt: 1,
                },
            );
        }
    }

    /// Mark a task as failed with an error message.
    pub fn record_failure(&mut self, task_id: &str, error: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(record) = self.tasks.get_mut(task_id) {
            record.status = TaskStatus::Failed;
            record.error = Some(error.to_string());
            record.completed_at = Some(now);
        } else {
            self.tasks.insert(
                task_id.to_string(),
                TaskRecord {
                    status: TaskStatus::Failed,
                    result: None,
                    error: Some(error.to_string()),
                    started_at: now.clone(),
                    completed_at: Some(now),
                    attempt: 1,
                },
            );
        }
    }

    /// Get the current step number.
    pub fn current_step(&self) -> usize {
        self.current_step
    }

    /// Increment the step counter and return the new value.
    pub fn advance_step(&mut self) -> usize {
        self.current_step += 1;
        self.current_step
    }
}
