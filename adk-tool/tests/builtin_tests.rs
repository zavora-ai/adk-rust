use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext,
};
use adk_tool::{
    AnthropicBashTool20250124, AnthropicTextEditorTool20250728, ExitLoopTool, GoogleSearchTool,
    OpenAIWebSearchTool, WebSearchTool,
};
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

#[test]
fn test_native_declarations_are_exposed() {
    let gemini_decl = GoogleSearchTool::new().declaration();
    assert_eq!(gemini_decl["x-adk-gemini-tool"]["google_search"], json!({}));

    let openai_decl = OpenAIWebSearchTool::new().declaration();
    assert_eq!(openai_decl["x-adk-openai-tool"]["type"], "web_search_2025_08_26");

    let anthropic_decl = WebSearchTool::new().declaration();
    assert_eq!(anthropic_decl["x-adk-anthropic-tool"]["type"], "web_search_20250305");
}

#[tokio::test]
async fn test_anthropic_bash_tool_executes_shell_command() {
    let tool = AnthropicBashTool20250124::new();
    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool
        .execute(ctx, json!({ "command": "printf hello", "restart": false }))
        .await
        .expect("bash should succeed");

    assert_eq!(result, json!("hello\nexit_code: 0\n"));
}

#[tokio::test]
async fn test_anthropic_text_editor_tool_executes_local_edits() {
    let tool = AnthropicTextEditorTool20250728::new();
    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let file_path = std::env::temp_dir().join(format!("adk-tool-{}.txt", uuid::Uuid::new_v4()));
    let file_path_str = file_path.to_string_lossy().to_string();

    tool.execute(
        Arc::clone(&ctx),
        json!({ "command": "create", "path": file_path_str, "file_text": "alpha\nbeta\n" }),
    )
    .await
    .expect("create should succeed");

    let viewed = tool
        .execute(
            Arc::clone(&ctx),
            json!({ "command": "view", "path": file_path.to_string_lossy() }),
        )
        .await
        .expect("view should succeed");
    assert_eq!(viewed, json!("alpha\nbeta\n"));

    tool.execute(
        Arc::clone(&ctx),
        json!({
            "command": "str_replace",
            "path": file_path.to_string_lossy(),
            "old_str": "beta",
            "new_str": "gamma"
        }),
    )
    .await
    .expect("replace should succeed");

    let updated = tool
        .execute(ctx, json!({ "command": "view", "path": file_path.to_string_lossy() }))
        .await
        .expect("view after replace should succeed");
    assert_eq!(updated, json!("alpha\ngamma\n"));

    let _ = std::fs::remove_file(file_path);
}
