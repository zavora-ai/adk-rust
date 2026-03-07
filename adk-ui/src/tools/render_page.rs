use crate::a2ui::{
    A2uiSchemaVersion, A2uiValidator, column, divider, encode_jsonl, image, row, stable_child_id,
    stable_id, stable_indexed_id, text,
};
use crate::catalog_registry::CatalogRegistry;
use crate::interop::{AgUiAdapter, McpAppsAdapter, UiProtocol, UiProtocolAdapter, UiSurface};
use crate::tools::SurfaceProtocolOptions;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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

/// Page action button definition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PageAction {
    /// Button label
    pub label: String,
    /// Action name emitted as A2UI action.event.name
    pub action: String,
    /// Button variant: "primary" or "borderless"
    #[serde(default)]
    pub variant: Option<String>,
    /// Optional action context (supports data bindings)
    #[serde(default)]
    pub context: Option<Value>,
}

/// A section in a page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PageSection {
    /// Section heading text
    pub heading: String,
    /// Optional body text
    #[serde(default)]
    pub body: Option<String>,
    /// Optional bullet list items
    #[serde(default)]
    pub bullets: Vec<String>,
    /// Optional image URL
    #[serde(default)]
    pub image_url: Option<String>,
    /// Optional action buttons
    #[serde(default)]
    pub actions: Vec<PageAction>,
}

/// Parameters for the render_page tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenderPageParams {
    /// Surface id (default: "main")
    #[serde(default = "default_surface_id")]
    pub surface_id: String,
    /// Catalog id (defaults to the embedded ADK catalog)
    #[serde(default)]
    pub catalog_id: Option<String>,
    /// Page title (rendered as h1)
    pub title: String,
    /// Optional description below the title
    #[serde(default)]
    pub description: Option<String>,
    /// Sections to include
    #[serde(default)]
    pub sections: Vec<PageSection>,
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

/// Tool for emitting A2UI JSONL for a multi-section page.
pub struct RenderPageTool;

impl RenderPageTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RenderPageTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for RenderPageTool {
    fn name(&self) -> &str {
        "render_page"
    }

    fn description(&self) -> &str {
        r#"Render a multi-section page as A2UI JSONL. Builds a root column with a title, optional description, and section blocks. Each section can include body text, bullets, images, and action buttons."#
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(super::generate_gemini_schema::<RenderPageParams>())
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let params: RenderPageParams = serde_json::from_value(args.clone()).map_err(|e| {
            adk_core::AdkError::Tool(format!("Invalid parameters: {}. Got: {}", e, args))
        })?;

        let registry = CatalogRegistry::new();
        let catalog_id =
            params.catalog_id.unwrap_or_else(|| registry.default_catalog_id().to_string());

        let page_id = stable_id(&format!("page:{}:{}", params.surface_id, params.title));
        let mut components: Vec<Value> = Vec::new();
        let mut root_children: Vec<String> = Vec::new();

        let title_id = stable_child_id(&page_id, "title");
        components.push(text(&title_id, &params.title, Some("h1")));
        root_children.push(title_id);

        if let Some(description) = params.description {
            let desc_id = stable_child_id(&page_id, "description");
            components.push(text(&desc_id, &description, None));
            root_children.push(desc_id);
        }

        for (index, section) in params.sections.iter().enumerate() {
            let section_id = stable_indexed_id(&page_id, "section", index);
            let mut section_children: Vec<String> = Vec::new();

            let heading_id = stable_child_id(&section_id, "heading");
            components.push(text(&heading_id, &section.heading, Some("h2")));
            section_children.push(heading_id);

            if let Some(body) = &section.body {
                let body_id = stable_child_id(&section_id, "body");
                components.push(text(&body_id, body, None));
                section_children.push(body_id);
            }

            if let Some(image_url) = &section.image_url {
                let image_id = stable_child_id(&section_id, "image");
                components.push(image(&image_id, image_url));
                section_children.push(image_id);
            }

            if !section.bullets.is_empty() {
                let list_id = stable_child_id(&section_id, "bullets");
                let mut bullet_ids = Vec::new();
                for (idx, bullet) in section.bullets.iter().enumerate() {
                    let bullet_id = stable_indexed_id(&list_id, "item", idx);
                    components.push(text(&bullet_id, bullet, None));
                    bullet_ids.push(bullet_id);
                }
                let bullet_ids_str: Vec<&str> = bullet_ids.iter().map(|s| s.as_str()).collect();
                components.push(column(&list_id, bullet_ids_str));
                section_children.push(list_id);
            }

            if !section.actions.is_empty() {
                let actions_id = stable_child_id(&section_id, "actions");
                let mut action_ids = Vec::new();
                for (idx, action) in section.actions.iter().enumerate() {
                    let button_id = stable_indexed_id(&actions_id, "button", idx);
                    let label_id = stable_child_id(&button_id, "label");
                    components.push(text(&label_id, &action.label, None));

                    // Build button with action
                    let mut button_comp = json!({
                        "id": button_id,
                        "component": "Button",
                        "child": label_id,
                        "action": {
                            "event": {
                                "name": action.action
                            }
                        }
                    });

                    if let Some(variant) = &action.variant {
                        button_comp["variant"] = json!(variant);
                    }
                    if let Some(context) = &action.context {
                        button_comp["action"]["event"]["context"] = context.clone();
                    }

                    components.push(button_comp);
                    action_ids.push(button_id);
                }
                let action_ids_str: Vec<&str> = action_ids.iter().map(|s| s.as_str()).collect();
                components.push(row(&actions_id, action_ids_str));
                section_children.push(actions_id);
            }

            let section_children_str: Vec<&str> =
                section_children.iter().map(|s| s.as_str()).collect();
            components.push(column(&section_id, section_children_str));
            root_children.push(section_id);

            if index + 1 < params.sections.len() {
                let divider_id = stable_indexed_id(&page_id, "divider", index);
                components.push(divider(&divider_id, "horizontal"));
                root_children.push(divider_id);
            }
        }

        let root_children_str: Vec<&str> = root_children.iter().map(|s| s.as_str()).collect();
        components.push(column("root", root_children_str));

        let surface = UiSurface::new(params.surface_id.clone(), catalog_id, components)
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

                let jsonl = encode_jsonl(messages).map_err(|e| {
                    adk_core::AdkError::Tool(format!("Failed to encode A2UI JSONL: {}", e))
                })?;

                // Keep historical return type for default protocol compatibility.
                Ok(Value::String(jsonl))
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
    async fn render_page_emits_jsonl() {
        let tool = RenderPageTool::new();
        let args = serde_json::json!({
            "title": "Launch",
            "sections": [
                {
                    "heading": "Features",
                    "body": "Fast and secure.",
                    "bullets": ["One", "Two"],
                    "actions": [
                        { "label": "Get Started", "action": "start", "variant": "primary" }
                    ]
                }
            ]
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();
        let jsonl = value.as_str().unwrap();
        let lines: Vec<Value> =
            jsonl.trim_end().lines().map(|line| serde_json::from_str(line).unwrap()).collect();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].get("createSurface").is_some());
        assert!(lines[1].get("updateComponents").is_some());
    }

    #[tokio::test]
    async fn render_page_emits_ag_ui_events() {
        let tool = RenderPageTool::new();
        let args = serde_json::json!({
            "protocol": "ag_ui",
            "title": "Launch",
            "sections": [{ "heading": "Features" }]
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();
        assert_eq!(value["protocol"], "ag_ui");
        let events = value["events"].as_array().unwrap();
        assert_eq!(events[1]["type"], "CUSTOM");
    }

    #[tokio::test]
    async fn render_page_emits_mcp_apps_payload() {
        let tool = RenderPageTool::new();
        let args = serde_json::json!({
            "protocol": "mcp_apps",
            "title": "Launch",
            "sections": [{ "heading": "Features" }],
            "mcp_apps": {
                "resource_uri": "ui://tests/page"
            }
        });

        let ctx: Arc<dyn ToolContext> = Arc::new(TestContext::new());
        let value = tool.execute(ctx, args).await.unwrap();
        assert_eq!(value["protocol"], "mcp_apps");
        assert_eq!(value["payload"]["toolMeta"]["_meta"]["ui"]["resourceUri"], "ui://tests/page");
    }
}
