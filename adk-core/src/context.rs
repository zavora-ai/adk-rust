use crate::identity::{AdkIdentity, AppName, ExecutionIdentity, InvocationId, SessionId, UserId};
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

    /// Returns the application name as a typed [`AppName`].
    ///
    /// Parses the value returned by [`app_name()`](Self::app_name). Returns an
    /// error if the raw string fails validation (empty, null bytes, or exceeds
    /// the maximum length).
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_app_name(&self) -> Result<AppName> {
        Ok(AppName::try_from(self.app_name())?)
    }

    /// Returns the user identifier as a typed [`UserId`].
    ///
    /// Parses the value returned by [`user_id()`](Self::user_id). Returns an
    /// error if the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_user_id(&self) -> Result<UserId> {
        Ok(UserId::try_from(self.user_id())?)
    }

    /// Returns the session identifier as a typed [`SessionId`].
    ///
    /// Parses the value returned by [`session_id()`](Self::session_id).
    /// Returns an error if the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_session_id(&self) -> Result<SessionId> {
        Ok(SessionId::try_from(self.session_id())?)
    }

    /// Returns the invocation identifier as a typed [`InvocationId`].
    ///
    /// Parses the value returned by [`invocation_id()`](Self::invocation_id).
    /// Returns an error if the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_invocation_id(&self) -> Result<InvocationId> {
        Ok(InvocationId::try_from(self.invocation_id())?)
    }

    /// Returns the stable session-scoped [`AdkIdentity`] triple.
    ///
    /// Combines [`try_app_name()`](Self::try_app_name),
    /// [`try_user_id()`](Self::try_user_id), and
    /// [`try_session_id()`](Self::try_session_id) into a single composite
    /// identity value.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the three constituent identifiers fail
    /// validation.
    fn try_identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity {
            app_name: self.try_app_name()?,
            user_id: self.try_user_id()?,
            session_id: self.try_session_id()?,
        })
    }

    /// Returns the full per-invocation [`ExecutionIdentity`].
    ///
    /// Combines [`try_identity()`](Self::try_identity) with the invocation,
    /// branch, and agent name from this context.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the four typed identifiers fail validation.
    fn try_execution_identity(&self) -> Result<ExecutionIdentity> {
        Ok(ExecutionIdentity {
            adk: self.try_identity()?,
            invocation_id: self.try_invocation_id()?,
            branch: self.branch().to_string(),
            agent_name: self.agent_name().to_string(),
        })
    }
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
    /// Returns conversation history filtered for a specific agent.
    ///
    /// When provided, events authored by other agents (not "user", not the
    /// named agent, and not function/tool responses) are excluded. This
    /// prevents a transferred sub-agent from seeing the parent's tool calls
    /// mapped as "model" role, which would cause the LLM to think work is
    /// already done.
    ///
    /// Default implementation delegates to [`conversation_history`](Self::conversation_history).
    fn conversation_history_for_agent(&self, _agent_name: &str) -> Vec<Content> {
        self.conversation_history()
    }
    /// Append content to conversation history (for sequential agent support)
    fn append_to_history(&self, _content: Content) {
        // Default no-op - implementations can override to track history
    }

    /// Returns the application name as a typed [`AppName`].
    ///
    /// Parses the value returned by [`app_name()`](Self::app_name). Returns an
    /// error if the raw string fails validation (empty, null bytes, or exceeds
    /// the maximum length).
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_app_name(&self) -> Result<AppName> {
        Ok(AppName::try_from(self.app_name())?)
    }

    /// Returns the user identifier as a typed [`UserId`].
    ///
    /// Parses the value returned by [`user_id()`](Self::user_id). Returns an
    /// error if the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_user_id(&self) -> Result<UserId> {
        Ok(UserId::try_from(self.user_id())?)
    }

    /// Returns the session identifier as a typed [`SessionId`].
    ///
    /// Parses the value returned by [`id()`](Self::id). Returns an error if
    /// the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](crate::AdkError::Config) when the
    /// underlying string is not a valid identifier.
    fn try_session_id(&self) -> Result<SessionId> {
        Ok(SessionId::try_from(self.id())?)
    }

    /// Returns the stable session-scoped [`AdkIdentity`] triple.
    ///
    /// Combines [`try_app_name()`](Self::try_app_name),
    /// [`try_user_id()`](Self::try_user_id), and
    /// [`try_session_id()`](Self::try_session_id) into a single composite
    /// identity value.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the three constituent identifiers fail
    /// validation.
    fn try_identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity {
            app_name: self.try_app_name()?,
            user_id: self.try_user_id()?,
            session_id: self.try_session_id()?,
        })
    }
}

/// Structured metadata about a completed tool execution.
///
/// Available via [`CallbackContext::tool_outcome()`] in after-tool callbacks,
/// plugins, and telemetry hooks. Provides structured access to execution
/// results without requiring JSON error parsing.
///
/// # Fields
///
/// - `tool_name` — Name of the tool that was executed.
/// - `tool_args` — Arguments passed to the tool as a JSON value.
/// - `success` — Whether the tool execution succeeded. Derived from the
///   Rust `Result` / timeout path, never from JSON content inspection.
/// - `duration` — Wall-clock duration of the tool execution.
/// - `error_message` — Error message if the tool failed; `None` on success.
/// - `attempt` — Retry attempt number (0 = first attempt, 1 = first retry, etc.).
///   Always 0 when retries are not configured.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    /// Name of the tool that was executed.
    pub tool_name: String,
    /// Arguments passed to the tool (JSON value).
    pub tool_args: serde_json::Value,
    /// Whether the tool execution succeeded.
    pub success: bool,
    /// Wall-clock duration of the tool execution.
    pub duration: std::time::Duration,
    /// Error message if the tool failed. `None` on success.
    pub error_message: Option<String>,
    /// Retry attempt number (0 = first attempt, 1 = first retry, etc.).
    /// Always 0 when retries are not configured.
    pub attempt: u32,
}

#[async_trait]
pub trait CallbackContext: ReadonlyContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;

    /// Returns structured metadata about the most recent tool execution.
    /// Available in after-tool callbacks and plugin hooks.
    /// Returns `None` when not in a tool execution context.
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        None // default for backward compatibility
    }
}

#[async_trait]
pub trait InvocationContext: CallbackContext {
    fn agent(&self) -> Arc<dyn Agent>;
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    fn session(&self) -> &dyn Session;
    fn run_config(&self) -> &RunConfig;
    fn end_invocation(&self);
    fn ended(&self) -> bool;

    /// Returns the scopes granted to the current user for this invocation.
    ///
    /// When a [`RequestContext`](crate::RequestContext) is present (set by the
    /// server's auth middleware bridge), this returns the scopes from that
    /// context. The default returns an empty vec (no scopes granted).
    fn user_scopes(&self) -> Vec<String> {
        vec![]
    }

    /// Returns the request metadata from the auth middleware bridge, if present.
    ///
    /// This provides access to custom key-value pairs extracted from the HTTP
    /// request by the [`RequestContextExtractor`](crate::RequestContext).
    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
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

    /// Verify backend connectivity.
    ///
    /// The default implementation succeeds, which is suitable for in-memory
    /// implementations and adapters without an external dependency.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
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
    /// Valid agent names this agent can transfer to (parent, peers, children).
    /// Set by the runner when invoking agents in a multi-agent tree.
    /// When non-empty, the `transfer_to_agent` tool is injected and validation
    /// uses this list instead of only checking `sub_agents`.
    pub transfer_targets: Vec<String>,
    /// The name of the parent agent, if this agent was invoked via transfer.
    /// Used by the agent to apply `disallow_transfer_to_parent` filtering.
    pub parent_agent: Option<String>,
    /// Enable automatic prompt caching for all providers that support it.
    ///
    /// When `true` (the default), the runner enables provider-level caching:
    /// - Anthropic: sets `prompt_caching = true` on the config
    /// - Bedrock: sets `prompt_caching = Some(BedrockCacheConfig::default())`
    /// - OpenAI / DeepSeek: no action needed (caching is automatic)
    /// - Gemini: handled separately via `ContextCacheConfig`
    pub auto_cache: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            streaming_mode: StreamingMode::SSE,
            tool_confirmation_decisions: HashMap::new(),
            cached_content: None,
            transfer_targets: Vec::new(),
            parent_agent: None,
            auto_cache: true,
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
