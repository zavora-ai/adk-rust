use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool,
    ToolContext, types::AdkIdentity,
};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct MockToolContext {
    identity: AdkIdentity,
    content: Content,
    metadata: HashMap<String, String>,
    actions: Mutex<EventActions>,
}

impl MockToolContext {
    fn new() -> Self {
        Self {
            identity: AdkIdentity::default(),
            content: Content::new("user"),
            metadata: HashMap::new(),
            actions: Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockToolContext {
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
    let tool =
        FunctionTool::new(
            "process",
            "Long process",
            |_ctx, _args| async move { Ok(json!("done")) },
        )
        .with_long_running(true);

    assert!(tool.is_long_running());
}

#[tokio::test]
async fn test_function_tool_long_running_enhanced_description() {
    // Test with description
    let tool =
        FunctionTool::new("process_video", "Process a video file", |_ctx, _args| async move {
            Ok(json!({"status": "pending", "task_id": "task-123"}))
        })
        .with_long_running(true);

    let enhanced = tool.enhanced_description();
    assert!(enhanced.contains("Process a video file"));
    assert!(enhanced.contains("NOTE: This is a long-running operation"));
    assert!(enhanced.contains("Do not call this tool again if it has already returned"));
}

#[tokio::test]
async fn test_function_tool_long_running_enhanced_description_empty() {
    // Test with empty description
    let tool =
        FunctionTool::new(
            "process",
            "",
            |_ctx, _args| async move { Ok(json!({"status": "pending"})) },
        )
        .with_long_running(true);

    let enhanced = tool.enhanced_description();
    assert!(enhanced.contains("NOTE: This is a long-running operation"));
    // Should not have double newlines from empty description
    assert!(!enhanced.starts_with("\n\n"));
}

#[tokio::test]
async fn test_function_tool_non_long_running_enhanced_description() {
    // Regular tools should return description as-is
    let tool = FunctionTool::new("quick_task", "Does something quick", |_ctx, _args| async move {
        Ok(json!("done"))
    });

    assert!(!tool.is_long_running());
    let enhanced = tool.enhanced_description();
    assert_eq!(enhanced, "Does something quick");
    assert!(!enhanced.contains("NOTE: This is a long-running operation"));
}

#[tokio::test]
async fn test_function_tool_long_running_returns_pending_status() {
    // Simulate typical long-running tool behavior - return task ID and status
    let tool =
        FunctionTool::new("analyze_data", "Analyze large dataset", |_ctx, _args| async move {
            Ok(json!({
                "status": "processing",
                "task_id": "task-abc123",
                "progress": 0,
                "estimated_time": "5 minutes"
            }))
        })
        .with_long_running(true);

    let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
    let result = tool.execute(ctx, json!({"dataset_path": "/data/large.csv"})).await.unwrap();

    assert_eq!(result["status"], "processing");
    assert_eq!(result["task_id"], "task-abc123");
    assert_eq!(result["progress"], 0);
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

// =============================================================================
// FunctionTool::with_scopes() tests
// =============================================================================

#[test]
fn test_function_tool_with_scopes() {
    let tool = FunctionTool::new("transfer", "Transfer funds", |_ctx, _args| async move {
        Ok(json!({"status": "ok"}))
    })
    .with_scopes(&["finance:write", "verified"]);

    assert_eq!(tool.required_scopes(), &["finance:write", "verified"]);
}

#[test]
fn test_function_tool_no_scopes_by_default() {
    let tool =
        FunctionTool::new(
            "search",
            "Search",
            |_ctx, _args| async move { Ok(json!({"results": []})) },
        );

    assert!(tool.required_scopes().is_empty());
}

#[test]
fn test_function_tool_scopes_chainable() {
    let tool = FunctionTool::new("admin", "Admin tool", |_ctx, _args| async move { Ok(json!({})) })
        .with_scopes(&["admin"])
        .with_long_running(true);

    assert_eq!(tool.required_scopes(), &["admin"]);
    assert!(tool.is_long_running());
}
