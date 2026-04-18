//! [`CodeTool`] — an [`adk_core::Tool`] implementation that dispatches
//! to language-specific executors.
//!
//! Currently supports Rust via [`RustExecutor`]. Other languages return a
//! descriptive "not yet supported" message. Errors from the executor are
//! converted to structured JSON responses (never propagated as `ToolError`),
//! following the same error-as-information pattern as
//! [`SandboxTool`](adk_sandbox::SandboxTool).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, instrument};

use adk_core::ToolContext;

use crate::error::CodeError;
use crate::rust_executor::RustExecutor;

/// Default timeout in seconds when `timeout_secs` is not provided.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Minimum allowed timeout in seconds.
const MIN_TIMEOUT_SECS: u64 = 1;

/// Maximum allowed timeout in seconds.
const MAX_TIMEOUT_SECS: u64 = 300;

/// Scopes required to execute this tool.
const REQUIRED_SCOPES: &[&str] = &["code:execute", "code:execute:rust"];

/// A tool that executes code through language-specific pipelines.
///
/// `CodeTool` wraps a [`RustExecutor`] and implements [`adk_core::Tool`],
/// making code execution with compiler diagnostics available to LLM agents.
/// Phase 1 supports Rust only; other languages return a descriptive error.
///
/// # Error Handling
///
/// Executor errors are **never** propagated as `ToolError`. Instead, they are
/// converted to structured JSON with a `"status"` field. Compile errors include
/// a `"diagnostics"` array with structured compiler output.
///
/// # Example
///
/// ```rust,ignore
/// use adk_code::{CodeTool, RustExecutor, RustExecutorConfig};
/// use adk_sandbox::ProcessBackend;
/// use std::sync::Arc;
///
/// let backend = Arc::new(ProcessBackend::default());
/// let executor = RustExecutor::new(backend, RustExecutorConfig::default());
/// let tool = CodeTool::new(executor);
/// assert_eq!(tool.name(), "code_exec");
/// ```
pub struct CodeTool {
    executor: RustExecutor,
}

impl CodeTool {
    /// Creates a new `CodeTool` wrapping the given Rust executor.
    pub fn new(executor: RustExecutor) -> Self {
        Self { executor }
    }
}

/// Converts a [`CodeError`] into a structured JSON value.
///
/// The returned JSON always contains a `"status"` field so the agent can
/// distinguish between different failure modes.
fn code_error_to_json(err: &CodeError) -> Value {
    match err {
        CodeError::CompileError { diagnostics, stderr } => {
            let diag_json: Vec<Value> = diagnostics
                .iter()
                .map(|d| {
                    json!({
                        "level": d.level,
                        "message": d.message,
                        "spans": d.spans.iter().map(|s| json!({
                            "file_name": s.file_name,
                            "line_start": s.line_start,
                            "line_end": s.line_end,
                            "column_start": s.column_start,
                            "column_end": s.column_end,
                        })).collect::<Vec<_>>(),
                        "code": d.code,
                    })
                })
                .collect();
            json!({
                "status": "compile_error",
                "diagnostics": diag_json,
                "stderr": stderr,
            })
        }
        CodeError::DependencyNotFound { name, searched } => json!({
            "status": "error",
            "stderr": format!("dependency not found: {name} (searched: {searched:?})"),
        }),
        CodeError::Sandbox(sandbox_err) => {
            use adk_sandbox::SandboxError;
            match sandbox_err {
                SandboxError::Timeout { timeout } => json!({
                    "status": "timeout",
                    "stderr": format!("execution timed out after {timeout:?}"),
                    "duration_ms": timeout.as_millis() as u64,
                }),
                SandboxError::MemoryExceeded { limit_mb } => json!({
                    "status": "memory_exceeded",
                    "stderr": format!("memory limit exceeded: {limit_mb} MB"),
                }),
                SandboxError::ExecutionFailed(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::InvalidRequest(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::BackendUnavailable(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
                SandboxError::EnforcerFailed { enforcer, message } => json!({
                    "status": "error",
                    "stderr": format!("sandbox enforcer '{enforcer}' failed: {message}"),
                }),
                SandboxError::EnforcerUnavailable { enforcer, message } => json!({
                    "status": "error",
                    "stderr": format!("sandbox enforcer '{enforcer}' unavailable: {message}"),
                }),
                SandboxError::PolicyViolation(msg) => json!({
                    "status": "error",
                    "stderr": msg,
                }),
            }
        }
        CodeError::InvalidCode(msg) => json!({
            "status": "error",
            "stderr": msg,
        }),
    }
}

#[async_trait]
impl adk_core::Tool for CodeTool {
    fn name(&self) -> &str {
        "code_exec"
    }

    fn description(&self) -> &str {
        "Execute Rust code through a check → build → execute pipeline. \
         The code must provide a `fn run(input: serde_json::Value) -> serde_json::Value` \
         entry point. Compile errors are returned as structured diagnostics."
    }

    fn required_scopes(&self) -> &[&str] {
        REQUIRED_SCOPES
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["rust"],
                    "description": "The programming language. Currently only \"rust\" is supported.",
                    "default": "rust"
                },
                "code": {
                    "type": "string",
                    "description": "The Rust source code to execute. Must provide `fn run(input: serde_json::Value) -> serde_json::Value`."
                },
                "input": {
                    "type": "object",
                    "description": "Optional JSON input passed to the `run()` function via stdin."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum execution time in seconds.",
                    "default": DEFAULT_TIMEOUT_SECS,
                    "minimum": MIN_TIMEOUT_SECS,
                    "maximum": MAX_TIMEOUT_SECS
                }
            },
            "required": ["code"]
        }))
    }

    #[instrument(skip_all, fields(tool = "code_exec"))]
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        // Parse language (optional, defaults to "rust").
        let language = args.get("language").and_then(|v| v.as_str()).unwrap_or("rust");

        if language != "rust" {
            return Ok(json!({
                "status": "error",
                "stderr": format!(
                    "unsupported language \"{language}\". Only \"rust\" is currently supported."
                ),
            }));
        }

        // Parse code (required).
        let code = match args.get("code").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return Ok(json!({
                    "status": "error",
                    "stderr": "missing required field \"code\"",
                }));
            }
        };

        // Parse input (optional JSON value).
        let input = args.get("input").cloned();

        // Parse timeout_secs (optional, default 30).
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);

        let timeout = Duration::from_secs(timeout_secs);

        debug!(language, timeout_secs, has_input = input.is_some(), "dispatching to RustExecutor");

        match self.executor.execute(code, input.as_ref(), timeout).await {
            Ok(result) => Ok(json!({
                "status": "success",
                "stdout": result.display_stdout,
                "stderr": result.exec_result.stderr,
                "exit_code": result.exec_result.exit_code,
                "duration_ms": result.exec_result.duration.as_millis() as u64,
                "output": result.output,
                "diagnostics": result.diagnostics.iter().map(|d| json!({
                    "level": d.level,
                    "message": d.message,
                    "spans": d.spans.iter().map(|s| json!({
                        "file_name": s.file_name,
                        "line_start": s.line_start,
                        "line_end": s.line_end,
                        "column_start": s.column_start,
                        "column_end": s.column_end,
                    })).collect::<Vec<_>>(),
                    "code": d.code,
                })).collect::<Vec<_>>(),
            })),
            Err(err) => Ok(code_error_to_json(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::RustDiagnostic;
    use crate::rust_executor::RustExecutorConfig;
    use adk_core::{CallbackContext, Content, EventActions, ReadonlyContext, Tool};
    use adk_sandbox::SandboxBackend;
    use adk_sandbox::backend::{BackendCapabilities, EnforcedLimits};
    use adk_sandbox::error::SandboxError;
    use adk_sandbox::types::{ExecRequest, ExecResult, Language};
    use std::sync::Mutex;
    use std::time::Duration;

    // -- Mock backend ----------------------------------------------------------

    struct MockBackend {
        response: Mutex<Option<Result<ExecResult, SandboxError>>>,
    }

    impl MockBackend {
        fn success(stdout: &str) -> Self {
            Self {
                response: Mutex::new(Some(Ok(ExecResult {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    exit_code: 0,
                    duration: Duration::from_millis(10),
                }))),
            }
        }
    }

    #[async_trait]
    impl SandboxBackend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supported_languages: vec![Language::Command],
                isolation_class: "mock".to_string(),
                enforced_limits: EnforcedLimits {
                    timeout: true,
                    memory: false,
                    network_isolation: false,
                    filesystem_isolation: false,
                    environment_isolation: false,
                },
            }
        }

        async fn execute(&self, _request: ExecRequest) -> Result<ExecResult, SandboxError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(SandboxError::ExecutionFailed("no canned response".to_string())))
        }
    }

    // -- Mock ToolContext -------------------------------------------------------

    struct MockToolContext {
        content: Content,
        actions: Mutex<EventActions>,
    }

    impl MockToolContext {
        fn new() -> Self {
            Self { content: Content::new("user"), actions: Mutex::new(EventActions::default()) }
        }
    }

    #[async_trait]
    impl ReadonlyContext for MockToolContext {
        fn invocation_id(&self) -> &str {
            "inv-1"
        }
        fn agent_name(&self) -> &str {
            "test-agent"
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
    impl CallbackContext for MockToolContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for MockToolContext {
        fn function_call_id(&self) -> &str {
            "call-1"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(
            &self,
            _query: &str,
        ) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
            Ok(vec![])
        }
    }

    fn ctx() -> Arc<dyn ToolContext> {
        Arc::new(MockToolContext::new())
    }

    fn make_tool() -> CodeTool {
        let backend = Arc::new(MockBackend::success(""));
        let executor = RustExecutor::new(backend, RustExecutorConfig::default());
        CodeTool::new(executor)
    }

    // -- Tests -----------------------------------------------------------------

    #[test]
    fn test_name() {
        let tool = make_tool();
        assert_eq!(tool.name(), "code_exec");
    }

    #[test]
    fn test_description_is_nonempty() {
        let tool = make_tool();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_required_scopes() {
        let tool = make_tool();
        assert_eq!(tool.required_scopes(), &["code:execute", "code:execute:rust"]);
    }

    #[test]
    fn test_parameters_schema_is_valid() {
        let tool = make_tool();
        let schema = tool.parameters_schema().expect("schema should be Some");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["language"].is_object());
        assert!(schema["properties"]["code"].is_object());
        assert!(schema["properties"]["input"].is_object());
        assert!(schema["properties"]["timeout_secs"].is_object());

        let required = schema["required"].as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"code"));
        // language is optional (defaults to "rust")
        assert!(!required_strs.contains(&"language"));
    }

    #[tokio::test]
    async fn test_missing_code_field() {
        let tool = make_tool();
        let args = json!({ "language": "rust" });
        let result = tool.execute(ctx(), args).await.unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["stderr"].as_str().unwrap().contains("code"));
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        let tool = make_tool();
        let args = json!({ "language": "python", "code": "print('hi')" });
        let result = tool.execute(ctx(), args).await.unwrap();
        assert_eq!(result["status"], "error");
        assert!(result["stderr"].as_str().unwrap().contains("python"));
        assert!(result["stderr"].as_str().unwrap().contains("unsupported"));
    }

    #[tokio::test]
    async fn test_missing_language_defaults_to_rust() {
        // Without a language field, it should default to "rust" and attempt
        // execution. The mock backend won't actually compile, but we verify
        // it doesn't return an "unsupported language" error.
        let tool = make_tool();
        let args =
            json!({ "code": "fn run(input: serde_json::Value) -> serde_json::Value { input }" });
        let result = tool.execute(ctx(), args).await.unwrap();
        // It will either succeed or fail with a compile/dependency error,
        // but NOT with "unsupported language".
        let status = result["status"].as_str().unwrap();
        assert_ne!(status, "error_unsupported_language");
        // The status should not mention "unsupported"
        if status == "error" {
            let stderr = result["stderr"].as_str().unwrap_or("");
            assert!(!stderr.contains("unsupported language"));
        }
    }

    #[test]
    fn test_code_error_to_json_compile_error() {
        let err = CodeError::CompileError {
            diagnostics: vec![RustDiagnostic {
                level: "error".to_string(),
                message: "expected `;`".to_string(),
                spans: vec![],
                code: Some("E0308".to_string()),
            }],
            stderr: "error: expected `;`".to_string(),
        };
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "compile_error");
        assert!(json["diagnostics"].is_array());
        assert_eq!(json["diagnostics"][0]["level"], "error");
        assert_eq!(json["diagnostics"][0]["message"], "expected `;`");
        assert_eq!(json["diagnostics"][0]["code"], "E0308");
        assert_eq!(json["stderr"], "error: expected `;`");
    }

    #[test]
    fn test_code_error_to_json_dependency_not_found() {
        let err = CodeError::DependencyNotFound {
            name: "serde_json".to_string(),
            searched: vec!["config: /fake/path".to_string()],
        };
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "error");
        assert!(json["stderr"].as_str().unwrap().contains("serde_json"));
    }

    #[test]
    fn test_code_error_to_json_sandbox_timeout() {
        let err = CodeError::Sandbox(SandboxError::Timeout { timeout: Duration::from_secs(5) });
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "timeout");
        assert!(json["stderr"].as_str().unwrap().contains("timed out"));
    }

    #[test]
    fn test_code_error_to_json_invalid_code() {
        let err = CodeError::InvalidCode("missing `fn run()` entry point".to_string());
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "error");
        assert!(json["stderr"].as_str().unwrap().contains("fn run()"));
    }

    #[test]
    fn test_code_error_to_json_sandbox_memory() {
        let err = CodeError::Sandbox(SandboxError::MemoryExceeded { limit_mb: 128 });
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "memory_exceeded");
    }

    #[test]
    fn test_code_error_to_json_sandbox_execution_failed() {
        let err = CodeError::Sandbox(SandboxError::ExecutionFailed("boom".into()));
        let json = code_error_to_json(&err);
        assert_eq!(json["status"], "error");
        assert_eq!(json["stderr"], "boom");
    }
}
