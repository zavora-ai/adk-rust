// MCP Task Support (SEP-1686)
//
// Implements async task lifecycle for long-running MCP tool operations.
// Tasks allow tools to be queued and polled rather than blocking.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

/// Configuration for MCP task-based execution
#[derive(Debug, Clone)]
pub struct McpTaskConfig {
    /// Enable task mode for long-running tools
    pub enable_tasks: bool,
    /// Default poll interval in milliseconds
    pub poll_interval_ms: u64,
    /// Maximum wait time before timeout (None = no timeout)
    pub timeout_ms: Option<u64>,
    /// Maximum number of poll attempts (None = unlimited)
    pub max_poll_attempts: Option<u32>,
}

impl Default for McpTaskConfig {
    fn default() -> Self {
        Self {
            enable_tasks: false,
            poll_interval_ms: 1000,
            timeout_ms: Some(300_000), // 5 minutes default
            max_poll_attempts: None,
        }
    }
}

impl McpTaskConfig {
    /// Create a new task config with tasks enabled
    pub fn enabled() -> Self {
        Self { enable_tasks: true, ..Default::default() }
    }

    /// Set the poll interval
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval_ms = interval.as_millis() as u64;
        self
    }

    /// Set the timeout
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(timeout.as_millis() as u64);
        self
    }

    /// Set no timeout (wait indefinitely)
    pub fn no_timeout(mut self) -> Self {
        self.timeout_ms = None;
        self
    }

    /// Set maximum poll attempts
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_poll_attempts = Some(attempts);
        self
    }

    /// Get poll interval as Duration
    pub fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// Get timeout as Duration
    pub fn timeout_duration(&self) -> Option<Duration> {
        self.timeout_ms.map(Duration::from_millis)
    }

    /// Convert to MCP task request parameters
    pub fn to_task_params(&self) -> Value {
        json!({
            "poll_interval_ms": self.poll_interval_ms
        })
    }
}

/// Status of an MCP task
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Task is queued but not started
    Pending,
    /// Task is currently running
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed with an error
    Failed,
    /// Task was cancelled
    Cancelled,
}

impl TaskStatus {
    /// Check if the task is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled)
    }

    /// Check if the task is still in progress
    pub fn is_in_progress(&self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::Running)
    }
}

/// Information about an MCP task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    /// Unique task identifier
    pub task_id: String,
    /// Current status
    pub status: TaskStatus,
    /// Progress percentage (0-100) if available
    pub progress: Option<u8>,
    /// Human-readable status message
    pub message: Option<String>,
    /// Estimated time remaining in milliseconds
    pub eta_ms: Option<u64>,
}

/// Result of creating a task
#[derive(Debug, Clone)]
pub struct CreateTaskResult {
    /// The task ID for polling
    pub task_id: String,
    /// Initial task info
    pub info: TaskInfo,
}

/// Error during task operations
#[derive(Debug, Clone)]
pub enum TaskError {
    /// Task creation failed
    CreateFailed(String),
    /// Task polling failed
    PollFailed(String),
    /// Task timed out
    Timeout { task_id: String, elapsed_ms: u64 },
    /// Task was cancelled
    Cancelled(String),
    /// Task failed with error
    TaskFailed { task_id: String, error: String },
    /// Maximum poll attempts exceeded
    MaxAttemptsExceeded { task_id: String, attempts: u32 },
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::CreateFailed(msg) => write!(f, "Failed to create task: {}", msg),
            TaskError::PollFailed(msg) => write!(f, "Failed to poll task: {}", msg),
            TaskError::Timeout { task_id, elapsed_ms } => {
                write!(f, "Task '{}' timed out after {}ms", task_id, elapsed_ms)
            }
            TaskError::Cancelled(task_id) => write!(f, "Task '{}' was cancelled", task_id),
            TaskError::TaskFailed { task_id, error } => {
                write!(f, "Task '{}' failed: {}", task_id, error)
            }
            TaskError::MaxAttemptsExceeded { task_id, attempts } => {
                write!(f, "Task '{}' exceeded {} poll attempts", task_id, attempts)
            }
        }
    }
}

impl std::error::Error for TaskError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_config_default() {
        let config = McpTaskConfig::default();
        assert!(!config.enable_tasks);
        assert_eq!(config.poll_interval_ms, 1000);
        assert_eq!(config.timeout_ms, Some(300_000));
    }

    #[test]
    fn test_task_config_enabled() {
        let config = McpTaskConfig::enabled();
        assert!(config.enable_tasks);
    }

    #[test]
    fn test_task_config_builder() {
        let config = McpTaskConfig::enabled()
            .poll_interval(Duration::from_secs(2))
            .timeout(Duration::from_secs(60))
            .max_attempts(10);

        assert!(config.enable_tasks);
        assert_eq!(config.poll_interval_ms, 2000);
        assert_eq!(config.timeout_ms, Some(60_000));
        assert_eq!(config.max_poll_attempts, Some(10));
    }

    #[test]
    fn test_task_status_terminal() {
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
    }

    #[test]
    fn test_task_status_in_progress() {
        assert!(TaskStatus::Pending.is_in_progress());
        assert!(TaskStatus::Running.is_in_progress());
        assert!(!TaskStatus::Completed.is_in_progress());
        assert!(!TaskStatus::Failed.is_in_progress());
        assert!(!TaskStatus::Cancelled.is_in_progress());
    }
}
