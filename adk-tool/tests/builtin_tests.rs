use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Role, Tool,
    ToolContext,
};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Mock context for testing
struct MockContext {
    actions: Mutex<EventActions>,
    identity: adk_core::types::AdkIdentity,
}

impl MockContext {
    fn new() -> Self {
        Self {
            actions: Mutex::new(EventActions::default()),
            identity: adk_core::types::AdkIdentity::default(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new(Role::User))
    }

    fn metadata(&self) -> &HashMap<String, String> {
        static METADATA: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        METADATA.get_or_init(HashMap::new)
    }
}

#[async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for MockContext {
    fn function_call_id(&self) -> &str {
        "test-call"
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

#[derive(JsonSchema, Deserialize, Serialize)]
struct TestArgs {
    message: String,
}

#[tokio::test]
async fn test_builtin_tool_execution() {
    let tool = FunctionTool::new(
        "test_tool",
        "A test tool",
        |ctx: Arc<dyn ToolContext>, args: Value| async move {
            let args: TestArgs = serde_json::from_value(args).unwrap();
            let mut actions = ctx.actions();
            actions.state_delta.insert("result".to_string(), json!(args.message));
            ctx.set_actions(actions);
            Ok(json!({ "status": "success" }))
        },
    );

    assert_eq!(tool.name(), "test_tool");
    assert_eq!(tool.description(), "A test tool");

    let ctx = Arc::new(MockContext::new());
    let args = json!({ "message": "hello" });

    let result = tool.execute(ctx.clone(), args).await.unwrap();
    assert_eq!(result, json!({ "status": "success" }));

    let actions = ctx.actions();
    assert_eq!(actions.state_delta.get("result").unwrap(), &json!("hello"));
}
