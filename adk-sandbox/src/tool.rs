//! [`SandboxTool`] — an [`adk_core::Tool`] implementation that delegates
//! execution to a configured [`SandboxBackend`].
//!
//! The tool exposes code execution to LLM agents via the standard Tool trait.
//! Errors from the backend are converted to structured JSON responses (never
//! propagated as `ToolError`), so the agent always receives actionable
//! information about what happened.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use adk_core::ToolContext;

use crate::backend::SandboxBackend;
use crate::error::SandboxError;
use crate::types::{ExecRequest, Language};

/// A tool that executes code in an isolated sandbox.
///
/// `SandboxTool` wraps a [`SandboxBackend`] and implements [`adk_core::Tool`],
/// making sandbox execution available to LLM agents. The tool accepts
/// `language`, `code`, optional `stdin`, and optional `timeout_secs` parameters.
///
/// # Error Handling
///
/// Backend errors are **never** propagated as `ToolError`. Instead, they are
/// converted to structured JSON with a `"status"` field (`"timeout"`,
/// `"memory_exceeded"`, or `"error"`). This lets the agent reason about
/// failures without triggering exception handling.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::{SandboxTool, ProcessBackend};
/// use std::sync::Arc;
///
/// let backend = Arc::new(ProcessBackend::default());
/// let tool = SandboxTool::new(backend);
/// assert_eq!(tool.name(), "sandbox_exec");
/// ```
pub struct SandboxTool {
    backend: Arc<dyn SandboxBackend>,
}

impl SandboxTool {
    /// Creates a new `SandboxTool` wrapping the given backend.
    pub fn new(backend: Arc<dyn SandboxBackend>) -> Self {
        Self { backend }
    }
}

/// Default timeout in seconds when `timeout_secs` is not provided.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Scopes required to execute this tool.
const REQUIRED_SCOPES: &[&str] = &["code:execute"];

/// Parses a `Language` from a JSON string value.
fn parse_language(value: &Value) -> Result<Language, String> {
    let s = value.as_str().ok_or_else(|| "\"language\" must be a string".to_string())?;
    match s {
        "rust" => Ok(Language::Rust),
        "python" => Ok(Language::Python),
        "javascript" => Ok(Language::JavaScript),
        "typescript" => Ok(Language::TypeScript),
        "wasm" => Ok(Language::Wasm),
        "command" => Ok(Language::Command),
        other => Err(format!(
            "unsupported language \"{other}\". Expected one of: rust, python, javascript, typescript, wasm, command"
        )),
    }
}

/// Converts a [`SandboxError`] into a structured JSON value.
///
/// The returned JSON always contains a `"status"` field so the agent can
/// distinguish between different failure modes.
fn sandbox_error_to_json(err: &SandboxError) -> Value {
    match err {
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
    }
}

#[async_trait]
impl adk_core::Tool for SandboxTool {
    fn name(&self) -> &str {
        "sandbox_exec"
    }

    fn description(&self) -> &str {
        "Execute code in an isolated sandbox. Supports multiple languages \
         including rust, python, javascript, typescript, wasm, and shell commands."
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
                    "enum": ["rust", "python", "javascript", "typescript", "wasm", "command"],
                    "description": "The programming language of the code to execute."
                },
                "code": {
                    "type": "string",
                    "description": "The source code or command to execute."
                },
                "stdin": {
                    "type": "string",
                    "description": "Optional standard input to feed to the process."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum execution time in seconds.",
                    "default": DEFAULT_TIMEOUT_SECS,
                    "minimum": 1,
                    "maximum": 300
                }
            },
            "required": ["language", "code"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        // Parse language (required)
        let language = match args.get("language") {
            Some(v) => match parse_language(v) {
                Ok(lang) => lang,
                Err(msg) => {
                    return Ok(json!({ "status": "error", "stderr": msg }));
                }
            },
            None => {
                return Ok(
                    json!({ "status": "error", "stderr": "missing required field \"language\"" }),
                );
            }
        };

        // Parse code (required)
        let code = match args.get("code").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return Ok(
                    json!({ "status": "error", "stderr": "missing required field \"code\"" }),
                );
            }
        };

        // Parse stdin (optional)
        let stdin = args.get("stdin").and_then(|v| v.as_str()).map(String::from);

        // Parse timeout_secs (optional, default 30)
        let timeout_secs =
            args.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_TIMEOUT_SECS);

        let request = ExecRequest {
            language,
            code,
            stdin,
            timeout: Duration::from_secs(timeout_secs),
            memory_limit_mb: None,
            env: HashMap::new(),
        };

        match self.backend.execute(request).await {
            Ok(result) => Ok(json!({
                "status": "success",
                "stdout": result.stdout,
                "stderr": result.stderr,
                "exit_code": result.exit_code,
                "duration_ms": result.duration.as_millis() as u64,
            })),
            Err(err) => Ok(sandbox_error_to_json(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCapabilities, EnforcedLimits};
    use crate::types::ExecResult;
    use adk_core::{CallbackContext, Content, EventActions, ReadonlyContext, Tool};
    use std::sync::Mutex;
    use std::time::Duration;

    // -- Mock backend ----------------------------------------------------------

    /// A configurable mock backend for testing `SandboxTool`.
    struct MockBackend {
        /// When `Some`, `execute()` returns this error.
        error: Option<SandboxError>,
        /// When `error` is `None`, `execute()` returns this result.
        result: ExecResult,
    }

    impl MockBackend {
        fn success(stdout: &str, exit_code: i32) -> Self {
            Self {
                error: None,
                result: ExecResult {
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                    exit_code,
                    duration: Duration::from_millis(42),
                },
            }
        }

        fn failing(err: SandboxError) -> Self {
            Self {
                error: Some(err),
                result: ExecResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    duration: Duration::ZERO,
                },
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
                supported_languages: vec![Language::Python],
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
            if let Some(ref err) = self.error { Err(err.clone()) } else { Ok(self.result.clone()) }
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

    // -- Tests -----------------------------------------------------------------

    #[test]
    fn test_name() {
        let tool = SandboxTool::new(Arc::new(MockBackend::success("", 0)));
        assert_eq!(tool.name(), "sandbox_exec");
    }

    #[test]
    fn test_required_scopes() {
        let tool = SandboxTool::new(Arc::new(MockBackend::success("", 0)));
        assert_eq!(tool.required_scopes(), &["code:execute"]);
    }

    #[test]
    fn test_parameters_schema_is_valid() {
        let tool = SandboxTool::new(Arc::new(MockBackend::success("", 0)));
        let schema = tool.parameters_schema().expect("schema should be Some");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["language"].is_object());
        assert!(schema["properties"]["code"].is_object());
        assert!(schema["properties"]["stdin"].is_object());
        assert!(schema["properties"]["timeout_secs"].is_object());

        let required = schema["required"].as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"language"));
        assert!(required_strs.contains(&"code"));
        assert!(!required_strs.contains(&"stdin"));
        assert!(!required_strs.contains(&"timeout_secs"));
    }

    #[tokio::test]
    async fn test_successful_execution() {
        let backend = Arc::new(MockBackend::success("hello\n", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "python", "code": "print('hello')" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "success");
        assert_eq!(result["stdout"], "hello\n");
        assert_eq!(result["exit_code"], 0);
        assert!(result["duration_ms"].is_number());
    }

    #[tokio::test]
    async fn test_timeout_error_as_information() {
        let backend = Arc::new(MockBackend::failing(SandboxError::Timeout {
            timeout: Duration::from_secs(5),
        }));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "python", "code": "import time; time.sleep(100)" });

        let result = tool.execute(ctx(), args).await;

        // Must be Ok, not Err
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "timeout");
        assert!(val["stderr"].as_str().unwrap().contains("timed out"));
        assert!(val["duration_ms"].is_number());
    }

    #[tokio::test]
    async fn test_memory_exceeded_error_as_information() {
        let backend = Arc::new(MockBackend::failing(SandboxError::MemoryExceeded { limit_mb: 64 }));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "wasm", "code": "(module)" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "memory_exceeded");
        assert!(result["stderr"].as_str().unwrap().contains("64 MB"));
    }

    #[tokio::test]
    async fn test_execution_failed_error_as_information() {
        let backend = Arc::new(MockBackend::failing(SandboxError::ExecutionFailed(
            "spawn failed".to_string(),
        )));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "python", "code": "x" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "error");
        assert_eq!(result["stderr"], "spawn failed");
    }

    #[tokio::test]
    async fn test_missing_language_field() {
        let backend = Arc::new(MockBackend::success("", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({ "code": "print('hi')" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "error");
        assert!(result["stderr"].as_str().unwrap().contains("language"));
    }

    #[tokio::test]
    async fn test_missing_code_field() {
        let backend = Arc::new(MockBackend::success("", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "python" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "error");
        assert!(result["stderr"].as_str().unwrap().contains("code"));
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        let backend = Arc::new(MockBackend::success("", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "cobol", "code": "DISPLAY 'HI'" });

        let result = tool.execute(ctx(), args).await.unwrap();

        assert_eq!(result["status"], "error");
        assert!(result["stderr"].as_str().unwrap().contains("cobol"));
    }

    #[tokio::test]
    async fn test_custom_timeout() {
        // Verify that a custom timeout_secs is parsed (we can't easily verify
        // the Duration passed to the backend without more instrumentation, but
        // we can at least confirm the call succeeds).
        let backend = Arc::new(MockBackend::success("ok", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({ "language": "python", "code": "print('ok')", "timeout_secs": 60 });

        let result = tool.execute(ctx(), args).await.unwrap();
        assert_eq!(result["status"], "success");
    }

    #[tokio::test]
    async fn test_stdin_passed_through() {
        let backend = Arc::new(MockBackend::success("echo", 0));
        let tool = SandboxTool::new(backend);
        let args = json!({
            "language": "python",
            "code": "import sys; print(sys.stdin.read())",
            "stdin": "hello"
        });

        let result = tool.execute(ctx(), args).await.unwrap();
        assert_eq!(result["status"], "success");
    }

    #[test]
    fn test_parse_language_all_variants() {
        assert_eq!(parse_language(&json!("rust")).unwrap(), Language::Rust);
        assert_eq!(parse_language(&json!("python")).unwrap(), Language::Python);
        assert_eq!(parse_language(&json!("javascript")).unwrap(), Language::JavaScript);
        assert_eq!(parse_language(&json!("typescript")).unwrap(), Language::TypeScript);
        assert_eq!(parse_language(&json!("wasm")).unwrap(), Language::Wasm);
        assert_eq!(parse_language(&json!("command")).unwrap(), Language::Command);
        assert!(parse_language(&json!("ruby")).is_err());
        assert!(parse_language(&json!(42)).is_err());
    }

    #[test]
    fn test_sandbox_error_to_json_variants() {
        let timeout_json =
            sandbox_error_to_json(&SandboxError::Timeout { timeout: Duration::from_secs(10) });
        assert_eq!(timeout_json["status"], "timeout");

        let mem_json = sandbox_error_to_json(&SandboxError::MemoryExceeded { limit_mb: 128 });
        assert_eq!(mem_json["status"], "memory_exceeded");

        let exec_json = sandbox_error_to_json(&SandboxError::ExecutionFailed("boom".into()));
        assert_eq!(exec_json["status"], "error");

        let invalid_json = sandbox_error_to_json(&SandboxError::InvalidRequest("bad".into()));
        assert_eq!(invalid_json["status"], "error");

        let unavail_json = sandbox_error_to_json(&SandboxError::BackendUnavailable("gone".into()));
        assert_eq!(unavail_json["status"], "error");
    }
}
