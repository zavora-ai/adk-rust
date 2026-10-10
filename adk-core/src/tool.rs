use crate::{
    CallbackContext, Event, EventActions, LlmRequest, Memory, MemoryEntry, ReadonlyContext, Result,
    RunConfig, Session,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// The core trait for all tools that agents can invoke.
///
/// Tools extend agent capabilities with custom functions. Each tool has a name,
/// description, optional parameter schema, and an async `execute` method.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the unique name of this tool.
    fn name(&self) -> &str;
    /// Returns a human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// Returns the tool declaration that should be exposed to model providers.
    ///
    /// The default implementation produces the standard ADK function-tool
    /// declaration (`name`, `description`, optional `parameters`, optional
    /// `response`). Provider-specific built-in tools may override this to attach
    /// additional metadata that the provider adapters understand.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn declaration(&self) -> serde_json::Value {
    ///     serde_json::json!({
    ///         "name": self.name(),
    ///         "description": self.description(),
    ///         "x-adk-openai-tool": {
    ///             "type": "web_search"
    ///         }
    ///     })
    /// }
    /// ```
    fn declaration(&self) -> Value {
        let mut decl = serde_json::json!({
            "name": self.name(),
            "description": self.enhanced_description(),
        });

        if let Some(params) = self.parameters_schema() {
            decl["parameters"] = params;
        }

        if let Some(response) = self.response_schema() {
            decl["response"] = response;
        }

        decl
    }

    /// Returns an enhanced description that may include additional notes.
    /// For long-running tools, this includes a warning not to call the tool
    /// again if it has already returned a pending status.
    /// Default implementation returns the base description.
    fn enhanced_description(&self) -> String {
        self.description().to_string()
    }

    /// Indicates whether the tool is a long-running operation.
    /// Long-running tools typically return a task ID immediately and
    /// complete the operation asynchronously.
    fn is_long_running(&self) -> bool {
        false
    }

    /// Indicates whether this tool is a built-in server-side tool (e.g., `google_search`, `url_context`).
    ///
    /// Built-in tools are executed server-side by the model provider and should not be
    /// executed locally by the agent. The default implementation returns `false`.
    fn is_builtin(&self) -> bool {
        false
    }

    /// Returns the JSON Schema for this tool's parameters, if any.
    fn parameters_schema(&self) -> Option<Value> {
        None
    }
    /// Returns the JSON Schema for this tool's response, if any.
    fn response_schema(&self) -> Option<Value> {
        None
    }

    /// Returns the scopes required to execute this tool.
    ///
    /// When non-empty, the framework can enforce that the calling user
    /// possesses **all** listed scopes before dispatching `execute()`.
    /// The default implementation returns an empty slice (no scopes required).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn required_scopes(&self) -> &[&str] {
    ///     &["finance:write", "verified"]
    /// }
    /// ```
    fn required_scopes(&self) -> &[&str] {
        &[]
    }

    /// Indicates whether this tool performs no side effects.
    ///
    /// [`ToolExecutionStrategy::Auto`] includes a call in its concurrent subset
    /// only when the selected tool is both read-only and concurrency-safe.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Returns the side-effect class of this tool.
    ///
    /// The runtime retries a failed call only when the effect is
    /// [`ToolEffect::ReadOnly`] or [`ToolEffect::Idempotent`] and the error is
    /// retryable. The default derives the effect from [`Tool::is_read_only`], so
    /// a tool that declares nothing is treated as non-idempotent and never
    /// retried.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Result, Tool, ToolContext, ToolEffect};
    /// use serde_json::{Value, json};
    /// use std::sync::Arc;
    ///
    /// struct SetThermostat;
    ///
    /// #[async_trait::async_trait]
    /// impl Tool for SetThermostat {
    ///     fn name(&self) -> &str {
    ///         "set_thermostat"
    ///     }
    ///     fn description(&self) -> &str {
    ///         "Sets the target temperature"
    ///     }
    ///     // Setting the same target twice leaves the same state as setting it once.
    ///     fn effect(&self) -> ToolEffect {
    ///         ToolEffect::Idempotent
    ///     }
    ///     async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    ///         Ok(json!({ "target": args["celsius"] }))
    ///     }
    /// }
    ///
    /// assert_eq!(SetThermostat.effect(), ToolEffect::Idempotent);
    /// ```
    fn effect(&self) -> ToolEffect {
        if self.is_read_only() { ToolEffect::ReadOnly } else { ToolEffect::NonIdempotent }
    }

    /// Overrides the per-call timeout the agent applies to this tool.
    ///
    /// | Value | Timeout applied to each call |
    /// |-------|------------------------------|
    /// | `None` (default) | The agent's own tool timeout |
    /// | `Some(None)` | None; the call runs until it finishes or the run is cancelled |
    /// | `Some(Some(duration))` | `duration` instead of the agent's tool timeout |
    ///
    /// Agent-as-tool delegations use this so a long sub-agent run is bounded by
    /// its own configuration rather than by a timeout sized for ordinary calls.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Result, Tool, ToolContext};
    /// use serde_json::Value;
    /// use std::{sync::Arc, time::Duration};
    ///
    /// struct BuildReport;
    ///
    /// #[async_trait::async_trait]
    /// impl Tool for BuildReport {
    ///     fn name(&self) -> &str {
    ///         "build_report"
    ///     }
    ///     fn description(&self) -> &str {
    ///         "Builds the quarterly report"
    ///     }
    ///     fn timeout_override(&self) -> Option<Option<Duration>> {
    ///         Some(Some(Duration::from_secs(30 * 60)))
    ///     }
    ///     async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
    ///         Ok(Value::Null)
    ///     }
    /// }
    ///
    /// assert_eq!(BuildReport.timeout_override(), Some(Some(Duration::from_secs(1800))));
    /// ```
    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        None
    }

    /// Indicates whether this tool is safe for concurrent execution.
    ///
    /// [`ToolExecutionStrategy::Auto`] requires this signal in addition to
    /// [`Tool::is_read_only`]. [`ToolExecutionStrategy::Parallel`] is an
    /// explicit caller override and does not inspect either signal.
    fn is_concurrency_safe(&self) -> bool {
        false
    }

    /// Indicates whether this tool delegates work to another agent.
    ///
    /// Agent implementations can use this marker to overlap consecutive
    /// delegation calls while preserving sequential execution for ordinary
    /// tools.
    fn is_agent_delegation(&self) -> bool {
        false
    }

    /// Executes the tool with the given context and arguments.
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}

/// Context available to tools during execution.
///
/// Extends [`CallbackContext`] with tool-specific operations like accessing
/// the function call ID, managing event actions, and searching memory.
#[async_trait]
pub trait ToolContext: CallbackContext {
    /// Returns the function call ID for this tool invocation.
    fn function_call_id(&self) -> &str;
    /// Returns the key that identifies this tool call across retries and replays.
    ///
    /// The default is `"{app}/{user}/{session}/{invocation}/{function_call_id}"`.
    /// Every attempt of one call shares the key and no other call has it, so a
    /// tool can pass it to a downstream API that accepts an idempotency key.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    ///     let key = ctx.idempotency_key();
    ///     self.payments.charge(&args, &key).await
    /// }
    /// ```
    fn idempotency_key(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.app_name(),
            self.user_id(),
            self.session_id(),
            self.invocation_id(),
            self.function_call_id()
        )
    }
    /// Get the current event actions. Returns an owned copy for thread safety.
    fn actions(&self) -> EventActions;
    /// Set the event actions (e.g., to trigger escalation or skip summarization).
    fn set_actions(&self, actions: EventActions);
    /// Searches memory for entries matching the query.
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>>;

    /// Returns the memory service backing the parent invocation, when exposed.
    ///
    /// The default keeps contexts written before this capability backward
    /// compatible. Agent-as-tool adapters use it only when memory forwarding is
    /// explicitly enabled.
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }

    /// Returns the parent session, when exposed by the runtime tool context.
    ///
    /// Agent-as-tool adapters can snapshot its history and state for a child
    /// invocation without sharing mutable session ownership.
    fn session(&self) -> Option<&dyn Session> {
        None
    }

    /// Returns the parent run configuration, when exposed by the runtime.
    fn run_config(&self) -> Option<&RunConfig> {
        None
    }

    /// Returns whether the parent invocation has been cancelled.
    fn is_cancelled(&self) -> bool {
        false
    }

    /// Returns authenticated request metadata inherited from the parent run.
    fn request_metadata(&self) -> std::collections::HashMap<String, Value> {
        std::collections::HashMap::new()
    }

    /// Returns the current nested agent-as-tool delegation depth.
    fn delegation_depth(&self) -> u32 {
        0
    }

    /// Returns the maximum nested agent-as-tool delegation depth.
    fn max_delegation_depth(&self) -> Option<u32> {
        None
    }

    /// Returns the root invocation that owns this orchestration tree.
    fn orchestration_root_invocation_id(&self) -> &str {
        self.invocation_id()
    }

    /// Returns the causal relationship execution containing this tool call.
    fn orchestration_edge_id(&self) -> Option<&str> {
        None
    }

    /// Emits a nested agent event through the parent agent's event stream.
    ///
    /// The default is a no-op. AgentTool uses this only when event forwarding is
    /// explicitly enabled, preserving existing callers' output behavior.
    async fn emit_event(&self, _event: Event) {}

    /// Emit streaming progress output during long-running tool execution.
    ///
    /// Tools call this to push intermediate stdout/stderr to the UI layer
    /// as it arrives, rather than waiting for the tool to finish. This enables
    /// streaming terminal output for shell commands, build logs, etc.
    ///
    /// # Arguments
    ///
    /// * `stream` - The output stream: `"stdout"`, `"stderr"`, or a custom label
    /// * `chunk` - The text chunk to emit
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Inside a tool's execute() method:
    /// ctx.emit_progress("stdout", "Compiling project...\n").await;
    /// ctx.emit_progress("stdout", "Build successful!\n").await;
    /// ctx.emit_progress("stderr", "warning: unused variable\n").await;
    /// ```
    ///
    /// The default implementation is a no-op. Runners and UI layers that support
    /// streaming output override this to forward chunks to the client.
    async fn emit_progress(&self, _stream: &str, _chunk: &str) {
        // Default: discard. Override in runners that support streaming tool output.
    }

    /// Returns the scopes granted to the current user for this invocation.
    ///
    /// Implementations may resolve scopes from session state, JWT claims,
    /// or an external identity provider. The default returns an empty set
    /// (no scopes granted), which means scope-protected tools will be denied
    /// unless the implementation is overridden.
    fn user_scopes(&self) -> Vec<String> {
        vec![]
    }

    /// Retrieve a secret by name from the configured secret provider.
    ///
    /// Returns `Ok(Some(value))` if a secret provider is configured and the
    /// secret exists, `Ok(None)` if no secret provider is configured, or an
    /// error if the provider fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn use_secret(ctx: &dyn ToolContext) -> adk_core::Result<()> {
    ///     if let Some(api_key) = ctx.get_secret("slack-bot-token").await? {
    ///         // use the secret
    ///     }
    ///     Ok(())
    /// }
    /// ```
    async fn get_secret(&self, _name: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Resolves a secret, stating why it is needed.
    ///
    /// The tool identity is added by the framework, not taken from the tool, so a
    /// purpose is the only part a tool contributes. An authorizing
    /// [`SecretService`](crate::SecretService) sees both.
    async fn get_secret_for_purpose(&self, name: &str, purpose: &str) -> Result<Option<String>> {
        let _ = purpose;
        self.get_secret(name).await
    }
}

/// The side-effect class of a tool, reported by [`Tool::effect`].
///
/// | Effect | Retried after a retryable error |
/// |--------|---------------------------------|
/// | `ReadOnly` | Yes |
/// | `Idempotent` | Yes |
/// | `NonIdempotent` | No |
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
///
/// assert!(ToolEffect::Idempotent.is_retry_safe());
/// assert!(!ToolEffect::NonIdempotent.is_retry_safe());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolEffect {
    /// Reads without changing anything.
    ReadOnly,
    /// Changes state, but repeating a call with the same arguments leaves the
    /// same state as making it once.
    Idempotent,
    /// Changes state, and repeating a call repeats the change — a payment, an
    /// email, an order.
    NonIdempotent,
}

impl ToolEffect {
    /// Returns `true` when repeating a call cannot repeat a side effect.
    pub fn is_retry_safe(self) -> bool {
        match self {
            Self::ReadOnly | Self::Idempotent => true,
            Self::NonIdempotent => false,
        }
    }
}

/// Configuration for automatic tool retry on failure.
///
/// A failed call is retried only when its tool's [`Tool::effect`] is
/// [`ToolEffect::ReadOnly`] or [`ToolEffect::Idempotent`] and the error is
/// [retryable](crate::AdkError::is_retryable) — a rate limit, an unavailable
/// dependency, or a timeout. The delay before retry `n` is
/// `delay * 2^(n - 1)`, capped at `max_delay`, with jitter; see
/// [`RetryBudget::backoff_delay`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::RetryBudget;
///
/// // Retry up to 2 times, starting at 500ms (3 total attempts)
/// let budget = RetryBudget::new(2, Duration::from_millis(500))
///     .with_max_delay(Duration::from_secs(5));
/// assert_eq!(budget.max_retries, 2);
/// assert!(budget.backoff_delay(1) <= Duration::from_millis(500));
/// ```
#[derive(Debug, Clone)]
pub struct RetryBudget {
    /// Maximum number of retry attempts (not counting the initial attempt).
    /// E.g., `max_retries: 2` means up to 3 total attempts.
    pub max_retries: u32,
    /// Base delay before the first retry. Each later retry doubles it.
    pub delay: std::time::Duration,
    /// Upper bound on the delay before any one retry.
    pub max_delay: std::time::Duration,
}

impl RetryBudget {
    /// The `max_delay` that [`RetryBudget::new`] applies.
    pub const DEFAULT_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

    /// Create a new retry budget.
    ///
    /// `max_delay` is [`RetryBudget::DEFAULT_MAX_DELAY`], or `delay` when that is
    /// larger.
    ///
    /// # Arguments
    ///
    /// * `max_retries` - Maximum retry attempts (not counting the initial attempt)
    /// * `delay` - Base delay before the first retry
    pub fn new(max_retries: u32, delay: std::time::Duration) -> Self {
        Self { max_retries, delay, max_delay: delay.max(Self::DEFAULT_MAX_DELAY) }
    }

    /// Sets the upper bound on the delay before any one retry.
    pub fn with_max_delay(mut self, max_delay: std::time::Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    /// Returns the delay before retry number `retry`, where `1` is the first retry.
    ///
    /// The ceiling is `delay * 2^(retry - 1)`, capped at `max_delay`. The
    /// returned delay is drawn uniformly from the upper half of that ceiling, so
    /// callers that failed together do not retry in lockstep.
    pub fn backoff_delay(&self, retry: u32) -> std::time::Duration {
        let exponent = retry.saturating_sub(1).min(31);
        let ceiling = self.delay.saturating_mul(1u32 << exponent).min(self.max_delay);
        let ceiling_nanos = ceiling.as_nanos();
        if ceiling_nanos == 0 {
            return std::time::Duration::ZERO;
        }
        let half = ceiling_nanos / 2;
        let jitter = uuid::Uuid::new_v4().as_u128() % (ceiling_nanos - half + 1);
        let nanos = u64::try_from(half + jitter).unwrap_or(u64::MAX);
        std::time::Duration::from_nanos(nanos)
    }
}

/// A collection of tools that can be resolved dynamically from context.
#[async_trait]
pub trait Toolset: Send + Sync {
    /// Returns the name of this toolset.
    fn name(&self) -> &str;
    /// Returns the tools available in this toolset for the given context.
    async fn tools(&self, ctx: Arc<dyn crate::ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>>;

    /// Applies toolset-specific context to an LLM request before each model call.
    ///
    /// Toolsets can use this hook to describe a dynamic tool protocol without
    /// requiring every agent that installs the toolset to repeat that guidance
    /// in its instruction. The default implementation leaves the request
    /// unchanged.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn process_llm_request(
    ///     &self,
    ///     _ctx: std::sync::Arc<dyn adk_core::ReadonlyContext>,
    ///     request: &mut adk_core::LlmRequest,
    /// ) -> adk_core::Result<()> {
    ///     request.contents.insert(0, adk_core::Content::new("user").with_text("Tool guidance"));
    ///     Ok(())
    /// }
    /// ```
    async fn process_llm_request(
        &self,
        _ctx: Arc<dyn ReadonlyContext>,
        _request: &mut LlmRequest,
    ) -> Result<()> {
        Ok(())
    }
}

/// Controls how multiple tool calls from a single LLM response are dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolExecutionStrategy {
    /// Execute tools one at a time in LLM-returned order. Default.
    #[default]
    Sequential,
    /// Execute all tools concurrently without inspecting tool metadata.
    ///
    /// This is an explicit caller override. The caller is responsible for
    /// ensuring every selected tool is safe to execute concurrently.
    Parallel,
    /// Execute calls whose tools report both read-only and concurrency-safe
    /// concurrently, then execute all remaining calls sequentially.
    Auto,
}

/// Controls how the framework handles skills/agents that request unavailable tools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationMode {
    /// Reject the operation entirely if any requested tool is missing from the registry.
    #[default]
    Strict,
    /// Bind available tools, omit missing ones, and log a warning.
    Permissive,
}

/// A registry that maps tool names to concrete tool instances.
///
/// Implementations resolve string identifiers (e.g. from a skill or config)
/// into executable `Arc<dyn Tool>` instances.
pub trait ToolRegistry: Send + Sync {
    /// Resolve a tool name to a concrete tool instance.
    /// Returns `None` if the tool is not available in this registry.
    fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>>;

    /// Returns a list of all tool names available in this registry.
    fn available_tools(&self) -> Vec<String> {
        vec![]
    }
}

/// A predicate function for filtering tools.
pub type ToolPredicate = Box<dyn Fn(&dyn Tool) -> bool + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, EventActions, ReadonlyContext, RunConfig};
    use std::sync::Mutex;

    struct TestTool {
        name: String,
    }

    #[allow(dead_code)]
    struct TestContext {
        content: Content,
        config: RunConfig,
        actions: Mutex<EventActions>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                content: Content::new("user"),
                config: RunConfig::default(),
                actions: Mutex::new(EventActions::default()),
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "test"
        }
        fn agent_name(&self) -> &str {
            "test"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn crate::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for TestContext {
        fn function_call_id(&self) -> &str {
            "call-123"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<crate::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test tool"
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::String("result".to_string()))
        }
    }

    #[test]
    fn test_tool_trait() {
        let tool = TestTool { name: "test".to_string() };
        assert_eq!(tool.name(), "test");
        assert_eq!(tool.description(), "test tool");
        assert!(!tool.is_long_running());
    }

    struct ReadOnlyTool;

    #[async_trait]
    impl Tool for ReadOnlyTool {
        fn name(&self) -> &str {
            "lookup"
        }
        fn description(&self) -> &str {
            "reads"
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn effect_defaults_follow_is_read_only() {
        let tool = TestTool { name: "test".to_string() };
        assert_eq!(tool.effect(), ToolEffect::NonIdempotent);
        assert_eq!(ReadOnlyTool.effect(), ToolEffect::ReadOnly);
        assert_eq!(tool.timeout_override(), None);
    }

    #[test]
    fn idempotency_key_joins_identity_and_call_id() {
        let ctx = TestContext::new();
        assert_eq!(ctx.idempotency_key(), "app/user/session/test/call-123");
    }

    #[test]
    fn backoff_grows_exponentially_with_jitter_and_respects_the_cap() {
        let budget = RetryBudget::new(5, std::time::Duration::from_millis(100))
            .with_max_delay(std::time::Duration::from_millis(350));
        for _ in 0..50 {
            let first = budget.backoff_delay(1);
            assert!(first >= std::time::Duration::from_millis(50));
            assert!(first <= std::time::Duration::from_millis(100));
            let second = budget.backoff_delay(2);
            assert!(second >= std::time::Duration::from_millis(100));
            assert!(second <= std::time::Duration::from_millis(200));
            let capped = budget.backoff_delay(10);
            assert!(capped >= std::time::Duration::from_millis(175));
            assert!(capped <= std::time::Duration::from_millis(350));
        }
        let zero = RetryBudget::new(1, std::time::Duration::ZERO);
        assert_eq!(zero.backoff_delay(1), std::time::Duration::ZERO);
        assert_eq!(zero.max_delay, RetryBudget::DEFAULT_MAX_DELAY);
    }

    #[tokio::test]
    async fn test_tool_execute() {
        let tool = TestTool { name: "test".to_string() };
        let ctx = Arc::new(TestContext::new()) as Arc<dyn ToolContext>;
        let result = tool.execute(ctx, Value::Null).await.unwrap();
        assert_eq!(result, Value::String("result".to_string()));
    }
}
