use adk_core::{Content, ReadonlyContext, Tool, Toolset};
use adk_tool::{string_predicate, BasicToolset, ExitLoopTool, GoogleSearchTool};
use async_trait::async_trait;
use std::sync::Arc;

struct MockContext {
    content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self { content: Content::new("user") }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
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

#[tokio::test]
async fn test_basic_toolset() {
    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(ExitLoopTool::new()), Arc::new(GoogleSearchTool::new())];

    let toolset = BasicToolset::new("test_toolset", tools);
    assert_eq!(toolset.name(), "test_toolset");

    let ctx = Arc::new(MockContext::new()) as Arc<dyn ReadonlyContext>;
    let result_tools = toolset.tools(ctx).await.unwrap();
    assert_eq!(result_tools.len(), 2);
}

#[tokio::test]
async fn test_toolset_with_predicate() {
    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(ExitLoopTool::new()), Arc::new(GoogleSearchTool::new())];

    let predicate = string_predicate(vec!["exit_loop".to_string()]);
    let toolset = BasicToolset::new("filtered_toolset", tools).with_predicate(predicate);

    let ctx = Arc::new(MockContext::new()) as Arc<dyn ReadonlyContext>;
    let result_tools = toolset.tools(ctx).await.unwrap();

    assert_eq!(result_tools.len(), 1);
    assert_eq!(result_tools[0].name(), "exit_loop");
}

#[tokio::test]
async fn test_string_predicate_multiple() {
    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(ExitLoopTool::new()), Arc::new(GoogleSearchTool::new())];

    let predicate = string_predicate(vec!["exit_loop".to_string(), "google_search".to_string()]);
    let toolset = BasicToolset::new("all_tools", tools).with_predicate(predicate);

    let ctx = Arc::new(MockContext::new()) as Arc<dyn ReadonlyContext>;
    let result_tools = toolset.tools(ctx).await.unwrap();

    assert_eq!(result_tools.len(), 2);
}

#[tokio::test]
async fn test_empty_predicate() {
    let tools: Vec<Arc<dyn Tool>> =
        vec![Arc::new(ExitLoopTool::new()), Arc::new(GoogleSearchTool::new())];

    let predicate = string_predicate(vec![]);
    let toolset = BasicToolset::new("no_tools", tools).with_predicate(predicate);

    let ctx = Arc::new(MockContext::new()) as Arc<dyn ReadonlyContext>;
    let result_tools = toolset.tools(ctx).await.unwrap();

    assert_eq!(result_tools.len(), 0);
}
