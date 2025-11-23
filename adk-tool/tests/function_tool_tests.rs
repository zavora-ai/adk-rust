use adk_core::{CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AddParams {
    a: i32,
    b: i32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AddResult {
    sum: i32,
}

#[tokio::test]
async fn test_function_tool_basic() {
    let tool = FunctionTool::new("add", "Adds two numbers", |_ctx, args| async move {
        let a = args["a"].as_i64().unwrap();
        let b = args["b"].as_i64().unwrap();
        Ok(json!(a + b))
    });

    assert_eq!(tool.name(), "add");
    assert_eq!(tool.description(), "Adds two numbers");
    assert!(!tool.is_long_running());

    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({"a": 5, "b": 3})).await.unwrap();
    assert_eq!(result, json!(8));
}

#[tokio::test]
async fn test_function_tool_with_schema() {
    let tool = FunctionTool::new("add", "Adds two numbers", |_ctx, args| async move {
        let a = args["a"].as_i64().unwrap();
        let b = args["b"].as_i64().unwrap();
        Ok(json!({"sum": a + b}))
    })
    .with_parameters_schema::<AddParams>()
    .with_response_schema::<AddResult>();

    assert!(tool.parameters_schema().is_some());
    assert!(tool.response_schema().is_some());

    let params_schema = tool.parameters_schema().unwrap();
    assert!(params_schema["properties"]["a"].is_object());
    assert!(params_schema["properties"]["b"].is_object());

    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({"a": 5, "b": 3})).await.unwrap();
    assert_eq!(result["sum"], json!(8));
}

#[tokio::test]
async fn test_function_tool_string() {
    let tool = FunctionTool::new("greet", "Greets a person", |_ctx, args| async move {
        let name = args["name"].as_str().unwrap();
        Ok(json!(format!("Hello, {}!", name)))
    });

    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({"name": "Alice"})).await.unwrap();
    assert_eq!(result, json!("Hello, Alice!"));
}

#[tokio::test]
async fn test_function_tool_long_running() {
    let tool = FunctionTool::new("process", "Long process", |_ctx, _args| async move {
        Ok(json!("done"))
    })
    .with_long_running(true);

    assert!(tool.is_long_running());
}

#[tokio::test]
async fn test_function_tool_error() {
    let tool = FunctionTool::new("fail", "Always fails", |_ctx, _args| async move {
        Err(adk_core::AdkError::Tool("intentional error".to_string()))
    });

    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({})).await;
    assert!(result.is_err());
}

