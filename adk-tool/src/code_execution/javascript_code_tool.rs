//! `JavaScriptCodeTool` — embedded JavaScript execution for lightweight transforms.
//!
//! This tool wraps the `adk-code` [`EmbeddedJsExecutor`] backend (via `boa_engine`)
//! to run JavaScript snippets in-process. It is useful for data transforms, filtering,
//! and lightweight scripting without requiring Docker.
//!
//! When the `embedded-js` feature is not enabled on `adk-code`, the tool returns
//! a descriptive error explaining how to enable it.
//!
//! # Required Scopes
//!
//! This tool declares `["code:execute"]` as its required scope. Embedded
//! JavaScript runs in-process with no network or filesystem access, so no
//! elevated container or host scopes are needed.

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Embedded JavaScript code execution tool.
///
/// Runs JavaScript snippets in-process via `boa_engine`. The code has access
/// to an `input` variable containing the JSON input, and should `return` a
/// value to produce structured output.
///
/// Requires the `embedded-js` feature on `adk-code`. Without it, the tool
/// returns a descriptive error.
///
/// # Required Scopes
///
/// Returns `["code:execute"]`. No elevated scopes needed.
///
/// # Example
///
/// ```rust
/// use adk_tool::{JavaScriptCodeTool, Tool};
///
/// let tool = JavaScriptCodeTool::new();
/// assert_eq!(tool.name(), "javascript_code");
/// assert_eq!(tool.required_scopes(), &["code:execute"]);
/// ```
pub struct JavaScriptCodeTool {
    _private: (),
}

impl JavaScriptCodeTool {
    /// Create a new `JavaScriptCodeTool`.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for JavaScriptCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for JavaScriptCodeTool {
    fn name(&self) -> &str {
        "javascript_code"
    }

    fn description(&self) -> &str {
        "Execute JavaScript for lightweight transforms and scripting. \
         Uses an embedded JS engine with no network or filesystem access. \
         Access input data via the `input` variable and use `return` to produce output. \
         Example: return input.items.filter(x => x.price > 10);"
    }

    fn required_scopes(&self) -> &[&str] {
        &["code:execute"]
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "JavaScript source code to execute. Use `input` to access JSON input and `return` to produce output."
                },
                "input": {
                    "description": "Optional JSON value passed as the `input` variable."
                }
            },
            "required": ["code"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let code = match args.get("code").and_then(Value::as_str) {
            Some(c) => c.to_string(),
            None => {
                return Ok(json!({
                    "status": "rejected",
                    "stdout": "",
                    "stderr": "missing required field: code",
                    "output": null,
                    "exitCode": null,
                    "durationMs": 0,
                }));
            }
        };

        let input = args.get("input").cloned();

        execute_js(code, input).await
    }
}

/// Execute JavaScript using EmbeddedJsExecutor when the feature is available.
#[cfg(feature = "code-embedded-js")]
async fn execute_js(code: String, input: Option<Value>) -> Result<Value> {
    use adk_code::{
        CodeExecutor, EmbeddedJsExecutor, ExecutionLanguage, ExecutionPayload, ExecutionRequest,
        SandboxPolicy,
    };

    let executor = EmbeddedJsExecutor::new();
    let request = ExecutionRequest {
        language: ExecutionLanguage::JavaScript,
        payload: ExecutionPayload::Source { code },
        argv: vec![],
        stdin: None,
        input,
        sandbox: SandboxPolicy::strict_js(),
        identity: None,
    };

    match executor.execute(request).await {
        Ok(result) => Ok(json!({
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "output": result.output,
            "exitCode": result.exit_code,
            "durationMs": result.duration_ms,
        })),
        Err(e) => Ok(json!({
            "status": "failed",
            "stdout": "",
            "stderr": e.to_string(),
            "output": null,
            "exitCode": null,
            "durationMs": 0,
        })),
    }
}

/// Fallback when embedded-js feature is not enabled.
#[cfg(not(feature = "code-embedded-js"))]
async fn execute_js(_code: String, _input: Option<Value>) -> Result<Value> {
    Ok(json!({
        "status": "rejected",
        "stdout": "",
        "stderr": "JavaScript execution requires the 'embedded-js' feature. \
                   Enable it with: adk-code = { features = [\"embedded-js\"] }",
        "output": null,
        "exitCode": null,
        "durationMs": 0,
    }))
}
