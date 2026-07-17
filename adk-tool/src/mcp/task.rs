// MCP Task Support
//
// Implements async task lifecycle for long-running MCP tool operations.
// Tasks allow tools to be queued and polled rather than blocking.

use std::time::Duration;

pub use rmcp::model::{CreateTaskResult, Task as TaskInfo, TaskStatus};

/// Configuration for MCP task-based execution
#[derive(Debug, Clone)]
pub struct McpTaskConfig {
    /// Allow task mode when a tool and server negotiate MCP task support.
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
}

/// Error during task operations
#[derive(Debug, Clone)]
pub enum TaskError {
    /// Task creation failed
    CreateFailed(String),
    /// Task polling failed
    PollFailed(String),
    /// Task timed out
    Timeout {
        /// ID of the timed-out task.
        task_id: String,
        /// Elapsed time in milliseconds before timeout.
        elapsed_ms: u64,
    },
    /// Task was cancelled
    Cancelled(String),
    /// Task failed with error
    TaskFailed {
        /// ID of the failed task.
        task_id: String,
        /// Error message from the task.
        error: String,
    },
    /// The remote task paused until more information is supplied.
    InputRequired {
        /// ID of the task waiting for input.
        task_id: String,
        /// Human-readable explanation supplied by the MCP server.
        message: String,
    },
    /// Maximum poll attempts exceeded
    MaxAttemptsExceeded {
        /// ID of the task that exceeded attempts.
        task_id: String,
        /// Number of attempts made.
        attempts: u32,
    },
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
            TaskError::InputRequired { task_id, message } => {
                write!(f, "Task '{task_id}' requires input: {message}")
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
    fn exports_the_rmcp_task_status_shape() {
        assert_eq!(TaskStatus::default(), TaskStatus::Working);
    }
}
