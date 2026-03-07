use adk_core::{Content, EventActions, ReadonlyContext, ToolContext, types::AdkIdentity};
use adk_ui::{UiToolset, column, text};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
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
        "call-matrix"
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MatrixSnapshot {
    tool: String,
    protocol: String,
    value_kind: String,
    top_level_keys: Vec<String>,
    jsonl_line_count: Option<usize>,
    event_types: Vec<String>,
    resource_uri: Option<String>,
    has_version: bool,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tool_protocol_matrix.snap.json")
}

fn templates_by_tool() -> BTreeMap<&'static str, Value> {
    let mut templates = BTreeMap::new();
    templates.insert(
        "render_screen",
        json!({
            "surface_id": "matrix-screen",
            "components": [
                text("title", "Matrix Screen", Some("h1")),
                text("body", "Coverage run", None),
                column("root", vec!["title", "body"])
            ],
            "data_model": { "status": "ok" }
        }),
    );
    templates.insert(
        "render_page",
        json!({
            "surface_id": "matrix-page",
            "title": "Matrix Page",
            "description": "Protocol coverage",
            "sections": [
                {
                    "heading": "Summary",
                    "body": "All adapters should emit valid outputs.",
                    "bullets": ["a2ui", "ag_ui", "mcp_apps"],
                    "actions": [{ "label": "Continue", "action": "continue" }]
                }
            ]
        }),
    );
    templates.insert(
        "render_kit",
        json!({
            "name": "Matrix Kit",
            "version": "0.1.0",
            "brand": { "vibe": "clean", "industry": "platform" },
            "colors": { "primary": "#2F6BFF" },
            "typography": { "family": "Source Sans 3" }
        }),
    );
    templates.insert(
        "render_form",
        json!({
            "title": "Matrix Form",
            "fields": [
                { "name": "email", "label": "Email", "type": "email", "required": true },
                { "name": "notes", "label": "Notes", "type": "textarea" }
            ],
            "submit_action": "matrix_submit"
        }),
    );
    templates.insert(
        "render_card",
        json!({
            "title": "Matrix Card",
            "content": "Card content for protocol matrix.",
            "actions": [{ "label": "Open", "action_id": "open_card" }]
        }),
    );
    templates.insert(
        "render_alert",
        json!({
            "title": "Matrix Alert",
            "description": "Protocol check",
            "variant": "warning"
        }),
    );
    templates.insert(
        "render_confirm",
        json!({
            "title": "Confirm Matrix",
            "message": "Proceed with protocol checks?",
            "confirm_action": "confirm_matrix",
            "destructive": false
        }),
    );
    templates.insert(
        "render_table",
        json!({
            "title": "Matrix Table",
            "columns": [
                { "header": "Name", "accessor_key": "name" },
                { "header": "Status", "accessor_key": "status" }
            ],
            "data": [
                { "name": "a2ui", "status": "ok" },
                { "name": "ag_ui", "status": "ok" },
                { "name": "mcp_apps", "status": "ok" }
            ]
        }),
    );
    templates.insert(
        "render_chart",
        json!({
            "title": "Matrix Chart",
            "type": "line",
            "data": [
                { "phase": "a2ui", "score": 1 },
                { "phase": "ag_ui", "score": 1 },
                { "phase": "mcp_apps", "score": 1 }
            ],
            "x_key": "phase",
            "y_keys": ["score"]
        }),
    );
    templates.insert(
        "render_layout",
        json!({
            "title": "Matrix Layout",
            "sections": [
                {
                    "title": "Protocol Status",
                    "type": "stats",
                    "stats": [
                        { "label": "A2UI", "value": "ok", "status": "success" },
                        { "label": "AG-UI", "value": "ok", "status": "success" }
                    ]
                }
            ]
        }),
    );
    templates.insert(
        "render_progress",
        json!({
            "title": "Matrix Progress",
            "value": 66,
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
            "title": "Matrix Modal",
            "message": "Protocol rendering in progress.",
            "confirm_label": "OK",
            "cancel_label": "Close"
        }),
    );
    templates.insert(
        "render_toast",
        json!({
            "message": "Matrix toast",
            "variant": "success",
            "duration": 1200
        }),
    );
    templates
}

fn args_with_protocol(template: &Value, protocol: &str) -> Value {
    let mut args = template.clone();
    let object = args.as_object_mut().expect("tool args must be object");
    object.insert("protocol".to_string(), Value::String(protocol.to_string()));
    args
}

fn extract_event_types(value: &Value) -> Vec<String> {
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .or_else(|| value.pointer("/payload/events").and_then(Value::as_array));
    let Some(events) = events else {
        return vec![];
    };

    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str).map(ToString::to_string))
        .collect()
}

fn extract_resource_uri(value: &Value) -> Option<String> {
    value.pointer("/payload/resource/uri").and_then(Value::as_str).map(ToString::to_string).or_else(
        || {
            value
                .pointer("/payload/payload/resource/uri")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        },
    )
}

fn extract_jsonl_line_count(value: &Value) -> Option<usize> {
    if let Some(s) = value.as_str() {
        return Some(s.lines().filter(|line| !line.trim().is_empty()).count());
    }
    if let Some(s) = value.get("jsonl").and_then(Value::as_str) {
        return Some(s.lines().filter(|line| !line.trim().is_empty()).count());
    }
    value
        .pointer("/payload/jsonl")
        .and_then(Value::as_str)
        .map(|s| s.lines().filter(|line| !line.trim().is_empty()).count())
}

fn validate_contract(tool: &str, protocol: &str, value: &Value) {
    match (tool, protocol) {
        ("render_page", "a2ui") => {
            let jsonl = value.as_str().expect("render_page a2ui should return JSONL string");
            assert!(jsonl.contains("\"createSurface\""));
            assert!(jsonl.contains("\"updateComponents\""));
        }
        ("render_kit", _) => {
            assert_eq!(value["protocol"], protocol);
            assert!(value.get("payload").is_some(), "render_kit should include payload");
        }
        (_, "a2ui") => {
            assert_eq!(value["protocol"], "a2ui");
            assert!(
                value.get("jsonl").and_then(Value::as_str).is_some()
                    || value.pointer("/payload/jsonl").and_then(Value::as_str).is_some(),
                "{tool} a2ui output should carry jsonl",
            );
        }
        (_, "ag_ui") => {
            assert_eq!(value["protocol"], "ag_ui");
            let has_events = value.get("events").and_then(Value::as_array).is_some()
                || value.pointer("/payload/events").and_then(Value::as_array).is_some();
            assert!(has_events, "{tool} ag_ui output should include events");
        }
        (_, "mcp_apps") => {
            assert_eq!(value["protocol"], "mcp_apps");
            let has_uri = extract_resource_uri(value).is_some();
            assert!(has_uri, "{tool} mcp_apps output should include resource uri");
        }
        _ => {}
    }
}

fn snapshot_record(tool: &str, protocol: &str, value: &Value) -> MatrixSnapshot {
    let mut top_level_keys = value
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    top_level_keys.sort();

    MatrixSnapshot {
        tool: tool.to_string(),
        protocol: protocol.to_string(),
        value_kind: if value.is_string() { "string".to_string() } else { "object".to_string() },
        top_level_keys,
        jsonl_line_count: extract_jsonl_line_count(value),
        event_types: extract_event_types(value),
        resource_uri: extract_resource_uri(value),
        has_version: value.get("version").is_some(),
    }
}

async fn collect_matrix_snapshots() -> Vec<MatrixSnapshot> {
    let templates = templates_by_tool();
    let tools = UiToolset::all_tools();
    let protocols = ["a2ui", "ag_ui", "mcp_apps"];
    let mut snapshots = Vec::new();

    assert_eq!(tools.len(), 13, "toolset should expose 13 tools");

    for protocol in protocols {
        for tool in &tools {
            let template = templates
                .get(tool.name())
                .unwrap_or_else(|| panic!("missing args for {}", tool.name()));
            let args = args_with_protocol(template, protocol);
            let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
            let value = tool
                .execute(ctx, args)
                .await
                .unwrap_or_else(|e| panic!("{} {} failed: {}", tool.name(), protocol, e));
            validate_contract(tool.name(), protocol, &value);
            snapshots.push(snapshot_record(tool.name(), protocol, &value));
        }
    }

    snapshots
}

#[tokio::test]
async fn tool_protocol_matrix_covers_all_tools_and_protocols() {
    let snapshots = collect_matrix_snapshots().await;
    assert_eq!(snapshots.len(), 39, "expected 13 tools x 3 protocols");
}

#[tokio::test]
async fn tool_protocol_matrix_is_deterministic() {
    let first = collect_matrix_snapshots().await;
    let second = collect_matrix_snapshots().await;
    assert_eq!(first, second, "matrix outputs should be deterministic across runs");
}

#[tokio::test]
async fn tool_protocol_matrix_snapshot_matches_fixture() {
    let snapshots = collect_matrix_snapshots().await;
    let path = fixture_path();

    if std::env::var("UPDATE_SNAPSHOTS").ok().as_deref() == Some("1") {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture directory");
        }
        let fixture = serde_json::to_string_pretty(&snapshots).expect("serialize snapshots");
        fs::write(&path, fixture).expect("write fixture file");
    }

    let expected_raw = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing fixture at {}. Run with UPDATE_SNAPSHOTS=1", path.display())
    });
    let expected: Vec<MatrixSnapshot> =
        serde_json::from_str(&expected_raw).expect("parse fixture JSON");

    assert_eq!(snapshots, expected, "matrix snapshots changed");
}
