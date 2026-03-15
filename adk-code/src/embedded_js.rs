//! Embedded JavaScript executor — secondary script backend.
//!
//! [`EmbeddedJsExecutor`] uses `boa_engine` to run JavaScript snippets
//! in-process. It is useful for lightweight transforms, deterministic
//! state shaping, and compatibility with existing Studio JS flows.
//!
//! This is **not** the primary code-execution path. The flagship backend
//! is [`crate::RustSandboxExecutor`] for authored Rust code.
//!
//! # Security Model
//!
//! - In-process execution (no container isolation)
//! - No filesystem access
//! - No network access
//! - No child process creation
//! - Timeout enforcement via wall-clock check after execution
//! - JSON input injected as `input` variable
//! - Return value converted back to JSON
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_code::{EmbeddedJsExecutor, CodeExecutor, ExecutionRequest,
//!     ExecutionLanguage, ExecutionPayload, SandboxPolicy};
//!
//! let executor = EmbeddedJsExecutor::new();
//! let request = ExecutionRequest {
//!     language: ExecutionLanguage::JavaScript,
//!     payload: ExecutionPayload::Source {
//!         code: "return input.x + 1;".to_string(),
//!     },
//!     argv: vec![],
//!     stdin: None,
//!     input: Some(serde_json::json!({ "x": 41 })),
//!     sandbox: SandboxPolicy::strict_js(),
//!     identity: None,
//! };
//! ```

use async_trait::async_trait;
use boa_engine::{Context, JsValue, Source};
use std::time::Instant;

use crate::{
    BackendCapabilities, CodeExecutor, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus,
};

/// Secondary embedded JavaScript executor using `boa_engine`.
///
/// Runs JavaScript snippets in-process for lightweight transforms.
/// Does not provide container-level isolation — the security boundary
/// is the `boa_engine` interpreter sandbox.
///
/// # Isolation Model — Enforcement by Omission
///
/// `boa_engine` is a pure ECMAScript interpreter with **no** built-in APIs for
/// network access, filesystem operations, or environment variable reads. Unlike
/// Node.js or Deno, Boa does not expose `fetch`, `fs`, `process.env`, or any
/// host-level I/O. This means network, filesystem, and environment policies are
/// enforced by omission — the engine simply cannot perform those operations.
///
/// [`capabilities()`](Self::capabilities) reports `enforce_network_policy`,
/// `enforce_filesystem_policy`, and `enforce_environment_policy` as `true`
/// because the isolation guarantee holds unconditionally.
///
/// # Product Posture
///
/// This backend is secondary scripting support. Use [`crate::RustSandboxExecutor`]
/// for the primary authored-code path.
#[derive(Debug, Default)]
pub struct EmbeddedJsExecutor;

impl EmbeddedJsExecutor {
    /// Create a new embedded JS executor.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl CodeExecutor for EmbeddedJsExecutor {
    fn name(&self) -> &str {
        "EmbeddedJsExecutor"
    }

    /// Returns the capabilities of this backend.
    ///
    /// Network, filesystem, and environment policies are reported as enforced
    /// because `boa_engine` has no APIs for those operations (enforcement by
    /// omission). See the [struct-level docs](Self) for details.
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            isolation: ExecutionIsolation::InProcess,
            // boa_engine has no network, filesystem, or environment APIs —
            // enforcement is by omission: the engine simply cannot perform
            // these operations, so the policies are inherently satisfied.
            enforce_network_policy: true,
            enforce_filesystem_policy: true,
            enforce_environment_policy: true,
            enforce_timeout: true,
            supports_structured_output: true,
            supports_process_execution: false,
            supports_persistent_workspace: false,
            supports_interactive_sessions: false,
        }
    }

    fn supports_language(&self, lang: &ExecutionLanguage) -> bool {
        matches!(lang, ExecutionLanguage::JavaScript)
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        crate::validate_request(&self.capabilities(), &[ExecutionLanguage::JavaScript], &request)?;

        let code = match &request.payload {
            ExecutionPayload::Source { code } => code.clone(),
            ExecutionPayload::GuestModule { .. } => {
                return Err(ExecutionError::InvalidRequest(
                    "EmbeddedJsExecutor does not support guest modules".to_string(),
                ));
            }
        };

        if code.trim().is_empty() {
            return Err(ExecutionError::InvalidRequest("empty JavaScript source".to_string()));
        }

        let timeout = request.sandbox.timeout;
        let input = request.input.clone();

        // Run JS in a blocking thread to avoid blocking the async runtime.
        let result = tokio::task::spawn_blocking(move || {
            let start = Instant::now();
            let mut context = Context::default();

            // Inject input as a global variable.
            let input_str = input
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
                .unwrap_or_else(|| "null".to_string());

            let setup = format!("var input = {input_str};");
            if let Err(e) = context.eval(Source::from_bytes(&setup)) {
                return ExecutionResult {
                    status: ExecutionStatus::Failed,
                    stdout: String::new(),
                    stderr: format!("Failed to inject input: {e:?}"),
                    output: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                    metadata: None,
                };
            }

            // Wrap user code in an IIFE so `return` works.
            let wrapped = format!("(function() {{ {code} }})()");
            let eval_result = context.eval(Source::from_bytes(&wrapped));

            let duration_ms = start.elapsed().as_millis() as u64;

            // Check timeout (wall-clock, post-execution).
            if start.elapsed() > timeout {
                return ExecutionResult {
                    status: ExecutionStatus::Timeout,
                    stdout: String::new(),
                    stderr: format!("execution exceeded timeout of {}ms", timeout.as_millis()),
                    output: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    duration_ms,
                    metadata: None,
                };
            }

            match eval_result {
                Ok(val) => {
                    let json_output = js_value_to_json(&val, &mut context);
                    ExecutionResult {
                        status: ExecutionStatus::Success,
                        stdout: String::new(),
                        stderr: String::new(),
                        output: Some(json_output),
                        exit_code: None,
                        stdout_truncated: false,
                        stderr_truncated: false,
                        duration_ms,
                        metadata: None,
                    }
                }
                Err(e) => ExecutionResult {
                    status: ExecutionStatus::Failed,
                    stdout: String::new(),
                    stderr: format!("JavaScript error: {e:?}"),
                    output: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    duration_ms,
                    metadata: None,
                },
            }
        })
        .await
        .map_err(|e| ExecutionError::InternalError(format!("JS thread panicked: {e}")))?;

        Ok(result)
    }
}

/// Convert a `boa_engine` JS value to a `serde_json::Value`.
fn js_value_to_json(val: &JsValue, context: &mut Context) -> serde_json::Value {
    match val {
        JsValue::Null | JsValue::Undefined => serde_json::Value::Null,
        JsValue::Boolean(b) => serde_json::Value::Bool(*b),
        JsValue::Integer(n) => serde_json::json!(*n),
        JsValue::Rational(n) => {
            if n.is_finite() {
                serde_json::json!(*n)
            } else {
                serde_json::Value::Null
            }
        }
        JsValue::String(s) => serde_json::Value::String(s.to_std_string_escaped()),
        JsValue::BigInt(n) => serde_json::Value::String(n.to_string()),
        JsValue::Symbol(_) => serde_json::Value::Null,
        JsValue::Object(_) => {
            // Use JSON.stringify to serialize complex objects.
            let stringify_code = format!(
                "JSON.stringify({})",
                // Re-evaluate the value by wrapping in a closure isn't practical,
                // so we use a global temp variable approach.
                "__adk_tmp__"
            );
            // Set the value as a global temp
            let global = context.global_object();
            let key = boa_engine::JsString::from("__adk_tmp__");
            let _ = global.set(key.clone(), val.clone(), false, context);
            let result = context.eval(Source::from_bytes(stringify_code.as_bytes()));
            // Clean up
            let _ = global.delete_property_or_throw(key, context);

            if let Ok(json_val) = result {
                if let Some(s) = json_val.as_string() {
                    let std_str: String = s.to_std_string_escaped();
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&std_str) {
                        return parsed;
                    }
                }
            }
            serde_json::Value::String("[object]".to_string())
        }
    }
}
