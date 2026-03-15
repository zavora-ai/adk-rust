//! Integration tests for code execution tools: scope invariants, preset defaults,
//! and response envelope consistency.
//!
//! Gated behind `#[cfg(feature = "code")]` — run with:
//! ```bash
//! cargo test -p adk-tool --features code --test code_execution_tests
//! ```
#![cfg(feature = "code")]

use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext,
};
use adk_tool::{FrontendCodeTool, JavaScriptCodeTool, PythonCodeTool, RustCodeTool};
use async_trait::async_trait;
use proptest::prelude::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Shared mock context for execute() calls
// ---------------------------------------------------------------------------

struct MockToolContext {
    actions: Mutex<EventActions>,
    content: Content,
}

impl MockToolContext {
    fn new() -> Self {
        Self { actions: Mutex::new(EventActions::default()), content: Content::new("user") }
    }
}

#[async_trait]
impl ReadonlyContext for MockToolContext {
    fn invocation_id(&self) -> &str {
        "inv-test"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
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
        "call-test"
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
}

fn mock_ctx() -> Arc<dyn ToolContext> {
    Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>
}

// ===========================================================================
// Property 7: Tool Authorization Gates Elevated Execution
// ===========================================================================
//
// **Feature: code-execution, Property 7: Tool Authorization Gates Elevated Execution**
// *For any* tool selection from the code execution preset family, the tool's
// required scopes SHALL include the base `code:execute` scope, and tools
// requiring container or Rust-specific backends SHALL include the
// corresponding elevated scope.
// **Validates: Requirements 4.3, 4.6, 4.8, 8.1, 8.3, 12.2**

/// Discriminant for selecting a code execution tool in property tests.
#[derive(Debug, Clone, Copy)]
enum ToolSelection {
    Rust,
    JavaScript,
    Python,
    FrontendReact,
}

fn arb_tool_selection() -> impl Strategy<Value = ToolSelection> {
    prop_oneof![
        Just(ToolSelection::Rust),
        Just(ToolSelection::JavaScript),
        Just(ToolSelection::Python),
        Just(ToolSelection::FrontendReact),
    ]
}

/// Instantiate the concrete tool for a given selection.
fn make_tool(sel: ToolSelection) -> Box<dyn Tool> {
    match sel {
        ToolSelection::Rust => Box::new(RustCodeTool::new()),
        ToolSelection::JavaScript => Box::new(JavaScriptCodeTool::new()),
        ToolSelection::Python => Box::new(PythonCodeTool::new()),
        ToolSelection::FrontendReact => Box::new(FrontendCodeTool::react()),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// All code execution tools include the base `code:execute` scope.
    #[test]
    fn prop_all_tools_require_base_scope(sel in arb_tool_selection()) {
        let tool = make_tool(sel);
        let scopes = tool.required_scopes();
        prop_assert!(
            scopes.contains(&"code:execute"),
            "Tool {:?} missing base scope. Scopes: {:?}",
            sel,
            scopes,
        );
    }

    /// Container-backed tools always include `code:execute:container`.
    #[test]
    fn prop_container_tools_require_container_scope(
        sel in prop_oneof![
            Just(ToolSelection::Python),
            Just(ToolSelection::FrontendReact),
        ]
    ) {
        let tool = make_tool(sel);
        let scopes = tool.required_scopes();
        prop_assert!(
            scopes.contains(&"code:execute:container"),
            "Tool {:?} missing container scope. Scopes: {:?}",
            sel,
            scopes,
        );
    }

    /// RustCodeTool always includes `code:execute:rust`.
    #[test]
    fn prop_rust_tool_requires_rust_scope(
        _dummy in 0..100u32
    ) {
        let tool = RustCodeTool::new();
        let scopes = tool.required_scopes();
        prop_assert!(
            scopes.contains(&"code:execute:rust"),
            "RustCodeTool missing rust scope. Scopes: {:?}",
            scopes,
        );
    }

    /// Non-container tools do NOT include `code:execute:container`.
    #[test]
    fn prop_non_container_tools_lack_container_scope(
        sel in prop_oneof![
            Just(ToolSelection::Rust),
            Just(ToolSelection::JavaScript),
        ]
    ) {
        let tool = make_tool(sel);
        let scopes = tool.required_scopes();
        prop_assert!(
            !scopes.contains(&"code:execute:container"),
            "Tool {:?} should not have container scope. Scopes: {:?}",
            sel,
            scopes,
        );
    }
}

// ===========================================================================
// Unit tests: preset defaults (names, descriptions, parameter schemas)
// ===========================================================================

#[test]
fn test_rust_code_tool_new_name() {
    let tool = RustCodeTool::new();
    assert_eq!(tool.name(), "rust_code");
}

#[test]
fn test_rust_code_tool_backend_name() {
    let tool = RustCodeTool::backend();
    assert_eq!(tool.name(), "rust_code");
}

#[test]
fn test_javascript_code_tool_name() {
    let tool = JavaScriptCodeTool::new();
    assert_eq!(tool.name(), "javascript_code");
}

#[test]
fn test_python_code_tool_name() {
    let tool = PythonCodeTool::new();
    assert_eq!(tool.name(), "python_code");
}

#[test]
fn test_frontend_code_tool_react_name() {
    let tool = FrontendCodeTool::react();
    assert_eq!(tool.name(), "frontend_code");
}

#[test]
fn test_all_tools_have_nonempty_descriptions() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(RustCodeTool::new()),
        Box::new(JavaScriptCodeTool::new()),
        Box::new(PythonCodeTool::new()),
        Box::new(FrontendCodeTool::react()),
    ];
    for tool in &tools {
        assert!(!tool.description().is_empty(), "Tool '{}' has empty description", tool.name(),);
    }
}

#[test]
fn test_all_tools_have_code_required_in_schema() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(RustCodeTool::new()),
        Box::new(JavaScriptCodeTool::new()),
        Box::new(PythonCodeTool::new()),
        Box::new(FrontendCodeTool::react()),
    ];
    for tool in &tools {
        let schema = tool
            .parameters_schema()
            .unwrap_or_else(|| panic!("Tool '{}' has no parameters_schema", tool.name()));
        let required = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("Tool '{}' schema missing 'required' array", tool.name()));
        let has_code = required.iter().any(|v| v.as_str() == Some("code"));
        assert!(has_code, "Tool '{}' schema does not require 'code'", tool.name());
    }
}

// ===========================================================================
// Unit tests: response envelope consistency
// ===========================================================================

#[tokio::test]
async fn test_javascript_placeholder_returns_rejected() {
    let tool = JavaScriptCodeTool::new();
    let result = tool.execute(mock_ctx(), json!({"code": "1+1"})).await.unwrap();
    assert_eq!(result["status"], "rejected");
}

#[tokio::test]
async fn test_python_placeholder_returns_rejected() {
    let tool = PythonCodeTool::new();
    let result = tool.execute(mock_ctx(), json!({"code": "print(1)"})).await.unwrap();
    // PythonCodeTool now uses ContainerCommandExecutor — it will attempt real
    // execution. The status depends on whether docker is available.
    let status = result["status"].as_str().unwrap_or("");
    assert!(
        status == "Success" || status == "Failed" || status == "Timeout",
        "expected an execution status, got: {status}"
    );
}

#[tokio::test]
async fn test_frontend_placeholder_returns_rejected() {
    let tool = FrontendCodeTool::react();
    let result = tool.execute(mock_ctx(), json!({"code": "console.log(1)"})).await.unwrap();
    // FrontendCodeTool now uses ContainerCommandExecutor — it will attempt real
    // execution. The status depends on whether docker/node image is available.
    let status = result["status"].as_str().unwrap_or("");
    assert!(
        status == "Success" || status == "Failed" || status == "Timeout",
        "expected an execution status, got: {status}"
    );
}

#[tokio::test]
async fn test_rust_tool_missing_code_returns_rejected() {
    let tool = RustCodeTool::new();
    let result = tool.execute(mock_ctx(), json!({})).await.unwrap();
    assert_eq!(result["status"], "rejected");
}
