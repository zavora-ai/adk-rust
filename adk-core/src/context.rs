use crate::identity::{AdkIdentity, AppName, ExecutionIdentity, InvocationId, SessionId, UserId};
use crate::{AdkError, Agent, Result, types::Content};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Policy for handling excess tool calls when the concurrency limit is reached.
///
/// Determines whether tool calls that exceed the configured concurrency limit
/// should wait in a queue or fail immediately.
///
/// # Example
///
/// ```rust
/// use adk_core::BackpressurePolicy;
///
/// // Default is Queue
/// let policy = BackpressurePolicy::default();
/// assert!(matches!(policy, BackpressurePolicy::Queue));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BackpressurePolicy {
    /// Queue excess calls until a permit becomes available.
    ///
    /// This is the default policy. Tool calls will await until a semaphore
    /// permit is released by a completing tool execution.
    #[default]
    Queue,

    /// Fail immediately with a concurrency limit error when no permit is available.
    ///
    /// Use this when latency is more important than throughput — callers receive
    /// an immediate error rather than waiting indefinitely.
    Fail,
}

/// Configuration for tool execution concurrency.
///
/// Controls how many tool calls can execute simultaneously, with support for
/// global limits, per-tool overrides, and configurable backpressure behavior.
///
/// # Example
///
/// ```rust
/// use adk_core::{BackpressurePolicy, ToolConcurrencyConfig};
/// use std::collections::HashMap;
///
/// let config = ToolConcurrencyConfig {
///     max_concurrency: Some(10),
///     per_tool: HashMap::from([
///         ("web_scraper".to_string(), 2),
///         ("calculator".to_string(), 8),
///     ]),
///     backpressure: BackpressurePolicy::Fail,
/// };
///
/// assert_eq!(config.max_concurrency, Some(10));
/// assert_eq!(config.per_tool.get("web_scraper"), Some(&2));
/// ```
#[derive(Debug, Clone, Default)]
pub struct ToolConcurrencyConfig {
    /// Global maximum concurrent tool calls. `None` means unlimited.
    pub max_concurrency: Option<usize>,

    /// Per-tool concurrency overrides. When a tool name is present in this map,
    /// its individual limit takes precedence over the global `max_concurrency`.
    pub per_tool: HashMap<String, usize>,

    /// What to do when the concurrency limit is reached.
    pub backpressure: BackpressurePolicy,
}

/// Read-only access to invocation metadata.
///
/// Provides identity information (user, app, session, invocation) and the
/// current user content. Implemented by all context types.
#[async_trait]
pub trait ReadonlyContext: Send + Sync {
    /// Returns the current invocation identifier.
    fn invocation_id(&self) -> &str;
    /// Returns the name of the currently executing agent.
    fn agent_name(&self) -> &str;
    /// Returns the user identifier for this session.
    fn user_id(&self) -> &str;
    /// Returns the application name for this session.
    fn app_name(&self) -> &str;
    /// Returns the session identifier.
    fn session_id(&self) -> &str;
    /// Returns the current conversation branch.
    fn branch(&self) -> &str;
    /// Returns the user's input content for this invocation.
    fn user_content(&self) -> &Content;

    /// Returns the application name as a typed [`AppName`].
    ///
    /// Parses the value returned by [`app_name()`](Self::app_name). Returns an
    /// error if the raw string fails validation (empty, null bytes, or exceeds
    /// the maximum length).
    ///
    /// # Errors
    ///
    /// Returns an error when the
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
    /// Returns an error when the
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
    /// Returns an error when the
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
    /// Returns an error when the
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

/// Mutable session state with key-value storage.
///
/// Implementations persist state across turns within a session.
pub trait State: Send + Sync {
    /// Returns the value for the given key, or `None` if not present.
    fn get(&self, key: &str) -> Option<Value>;
    /// Set a state value. Implementations should call [`validate_state_key`] and
    /// reject invalid keys (e.g., by logging a warning or panicking).
    fn set(&mut self, key: String, value: Value);
    /// Returns all key-value pairs in the state.
    fn all(&self) -> HashMap<String, Value>;
}

/// Read-only view of session state.
pub trait ReadonlyState: Send + Sync {
    /// Returns the value for the given key, or `None` if not present.
    fn get(&self, key: &str) -> Option<Value>;
    /// Returns all key-value pairs in the state.
    fn all(&self) -> HashMap<String, Value>;
}

// Session trait
/// Represents an active conversation session with identity and state.
pub trait Session: Send + Sync {
    /// Returns the session identifier.
    fn id(&self) -> &str;
    /// Returns the application name this session belongs to.
    fn app_name(&self) -> &str;
    /// Returns the user identifier for this session.
    fn user_id(&self) -> &str;
    /// Returns the mutable state associated with this session.
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
    /// Returns an error when the
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
    /// Returns an error when the
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
    /// Returns an error when the
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

/// Context available to agent lifecycle callbacks.
///
/// Extends [`ReadonlyContext`] with access to artifacts and tool execution metadata.
#[async_trait]
pub trait CallbackContext: ReadonlyContext {
    /// Returns the artifact store, if one is configured.
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;

    /// Returns structured metadata about the most recent tool execution.
    /// Available in after-tool callbacks and plugin hooks.
    /// Returns `None` when not in a tool execution context.
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        None // default for backward compatibility
    }

    /// Returns the name of the tool about to be executed.
    /// Available in before-tool and after-tool callback contexts.
    fn tool_name(&self) -> Option<&str> {
        None
    }

    /// Returns the input arguments for the tool about to be executed.
    /// Available in before-tool and after-tool callback contexts.
    fn tool_input(&self) -> Option<&serde_json::Value> {
        None
    }

    /// Returns the shared state for parallel agent coordination.
    /// Returns `None` when not running inside a `ParallelAgent` with shared state enabled.
    fn shared_state(&self) -> Option<Arc<crate::SharedState>> {
        None
    }
}

/// Wraps a [`CallbackContext`] to inject tool name and input for before-tool
/// and after-tool callbacks.
///
/// Used by the agent runtime to provide tool context to `BeforeToolCallback`
/// and `AfterToolCallback` invocations.
///
/// # Example
///
/// ```rust,ignore
/// let tool_ctx = Arc::new(ToolCallbackContext::new(
///     ctx.clone(),
///     "search".to_string(),
///     serde_json::json!({"query": "hello"}),
/// ));
/// callback(tool_ctx as Arc<dyn CallbackContext>).await;
/// ```
pub struct ToolCallbackContext {
    /// The inner callback context to delegate to.
    pub inner: Arc<dyn CallbackContext>,
    /// The name of the tool being executed.
    pub tool_name: String,
    /// The input arguments for the tool being executed.
    pub tool_input: serde_json::Value,
}

impl ToolCallbackContext {
    /// Creates a new `ToolCallbackContext` wrapping the given inner context.
    pub fn new(
        inner: Arc<dyn CallbackContext>,
        tool_name: String,
        tool_input: serde_json::Value,
    ) -> Self {
        Self { inner, tool_name, tool_input }
    }
}

#[async_trait]
impl ReadonlyContext for ToolCallbackContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }

    fn user_id(&self) -> &str {
        self.inner.user_id()
    }

    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    fn session_id(&self) -> &str {
        self.inner.session_id()
    }

    fn branch(&self) -> &str {
        self.inner.branch()
    }

    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for ToolCallbackContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }

    fn tool_outcome(&self) -> Option<ToolOutcome> {
        self.inner.tool_outcome()
    }

    fn tool_name(&self) -> Option<&str> {
        Some(&self.tool_name)
    }

    fn tool_input(&self) -> Option<&serde_json::Value> {
        Some(&self.tool_input)
    }

    fn shared_state(&self) -> Option<Arc<crate::SharedState>> {
        self.inner.shared_state()
    }
}

/// Full invocation context available to agents during execution.
///
/// Extends [`CallbackContext`] with access to the agent itself, memory,
/// session, and run configuration.
#[async_trait]
pub trait InvocationContext: CallbackContext {
    /// Returns the agent being executed.
    fn agent(&self) -> Arc<dyn Agent>;
    /// Returns the memory service, if one is configured.
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    /// Returns the current session.
    fn session(&self) -> &dyn Session;
    /// Returns the run configuration for this invocation.
    fn run_config(&self) -> &RunConfig;
    /// Signals that this invocation should end after the current turn.
    fn end_invocation(&self);
    /// Returns whether the invocation has been ended.
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

    /// Retrieve a secret by name from the configured secret provider.
    ///
    /// Returns `Ok(Some(value))` when a provider is configured and the secret
    /// exists, `Ok(None)` when no provider is configured, or an error on
    /// provider failure. The default returns `Ok(None)`.
    async fn get_secret(&self, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

// Placeholder service traits
/// Binary artifact storage for agents.
#[async_trait]
pub trait Artifacts: Send + Sync {
    /// Saves a binary artifact and returns its version number.
    async fn save(&self, name: &str, data: &crate::Part) -> Result<i64>;
    /// Loads a binary artifact by name.
    async fn load(&self, name: &str) -> Result<crate::Part>;
    /// Lists all artifact names.
    async fn list(&self) -> Result<Vec<String>>;
}

/// Semantic memory search for agents.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Searches memory for entries matching the query.
    async fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;

    /// Verify backend connectivity.
    ///
    /// The default implementation succeeds, which is suitable for in-memory
    /// implementations and adapters without an external dependency.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Add a single memory entry.
    ///
    /// The default implementation returns an "not implemented" error, which is
    /// suitable for read-only memory backends.
    async fn add(&self, entry: MemoryEntry) -> Result<()> {
        let _ = entry;
        Err(AdkError::memory("add not implemented"))
    }

    /// Delete entries matching a query. Returns count of deleted entries.
    ///
    /// The default implementation returns an "not implemented" error, which is
    /// suitable for read-only memory backends.
    async fn delete(&self, query: &str) -> Result<u64> {
        let _ = query;
        Err(AdkError::memory("delete not implemented"))
    }

    /// Search for memories within a specific project.
    /// Returns global entries + entries for the given project.
    /// Default delegates to `search` (global-only results).
    async fn search_in_project(&self, query: &str, project_id: &str) -> Result<Vec<MemoryEntry>> {
        let _ = project_id;
        self.search(query).await
    }

    /// Add a memory entry scoped to a specific project.
    /// Default delegates to `add` (global entry).
    async fn add_to_project(&self, entry: MemoryEntry, project_id: &str) -> Result<()> {
        let _ = project_id;
        self.add(entry).await
    }
}

/// Trait for retrieving secrets at runtime.
///
/// This is the core-level abstraction used by [`ToolContext::get_secret`] and
/// [`InvocationContext::get_secret`]. Concrete implementations (e.g., AWS
/// Secrets Manager, Azure Key Vault, GCP Secret Manager) live in `adk-auth`
/// behind feature flags and implement this trait via the `SecretProvider`
/// adapter.
///
/// # Example
///
/// ```rust,ignore
/// use adk_core::SecretService;
///
/// struct EnvSecretService;
///
/// #[async_trait::async_trait]
/// impl SecretService for EnvSecretService {
///     async fn get_secret(&self, name: &str) -> adk_core::Result<String> {
///         std::env::var(name).map_err(|_| adk_core::AdkError::not_found(
///             format!("secret '{name}' not found in environment"),
///         ))
///     }
/// }
/// ```
#[async_trait]
pub trait SecretService: Send + Sync {
    /// Retrieve a secret value by name.
    ///
    /// Returns the secret string on success, or an [`AdkError`] on failure.
    async fn get_secret(&self, name: &str) -> Result<String>;
}

/// A single entry returned from memory search.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    /// The content of this memory entry.
    pub content: Content,
    /// The author who created this memory entry.
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
    /// Approve the tool execution.
    Approve,
    /// Deny the tool execution.
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
    /// Name of the tool awaiting confirmation.
    pub tool_name: String,
    /// The function call ID from the LLM, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call_id: Option<String>,
    /// Arguments the tool would be called with.
    pub args: Value,
}

/// Configuration for a single agent run.
///
/// Controls streaming behavior, tool confirmation, caching, transfer targets,
/// and concurrency settings. Use [`RunConfig::builder()`] to construct from
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The streaming mode for agent responses.
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
    /// Maximum number of recent persisted events to load at the start of a run.
    ///
    /// `None` preserves the previous behavior and loads the full session
    /// history. Set this for chat surfaces that already summarize older turns
    /// and need predictable startup latency.
    pub history_max_events: Option<usize>,
    /// Tool concurrency configuration controlling parallel tool dispatch limits,
    /// per-tool overrides, and backpressure behavior.
    ///
    /// The default (`ToolConcurrencyConfig::default()`) imposes no limits,
    /// preserving backward compatibility with the previous `max_tool_concurrency: None`.
    pub tool_concurrency: ToolConcurrencyConfig,
    /// Whether tracing spans may include full request, response, and tool
    /// payloads when the `record-payloads` crate feature is enabled.
    pub record_payloads: bool,
    /// Maximum serialized bytes recorded for tracing payload fields when full
    /// payload recording is disabled.
    pub trace_payload_max_bytes: usize,
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
            history_max_events: None,
            tool_concurrency: ToolConcurrencyConfig::default(),
            record_payloads: false,
            trace_payload_max_bytes: 2048,
        }
    }
}

impl RunConfig {
    /// Creates a new [`RunConfigBuilder`] initialized with default values.
    ///
    /// Use the builder to construct a `RunConfig` when struct literal syntax
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{RunConfig, StreamingMode};
    ///
    /// let config = RunConfig::builder()
    ///     .streaming_mode(StreamingMode::None)
    ///     .auto_cache(false)
    ///     .build();
    ///
    /// assert_eq!(config.streaming_mode, StreamingMode::None);
    /// assert!(!config.auto_cache);
    /// ```
    pub fn builder() -> RunConfigBuilder {
        RunConfigBuilder::default()
    }
}

/// Builder for [`RunConfig`].
///
/// Provides a fluent API for constructing `RunConfig` instances. All fields
/// start with their default values and can be overridden individually.
///
/// # Example
///
/// ```rust
/// use adk_core::{RunConfig, RunConfigBuilder, StreamingMode, ToolConcurrencyConfig};
///
/// let config = RunConfigBuilder::default()
///     .streaming_mode(StreamingMode::Bidi)
///     .history_max_events(Some(50))
///     .build();
/// ```
#[derive(Debug, Clone, Default)]
pub struct RunConfigBuilder {
    config: RunConfig,
}

impl RunConfigBuilder {
    /// Sets the streaming mode for the run.
    pub fn streaming_mode(mut self, mode: StreamingMode) -> Self {
        self.config.streaming_mode = mode;
        self
    }

    /// Sets per-tool confirmation decisions for the current run.
    pub fn tool_confirmation_decisions(
        mut self,
        decisions: HashMap<String, ToolConfirmationDecision>,
    ) -> Self {
        self.config.tool_confirmation_decisions = decisions;
        self
    }

    /// Sets the cached content name for automatic prompt caching.
    pub fn cached_content(mut self, name: impl Into<String>) -> Self {
        self.config.cached_content = Some(name.into());
        self
    }

    /// Sets the valid agent names this agent can transfer to.
    pub fn transfer_targets(mut self, targets: Vec<String>) -> Self {
        self.config.transfer_targets = targets;
        self
    }

    /// Sets the parent agent name.
    pub fn parent_agent(mut self, name: impl Into<String>) -> Self {
        self.config.parent_agent = Some(name.into());
        self
    }

    /// Enables or disables automatic prompt caching for supported providers.
    pub fn auto_cache(mut self, enabled: bool) -> Self {
        self.config.auto_cache = enabled;
        self
    }

    /// Sets the maximum number of recent persisted events to load at run start.
    pub fn history_max_events(mut self, max: Option<usize>) -> Self {
        self.config.history_max_events = max;
        self
    }

    /// Sets the tool concurrency configuration.
    pub fn tool_concurrency(mut self, config: ToolConcurrencyConfig) -> Self {
        self.config.tool_concurrency = config;
        self
    }

    /// Enables or disables full payload recording in tracing spans.
    pub fn record_payloads(mut self, enabled: bool) -> Self {
        self.config.record_payloads = enabled;
        self
    }

    /// Sets the maximum serialized bytes for tracing payload fields.
    pub fn trace_payload_max_bytes(mut self, max: usize) -> Self {
        self.config.trace_payload_max_bytes = max;
        self
    }

    /// Consumes the builder and returns the configured [`RunConfig`].
    pub fn build(self) -> RunConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_config_default() {
        let config = RunConfig::default();
        assert_eq!(config.streaming_mode, StreamingMode::SSE);
        assert_eq!(config.history_max_events, None);
        assert_eq!(config.tool_concurrency.max_concurrency, None);
        assert!(config.tool_concurrency.per_tool.is_empty());
        assert_eq!(config.tool_concurrency.backpressure, BackpressurePolicy::Queue);
        assert!(!config.record_payloads);
        assert_eq!(config.trace_payload_max_bytes, 2048);
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
    fn test_run_config_builder_defaults() {
        let config = RunConfig::builder().build();
        let default = RunConfig::default();
        assert_eq!(config.streaming_mode, default.streaming_mode);
        assert_eq!(config.auto_cache, default.auto_cache);
        assert_eq!(config.history_max_events, default.history_max_events);
        assert_eq!(config.record_payloads, default.record_payloads);
        assert_eq!(config.trace_payload_max_bytes, default.trace_payload_max_bytes);
        assert!(config.tool_confirmation_decisions.is_empty());
        assert!(config.transfer_targets.is_empty());
        assert!(config.cached_content.is_none());
        assert!(config.parent_agent.is_none());
    }

    #[test]
    fn test_run_config_builder_all_fields() {
        let mut decisions = HashMap::new();
        decisions.insert("delete".to_string(), ToolConfirmationDecision::Approve);

        let config = RunConfig::builder()
            .streaming_mode(StreamingMode::None)
            .tool_confirmation_decisions(decisions.clone())
            .cached_content("my-cache")
            .transfer_targets(vec!["agent_a".to_string(), "agent_b".to_string()])
            .parent_agent("parent")
            .auto_cache(false)
            .history_max_events(Some(50))
            .tool_concurrency(ToolConcurrencyConfig {
                max_concurrency: Some(4),
                per_tool: HashMap::new(),
                backpressure: BackpressurePolicy::Fail,
            })
            .record_payloads(true)
            .trace_payload_max_bytes(4096)
            .build();

        assert_eq!(config.streaming_mode, StreamingMode::None);
        assert_eq!(config.tool_confirmation_decisions, decisions);
        assert_eq!(config.cached_content.as_deref(), Some("my-cache"));
        assert_eq!(config.transfer_targets, vec!["agent_a", "agent_b"]);
        assert_eq!(config.parent_agent.as_deref(), Some("parent"));
        assert!(!config.auto_cache);
        assert_eq!(config.history_max_events, Some(50));
        assert_eq!(config.tool_concurrency.max_concurrency, Some(4));
        assert_eq!(config.tool_concurrency.backpressure, BackpressurePolicy::Fail);
        assert!(config.record_payloads);
        assert_eq!(config.trace_payload_max_bytes, 4096);
    }
}
