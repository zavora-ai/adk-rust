use crate::{
    Agent, Result,
    types::{AdkIdentity, Content, InvocationId, SessionId, UserId},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Foundation for all ADK contexts.
///
/// This trait provides read-only access to the foundational identifiers of an
/// ADK execution. It is purposely NOT async to allow usage in hot paths and
/// synchronization primitives without overhead.
pub trait ReadonlyContext: Send + Sync {
    /// Returns the consolidated identity capsule for this context.
    fn identity(&self) -> &AdkIdentity;

    /// Convenience: returns the invocation ID.
    fn invocation_id(&self) -> &InvocationId {
        &self.identity().invocation_id
    }

    /// Convenience: returns the agent name.
    fn agent_name(&self) -> &str {
        &self.identity().agent_name
    }

    /// Convenience: returns the user ID.
    fn user_id(&self) -> &UserId {
        &self.identity().user_id
    }

    /// Convenience: returns the app name.
    fn app_name(&self) -> &str {
        &self.identity().app_name
    }

    /// Convenience: returns the session ID.
    fn session_id(&self) -> &SessionId {
        &self.identity().session_id
    }

    /// Convenience: returns the branch name.
    fn branch(&self) -> &str {
        &self.identity().branch
    }

    /// Returns the initial user content that triggered this context.
    fn user_content(&self) -> &Content;

    /// Returns the metadata map for platform-specific identifiers.
    fn metadata(&self) -> &HashMap<String, String>;
}

impl<T: ?Sized + ReadonlyContext> ReadonlyContext for Arc<T> {
    fn identity(&self) -> &AdkIdentity {
        (**self).identity()
    }
    fn user_content(&self) -> &Content {
        (**self).user_content()
    }
    fn metadata(&self) -> &HashMap<String, String> {
        (**self).metadata()
    }
}

/// A concrete, domain-focused implementation of `ReadonlyContext`.
///
/// This struct holds the foundational identifiers for an ADK execution (Invocation, Session, etc.)
/// without being tied to any specific observability framework.
///
/// It is the standard, lightweight context implementation for use cases where the full `Runner`
/// environment is not required (e.g., lightweight tools, simple agents, or tests).
///
/// # Extensibility
///
/// This struct is designed to be reusable and extendable. For example, high-fidelity observability
/// can be added by importing the `TraceContextExt` trait from `adk-telemetry`, which implements
/// tracing logic on top of any `ReadonlyContext`.
///
/// Tracing capabilities are provided as extension traits in `adk-telemetry`.
#[derive(Debug, Clone, Default)]
pub struct AdkContext {
    identity: AdkIdentity,
    user_content: Content,
    /// Extensible metadata for any framework-specific attributes.
    metadata: HashMap<String, String>,
}

impl AdkContext {
    /// Create a new builder for `AdkContext`.
    pub fn builder() -> AdkContextBuilder {
        AdkContextBuilder::default()
    }

    /// Update the branch name.
    pub fn set_branch(&mut self, branch: impl Into<String>) {
        self.identity.branch = branch.into();
    }
}

/// Fluent builder for `AdkContext` following Rust API guidelines.
#[derive(Debug, Clone, Default)]
pub struct AdkContextBuilder {
    identity: AdkIdentity,
    user_content: Option<Content>,
    metadata: HashMap<String, String>,
}

impl AdkContextBuilder {
    pub fn invocation_id(mut self, id: impl Into<InvocationId>) -> Self {
        self.identity.invocation_id = id.into();
        self
    }

    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.identity.agent_name = name.into();
        self
    }

    pub fn user_id(mut self, id: impl Into<UserId>) -> Self {
        self.identity.user_id = id.into();
        self
    }

    pub fn app_name(mut self, name: impl Into<String>) -> Self {
        self.identity.app_name = name.into();
        self
    }

    pub fn session_id(mut self, id: impl Into<SessionId>) -> Self {
        self.identity.session_id = id.into();
        self
    }

    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.identity.branch = branch.into();
        self
    }

    pub fn user_content(mut self, content: impl Into<Content>) -> Self {
        self.user_content = Some(content.into());
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> AdkContext {
        AdkContext {
            identity: self.identity,
            user_content: self.user_content.unwrap_or_default(),
            metadata: self.metadata,
        }
    }
}

impl ReadonlyContext for AdkContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }

    fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
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
    fn id(&self) -> &SessionId;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &UserId;
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

    #[test]
    fn test_adk_context_builder() {
        let ctx = AdkContext::builder()
            .invocation_id(crate::types::InvocationId::new("inv-123").unwrap())
            .agent_name("test-agent")
            .user_id(crate::types::UserId::new("user-456").unwrap())
            .session_id(crate::types::SessionId::new("sess-789").unwrap())
            .metadata("custom.key", "custom-value")
            .build();

        let id = ctx.identity();
        assert_eq!(id.invocation_id.as_str(), "inv-123");
        assert_eq!(id.agent_name, "test-agent");
        assert_eq!(id.user_id.as_str(), "user-456");
        assert_eq!(id.session_id.as_str(), "sess-789");
        assert_eq!(ctx.app_name(), "adk-app"); // Default
        assert_eq!(ctx.metadata().get("custom.key").unwrap(), "custom-value");
    }
}
