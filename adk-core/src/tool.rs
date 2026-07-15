use crate::{CallbackContext, EventActions, MemoryEntry, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
///
/// This type establishes the provider-neutral boundary for tool contracts.
/// It preserves the original JSON Schema documents before provider-specific
/// transformations are applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// JSON Schema for this tool's parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// JSON Schema for this tool's response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}

impl ToolSchema {
    /// Create a new tool schema with optional parameters and response.
    pub fn new(parameters: Option<Value>, response: Option<Value>) -> Self {
        Self { parameters, response }
    }
}

/// A provider-neutral tool contract.
///
/// Encapsulates all metadata required by an LLM provider to declare a tool,
/// including name, model-facing description, and optional schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolContract {
    /// Unique name of the tool.
    pub name: String,
    /// Model-facing description of what the tool does.
    pub description: String,
    /// Input and output schemas for the tool.
    pub schema: ToolSchema,
}

impl ToolContract {
    /// Create a new tool contract.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: ToolSchema,
    ) -> Self {
        Self { name: name.into(), description: description.into(), schema }
    }
}

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

    /// Returns a provider-neutral contract for this tool.
    ///
    /// The contract contains the tool name, enhanced description, and schemas.
    /// Providers consume this contract to build their wire-format declarations.
    fn contract(&self) -> ToolContract {
        ToolContract::new(
            self.name(),
            self.enhanced_description(),
            ToolSchema::new(self.parameters_schema(), self.response_schema()),
        )
    }

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
    ///             "type": "web_search_2025_08_26"
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
    /// Read-only tools may be executed concurrently in Auto mode.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Indicates whether this tool is safe for concurrent execution.
    /// Used by the Parallel strategy to validate dispatch safety.
    fn is_concurrency_safe(&self) -> bool {
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
    /// Get the current event actions. Returns an owned copy for thread safety.
    fn actions(&self) -> EventActions;
    /// Set the event actions (e.g., to trigger escalation or skip summarization).
    fn set_actions(&self, actions: EventActions);
    /// Searches memory for entries matching the query.
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>>;

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
}

/// Configuration for automatic tool retry on failure.
///
/// Controls how many times a failed tool execution is retried before
/// propagating the error. Applied as a flat delay between attempts
/// (no exponential backoff in V1).
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::RetryBudget;
///
/// // Retry up to 2 times with 500ms between attempts (3 total attempts)
/// let budget = RetryBudget::new(2, Duration::from_millis(500));
/// assert_eq!(budget.max_retries, 2);
/// ```
#[derive(Debug, Clone)]
pub struct RetryBudget {
    /// Maximum number of retry attempts (not counting the initial attempt).
    /// E.g., `max_retries: 2` means up to 3 total attempts.
    pub max_retries: u32,
    /// Delay between retries. Applied as a flat delay (no backoff in V1).
    pub delay: std::time::Duration,
}

impl RetryBudget {
    /// Create a new retry budget.
    ///
    /// # Arguments
    ///
    /// * `max_retries` - Maximum retry attempts (not counting the initial attempt)
    /// * `delay` - Flat delay between retry attempts
    pub fn new(max_retries: u32, delay: std::time::Duration) -> Self {
        Self { max_retries, delay }
    }
}

/// A collection of tools that can be resolved dynamically from context.
#[async_trait]
pub trait Toolset: Send + Sync {
    /// Returns the name of this toolset.
    fn name(&self) -> &str;
    /// Returns the tools available in this toolset for the given context.
    async fn tools(&self, ctx: Arc<dyn crate::ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>>;
}

/// Controls how multiple tool calls from a single LLM response are dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolExecutionStrategy {
    /// Execute tools one at a time in LLM-returned order. Default.
    #[default]
    Sequential,
    /// Execute all tools concurrently via `join_all`.
    Parallel,
    /// Execute read-only tools concurrently, then mutable tools sequentially.
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

    #[test]
    fn test_tool_contract() {
        let tool = TestTool { name: "test".to_string() };
        let contract = tool.contract();
        assert_eq!(contract.name, "test");
        assert_eq!(contract.description, "test tool");
        assert_eq!(contract.schema.parameters, None);
        assert_eq!(contract.schema.response, None);
    }

    #[tokio::test]
    async fn test_tool_execute() {
        let tool = TestTool { name: "test".to_string() };
        let ctx = Arc::new(TestContext::new()) as Arc<dyn ToolContext>;
        let result = tool.execute(ctx, Value::Null).await.unwrap();
        assert_eq!(result, Value::String("result".to_string()));
    }
}
