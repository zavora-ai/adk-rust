use crate::{Agent, Result, types::Content};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

#[async_trait]
pub trait ReadonlyContext: Send + Sync {
    fn invocation_id(&self) -> &str;
    fn agent_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn session_id(&self) -> &str;
    fn branch(&self) -> &str;
    fn user_content(&self) -> &Content;
}

// State management traits

/// Maximum allowed length for state keys (256 bytes).
pub const MAX_STATE_KEY_LEN: usize = 256;

/// Validates a state key. Returns `Ok(())` if the key is safe, or an error message.
///
/// Rules:
/// - Must not be empty
/// - Must not exceed [`MAX_STATE_KEY_LEN`] bytes
/// - Must not contain path separators (`/`, `\`) or `..`
/// - Must not contain null bytes
pub fn validate_state_key(key: &str) -> std::result::Result<(), &'static str> {
    if key.is_empty() {
        return Err("state key must not be empty");
    }
    if key.len() > MAX_STATE_KEY_LEN {
        return Err("state key exceeds maximum length of 256 bytes");
    }
    if key.contains('/') || key.contains('\\') || key.contains("..") {
        return Err("state key must not contain path separators or '..'");
    }
    if key.contains('\0') {
        return Err("state key must not contain null bytes");
    }
    Ok(())
}

pub trait State: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    /// Set a state value. Implementations should call [`validate_state_key`] and
    /// reject invalid keys (e.g., by logging a warning or panicking).
    fn set(&mut self, key: String, value: Value);
    fn all(&self) -> HashMap<String, Value>;
}

pub trait ReadonlyState: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn all(&self) -> HashMap<String, Value>;
}

// Session trait
pub trait Session: Send + Sync {
    fn id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn state(&self) -> &dyn State;
    /// Returns the conversation history from this session as Content items
    fn conversation_history(&self) -> Vec<Content>;
    /// Append content to conversation history (for sequential agent support)
    fn append_to_history(&self, _content: Content) {
        // Default no-op - implementations can override to track history
    }
}

#[async_trait]
pub trait CallbackContext: ReadonlyContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
}

#[async_trait]
pub trait InvocationContext: CallbackContext {
    fn agent(&self) -> Arc<dyn Agent>;
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    fn session(&self) -> &dyn Session;
    fn run_config(&self) -> &RunConfig;
    fn end_invocation(&self);
    fn ended(&self) -> bool;
}

// Placeholder service traits
#[async_trait]
pub trait Artifacts: Send + Sync {
    async fn save(&self, name: &str, data: &crate::Part) -> Result<i64>;
    async fn load(&self, name: &str) -> Result<crate::Part>;
    async fn list(&self) -> Result<Vec<String>>;
}

#[async_trait]
pub trait Memory: Send + Sync {
    async fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub content: Content,
    pub author: String,
}

/// Streaming mode for agent responses.
/// Matches ADK Python/Go specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamingMode {
    /// No streaming; responses delivered as complete units.
    /// Agent collects all chunks internally and yields a single final event.
    None,
    /// Server-Sent Events streaming; one-way streaming from server to client.
    /// Agent yields each chunk as it arrives with stable event ID.
    #[default]
    SSE,
    /// Bidirectional streaming; simultaneous communication in both directions.
    /// Used for realtime audio/video agents.
    Bidi,
}

/// Controls what parts of prior conversation history is received by llmagent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IncludeContents {
    /// The llmagent operates solely on its current turn (latest user input + any following agent events)
    None,
    /// Default - The llmagent receives the relevant conversation history
    #[default]
    Default,
}

/// Decision applied when a tool execution requires human confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConfirmationDecision {
    Approve,
    Deny,
}

/// Policy defining which tools require human confirmation before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolConfirmationPolicy {
    /// No tool confirmation is required.
    #[default]
    Never,
    /// Every tool call requires confirmation.
    Always,
    /// Only the listed tool names require confirmation.
    PerTool(BTreeSet<String>),
}

impl ToolConfirmationPolicy {
    /// Returns true when the given tool name must be confirmed before execution.
    pub fn requires_confirmation(&self, tool_name: &str) -> bool {
        match self {
            Self::Never => false,
            Self::Always => true,
            Self::PerTool(tools) => tools.contains(tool_name),
        }
    }

    /// Add one tool name to the confirmation policy (converts `Never` to `PerTool`).
    pub fn with_tool(mut self, tool_name: impl Into<String>) -> Self {
        let tool_name = tool_name.into();
        match &mut self {
            Self::Never => {
                let mut tools = BTreeSet::new();
                tools.insert(tool_name);
                Self::PerTool(tools)
            }
            Self::Always => Self::Always,
            Self::PerTool(tools) => {
                tools.insert(tool_name);
                self
            }
        }
    }
}

/// Payload describing a tool call awaiting human confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfirmationRequest {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call_id: Option<String>,
    pub args: Value,
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub streaming_mode: StreamingMode,
    /// Optional per-tool confirmation decisions for the current run.
    /// Keys are tool names.
    pub tool_confirmation_decisions: HashMap<String, ToolConfirmationDecision>,
    /// Optional cached content name for automatic prompt caching.
    /// When set by the runner's cache lifecycle manager, agents should attach
    /// this name to their `GenerateContentConfig` so the LLM provider can
    /// reuse cached system instructions and tool definitions.
    pub cached_content: Option<String>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            streaming_mode: StreamingMode::SSE,
            tool_confirmation_decisions: HashMap::new(),
            cached_content: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_config_default() {
        let config = RunConfig::default();
        assert_eq!(config.streaming_mode, StreamingMode::SSE);
        assert!(config.tool_confirmation_decisions.is_empty());
    }

    #[test]
    fn test_streaming_mode() {
        assert_eq!(StreamingMode::SSE, StreamingMode::SSE);
        assert_ne!(StreamingMode::SSE, StreamingMode::None);
        assert_ne!(StreamingMode::None, StreamingMode::Bidi);
    }

    #[test]
    fn test_tool_confirmation_policy() {
        let policy = ToolConfirmationPolicy::default();
        assert!(!policy.requires_confirmation("search"));

        let policy = policy.with_tool("search");
        assert!(policy.requires_confirmation("search"));
        assert!(!policy.requires_confirmation("write_file"));

        assert!(ToolConfirmationPolicy::Always.requires_confirmation("any_tool"));
    }

    #[test]
    fn test_validate_state_key_valid() {
        assert!(validate_state_key("user_name").is_ok());
        assert!(validate_state_key("app:config").is_ok());
        assert!(validate_state_key("temp:data").is_ok());
        assert!(validate_state_key("a").is_ok());
    }

    #[test]
    fn test_validate_state_key_empty() {
        assert_eq!(validate_state_key(""), Err("state key must not be empty"));
    }

    #[test]
    fn test_validate_state_key_too_long() {
        let long_key = "a".repeat(MAX_STATE_KEY_LEN + 1);
        assert!(validate_state_key(&long_key).is_err());
    }

    #[test]
    fn test_validate_state_key_path_traversal() {
        assert!(validate_state_key("../etc/passwd").is_err());
        assert!(validate_state_key("foo/bar").is_err());
        assert!(validate_state_key("foo\\bar").is_err());
        assert!(validate_state_key("..").is_err());
    }

    #[test]
    fn test_validate_state_key_null_byte() {
        assert!(validate_state_key("foo\0bar").is_err());
    }
}
