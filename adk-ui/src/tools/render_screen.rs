use crate::a2ui::{A2uiSchemaVersion, A2uiValidator};
use crate::catalog_registry::CatalogRegistry;
use crate::interop::{
    A2uiAdapter, AgUiAdapter, McpAppsAdapter, UiProtocol, UiProtocolAdapter, UiSurface,
};
use crate::tools::SurfaceProtocolOptions;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

fn default_surface_id() -> String {
    "main".to_string()
}

fn default_send_data_model() -> bool {
    true
}

fn default_validate() -> bool {
    true
}

/// Parameters for the render_screen tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderScreenParams {
    /// Surface id (default: "main")
    #[serde(default = "default_surface_id")]
    pub surface_id: String,
    /// Catalog id (defaults to the embedded ADK catalog)
    #[serde(default)]
    pub catalog_id: Option<String>,
    /// A2UI component definitions (must include a component with id "root")
    pub components: Vec<Value>,
    /// Optional initial data model (sent via updateDataModel at path "/")
    #[serde(default)]
    pub data_model: Option<Value>,
    /// Optional theme object for createSurface
    #[serde(default)]
    pub theme: Option<Value>,
    /// If true, the client should include the data model in action metadata (default: true)
    #[serde(default = "default_send_data_model")]
    pub send_data_model: bool,
    /// Validate generated messages against the A2UI v0.9 schema (default: true)
    #[serde(default = "default_validate")]
    pub validate: bool,
    /// Shared protocol output options.
    #[serde(flatten)]
    pub protocol_options: SurfaceProtocolOptions,
}

/// Tool for emitting A2UI JSONL for a single screen (surface).
///
/// This tool wraps a list of A2UI components with the standard envelope messages:
/// - createSurface
/// - updateDataModel (optional)
/// - updateComponents
pub struct RenderScreenTool;

impl RenderScreenTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RenderScreenTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for RenderScreenTool {
    fn name(&self) -> &str {
        "render_screen"
    }

    fn description(&self) -> &str {
        r#"Emit A2UI JSONL for a single screen (surface). Input must include A2UI component objects with ids, including a root component with id "root".
Returns a JSONL string with createSurface/updateDataModel/updateComponents messages."#
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(super::generate_gemini_schema::<RenderScreenParams>())
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: RenderScreenParams = serde_json::from_value(args.clone()).map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid parameters: {}. Got: {}", e, args))
        })?;

        if params.components.is_empty() {
            return Err(adk_core::AdkError::Tool(
                "Invalid parameters: components must not be empty.".to_string(),
            ));
        }

        let has_root = params.components.iter().any(|component| {
            component.get("id").and_then(Value::as_str).map(|id| id == "root").unwrap_or(false)
        });

        if !has_root {
            return Err(adk_core::AdkError::Tool(
                "Invalid parameters: components must include a root component with id \"root\"."
                    .to_string(),
            ));
        }

        let registry = CatalogRegistry::new();
        let catalog_id =
            params.catalog_id.unwrap_or_else(|| registry.default_catalog_id().to_string());

        let surface =
            UiSurface::new(params.surface_id.clone(), catalog_id, params.components.clone())
                .with_data_model(params.data_model.clone())
                .with_theme(params.theme.clone())
                .with_send_data_model(params.send_data_model);

        match params.protocol_options.protocol {
            UiProtocol::A2ui => {
                let messages = surface.to_a2ui_messages();
                if params.validate {
                    let validator = A2uiValidator::new().map_err(|e| {
                        adk_core::AdkError::Tool(format!(
                            "Failed to initialize A2UI validator: {}",
                            e
                        ))
                    })?;
                    for message in &messages {
                        if let Err(errors) =
                            validator.validate_message(message, A2uiSchemaVersion::V0_9)
                        {
                            let details = errors
                                .iter()
                                .map(|err| format!("{} at {}", err.message, err.instance_path))
                                .collect::<Vec<_>>()
                                .join("; ");
                            return Err(adk_core::AdkError::Tool(format!(
                                "A2UI validation failed: {}",
                                details
                            )));
                        }
                    }
                }

                let adapter = A2uiAdapter;
                let payload = adapter.to_protocol_payload(&surface)?;
                adapter.validate(&payload)?;
                Ok(payload)
            }
            UiProtocol::AgUi => {
                let thread_id =
                    params.protocol_options.resolved_ag_ui_thread_id(&params.surface_id);
                let run_id = params.protocol_options.resolved_ag_ui_run_id(&params.surface_id);
                let adapter = AgUiAdapter::new(thread_id, run_id);
                adapter.to_protocol_payload(&surface)
            }
            UiProtocol::McpApps => {
                let options = params.protocol_options.parse_mcp_options()?;
                let adapter = McpAppsAdapter::new(options);
                adapter.to_protocol_payload(&surface)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, ReadonlyContext, types::AdkIdentity};
    use async_trait::async_trait;
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
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn render_screen_emits_jsonl() {
        use crate::a2ui::{column, text};

        let tool = RenderScreenTool::new();
        let args = serde_json::json!({
            "components": [
                text("title", "Hello World", Some("h1")),
                text("desc", "Welcome", None),
                column("root", vec!["title", "desc"])
            ],
            "data_model": { "title": "Hello" }
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();

        // The tool now returns a JSON object with components, data_model, and jsonl
        assert!(value.is_object());
        assert!(value.get("surface_id").is_some());
        assert!(value.get("components").is_some());
        assert!(value.get("jsonl").is_some());

        // Verify JSONL is still generated
        let jsonl = value["jsonl"].as_str().unwrap();
        let lines: Vec<Value> =
            jsonl.trim_end().lines().map(|line| serde_json::from_str(line).unwrap()).collect();

        assert_eq!(lines.len(), 3);
        assert!(lines[0].get("createSurface").is_some());
        assert!(lines[1].get("updateDataModel").is_some());
        assert!(lines[2].get("updateComponents").is_some());

        // Verify component structure in the returned JSON
        let components = value["components"].as_array().unwrap();
        assert_eq!(components.len(), 3);
        let root = &components[2];
        assert_eq!(root["id"], "root");
        assert_eq!(root["component"], "Column");
    }

    #[tokio::test]
    async fn render_screen_emits_ag_ui_events() {
        use crate::a2ui::{column, text};

        let tool = RenderScreenTool::new();
        let args = serde_json::json!({
            "protocol": "ag_ui",
            "components": [
                text("title", "Hello World", Some("h1")),
                column("root", vec!["title"])
            ]
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();

        assert_eq!(value["protocol"], "ag_ui");
        let events = value["events"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["type"], "RUN_STARTED");
        assert_eq!(events[1]["type"], "CUSTOM");
        assert_eq!(events[2]["type"], "RUN_FINISHED");
    }

    #[tokio::test]
    async fn render_screen_emits_mcp_apps_payload() {
        use crate::a2ui::{column, text};

        let tool = RenderScreenTool::new();
        let args = serde_json::json!({
            "protocol": "mcp_apps",
            "components": [
                text("title", "Hello World", Some("h1")),
                column("root", vec!["title"])
            ],
            "mcp_apps": {
                "resource_uri": "ui://tests/screen"
            }
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();

        assert_eq!(value["protocol"], "mcp_apps");
        assert_eq!(value["payload"]["resource"]["uri"], "ui://tests/screen");
        assert_eq!(value["payload"]["toolMeta"]["_meta"]["ui"]["resourceUri"], "ui://tests/screen");
    }
}
