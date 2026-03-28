//! `FrontendCodeTool` — container-backed frontend code execution preset.
//!
//! This tool provides named presets (e.g., [`FrontendCodeTool::react`]) for
//! frontend code execution in isolated containers.
//!
//! # Required Scopes
//!
//! This tool declares `["code:execute", "code:execute:container"]` as
//! required scopes.

use adk_code::{
    CodeExecutor, ContainerCommandExecutor, ContainerConfig, ExecutionLanguage, ExecutionPayload,
    ExecutionRequest, SandboxPolicy,
};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Container-backed frontend code execution tool.
///
/// # Required Scopes
///
/// Returns `["code:execute", "code:execute:container"]`.
///
/// # Example
///
/// ```rust
/// use adk_tool::{FrontendCodeTool, Tool};
/// use std::sync::Arc;
///
/// let tool = Arc::new(FrontendCodeTool::react());
/// assert_eq!(tool.name(), "frontend_code");
/// assert_eq!(tool.required_scopes(), &["code:execute", "code:execute:container"]);
/// ```
pub struct FrontendCodeTool {
    framework: String,
    executor: Arc<dyn CodeExecutor>,
}

impl FrontendCodeTool {
    /// React/TypeScript frontend preset using a Node.js container.
    pub fn react() -> Self {
        Self {
            framework: "react".to_string(),
            executor: Arc::new(ContainerCommandExecutor::new(ContainerConfig {
                runtime: "docker".to_string(),
                default_image: "node:20-slim".to_string(),
                extra_flags: vec![],
                auto_remove: true,
            })),
        }
    }

    /// Generic frontend preset with a custom framework label.
    pub fn new(framework: impl Into<String>) -> Self {
        Self {
            framework: framework.into(),
            executor: Arc::new(ContainerCommandExecutor::new(ContainerConfig {
                runtime: "docker".to_string(),
                default_image: "node:20-slim".to_string(),
                extra_flags: vec![],
                auto_remove: true,
            })),
        }
    }

    /// Create with a custom executor (e.g., `DockerExecutor` for persistent containers).
    pub fn with_executor(framework: impl Into<String>, executor: Arc<dyn CodeExecutor>) -> Self {
        Self { framework: framework.into(), executor }
    }

    /// Create with a custom CLI container configuration.
    pub fn with_config(framework: impl Into<String>, config: ContainerConfig) -> Self {
        Self {
            framework: framework.into(),
            executor: Arc::new(ContainerCommandExecutor::new(config)),
        }
    }

    /// The framework label for this preset.
    pub fn framework(&self) -> &str {
        &self.framework
    }
}

#[async_trait]
impl Tool for FrontendCodeTool {
    fn name(&self) -> &str {
        "frontend_code"
    }

    fn description(&self) -> &str {
        "Execute frontend code in an isolated container. \
         Suitable for React, Vue, and other frontend framework tasks."
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
                    "description": "Frontend source code to execute."
                },
                "input": {
                    "description": "Optional JSON input."
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
            language: ExecutionLanguage::JavaScript,
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
            .map_err(|e| adk_core::AdkError::tool(format!("Frontend execution failed: {e}")))?;

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
