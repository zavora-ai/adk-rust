//! `PythonCodeTool` — container-backed Python execution preset.
//!
//! This tool wraps the `adk-code` execution substrate with defaults
//! appropriate for isolated Python execution. It supports two backends:
//!
//! - [`DockerExecutor`] (persistent container, recommended) — requires `docker` feature
//! - [`ContainerCommandExecutor`] (ephemeral, CLI-based fallback)
//!
//! # Required Scopes
//!
//! This tool declares `["code:execute", "code:execute:container"]` as
//! required scopes. Container-backed execution is an elevated mode.

use adk_code::{
    CodeExecutor, ContainerCommandExecutor, ContainerConfig, ExecutionLanguage, ExecutionPayload,
    ExecutionRequest, SandboxPolicy,
};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Container-backed Python code execution tool.
///
/// # Required Scopes
///
/// Returns `["code:execute", "code:execute:container"]`.
///
/// # Example
///
/// ```rust
/// use adk_tool::{PythonCodeTool, Tool};
///
/// let tool = PythonCodeTool::new();
/// assert_eq!(tool.name(), "python_code");
/// assert_eq!(tool.required_scopes(), &["code:execute", "code:execute:container"]);
/// ```
pub struct PythonCodeTool {
    executor: Arc<dyn CodeExecutor>,
}

impl PythonCodeTool {
    /// Create a new `PythonCodeTool` with the CLI-based container executor.
    pub fn new() -> Self {
        Self {
            executor: Arc::new(ContainerCommandExecutor::new(ContainerConfig {
                runtime: "docker".to_string(),
                default_image: "python:3.12-slim".to_string(),
                extra_flags: vec![],
                auto_remove: true,
            })),
        }
    }

    /// Create with a custom executor (e.g., `DockerExecutor` for persistent containers).
    pub fn with_executor(executor: Arc<dyn CodeExecutor>) -> Self {
        Self { executor }
    }

    /// Create with a custom CLI container configuration.
    pub fn with_config(config: ContainerConfig) -> Self {
        Self { executor: Arc::new(ContainerCommandExecutor::new(config)) }
    }
}

impl Default for PythonCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for PythonCodeTool {
    fn name(&self) -> &str {
        "python_code"
    }

    fn description(&self) -> &str {
        "Execute Python code in an isolated container. \
         Suitable for data processing, scripting, and general-purpose computation. \
         Print results as JSON to stdout using json.dumps()."
    }

    fn required_scopes(&self) -> &[&str] {
        &["code:execute", "code:execute:container"]
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Python source code to execute."
                },
                "input": {
                    "description": "Optional JSON value passed via stdin."
                }
            },
            "required": ["code"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let code = args["code"]
            .as_str()
            .ok_or_else(|| adk_core::AdkError::tool("missing 'code' parameter"))?
            .to_string();

        let input = args.get("input").cloned();

        let request = ExecutionRequest {
            language: ExecutionLanguage::Python,
            payload: ExecutionPayload::Source { code },
            argv: vec![],
            stdin: None,
            input,
            sandbox: SandboxPolicy::strict_rust(),
            identity: None,
        };

        let result = self
            .executor
            .execute(request)
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("Python execution failed: {e}")))?;

        Ok(json!({
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "output": result.output,
            "exitCode": result.exit_code,
            "durationMs": result.duration_ms,
        }))
    }
}
