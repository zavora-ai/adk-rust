use adk_core::{CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext};
use adk_tool::{ExitLoopTool, GoogleSearchTool};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

struct MockToolContext {
    actions: EventActions,
    content: Content,
}

impl MockToolContext {
    fn new() -> Self {
        Self {
            actions: EventActions::default(),
            content: Content::new("user"),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockToolContext {
    fn invocation_id(&self) -> &str { "inv-1" }
    fn agent_name(&self) -> &str { "test-agent" }
    fn user_id(&self) -> &str { "user-1" }
    fn app_name(&self) -> &str { "test-app" }
    fn session_id(&self) -> &str { "session-1" }
    fn branch(&self) -> &str { "" }
    fn user_content(&self) -> &Content { &self.content }
}

#[async_trait]
impl CallbackContext for MockToolContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
}

#[async_trait]
impl ToolContext for MockToolContext {
    fn function_call_id(&self) -> &str { "call-1" }
    fn actions(&self) -> &EventActions { &self.actions }
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
}

#[test]
fn test_exit_loop_tool_metadata() {
    let tool = ExitLoopTool::new();
    assert_eq!(tool.name(), "exit_loop");
    assert!(tool.description().contains("Exits the loop"));
    assert!(!tool.is_long_running());
}

#[tokio::test]
async fn test_exit_loop_execute() {
    let tool = ExitLoopTool::new();
    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({})).await;
    assert!(result.is_ok());
}

#[test]
fn test_google_search_tool_metadata() {
    let tool = GoogleSearchTool::new();
    assert_eq!(tool.name(), "google_search");
    assert!(tool.description().contains("Google search"));
    assert!(!tool.is_long_running());
}

#[tokio::test]
async fn test_google_search_not_executable() {
    let tool = GoogleSearchTool::new();
    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({})).await;
    assert!(result.is_err());
}
