use adk_core::{Content, ReadonlyContext, Tool, Toolset, types::AdkIdentity};
use adk_tool::{BasicToolset, ExitLoopTool, GoogleSearchTool, string_predicate};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

struct MockContext {
    identity: AdkIdentity,
    content: Content,
    metadata: HashMap<String, String>,
}

impl MockContext {
    fn new() -> Self {
        Self {
            identity: AdkIdentity::default(),
            content: Content::new("user"),
            metadata: HashMap::new(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.content
    }

    fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
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
