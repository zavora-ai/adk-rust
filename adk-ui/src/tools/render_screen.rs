use crate::a2ui::{
    A2uiMessage, A2uiSchemaVersion, A2uiValidator, CreateSurface, CreateSurfaceMessage,
    UpdateComponents, UpdateComponentsMessage, UpdateDataModel, UpdateDataModelMessage,
    encode_jsonl,
};
use crate::catalog_registry::CatalogRegistry;
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

        let mut messages: Vec<A2uiMessage> = Vec::new();

        messages.push(A2uiMessage::CreateSurface(CreateSurfaceMessage {
            create_surface: CreateSurface {
                surface_id: params.surface_id.clone(),
                catalog_id,
                theme: params.theme.clone(),
                send_data_model: Some(params.send_data_model),
            },
        }));

        if let Some(data_model) = params.data_model.clone() {
            messages.push(A2uiMessage::UpdateDataModel(UpdateDataModelMessage {
                update_data_model: UpdateDataModel {
                    surface_id: params.surface_id.clone(),
                    path: Some("/".to_string()),
                    value: Some(data_model),
                },
            }));
        }

        messages.push(A2uiMessage::UpdateComponents(UpdateComponentsMessage {
            update_components: UpdateComponents {
                surface_id: params.surface_id.clone(),
                components: params.components.clone(),
            },
        }));

        if params.validate {
            let validator = A2uiValidator::new().map_err(|e| {
                adk_core::AdkError::Tool(format!("Failed to initialize A2UI validator: {}", e))
            })?;
            for message in &messages {
                if let Err(errors) = validator.validate_message(message, A2uiSchemaVersion::V0_9) {
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

        let jsonl = encode_jsonl(messages)
            .map_err(|e| adk_core::AdkError::Tool(format!("Failed to encode A2UI JSONL: {}", e)))?;

        // Return as JSON object with components for LLM compatibility
        // The frontend will receive this and can render it
        Ok(serde_json::json!({
            "surface_id": params.surface_id,
            "components": params.components,
            "data_model": params.data_model,
            "jsonl": jsonl
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, ReadonlyContext};
    use async_trait::async_trait;
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
        assert!(root["component"]["Column"].is_object());
    }
}
