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
//! - In-process execution on a dedicated thread (no container isolation)
//! - No filesystem access
//! - No network access
//! - No child process creation
//! - Deadline enforced on the result: `execute` returns
//!   [`ExecutionStatus::Timeout`] once the policy timeout passes
//! - Runtime limits stop runaway scripts: loop iterations per call frame,
//!   call depth, and VM stack size
//! - No heap limit — `boa_engine` 0.20 has none
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
use boa_engine::{Context, JsNativeError, JsValue, Source};
use std::time::Instant;
use tracing::warn;

use crate::{
    BackendCapabilities, CodeExecutor, ExecutionError, ExecutionIsolation, ExecutionLanguage,
    ExecutionPayload, ExecutionRequest, ExecutionResult, ExecutionStatus,
};

/// Loop iterations allowed per call frame before Boa throws an uncatchable
/// `RuntimeLimit` error. Boa has no preemption, so this is what stops a
/// `while (true) {}` after the caller's deadline has passed.
const LOOP_ITERATION_LIMIT: u64 = 10_000_000;

/// Maximum JavaScript call depth.
const RECURSION_LIMIT: usize = 512;

/// Maximum number of values on the Boa VM stack.
const STACK_SIZE_LIMIT: usize = 10 * 1024;

/// Native stack for the interpreter thread. JavaScript calls made through
/// native builtins (`Array.prototype.map` callbacks, getters) recurse on the
/// Rust stack; 8 MiB leaves headroom for unoptimised `boa_engine` builds, whose
/// frames are larger, to reach `RECURSION_LIMIT` before overflowing.
const INTERPRETER_STACK_BYTES: usize = 8 * 1024 * 1024;

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
/// # Timeouts and Runtime Limits
///
/// Each evaluation runs on its own thread. [`execute`](CodeExecutor::execute)
/// waits for it until [`SandboxPolicy::timeout`](crate::SandboxPolicy::timeout)
/// and then returns [`ExecutionStatus::Timeout`].
///
/// Boa cannot be interrupted from another thread, so a timed-out evaluation keeps
/// running in the background, consuming a core, until it finishes or reaches a
/// runtime limit. The loop limit counts per call frame, so a loop that repeatedly
/// calls a looping function can run far longer than one frame's limit.
///
/// | Limit | Value | Error |
/// |---|---|---|
/// | Loop iterations per call frame | 10,000,000 | `RuntimeLimit` |
/// | Call depth | 512 | `RuntimeLimit` |
/// | VM stack values | 10,240 | `RuntimeLimit` |
///
/// `RuntimeLimit` errors cannot be caught by `try`/`catch`. A limit reached
/// before the deadline yields [`ExecutionStatus::Failed`] with the limit named in
/// `stderr`. Work inside a single native builtin — a pathological regular
/// expression, for example — is not counted by any limit, and there is no heap
/// limit.
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
    /// omission). The timeout is enforced on the result: `execute` returns at the
    /// deadline, and runtime limits stop the abandoned interpreter. See the
    /// [struct-level docs](Self) for details.
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
        let start = Instant::now();

        // A dedicated thread rather than `spawn_blocking`: a runaway script must
        // not occupy a blocking-pool thread or hold up runtime shutdown.
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("adk-embedded-js".to_string())
            .stack_size(INTERPRETER_STACK_BYTES)
            .spawn(move || {
                // The receiver is gone once the caller has timed out; the result is discarded.
                let _ = sender.send(evaluate(&code, input.as_ref()));
            })
            .map_err(|e| {
                ExecutionError::InternalError(format!("failed to spawn JavaScript thread: {e}"))
            })?;

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(ExecutionError::InternalError("JS thread panicked".to_string())),
            Err(_) => {
                warn!(
                    timeout_ms = timeout.as_millis() as u64,
                    "javascript execution timed out; the interpreter stops at its next runtime limit"
                );
                Ok(ExecutionResult {
                    status: ExecutionStatus::Timeout,
                    stdout: String::new(),
                    stderr: format!("execution exceeded timeout of {}ms", timeout.as_millis()),
                    output: None,
                    exit_code: None,
                    stdout_truncated: false,
                    stderr_truncated: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                    metadata: None,
                })
            }
        }
    }
}

/// Evaluates `code` in a fresh, limited Boa context with `input` bound as a global.
fn evaluate(code: &str, input: Option<&serde_json::Value>) -> ExecutionResult {
    let start = Instant::now();
    let mut context = Context::default();
    let limits = context.runtime_limits_mut();
    limits.set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
    limits.set_recursion_limit(RECURSION_LIMIT);
    limits.set_stack_size_limit(STACK_SIZE_LIMIT);

    let failed = |stderr: String, start: Instant| ExecutionResult {
        status: ExecutionStatus::Failed,
        stdout: String::new(),
        stderr,
        output: None,
        exit_code: None,
        stdout_truncated: false,
        stderr_truncated: false,
        duration_ms: start.elapsed().as_millis() as u64,
        metadata: None,
    };

    // Inject input as a global variable.
    let input_str = input
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| "null".to_string());

    let setup = format!("var input = {input_str};");
    if let Err(e) = context.eval(Source::from_bytes(&setup)) {
        return failed(format!("Failed to inject input: {e:?}"), start);
    }

    // Wrap user code in an IIFE so `return` works.
    let wrapped = format!("(function() {{ {code} }})()");
    match context.eval(Source::from_bytes(&wrapped)) {
        Ok(val) => {
            let output = js_value_to_json(&val, &mut context);
            ExecutionResult {
                status: ExecutionStatus::Success,
                stdout: String::new(),
                stderr: String::new(),
                output: Some(output),
                exit_code: None,
                stdout_truncated: false,
                stderr_truncated: false,
                duration_ms: start.elapsed().as_millis() as u64,
                metadata: None,
            }
        }
        Err(e) if e.as_native().is_some_and(JsNativeError::is_runtime_limit) => {
            failed(format!("JavaScript runtime limit exceeded: {e}"), start)
        }
        Err(e) => failed(format!("JavaScript error: {e:?}"), start),
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

            if let Ok(json_val) = result
                && let Some(s) = json_val.as_string()
            {
                let std_str: String = s.to_std_string_escaped();
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&std_str) {
                    return parsed;
                }
            }
            serde_json::Value::String("[object]".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxPolicy;
    use std::time::Duration;

    fn js_request(code: &str, timeout: Duration) -> ExecutionRequest {
        ExecutionRequest {
            language: ExecutionLanguage::JavaScript,
            payload: ExecutionPayload::Source { code: code.to_string() },
            argv: vec![],
            stdin: None,
            input: Some(serde_json::json!({ "x": 41 })),
            sandbox: SandboxPolicy { timeout, ..SandboxPolicy::strict_js() },
            identity: None,
        }
    }

    #[tokio::test]
    async fn returns_json_output() {
        let result = EmbeddedJsExecutor::new()
            .execute(js_request("return { y: input.x + 1 };", Duration::from_secs(5)))
            .await
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::Success);
        assert_eq!(result.output, Some(serde_json::json!({ "y": 42 })));
    }

    /// An infinite loop returns within the deadline instead of blocking the caller.
    #[tokio::test]
    async fn infinite_loop_returns_at_the_deadline() {
        let timeout = Duration::from_millis(500);
        let started = Instant::now();
        let result = EmbeddedJsExecutor::new()
            .execute(js_request("while (true) {}", timeout))
            .await
            .unwrap();

        assert!(
            started.elapsed() < timeout + Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
        let stopped_by_limit = result.status == ExecutionStatus::Failed
            && result.stderr.contains("runtime limit exceeded");
        assert!(
            result.status == ExecutionStatus::Timeout || stopped_by_limit,
            "unexpected result: {result:?}"
        );
    }

    /// The loop limit stops the interpreter itself, so an abandoned evaluation
    /// does not spin forever. `try`/`catch` cannot swallow it.
    #[test]
    fn loop_limit_stops_the_interpreter_and_cannot_be_caught() {
        let result = evaluate("try { while (true) {} } catch (e) { return 'caught'; }", None);
        assert_eq!(result.status, ExecutionStatus::Failed, "{result:?}");
        assert!(result.stderr.contains("runtime limit exceeded"), "{}", result.stderr);
        assert!(result.stderr.contains("loop iteration"), "{}", result.stderr);
    }

    /// Loop limits are per call frame, so repeated calls into a looping function
    /// outlast any single limit; the deadline still returns control to the caller.
    #[tokio::test]
    async fn work_spread_across_frames_returns_timeout_at_the_deadline() {
        let timeout = Duration::from_millis(300);
        let code =
            "function spin() { for (let i = 0; i < 1000000; i++) {} } while (true) { spin(); }";
        let started = Instant::now();
        let result = EmbeddedJsExecutor::new().execute(js_request(code, timeout)).await.unwrap();

        assert_eq!(result.status, ExecutionStatus::Timeout, "{result:?}");
        assert!(
            started.elapsed() < timeout + Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn unbounded_recursion_hits_the_recursion_limit() {
        let result = evaluate("function f() { return f(); } return f();", None);
        assert_eq!(result.status, ExecutionStatus::Failed, "{result:?}");
        assert!(result.stderr.contains("runtime limit exceeded"), "{}", result.stderr);
    }

    /// Recursion through a native builtin uses the Rust stack; the interpreter
    /// thread must hit the call-depth limit before it overflows.
    #[tokio::test]
    async fn recursion_through_native_builtins_hits_the_limit_not_the_native_stack() {
        let code = "function f(n) { return [n].map(x => f(x + 1)); } return f(0);";
        let result = EmbeddedJsExecutor::new()
            .execute(js_request(code, Duration::from_secs(30)))
            .await
            .unwrap();
        assert_eq!(result.status, ExecutionStatus::Failed, "{result:?}");
        assert!(result.stderr.contains("runtime limit exceeded"), "{}", result.stderr);
    }
}
