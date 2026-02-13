//! Error types for Ralph autonomous agent.
//!
//! This module defines comprehensive error types with actionable error messages
//! for all failure modes in the Ralph system.

/// Result type alias for Ralph operations.
pub type Result<T> = std::result::Result<T, RalphError>;

/// Comprehensive error types for the Ralph autonomous agent system.
#[derive(Debug)]
pub enum RalphError {
    /// PRD loading or parsing failed
    PrdLoad(String),

    /// Task execution failed with details
    TaskExecution { task_id: String, reason: String },

    /// Quality checks failed with detailed feedback
    QualityCheck { details: String },

    /// Git operation failed with operation and error details
    GitOperation { operation: String, error: String },

    /// Configuration error with actionable message
    Configuration(String),

    /// Model provider error with provider and error details
    Model { provider: String, error: String },

    /// File system operation failed
    FileSystem { operation: String, path: String, error: String },

    /// Agent execution failed
    AgentExecution { agent_name: String, reason: String },

    /// Tool execution failed
    ToolExecution { tool_name: String, reason: String },

    /// Serialization/deserialization error
    Serialization(serde_json::Error),

    /// IO error
    Io(std::io::Error),

    /// Parse error
    Parse(std::num::ParseIntError),
}

impl std::fmt::Display for RalphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RalphError::PrdLoad(msg) => write!(f, "PRD loading failed: {}", msg),
            RalphError::TaskExecution { task_id, reason } => {
                write!(f, "Task execution failed: {} - {}", task_id, reason)
            }
            RalphError::QualityCheck { details } => {
                write!(f, "Quality checks failed: {}", details)
            }
            RalphError::GitOperation { operation, error } => {
                write!(f, "Git operation failed: {} - {}", operation, error)
            }
            RalphError::Configuration(msg) => write!(f, "Configuration error: {}", msg),
            RalphError::Model { provider, error } => {
                write!(f, "Model error: {} - {}", provider, error)
            }
            RalphError::FileSystem { operation, path, error } => {
                write!(f, "File system error: {} - {} - {}", operation, path, error)
            }
            RalphError::AgentExecution { agent_name, reason } => {
                write!(f, "Agent execution failed: {} - {}", agent_name, reason)
            }
            RalphError::ToolExecution { tool_name, reason } => {
                write!(f, "Tool execution failed: {} - {}", tool_name, reason)
            }
            RalphError::Serialization(e) => write!(f, "Serialization error: {}", e),
            RalphError::Io(e) => write!(f, "IO error: {}", e),
            RalphError::Parse(e) => write!(f, "Parse error: {}", e),
        }
    }
}

impl std::error::Error for RalphError {}

impl From<serde_json::Error> for RalphError {
    fn from(error: serde_json::Error) -> Self {
        RalphError::Serialization(error)
    }
}

impl From<std::io::Error> for RalphError {
    fn from(error: std::io::Error) -> Self {
        RalphError::Io(error)
    }
}

impl From<std::num::ParseIntError> for RalphError {
    fn from(error: std::num::ParseIntError) -> Self {
        RalphError::Parse(error)
    }
}
