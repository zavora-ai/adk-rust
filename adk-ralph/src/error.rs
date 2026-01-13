//! Error types for the Ralph multi-agent system.

use crate::models::config::ValidationError;
use thiserror::Error;

/// Result type alias for Ralph operations.
pub type Result<T> = std::result::Result<T, RalphError>;

/// Errors that can occur in the Ralph system.
#[derive(Debug, Error)]
pub enum RalphError {
    /// Configuration error
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Configuration validation error with detailed context
    #[error("Configuration validation error: {0}")]
    ConfigValidation(#[from] ValidationError),

    /// Model/LLM error
    #[error("Model error ({provider}): {message}")]
    Model { provider: String, message: String },

    /// File I/O error
    #[error("File error ({path}): {message}")]
    File { path: String, message: String },

    /// PRD parsing or validation error
    #[error("PRD error: {0}")]
    Prd(String),

    /// Design document error
    #[error("Design error: {0}")]
    Design(String),

    /// Task list error
    #[error("Task error: {0}")]
    Task(String),

    /// Progress log error
    #[error("Progress error: {0}")]
    Progress(String),

    /// Git operation error
    #[error("Git error: {0}")]
    Git(String),

    /// Test execution error
    #[error("Test error: {0}")]
    Test(String),

    /// Agent execution error
    #[error("Agent error ({agent}): {message}")]
    Agent { agent: String, message: String },

    /// Tool execution error
    #[error("Tool error ({tool}): {message}")]
    Tool { tool: String, message: String },

    /// Maximum iterations reached
    #[error("Maximum iterations ({max}) reached with {remaining} tasks remaining")]
    MaxIterations { max: usize, remaining: usize },

    /// All tasks blocked
    #[error("All remaining tasks are blocked: {reason}")]
    AllTasksBlocked { reason: String },

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Generic internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl RalphError {
    /// Create a configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        RalphError::Configuration(msg.into())
    }

    /// Create a model error.
    pub fn model(provider: impl Into<String>, msg: impl Into<String>) -> Self {
        RalphError::Model {
            provider: provider.into(),
            message: msg.into(),
        }
    }

    /// Create a file error.
    pub fn file(path: impl Into<String>, msg: impl Into<String>) -> Self {
        RalphError::File {
            path: path.into(),
            message: msg.into(),
        }
    }

    /// Create an agent error.
    pub fn agent(agent: impl Into<String>, msg: impl Into<String>) -> Self {
        RalphError::Agent {
            agent: agent.into(),
            message: msg.into(),
        }
    }

    /// Create a tool error.
    pub fn tool(tool: impl Into<String>, msg: impl Into<String>) -> Self {
        RalphError::Tool {
            tool: tool.into(),
            message: msg.into(),
        }
    }
}

impl From<std::io::Error> for RalphError {
    fn from(err: std::io::Error) -> Self {
        RalphError::File {
            path: "unknown".to_string(),
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for RalphError {
    fn from(err: serde_json::Error) -> Self {
        RalphError::Serialization(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = RalphError::config("Invalid provider");
        assert!(err.to_string().contains("Configuration error"));

        let err = RalphError::model("anthropic", "API key missing");
        assert!(err.to_string().contains("anthropic"));
        assert!(err.to_string().contains("API key missing"));
    }

    #[test]
    fn test_error_constructors() {
        let err = RalphError::file("test.json", "Not found");
        assert!(matches!(err, RalphError::File { .. }));

        let err = RalphError::agent("PRD", "Failed to generate");
        assert!(matches!(err, RalphError::Agent { .. }));
    }
}
