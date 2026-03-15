//! `RustCodeTool` — the primary documented code-execution tool for ADK agents.
//!
//! Wraps [`RustSandboxExecutor`] with strict defaults and a consistent JSON
//! response envelope so LLM agents can execute authored Rust safely.
//!
//! # Deprecation
//!
//! `RustCodeTool` is deprecated in favor of [`adk_code::CodeTool`], which uses
//! the new `RustExecutor` + `SandboxBackend` pipeline. See
//! [`adk_code::compat`] for the full migration guide.

// Allow deprecated usage within this module since we're defining and using
// the deprecated type itself.
#![allow(deprecated)]
//! # Required Scopes
//!
//! This tool declares `["code:execute", "code:execute:rust"]` as required
//! scopes.  When a [`ScopeGuard`](adk_auth::ScopeGuard) is active, the
//! calling user must possess both scopes before execution is dispatched.
//!
//! # Confirmation
//!
//! The default strict sandbox policy does not require additional confirmation
//! beyond scope checks.  If the tool is reconfigured with elevated modes
//! (e.g. network access via `code:network` or writable filesystem via
//! `code:filesystem:write`), the deployer should layer confirmation gating
//! through the standard ADK confirmation flow.

use adk_code::{
    CodeExecutor, ExecutionLanguage, ExecutionPayload, ExecutionRequest, ExecutionResult,
    RustSandboxExecutor, SandboxPolicy,
};
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Primary Rust-first code execution tool.
///
/// Accepts a `code` string and optional `input` JSON, compiles and runs the
/// Rust source in a sandbox with strict defaults, and returns a structured
/// JSON response envelope.
///
/// # Deprecation
///
/// This type is deprecated in favor of [`adk_code::CodeTool`], which uses the
/// new `RustExecutor` + `SandboxBackend` pipeline with structured diagnostics.
/// `RustCodeTool` will be removed in a future release.
///
/// # Required Scopes
///
/// Returns `["code:execute", "code:execute:rust"]`.  The framework enforces
/// these via [`ScopeGuard`](adk_auth::ScopeGuard) when active.
///
/// # Example
///
/// ```rust
/// use adk_tool::{RustCodeTool, Tool};
///
/// let tool = RustCodeTool::new();
/// assert_eq!(tool.name(), "rust_code");
/// assert_eq!(tool.required_scopes(), &["code:execute", "code:execute:rust"]);
/// ```
#[deprecated(since = "0.5.0", note = "Use adk_code::CodeTool instead")]
pub struct RustCodeTool {
    executor: RustSandboxExecutor,
}

impl RustCodeTool {
    /// Create a new `RustCodeTool` with default configuration.
    pub fn new() -> Self {
        Self { executor: RustSandboxExecutor::default() }
    }

    /// Preset for backend Rust development.
    ///
    /// Uses the same strict sandbox defaults as [`RustCodeTool::new`] but
    /// provides a named constructor that reads clearly in collaborative
    /// workspace examples where multiple specialist tools coexist.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_tool::{RustCodeTool, Tool};
    /// use std::sync::Arc;
    ///
    /// let tool = Arc::new(RustCodeTool::backend());
    /// assert_eq!(tool.name(), "rust_code");
    /// ```
    pub fn backend() -> Self {
        Self::new()
    }

    /// Create a `RustCodeTool` with a custom executor.
    ///
    /// Use this when you need non-default sandbox configuration such as a
    /// custom toolchain path, adjusted timeout, or different output limits.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_code::RustSandboxExecutor;
    /// use adk_tool::RustCodeTool;
    ///
    /// let executor = RustSandboxExecutor::default();
    /// let tool = RustCodeTool::with_executor(executor);
    /// ```
    pub fn with_executor(executor: RustSandboxExecutor) -> Self {
        Self { executor }
    }
}

impl Default for RustCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert an [`ExecutionResult`] into the consistent JSON response envelope.
fn result_to_json(result: &ExecutionResult) -> Value {
    json!({
        "status": result.status,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "output": result.output,
        "exitCode": result.exit_code,
        "durationMs": result.duration_ms,
    })
}

#[async_trait]
impl Tool for RustCodeTool {
    fn name(&self) -> &str {
        "rust_code"
    }

    fn description(&self) -> &str {
        "Execute Rust code in a sandbox. The code must define a \
         `fn run(input: serde_json::Value) -> serde_json::Value` entry point. \
         Returns structured output, stdout, stderr, and execution status."
    }

    fn required_scopes(&self) -> &[&str] {
        &["code:execute", "code:execute:rust"]
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Rust source code to execute. Must define a `fn run(input: serde_json::Value) -> serde_json::Value` entry point."
                },
                "input": {
                    "description": "Optional JSON value passed as the `input` argument to the `run` function."
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

        // LLMs often send literal "\\n" and "\\t" escape sequences in code strings
        // instead of actual newlines/tabs. Unescape them so the Rust source compiles.
        let code = code.replace("\\n", "\n").replace("\\t", "\t");

        let input = args.get("input").cloned();

        let request = ExecutionRequest {
            language: ExecutionLanguage::Rust,
            payload: ExecutionPayload::Source { code },
            argv: vec![],
            stdin: None,
            input,
            sandbox: SandboxPolicy::host_local(),
            identity: None,
        };

        match self.executor.execute(request).await {
            Ok(result) => Ok(result_to_json(&result)),
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
}
