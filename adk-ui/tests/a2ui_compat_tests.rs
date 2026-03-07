use adk_core::{Content, EventActions, ReadonlyContext, Tool, ToolContext, types::AdkIdentity};
use adk_ui::tools::{RenderPageTool, RenderScreenTool};
use async_trait::async_trait;
use jsonschema::{Resource, Validator, options};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn build_reference_validator() -> Validator {
    let server_schema: Value =
        serde_json::from_str(include_str!("../catalog/a2ui/v0_9/server_to_client.json"))
            .expect("server_to_client.json should parse");
    let standard_catalog: Value =
        serde_json::from_str(include_str!("../catalog/a2ui/v0_9/standard_catalog.json"))
            .expect("standard_catalog.json should parse");
    let common_types: Value =
        serde_json::from_str(include_str!("../catalog/a2ui/v0_9/common_types.json"))
            .expect("common_types.json should parse");

    options()
        .with_resource(
            "standard_catalog.json",
            Resource::from_contents(standard_catalog).expect("catalog resource"),
        )
        .with_resource(
            "common_types.json",
            Resource::from_contents(common_types).expect("types resource"),
        )
        .build(&server_schema)
        .expect("validator should build")
}

fn validate_jsonl(validator: &Validator, jsonl: &str) {
    for line in jsonl.trim_end().lines() {
        let value: Value = serde_json::from_str(line).expect("json line should parse");
        let is_valid = validator.is_valid(&value);
        assert!(is_valid, "A2UI schema validation failed for: {}", line);
    }
}

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
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn render_screen_matches_reference_schema() {
    let validator = build_reference_validator();
    let tool = RenderScreenTool::new();
    let args = serde_json::json!({
        "components": [
            { "id": "root", "component": "Column", "children": ["title", "cta"] },
            { "id": "title", "component": "Text", "text": "Welcome", "variant": "h1" },
            { "id": "cta_label", "component": "Text", "text": "Continue", "variant": "body" },
            {
                "id": "cta",
                "component": "Button",
                "child": "cta_label",
                "variant": "primary",
                "action": { "event": { "name": "continue" } }
            }
        ]
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.unwrap();
    let jsonl = value
        .as_str()
        .or_else(|| value.get("jsonl").and_then(Value::as_str))
        .expect("jsonl string");
    validate_jsonl(&validator, jsonl);
}

#[tokio::test]
async fn render_page_matches_reference_schema() {
    let validator = build_reference_validator();
    let tool = RenderPageTool::new();
    let args = serde_json::json!({
        "title": "Release Notes",
        "description": "Highlights for the latest launch.",
        "sections": [
            {
                "heading": "What’s new",
                "body": "Three big improvements shipped this week.",
                "bullets": ["Faster onboarding", "Better search", "New dashboards"],
                "actions": [
                    { "label": "View details", "action": "view_details", "variant": "borderless" }
                ]
            }
        ]
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.unwrap();
    let jsonl = value.as_str().expect("jsonl string");
    validate_jsonl(&validator, jsonl);
}
