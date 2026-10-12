//! Agent builder — constructs a runnable agent from a [`ManagedAgentDef`].
//!
//! The [`build_agent`] function is the bridge between the declarative agent
//! definition and the live `LlmAgent` that the session loop drives. It wires:
//!
//! - Model (`Arc<dyn Llm>`) + system prompt → `LlmAgentBuilder`
//! - Built-in tool declarations → in-process tool implementations
//! - Custom tools → [`ManagedCustomTool`] wrappers (the turn waits on the client)
//! - Permission policy → `ToolConfirmationPolicy`
//! - Description → agent description
//!
//! MCP servers and skills are noted but their full integration is deferred to
//! later tasks (MCP toolset lifecycle, skill injection).

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm, Tool, ToolConfirmationPolicy, ToolContext};
#[cfg(feature = "sandbox")]
use adk_sandbox::{ExecRequest, Language, SandboxBackend};

use crate::types::{ManagedAgentDef, PermissionMode, PermissionPolicy, ToolConfig};

/// Errors that can occur during agent construction.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The agent definition is invalid.
    #[error("invalid agent definition: {0}")]
    InvalidDef(String),

    /// Agent construction failed.
    #[error("agent build failed: {0}")]
    BuildFailed(String),
}

/// Build a runnable agent from a [`ManagedAgentDef`] and a resolved model.
///
/// This function constructs an `LlmAgent` by wiring the declarative definition
/// fields into the builder. The resulting agent can be driven by the session loop.
///
/// # Arguments
///
/// * `def` — The declarative agent definition.
/// * `model` — A resolved LLM instance (from [`ModelResolver`](crate::resolver::ModelResolver)).
/// * `sandbox` — Optional sandbox backend for isolated tool execution (sandbox feature only).
///
/// # Returns
///
/// An `Arc<dyn Agent>` ready for execution by the `Runner`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_managed::agent_builder::build_agent;
/// use adk_managed::types::ManagedAgentDef;
///
/// let agent = test_build_agent(&def, model);
/// println!("Built agent: {}", agent.name());
/// ```
#[cfg(feature = "sandbox")]
pub fn build_agent(
    def: &ManagedAgentDef,
    model: Arc<dyn Llm>,
    sandbox: Option<Arc<dyn SandboxBackend>>,
) -> Result<Arc<dyn Agent>, BuildError> {
    let mut builder = LlmAgentBuilder::new(&def.name).model(model);

    // Wire system prompt
    if let Some(ref system) = def.system {
        builder = builder.instruction(system.clone());
    }

    // Wire description
    if let Some(ref description) = def.description {
        builder = builder.description(description.clone());
    }

    // Wire tools
    for tool_config in &def.tools {
        let tool: Arc<dyn Tool> = match tool_config {
            ToolConfig::Bash {} => Arc::new(ManagedBuiltinTool::new(
                "bash",
                "Execute bash shell commands in the agent's workspace.",
                sandbox.clone(),
            )),
            ToolConfig::Filesystem {} => Arc::new(ManagedBuiltinTool::new(
                "filesystem",
                "Read, write, and manage files in the agent's workspace.",
                sandbox.clone(),
            )),
            ToolConfig::WebSearch {} => Arc::new(ManagedBuiltinTool::new(
                "web_search",
                "Search the web for information.",
                sandbox.clone(),
            )),
            ToolConfig::WebFetch {} => Arc::new(ManagedBuiltinTool::new(
                "web_fetch",
                "Fetch and extract content from a URL.",
                sandbox.clone(),
            )),
            ToolConfig::CodeExecution {} => Arc::new(ManagedBuiltinTool::new(
                "code_execution",
                "Execute code in a sandboxed environment.",
                sandbox.clone(),
            )),
            ToolConfig::Custom { name, description, input_schema } => {
                Arc::new(ManagedCustomTool::new(
                    name.clone(),
                    description.clone().unwrap_or_default(),
                    input_schema.clone(),
                ))
            }
        };
        builder = builder.tool(tool);
    }

    // Wire permission policy → ToolConfirmationPolicy
    if let Some(ref policy) = def.permission_policy {
        let confirmation_policy = map_permission_policy(policy);
        builder = builder.tool_confirmation_policy(confirmation_policy);
    }

    // Note: MCP servers and skills are registered in later tasks.
    if !def.mcp_servers.is_empty() {
        tracing::debug!(
            mcp_count = def.mcp_servers.len(),
            "MCP server configs noted (wiring deferred to session loop)"
        );
    }
    if !def.skills.is_empty() {
        tracing::debug!(
            skill_count = def.skills.len(),
            "skill refs noted (wiring deferred to session loop)"
        );
    }

    let agent = builder.build().map_err(|e| BuildError::BuildFailed(e.to_string()))?;

    Ok(Arc::new(agent))
}

/// Build a runnable agent from a [`ManagedAgentDef`] and a resolved model.
///
/// See the `sandbox`-enabled variant for full documentation.
#[cfg(not(feature = "sandbox"))]
pub fn build_agent(
    def: &ManagedAgentDef,
    model: Arc<dyn Llm>,
) -> Result<Arc<dyn Agent>, BuildError> {
    let mut builder = LlmAgentBuilder::new(&def.name).model(model);

    // Wire system prompt
    if let Some(ref system) = def.system {
        builder = builder.instruction(system.clone());
    }

    // Wire description
    if let Some(ref description) = def.description {
        builder = builder.description(description.clone());
    }

    // Wire tools
    for tool_config in &def.tools {
        let tool: Arc<dyn Tool> = match tool_config {
            ToolConfig::Bash {} => Arc::new(ManagedBuiltinTool::new(
                "bash",
                "Execute bash shell commands in the agent's workspace.",
            )),
            ToolConfig::Filesystem {} => Arc::new(ManagedBuiltinTool::new(
                "filesystem",
                "Read, write, and manage files in the agent's workspace.",
            )),
            ToolConfig::WebSearch {} => {
                Arc::new(ManagedBuiltinTool::new("web_search", "Search the web for information."))
            }
            ToolConfig::WebFetch {} => Arc::new(ManagedBuiltinTool::new(
                "web_fetch",
                "Fetch and extract content from a URL.",
            )),
            ToolConfig::CodeExecution {} => Arc::new(ManagedBuiltinTool::new(
                "code_execution",
                "Execute code in a sandboxed environment.",
            )),
            ToolConfig::Custom { name, description, input_schema } => {
                Arc::new(ManagedCustomTool::new(
                    name.clone(),
                    description.clone().unwrap_or_default(),
                    input_schema.clone(),
                ))
            }
        };
        builder = builder.tool(tool);
    }

    // Wire permission policy → ToolConfirmationPolicy
    if let Some(ref policy) = def.permission_policy {
        let confirmation_policy = map_permission_policy(policy);
        builder = builder.tool_confirmation_policy(confirmation_policy);
    }

    // Note: MCP servers and skills are registered in later tasks.
    if !def.mcp_servers.is_empty() {
        tracing::debug!(
            mcp_count = def.mcp_servers.len(),
            "MCP server configs noted (wiring deferred to session loop)"
        );
    }
    if !def.skills.is_empty() {
        tracing::debug!(
            skill_count = def.skills.len(),
            "skill refs noted (wiring deferred to session loop)"
        );
    }

    let agent = builder.build().map_err(|e| BuildError::BuildFailed(e.to_string()))?;

    Ok(Arc::new(agent))
}

/// Map a [`PermissionPolicy`] to a [`ToolConfirmationPolicy`].
///
/// The mapping logic:
/// - `default: AutoApprove` with no per-tool overrides → `Never`
/// - `default: Prompt` with no per-tool overrides → `Always`
/// - `default: Deny` → `Always` (deny requires confirmation; the runtime
///   can then reject on deny)
/// - Per-tool overrides with `Prompt` or `Deny` → `PerTool` containing those names
fn map_permission_policy(policy: &PermissionPolicy) -> ToolConfirmationPolicy {
    // Collect tools that require confirmation (Prompt or Deny modes)
    let tools_requiring_confirmation: BTreeSet<String> = policy
        .tools
        .iter()
        .filter(|(_, mode)| matches!(mode, PermissionMode::Prompt | PermissionMode::Deny))
        .map(|(name, _)| name.clone())
        .collect();

    match policy.default {
        PermissionMode::AutoApprove => {
            if tools_requiring_confirmation.is_empty() {
                ToolConfirmationPolicy::Never
            } else {
                ToolConfirmationPolicy::PerTool(tools_requiring_confirmation)
            }
        }
        PermissionMode::Prompt | PermissionMode::Deny => {
            // If the default requires confirmation, use Always unless there
            // are explicit auto_approve overrides that narrow it down.
            // In practice, "default: prompt" means all tools need confirmation.
            ToolConfirmationPolicy::Always
        }
    }
}

// ─── ManagedBuiltinTool ──────────────────────────────────────────────────────

/// Built-in tool for server-side execution (bash, filesystem, web_search, etc.).
///
/// When the Runner calls `execute()`, this tool performs the operation in-process.
/// For `bash`, it spawns a child process via `tokio::process::Command`. For
/// tools that require external services (web_search, web_fetch), it returns a
/// structured error indicating the service is unavailable unless explicitly
/// configured.
///
/// When a sandbox backend is configured, execution tools (`bash`, `code_execution`)
/// delegate to the sandbox for isolated execution instead of running in-process.
#[derive(Clone)]
pub struct ManagedBuiltinTool {
    name: String,
    description: String,
    /// Optional sandbox backend for isolated execution.
    #[cfg(feature = "sandbox")]
    sandbox: Option<Arc<dyn SandboxBackend>>,
}

impl std::fmt::Debug for ManagedBuiltinTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("ManagedBuiltinTool");
        d.field("name", &self.name).field("description", &self.description);
        #[cfg(feature = "sandbox")]
        d.field("sandbox", &self.sandbox.as_ref().map(|s| s.name()));
        d.finish()
    }
}

impl ManagedBuiltinTool {
    /// Create a new built-in tool with sandbox support.
    #[cfg(feature = "sandbox")]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        sandbox: Option<Arc<dyn SandboxBackend>>,
    ) -> Self {
        Self { name: name.into(), description: description.into(), sandbox }
    }

    /// Create a new built-in tool (no sandbox).
    #[cfg(not(feature = "sandbox"))]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into() }
    }

    /// Execute a bash command in a child process.
    async fn execute_bash(&self, args: &Value) -> adk_core::Result<Value> {
        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or_default();

        if command.is_empty() {
            return Ok(serde_json::json!({
                "error": "no command provided",
                "exit_code": 1
            }));
        }

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("failed to spawn bash: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        Ok(serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code
        }))
    }

    /// Execute a filesystem operation.
    async fn execute_filesystem(&self, args: &Value) -> adk_core::Result<Value> {
        let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("read");

        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");

        match operation {
            "read" => {
                if path.is_empty() {
                    return Ok(serde_json::json!({"error": "path is required"}));
                }
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => Ok(serde_json::json!({"content": content})),
                    Err(e) => Ok(serde_json::json!({"error": format!("read failed: {e}")})),
                }
            }
            "write" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if path.is_empty() {
                    return Ok(serde_json::json!({"error": "path is required"}));
                }
                match tokio::fs::write(path, content).await {
                    Ok(()) => Ok(serde_json::json!({"status": "written", "path": path})),
                    Err(e) => Ok(serde_json::json!({"error": format!("write failed: {e}")})),
                }
            }
            "list" => {
                let target = if path.is_empty() { "." } else { path };
                match tokio::fs::read_dir(target).await {
                    Ok(mut entries) => {
                        let mut files = Vec::new();
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            files.push(entry.file_name().to_string_lossy().to_string());
                        }
                        Ok(serde_json::json!({"files": files}))
                    }
                    Err(e) => Ok(serde_json::json!({"error": format!("list failed: {e}")})),
                }
            }
            other => Ok(serde_json::json!({
                "error": format!("unsupported filesystem operation: {other}")
            })),
        }
    }

    /// Execute via sandbox backend — delegates to `SandboxBackend::execute()`.
    #[cfg(feature = "sandbox")]
    async fn execute_via_sandbox(
        &self,
        sandbox: &Arc<dyn SandboxBackend>,
        language: Language,
        args: &Value,
    ) -> adk_core::Result<Value> {
        use std::collections::HashMap;
        use std::time::Duration;

        let code = match language {
            Language::Command => {
                // For bash/command, the code is in the "command" field
                args.get("command").and_then(|v| v.as_str()).unwrap_or_default().to_string()
            }
            _ => {
                // For language execution, the code is in the "code" field
                args.get("code").and_then(|v| v.as_str()).unwrap_or_default().to_string()
            }
        };

        if code.is_empty() {
            return Ok(serde_json::json!({"error": "no code/command provided"}));
        }

        let timeout_secs = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let request = ExecRequest {
            language,
            code,
            stdin: args.get("stdin").and_then(|v| v.as_str()).map(String::from),
            timeout: Duration::from_secs(timeout_secs),
            memory_limit_mb: args.get("memory_limit_mb").and_then(|v| v.as_u64()).map(|v| v as u32),
            env: HashMap::new(),
        };

        match sandbox.execute(request).await {
            Ok(result) => Ok(serde_json::json!({
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "duration_ms": result.duration.as_millis() as u64
            })),
            Err(e) => Ok(serde_json::json!({
                "error": format!("sandbox execution failed: {e}"),
                "exit_code": -1
            })),
        }
    }
}

#[async_trait]
impl Tool for ManagedBuiltinTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        match self.name.as_str() {
            "bash" => {
                #[cfg(feature = "sandbox")]
                if let Some(ref sandbox) = self.sandbox {
                    return self.execute_via_sandbox(sandbox, Language::Command, &args).await;
                }
                self.execute_bash(&args).await
            }
            "filesystem" => self.execute_filesystem(&args).await,
            "code_execution" => {
                // Code execution requires a sandbox backend. Without one configured,
                // we fall back to running via bash with the appropriate interpreter.
                let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("python");
                let code = args.get("code").and_then(|v| v.as_str()).unwrap_or_default();

                if code.is_empty() {
                    return Ok(serde_json::json!({"error": "no code provided"}));
                }

                #[cfg(feature = "sandbox")]
                if let Some(ref sandbox) = self.sandbox {
                    let lang = match language {
                        "python" | "python3" => Language::Python,
                        "javascript" | "js" | "node" => Language::JavaScript,
                        "bash" | "sh" => Language::Command,
                        "rust" => Language::Rust,
                        "typescript" | "ts" => Language::TypeScript,
                        other => {
                            return Ok(serde_json::json!({
                                "error": format!("unsupported language for sandbox: {other}")
                            }));
                        }
                    };
                    return self.execute_via_sandbox(sandbox, lang, &args).await;
                }

                let interpreter = match language {
                    "python" | "python3" => "python3",
                    "javascript" | "js" | "node" => "node",
                    "bash" | "sh" => "sh",
                    other => {
                        return Ok(serde_json::json!({
                            "error": format!("unsupported language: {other}. Configure a sandbox backend for full language support.")
                        }));
                    }
                };

                let output = tokio::process::Command::new(interpreter)
                    .arg("-c")
                    .arg(code)
                    .output()
                    .await
                    .map_err(|e| {
                        adk_core::AdkError::tool(format!("failed to spawn {interpreter}: {e}"))
                    })?;

                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                Ok(serde_json::json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": exit_code
                }))
            }
            "web_search" => {
                // Web search requires an external API (Google, Bing, etc.).
                // Return a structured response indicating the service needs configuration.
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or_default();
                Ok(serde_json::json!({
                    "error": "web_search is not configured for in-process execution. Configure an API key or use a provider with built-in search grounding.",
                    "query": query
                }))
            }
            "web_fetch" => {
                let url = args.get("url").and_then(|v| v.as_str()).unwrap_or_default();
                Ok(serde_json::json!({
                    "error": "web_fetch is not configured for in-process execution. Configure an HTTP client or sandbox with network access.",
                    "url": url
                }))
            }
            other => Err(adk_core::AdkError::tool(format!("unknown built-in tool: {other}"))),
        }
    }
}

// ─── ManagedCustomTool ───────────────────────────────────────────────────────

/// Wrapper for custom (client-executed) tools declared in a [`ManagedAgentDef`].
///
/// The client executes the tool, so `execute()` does not: it answers the call with a
/// pending response and ends the agent's turn (`skip_summarization`), so the model is not
/// asked to continue from a placeholder. The session loop then reports
/// `agent.custom_tool_use`, ends the turn with `requires_action`, and parks the turn's
/// durable run until the client sends `user.custom_tool_result`.
///
/// # Multi-Turn Flow
///
/// 1. The model calls this tool.
/// 2. `execute()` answers with a pending response and ends the turn.
/// 3. The session loop emits `agent.custom_tool_use` and `status.idle` with
///    `requires_action`, and parks the run.
/// 4. `user.custom_tool_result` records the result in the decision store and wakes the run.
/// 5. The run resumes: the result replaces the pending response in the conversation, and
///    the model continues from it.
#[derive(Debug, Clone)]
pub struct ManagedCustomTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl ManagedCustomTool {
    /// Create a new custom tool wrapper.
    ///
    /// # Arguments
    ///
    /// * `name` — Tool name as declared in the agent definition.
    /// * `description` — Human-readable description for the LLM.
    /// * `input_schema` — JSON Schema for the tool's input parameters.
    pub fn new(name: String, description: String, input_schema: Value) -> Self {
        Self { name, description, input_schema }
    }
}

/// The `awaiting` value that marks a custom tool call's pending response.
pub(crate) const AWAITING_CLIENT_RESULT: &str = "client_result";

/// Whether `response` is the pending response a [`ManagedCustomTool`] answers with.
pub(crate) fn is_custom_tool_pending(response: &Value) -> bool {
    adk_core::is_confirmation_pending(response)
        && response.get("awaiting").and_then(Value::as_str) == Some(AWAITING_CLIENT_RESULT)
}

#[async_trait]
impl Tool for ManagedCustomTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.input_schema.clone())
    }

    fn is_long_running(&self) -> bool {
        // The result arrives later, from the client.
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        // A confirmation-pending response is superseded in place by the call's real
        // response, so the client's result takes this response's slot in the history.
        let mut response = adk_core::confirmation_pending_response(format!(
            "custom tool '{}' is executed by the client; its result is pending",
            self.name
        ));
        response["awaiting"] = Value::String(AWAITING_CLIENT_RESULT.to_string());
        let mut actions = ctx.actions();
        actions.skip_summarization = true;
        ctx.set_actions(actions);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ManagedAgentDef, ModelRef, PermissionMode, PermissionPolicy, ToolConfig};
    use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
    use async_stream::stream;
    use std::collections::HashMap;

    /// Mock LLM for testing agent construction.
    struct MockLlm {
        name: String,
    }

    impl MockLlm {
        fn new(name: &str) -> Self {
            Self { name: name.to_string() }
        }
    }

    #[async_trait]
    impl Llm for MockLlm {
        fn name(&self) -> &str {
            &self.name
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(Content::new("model").with_text("Hello")),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    /// Test helper: call build_agent with correct arity for current feature set.
    #[cfg(feature = "sandbox")]
    fn test_build_agent(def: &ManagedAgentDef, model: Arc<dyn Llm>) -> Arc<dyn Agent> {
        build_agent(def, model, None).unwrap()
    }

    #[cfg(not(feature = "sandbox"))]
    fn test_build_agent(def: &ManagedAgentDef, model: Arc<dyn Llm>) -> Arc<dyn Agent> {
        build_agent(def, model).unwrap()
    }

    #[test]
    fn test_build_agent_minimal_def() {
        let def = ManagedAgentDef {
            name: "test-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);

        assert_eq!(agent.name(), "test-agent");
    }

    #[test]
    fn test_build_agent_with_system_prompt() {
        let def = ManagedAgentDef {
            name: "prompted-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: Some("You are a helpful assistant.".to_string()),
            description: Some("A helpful agent".to_string()),
            tools: vec![],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);

        assert_eq!(agent.name(), "prompted-agent");
        assert_eq!(agent.description(), "A helpful agent");
    }

    #[test]
    fn test_build_agent_with_builtin_tools() {
        let def = ManagedAgentDef {
            name: "tool-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![
                ToolConfig::Bash {},
                ToolConfig::Filesystem {},
                ToolConfig::WebSearch {},
                ToolConfig::WebFetch {},
                ToolConfig::CodeExecution {},
            ],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);
        assert_eq!(agent.name(), "tool-agent");
    }

    #[test]
    fn test_build_agent_with_custom_tool() {
        let def = ManagedAgentDef {
            name: "custom-tool-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![ToolConfig::Custom {
                name: "get_weather".to_string(),
                description: Some("Get current weather".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    },
                    "required": ["city"]
                }),
            }],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: None,
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);
        assert_eq!(agent.name(), "custom-tool-agent");
    }

    #[test]
    fn test_build_agent_with_permission_policy_auto_approve() {
        let def = ManagedAgentDef {
            name: "auto-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![ToolConfig::Bash {}],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: Some(PermissionPolicy {
                default: PermissionMode::AutoApprove,
                tools: HashMap::new(),
            }),
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);
        assert_eq!(agent.name(), "auto-agent");
    }

    #[test]
    fn test_build_agent_with_permission_policy_prompt_default() {
        let def = ManagedAgentDef {
            name: "prompt-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![ToolConfig::Bash {}],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: Some(PermissionPolicy {
                default: PermissionMode::Prompt,
                tools: HashMap::new(),
            }),
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);
        assert_eq!(agent.name(), "prompt-agent");
    }

    #[test]
    fn test_build_agent_with_per_tool_permission() {
        let def = ManagedAgentDef {
            name: "mixed-agent".to_string(),
            model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
            system: None,
            description: None,
            tools: vec![ToolConfig::Bash {}, ToolConfig::Filesystem {}],
            mcp_servers: vec![],
            skills: vec![],
            permission_policy: Some(PermissionPolicy {
                default: PermissionMode::AutoApprove,
                tools: HashMap::from([
                    ("bash".to_string(), PermissionMode::Prompt),
                    ("delete_file".to_string(), PermissionMode::Deny),
                ]),
            }),
            metadata: None,
        };

        let model: Arc<dyn Llm> = Arc::new(MockLlm::new("mock-model"));
        let agent = test_build_agent(&def, model);
        assert_eq!(agent.name(), "mixed-agent");
    }

    // ─── map_permission_policy tests ─────────────────────────────────────────

    #[test]
    fn test_map_auto_approve_no_overrides() {
        let policy =
            PermissionPolicy { default: PermissionMode::AutoApprove, tools: HashMap::new() };
        assert_eq!(map_permission_policy(&policy), ToolConfirmationPolicy::Never);
    }

    #[test]
    fn test_map_prompt_default() {
        let policy = PermissionPolicy { default: PermissionMode::Prompt, tools: HashMap::new() };
        assert_eq!(map_permission_policy(&policy), ToolConfirmationPolicy::Always);
    }

    #[test]
    fn test_map_deny_default() {
        let policy = PermissionPolicy { default: PermissionMode::Deny, tools: HashMap::new() };
        assert_eq!(map_permission_policy(&policy), ToolConfirmationPolicy::Always);
    }

    #[test]
    fn test_map_auto_approve_with_per_tool_prompt() {
        let policy = PermissionPolicy {
            default: PermissionMode::AutoApprove,
            tools: HashMap::from([
                ("bash".to_string(), PermissionMode::Prompt),
                ("delete_file".to_string(), PermissionMode::Deny),
            ]),
        };
        let result = map_permission_policy(&policy);
        match result {
            ToolConfirmationPolicy::PerTool(tools) => {
                assert!(tools.contains("bash"));
                assert!(tools.contains("delete_file"));
                assert_eq!(tools.len(), 2);
            }
            other => panic!("expected PerTool, got: {other:?}"),
        }
    }

    #[test]
    fn test_map_auto_approve_with_auto_approve_overrides_only() {
        let policy = PermissionPolicy {
            default: PermissionMode::AutoApprove,
            tools: HashMap::from([("read_file".to_string(), PermissionMode::AutoApprove)]),
        };
        // AutoApprove overrides don't add to confirmation set
        assert_eq!(map_permission_policy(&policy), ToolConfirmationPolicy::Never);
    }

    // ─── ManagedBuiltinTool tests ────────────────────────────────────────────

    /// Helper to create a builtin tool for testing (handles feature-gated constructor).
    #[cfg(feature = "sandbox")]
    fn make_builtin_tool(name: &str, desc: &str) -> ManagedBuiltinTool {
        ManagedBuiltinTool::new(name, desc, None)
    }

    #[cfg(not(feature = "sandbox"))]
    fn make_builtin_tool(name: &str, desc: &str) -> ManagedBuiltinTool {
        ManagedBuiltinTool::new(name, desc)
    }

    #[test]
    fn test_builtin_tool_metadata() {
        let tool = make_builtin_tool("bash", "Execute bash commands.");
        assert_eq!(tool.name(), "bash");
        assert_eq!(tool.description(), "Execute bash commands.");
    }

    #[tokio::test]
    async fn test_builtin_tool_bash_executes() {
        let tool = make_builtin_tool("bash", "Execute bash commands.");
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test-caller"));
        let result = tool.execute(ctx, serde_json::json!({"command": "echo hello"})).await.unwrap();
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_builtin_tool_web_search_returns_error() {
        let tool = make_builtin_tool("web_search", "Search the web.");
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test-caller"));
        let result = tool.execute(ctx, serde_json::json!({"query": "rust lang"})).await.unwrap();
        assert!(result["error"].as_str().unwrap().contains("not configured"));
    }

    // ─── ManagedCustomTool tests ─────────────────────────────────────────────

    #[test]
    fn test_custom_tool_metadata() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}}
        });
        let tool = ManagedCustomTool::new(
            "get_weather".to_string(),
            "Get current weather".to_string(),
            schema.clone(),
        );
        assert_eq!(tool.name(), "get_weather");
        assert_eq!(tool.description(), "Get current weather");
        assert_eq!(tool.parameters_schema(), Some(schema));
        assert!(tool.is_long_running());
    }

    #[tokio::test]
    async fn a_custom_tool_answers_pending_and_ends_the_turn() {
        let tool = ManagedCustomTool::new(
            "my_tool".to_string(),
            "A custom tool".to_string(),
            serde_json::json!({"type": "object"}),
        );
        let ctx = Arc::new(adk_tool::SimpleToolContext::new("test-caller"));

        let result =
            tool.execute(ctx.clone(), serde_json::json!({"city": "Seattle"})).await.unwrap();

        assert!(is_custom_tool_pending(&result), "{result}");
        assert!(ctx.actions().skip_summarization, "the model must not answer a placeholder");
    }

    #[test]
    fn test_custom_tool_is_long_running() {
        let tool = ManagedCustomTool::new(
            "deploy".to_string(),
            "Deploy to production".to_string(),
            serde_json::json!({"type": "object"}),
        );
        // Must be true so the agent loop breaks after this tool call
        assert!(tool.is_long_running());
    }
}
