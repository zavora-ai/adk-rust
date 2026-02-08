use adk_core::{Content, EventActions, ReadonlyContext, Tool, ToolContext};
use adk_ui::tools::{RenderFormTool, RenderScreenTool};
use async_trait::async_trait;
use jsonschema::Validator;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct TestContext {
    content: Content,
    actions: Mutex<EventActions>,
}

impl TestContext {
    fn new() -> Self {
        Self { content: Content::new("user"), actions: Mutex::new(EventActions::default()) }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "test"
    }
    fn agent_name(&self) -> &str {
        "test"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
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
        self.actions.lock().expect("actions lock").clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().expect("actions lock") = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

fn ag_ui_events_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "required": ["type"],
            "properties": {
                "type": {
                    "enum": [
                        "RUN_STARTED",
                        "RUN_FINISHED",
                        "STEP_STARTED",
                        "STEP_FINISHED",
                        "TEXT_MESSAGE_START",
                        "TEXT_MESSAGE_DELTA",
                        "TEXT_MESSAGE_END",
                        "TOOL_CALL_START",
                        "TOOL_CALL_ARGS",
                        "TOOL_CALL_END",
                        "TOOL_CALL_RESULT",
                        "STATE_SNAPSHOT",
                        "STATE_DELTA",
                        "ERROR",
                        "CUSTOM"
                    ]
                }
            },
            "additionalProperties": true
        }
    })
}

fn mcp_apps_payload_schema() -> Value {
    json!({
        "type": "object",
        "required": ["resource", "resourceReadResponse", "toolMeta"],
        "properties": {
            "resource": {
                "type": "object",
                "required": ["uri", "name", "mimeType"],
                "properties": {
                    "uri": { "type": "string", "pattern": "^ui://" },
                    "name": { "type": "string" },
                    "mimeType": { "type": "string" },
                    "_meta": { "type": "object" }
                },
                "additionalProperties": true
            },
            "resourceReadResponse": {
                "type": "object",
                "required": ["contents"],
                "properties": {
                    "contents": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "required": ["uri", "mimeType"],
                            "properties": {
                                "uri": { "type": "string", "pattern": "^ui://" },
                                "mimeType": { "type": "string" },
                                "text": { "type": "string" },
                                "_meta": { "type": "object" }
                            },
                            "additionalProperties": true
                        }
                    }
                },
                "additionalProperties": true
            },
            "toolMeta": {
                "type": "object",
                "required": ["_meta"],
                "properties": {
                    "_meta": { "type": "object" }
                },
                "additionalProperties": true
            }
        },
        "additionalProperties": true
    })
}

fn collect_errors(validator: &Validator, instance: &Value) -> Vec<String> {
    validator.iter_errors(instance).map(|error| error.to_string()).collect()
}

#[tokio::test]
async fn render_screen_ag_ui_events_match_schema() {
    let validator = Validator::new(&ag_ui_events_schema()).expect("build ag_ui validator");
    let tool = RenderScreenTool::new();
    let args = serde_json::json!({
        "protocol": "ag_ui",
        "components": [
            { "id": "root", "component": "Column", "children": ["title"] },
            { "id": "title", "component": "Text", "text": "Schema Check", "variant": "h1" }
        ]
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.expect("render_screen ag_ui");
    let events = value.get("events").expect("ag_ui events");
    let errors = collect_errors(&validator, events);

    assert!(errors.is_empty(), "ag_ui schema validation errors: {:?}", errors);
}

#[tokio::test]
async fn render_form_ag_ui_envelope_events_match_schema() {
    let validator = Validator::new(&ag_ui_events_schema()).expect("build ag_ui validator");
    let tool = RenderFormTool::new();
    let args = serde_json::json!({
        "protocol": "ag_ui",
        "title": "Signup",
        "fields": [{ "name": "email", "label": "Email", "type": "email", "required": true }]
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.expect("render_form ag_ui");
    let events = value.get("events").expect("ag_ui envelope events");
    let errors = collect_errors(&validator, events);

    assert!(errors.is_empty(), "ag_ui envelope schema errors: {:?}", errors);
}

#[tokio::test]
async fn render_screen_mcp_apps_payload_matches_schema() {
    let validator = Validator::new(&mcp_apps_payload_schema()).expect("build mcp schema validator");
    let tool = RenderScreenTool::new();
    let args = serde_json::json!({
        "protocol": "mcp_apps",
        "components": [
            { "id": "root", "component": "Column", "children": ["title"] },
            { "id": "title", "component": "Text", "text": "Schema Check", "variant": "h1" }
        ],
        "mcp_apps": {
            "resource_uri": "ui://tests/schemas",
            "domain": "https://example.com",
            "csp": {
                "connectDomains": ["https://example.com"],
                "resourceDomains": ["https://example.com"]
            }
        }
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.expect("render_screen mcp_apps");
    let payload = value.get("payload").expect("mcp payload");
    let errors = collect_errors(&validator, payload);

    assert!(errors.is_empty(), "mcp payload schema errors: {:?}", errors);
}

#[tokio::test]
async fn render_form_mcp_apps_envelope_payload_matches_schema() {
    let validator = Validator::new(&mcp_apps_payload_schema()).expect("build mcp schema validator");
    let tool = RenderFormTool::new();
    let args = serde_json::json!({
        "protocol": "mcp_apps",
        "title": "Signup",
        "fields": [{ "name": "email", "label": "Email", "type": "email", "required": true }]
    });

    let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
    let value = tool.execute(ctx, args).await.expect("render_form mcp_apps");
    let payload = value.get("payload").expect("mcp envelope payload");
    let errors = collect_errors(&validator, payload);

    assert!(errors.is_empty(), "mcp envelope schema errors: {:?}", errors);
}

#[test]
fn ag_ui_invalid_vector_reports_diagnostic() {
    let validator = Validator::new(&ag_ui_events_schema()).expect("build ag_ui validator");
    let invalid = json!([
        {
            "threadId": "thread-only"
        }
    ]);
    let errors = collect_errors(&validator, &invalid);
    assert!(!errors.is_empty(), "expected ag_ui schema failure");
    assert!(
        errors.iter().any(|error| error.contains("\"type\"") || error.contains("required")),
        "expected missing type diagnostic, got {:?}",
        errors
    );
}

#[test]
fn mcp_apps_invalid_vector_reports_diagnostic() {
    let validator = Validator::new(&mcp_apps_payload_schema()).expect("build mcp schema validator");
    let invalid = json!({
        "resource": {
            "name": "Broken payload",
            "mimeType": "text/html;profile=mcp-app"
        },
        "resourceReadResponse": { "contents": [] },
        "toolMeta": {}
    });
    let errors = collect_errors(&validator, &invalid);
    assert!(!errors.is_empty(), "expected mcp schema failure");
    assert!(
        errors.iter().any(|error| error.contains("uri")
            || error.contains("minItems")
            || error.contains("_meta")),
        "expected mcp diagnostic mentioning required fields, got {:?}",
        errors
    );
}
