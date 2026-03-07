use adk_core::{Content, EventActions, ReadonlyContext, Tool, ToolContext, types::AdkIdentity};
use adk_ui::{RenderModalTool, RenderProgressTool, RenderTableTool, RenderToastTool};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct TestContext {
    identity: AdkIdentity,
    content: Content,
    metadata: HashMap<String, String>,
    actions: Mutex<EventActions>,
}

impl TestContext {
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
impl ReadonlyContext for TestContext {
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
impl adk_core::CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestContext {
    fn function_call_id(&self) -> &str {
        "call-123"
    }

    fn actions(&self) -> EventActions {
        self.actions.lock().expect("actions").clone()
    }

    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().expect("actions") = actions;
    }

    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

async fn run_tool(tool: &dyn Tool, args: Value) -> Value {
    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    tool.execute(ctx, args).await.expect("tool execution")
}

#[tokio::test]
async fn migrated_legacy_tools_default_to_adk_ui_payload() {
    let table = run_tool(
        &RenderTableTool::new(),
        json!({
            "title": "Users",
            "columns": [{"header": "Name", "accessor_key": "name"}],
            "data": [{"name": "Alice"}]
        }),
    )
    .await;
    assert!(table.get("components").is_some());
    assert!(table.get("protocol").is_none());

    let progress = run_tool(
        &RenderProgressTool::new(),
        json!({
            "title": "Deploy",
            "value": 55
        }),
    )
    .await;
    assert!(progress.get("components").is_some());
    assert!(progress.get("protocol").is_none());

    let modal = run_tool(
        &RenderModalTool::new(),
        json!({
            "title": "Confirm",
            "message": "Proceed?"
        }),
    )
    .await;
    assert!(modal.get("components").is_some());
    assert!(modal.get("protocol").is_none());

    let toast = run_tool(
        &RenderToastTool::new(),
        json!({
            "message": "Saved"
        }),
    )
    .await;
    assert!(toast.get("components").is_some());
    assert!(toast.get("protocol").is_none());
}

#[tokio::test]
async fn migrated_legacy_tools_emit_mcp_apps_payload() {
    let table = run_tool(
        &RenderTableTool::new(),
        json!({
            "title": "Users",
            "columns": [{"header": "Name", "accessor_key": "name"}],
            "data": [{"name": "Alice"}],
            "protocol": "mcp_apps"
        }),
    )
    .await;
    assert_eq!(table["protocol"], "mcp_apps");
    assert!(table["payload"]["resource"]["uri"].is_string());

    let progress = run_tool(
        &RenderProgressTool::new(),
        json!({
            "title": "Deploy",
            "value": 55,
            "protocol": "mcp_apps"
        }),
    )
    .await;
    assert_eq!(progress["protocol"], "mcp_apps");
    assert!(progress["payload"]["resource"]["uri"].is_string());

    let modal = run_tool(
        &RenderModalTool::new(),
        json!({
            "title": "Confirm",
            "message": "Proceed?",
            "protocol": "mcp_apps"
        }),
    )
    .await;
    assert_eq!(modal["protocol"], "mcp_apps");
    assert!(modal["payload"]["resource"]["uri"].is_string());

    let toast = run_tool(
        &RenderToastTool::new(),
        json!({
            "message": "Saved",
            "protocol": "mcp_apps"
        }),
    )
    .await;
    assert_eq!(toast["protocol"], "mcp_apps");
    assert!(toast["payload"]["resource"]["uri"].is_string());
}
