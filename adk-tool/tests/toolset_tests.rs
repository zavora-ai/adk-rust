use adk_core::{Content, ReadonlyContext, Tool, Toolset};
use adk_tool::{BasicToolset, ExitLoopTool, GoogleSearchTool, string_predicate};
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

/// Declares itself read-only and concurrency-safe, with publisher annotations.
struct HintedTool {
    annotations: serde_json::Value,
}

#[async_trait]
impl Tool for HintedTool {
    fn name(&self) -> &str {
        "read_temperature"
    }
    fn description(&self) -> &str {
        "Reads the temperature"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn is_concurrency_safe(&self) -> bool {
        true
    }
    fn annotations(&self) -> Option<&serde_json::Value> {
        Some(&self.annotations)
    }
    async fn execute(
        &self,
        _ctx: Arc<dyn adk_core::ToolContext>,
        _args: serde_json::Value,
    ) -> adk_core::Result<serde_json::Value> {
        Ok(serde_json::json!(21.5))
    }
}

#[tokio::test]
async fn prefixed_tools_keep_the_safety_signals_and_annotations_of_the_tool_they_wrap() {
    let annotations = serde_json::json!({ "readOnlyHint": true, "openWorldHint": false });
    let inner = BasicToolset::new(
        "home",
        vec![Arc::new(HintedTool { annotations: annotations.clone() }) as Arc<dyn Tool>],
    );
    let prefixed = adk_tool::PrefixedToolset::new(Arc::new(inner), "home");

    let ctx = Arc::new(MockContext::new()) as Arc<dyn ReadonlyContext>;
    let tools = prefixed.tools(ctx).await.unwrap();

    let described: Vec<_> = tools
        .iter()
        .map(|tool| {
            (
                tool.name().to_string(),
                tool.is_read_only(),
                tool.is_concurrency_safe(),
                tool.effect(),
                tool.annotations().cloned(),
            )
        })
        .collect();
    let bare = HintedTool { annotations: described[0].4.clone().unwrap() };
    assert_eq!(tools[0].origin(), bare.origin(), "a prefix is not a new publisher");
    assert_eq!(
        adk_core::ToolOrigin::of(tools[0].as_ref()).source,
        adk_core::ToolOrigin::of(&bare).source
    );
    assert_eq!(
        described,
        vec![(
            "home_read_temperature".to_string(),
            true,
            true,
            adk_core::ToolEffect::ReadOnly,
            Some(annotations),
        )]
    );
}

/// Counts how often it is closed.
struct ClosingToolset {
    name: &'static str,
    closed: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl Toolset for ClosingToolset {
    fn name(&self) -> &str {
        self.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        Ok(Vec::new())
    }

    async fn close(&self) {
        self.closed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test]
async fn composed_toolsets_close_every_member_once() {
    let counters: Vec<Arc<std::sync::atomic::AtomicUsize>> =
        (0..3).map(|_| Arc::default()).collect();
    let member = |index: usize, name| -> Arc<dyn Toolset> {
        Arc::new(ClosingToolset { name, closed: Arc::clone(&counters[index]) })
    };
    let filtered = adk_tool::FilteredToolset::new(
        member(0, "files"),
        string_predicate(vec!["read_file".to_string()]),
    );
    let merged = adk_tool::MergedToolset::new(
        "workspace",
        vec![Arc::new(filtered) as Arc<dyn Toolset>, member(1, "search")],
    )
    .with_toolset(member(2, "shell"));
    let composed = adk_tool::PrefixedToolset::new(Arc::new(merged), "ws");

    composed.close().await;

    let closed: Vec<usize> =
        counters.iter().map(|counter| counter.load(std::sync::atomic::Ordering::SeqCst)).collect();
    assert_eq!(closed, vec![1, 1, 1]);
}
