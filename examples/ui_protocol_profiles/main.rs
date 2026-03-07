use adk_core::{Content, EventActions, ReadonlyContext, ToolContext, types::AdkIdentity};
use adk_ui::{UiToolset, column, text};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

struct DemoContext {
    identity: AdkIdentity,
    content: Content,
    metadata: HashMap<String, String>,
    actions: Mutex<EventActions>,
}

impl DemoContext {
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
impl ReadonlyContext for DemoContext {
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
impl adk_core::CallbackContext for DemoContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for DemoContext {
    fn function_call_id(&self) -> &str {
        "call-ui-protocol-profiles"
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

fn templates_by_tool() -> HashMap<&'static str, Value> {
    let mut templates = HashMap::new();
    templates.insert(
        "render_screen",
        json!({
            "surface_id": "main",
            "components": [
                text("title", "Protocol Coverage", Some("h1")),
                text("body", "This screen is emitted by adk-ui.", None),
                column("root", vec!["title", "body"])
            ],
            "data_model": { "status": "ok" }
        }),
    );
    templates.insert(
        "render_page",
        json!({
            "surface_id": "overview",
            "title": "Protocol Coverage",
            "description": "Page output across A2UI, AG-UI, and MCP Apps.",
            "sections": [{
                "heading": "What is covered",
                "body": "All adk-ui render tools in one run.",
                "bullets": ["a2ui", "ag_ui", "mcp_apps"]
            }]
        }),
    );
    templates.insert(
        "render_kit",
        json!({
            "name": "Coverage Kit",
            "version": "0.1.0",
            "brand": { "vibe": "clean", "industry": "platform" },
            "colors": { "primary": "#2563eb" },
            "typography": { "family": "Source Sans 3" }
        }),
    );
    templates.insert(
        "render_form",
        json!({
            "title": "Signup",
            "fields": [{ "name": "email", "label": "Email", "type": "email", "required": true }]
        }),
    );
    templates.insert(
        "render_card",
        json!({
            "title": "Coverage Card",
            "content": "Card payload generated successfully.",
            "actions": [{ "label": "Continue", "action_id": "continue" }]
        }),
    );
    templates.insert(
        "render_alert",
        json!({
            "title": "Coverage Alert",
            "description": "Protocol run in progress.",
            "variant": "info"
        }),
    );
    templates.insert(
        "render_confirm",
        json!({
            "title": "Confirm",
            "message": "Continue protocol checks?",
            "confirm_action": "confirm"
        }),
    );
    templates.insert(
        "render_table",
        json!({
            "title": "Coverage Table",
            "columns": [
                { "header": "Protocol", "accessor_key": "protocol" },
                { "header": "Status", "accessor_key": "status" }
            ],
            "data": [
                { "protocol": "a2ui", "status": "ok" },
                { "protocol": "ag_ui", "status": "ok" },
                { "protocol": "mcp_apps", "status": "ok" }
            ]
        }),
    );
    templates.insert(
        "render_chart",
        json!({
            "title": "Coverage Chart",
            "type": "bar",
            "data": [
                { "protocol": "a2ui", "score": 1 },
                { "protocol": "ag_ui", "score": 1 },
                { "protocol": "mcp_apps", "score": 1 }
            ],
            "x_key": "protocol",
            "y_keys": ["score"]
        }),
    );
    templates.insert(
        "render_layout",
        json!({
            "title": "Coverage Layout",
            "sections": [{
                "title": "Protocols",
                "type": "stats",
                "stats": [
                    { "label": "A2UI", "value": "OK", "status": "success" },
                    { "label": "AG-UI", "value": "OK", "status": "success" },
                    { "label": "MCP Apps", "value": "OK", "status": "success" }
                ]
            }]
        }),
    );
    templates.insert(
        "render_progress",
        json!({
            "title": "Coverage Progress",
            "value": 80,
            "steps": [
                { "label": "A2UI", "completed": true },
                { "label": "AG-UI", "completed": true },
                { "label": "MCP Apps", "current": true }
            ]
        }),
    );
    templates.insert(
        "render_modal",
        json!({
            "title": "Coverage Modal",
            "message": "Validating protocol outputs.",
            "confirm_label": "OK",
            "cancel_label": "Close"
        }),
    );
    templates.insert(
        "render_toast",
        json!({
            "message": "Coverage run complete",
            "variant": "success",
            "duration": 1200
        }),
    );
    templates
}

fn args_with_protocol(template: &Value, protocol: &str) -> Value {
    let mut args = template.clone();
    let object = args.as_object_mut().expect("tool args should be object");
    object.insert("protocol".to_string(), json!(protocol));
    if protocol == "mcp_apps" {
        object.insert(
            "mcp_apps".to_string(),
            json!({
                "resource_uri": format!("ui://examples/{}", object.get("surface_id").and_then(Value::as_str).unwrap_or("surface")),
                "domain": "https://example.com"
            }),
        );
    }
    args
}

fn summarize(value: &Value) -> String {
    if let Some(jsonl) = value.as_str() {
        return format!(
            "jsonl_string(lines={})",
            jsonl.lines().filter(|line| !line.is_empty()).count()
        );
    }
    if let Some(object) = value.as_object() {
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let resource_uri = value
            .pointer("/payload/resource/uri")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/payload/payload/resource/uri").and_then(Value::as_str));
        if let Some(uri) = resource_uri {
            return format!("keys={:?}, resource_uri={}", keys, uri);
        }
        return format!("keys={:?}", keys);
    }
    "unknown_output".to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let tools = UiToolset::all_tools();
    let templates = templates_by_tool();
    let protocols = ["a2ui", "ag_ui", "mcp_apps"];
    let mut failures = Vec::new();

    println!("Running tri-protocol coverage example for {} render tools.", tools.len());

    for protocol in protocols {
        println!("\n=== protocol: {} ===", protocol);
        for tool in &tools {
            let Some(template) = templates.get(tool.name()) else {
                failures.push(format!("missing template for {}", tool.name()));
                continue;
            };
            let args = args_with_protocol(template, protocol);
            let ctx: Arc<dyn ToolContext> = Arc::new(DemoContext::new());
            match tool.execute(ctx, args).await {
                Ok(value) => {
                    println!("{:<15} {}", tool.name(), summarize(&value));
                }
                Err(error) => {
                    let detail = format!("{} with {} failed: {}", tool.name(), protocol, error);
                    println!("{:<15} ERROR {}", tool.name(), error);
                    failures.push(detail);
                }
            }
        }
    }

    if failures.is_empty() {
        println!("\nCoverage run complete: all tool/protocol combinations succeeded.");
        Ok(())
    } else {
        eprintln!("\nCoverage run failed:");
        for failure in failures {
            eprintln!("- {}", failure);
        }
        Err("one or more tool/protocol combinations failed".into())
    }
}
