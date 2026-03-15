//! Integration tests for `CodeTool`.
//!
//! These tests require `rustc` and a discoverable `serde_json` rlib.
//! Run with:
//!
//! ```bash
//! cargo test -p adk-code --test code_tool_integration -- --ignored
//! ```

use adk_code::{CodeTool, RustExecutor, RustExecutorConfig};
use adk_core::{CallbackContext, Content, EventActions, ReadonlyContext, Tool, ToolContext};
use adk_sandbox::ProcessBackend;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Mock ToolContext (required by Tool::execute)
// ---------------------------------------------------------------------------

struct TestToolContext {
    content: Content,
    actions: Mutex<EventActions>,
}

impl TestToolContext {
    fn new() -> Self {
        Self { content: Content::new("user"), actions: Mutex::new(EventActions::default()) }
    }
}

#[async_trait]
impl ReadonlyContext for TestToolContext {
    fn invocation_id(&self) -> &str {
        "inv-integration"
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
impl CallbackContext for TestToolContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestToolContext {
    fn function_call_id(&self) -> &str {
        "call-integration"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

fn ctx() -> Arc<dyn ToolContext> {
    Arc::new(TestToolContext::new())
}

fn make_tool() -> CodeTool {
    let backend = Arc::new(ProcessBackend::default());
    let executor = RustExecutor::new(backend, RustExecutorConfig::default());
    CodeTool::new(executor)
}

// ---------------------------------------------------------------------------
// Valid Rust code → success
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn code_tool_valid_rust_returns_success() {
    let tool = make_tool();
    let args = json!({
        "code": r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "result": 42 })
}
"#,
        "timeout_secs": 60
    });

    let result = tool.execute(ctx(), args).await.expect("Tool::execute never returns Err");
    assert_eq!(result["status"], "success", "result: {result}");
    assert_eq!(result["exit_code"], 0);
}

// ---------------------------------------------------------------------------
// Compile error → structured diagnostics (not ToolError)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn code_tool_compile_error_returns_diagnostics() {
    let tool = make_tool();
    let args = json!({
        "code": r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    let x: i32 = "oops";
    input
}
"#,
        "timeout_secs": 60
    });

    let result = tool.execute(ctx(), args).await.expect("Tool::execute never returns Err");
    assert_eq!(
        result["status"], "compile_error",
        "compile errors should be returned as information, not ToolError: {result}"
    );
    assert!(result["diagnostics"].is_array(), "should include diagnostics array");
}

// ---------------------------------------------------------------------------
// Runtime error (panic) → non-zero exit code
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn code_tool_runtime_panic_returns_nonzero_exit_code() {
    let tool = make_tool();
    let args = json!({
        "code": r#"
fn run(_input: serde_json::Value) -> serde_json::Value {
    panic!("intentional panic");
}
"#,
        "timeout_secs": 60
    });

    let result = tool.execute(ctx(), args).await.expect("Tool::execute never returns Err");
    // A panic produces a non-zero exit code. The status may be "success"
    // (the pipeline succeeded) but exit_code will be non-zero.
    let exit_code = result["exit_code"].as_i64().unwrap_or(-1);
    assert_ne!(exit_code, 0, "panic should produce non-zero exit code: {result}");
}
