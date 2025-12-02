use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext,
};
use adk_tool::{ExitLoopTool, GoogleSearchTool};
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct MockToolContext {
    actions: Mutex<EventActions>,
    content: Content,
}

impl MockToolContext {
    fn new() -> Self {
        Self { actions: Mutex::new(EventActions::default()), content: Content::new("user") }
    }

    fn current_actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
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
        "call-1"
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
    let ctx = Arc::new(MockToolContext::new());
    let tool_ctx: Arc<dyn ToolContext> = ctx.clone();
    let result = tool.execute(tool_ctx, json!({})).await;
    assert!(result.is_ok());
    let actions = ctx.current_actions();
    assert!(actions.escalate);
    assert!(actions.skip_summarization);
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
