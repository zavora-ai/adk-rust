use crate::a2ui::{column, stable_id, stable_indexed_id, text};
use crate::catalog_registry::CatalogRegistry;
use crate::interop::{
    AgUiEvent, McpAppsRenderOptions, McpAppsSurfacePayload, UiProtocol, UiSurface,
    surface_to_event_stream, surface_to_mcp_apps_payload, validate_mcp_apps_render_options,
};
use crate::model::{ToolEnvelope, ToolEnvelopeProtocol};
use crate::schema::{Component, UiResponse};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LegacyProtocolOptions {
    /// Optional protocol override. If omitted, tool returns legacy adk-ui payload.
    #[serde(default)]
    pub protocol: Option<UiProtocol>,
    /// Optional surface id for protocol adapters.
    #[serde(default)]
    pub surface_id: Option<String>,
    /// Optional AG-UI thread id.
    #[serde(default)]
    pub ag_ui_thread_id: Option<String>,
    /// Optional AG-UI run id.
    #[serde(default)]
    pub ag_ui_run_id: Option<String>,
    /// Optional MCP Apps adapter options.
    #[serde(default)]
    pub mcp_apps: Option<Value>,
}

impl LegacyProtocolOptions {
    pub fn resolved_surface_id(&self, fallback: &str) -> String {
        self.surface_id.clone().unwrap_or_else(|| fallback.to_string())
    }
}

fn default_surface_protocol() -> UiProtocol {
    UiProtocol::A2ui
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SurfaceProtocolOptions {
    /// Output protocol (a2ui, ag_ui, mcp_apps). Default: a2ui.
    #[serde(default = "default_surface_protocol")]
    pub protocol: UiProtocol,
    /// Optional AG-UI thread id.
    #[serde(default)]
    pub ag_ui_thread_id: Option<String>,
    /// Optional AG-UI run id.
    #[serde(default)]
    pub ag_ui_run_id: Option<String>,
    /// Optional MCP Apps adapter options.
    #[serde(default)]
    pub mcp_apps: Option<Value>,
}

impl Default for SurfaceProtocolOptions {
    fn default() -> Self {
        Self {
            protocol: default_surface_protocol(),
            ag_ui_thread_id: None,
            ag_ui_run_id: None,
            mcp_apps: None,
        }
    }
}

impl SurfaceProtocolOptions {
    pub fn resolved_ag_ui_thread_id(&self, surface_id: &str) -> String {
        self.ag_ui_thread_id.clone().unwrap_or_else(|| format!("thread-{}", surface_id))
    }

    pub fn resolved_ag_ui_run_id(&self, surface_id: &str) -> String {
        self.ag_ui_run_id.clone().unwrap_or_else(|| format!("run-{}", surface_id))
    }

    pub fn parse_mcp_options(&self) -> Result<McpAppsRenderOptions, adk_core::AdkError> {
        let options = match &self.mcp_apps {
            Some(value) => {
                serde_json::from_value::<McpAppsRenderOptions>(value.clone()).map_err(|error| {
                    adk_core::AdkError::Tool(format!("Invalid mcp_apps options payload: {}", error))
                })
            }
            None => Ok(McpAppsRenderOptions::default()),
        }?;
        validate_mcp_apps_render_options(&options)?;
        Ok(options)
    }
}

fn summarize_component(component: &Component) -> String {
    let value = serde_json::to_value(component).unwrap_or_else(|_| json!({"type": "unknown"}));
    let component_type = value.get("type").and_then(Value::as_str).unwrap_or("component");
    format!("Rendered {}", component_type)
}

fn project_ui_response_to_surface(ui: &UiResponse, surface_id: &str) -> UiSurface {
    let registry = CatalogRegistry::new();
    let catalog_id = registry.default_catalog_id().to_string();
    let projection_root = stable_id(&format!("legacy:{}:projection", surface_id));

    let mut components: Vec<Value> = Vec::new();
    let mut child_ids: Vec<String> = Vec::new();

    for (index, component) in ui.components.iter().enumerate() {
        let child_id = stable_indexed_id(&projection_root, "item", index);
        child_ids.push(child_id.clone());
        components.push(text(&child_id, &summarize_component(component), Some("body")));
    }

    if child_ids.is_empty() {
        let empty_id = stable_indexed_id(&projection_root, "item", 0);
        child_ids.push(empty_id.clone());
        components.push(text(&empty_id, "Rendered empty_ui", Some("caption")));
    }

    let child_refs: Vec<&str> = child_ids.iter().map(String::as_str).collect();
    components.push(column("root", child_refs));

    UiSurface::new(surface_id.to_string(), catalog_id, components).with_data_model(Some(json!({
        "adk_ui_response": ui
    })))
}

#[derive(Debug, Clone, Serialize)]
struct A2uiEnvelopePayload {
    components: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_model: Option<Value>,
    jsonl: String,
}

#[derive(Debug, Clone, Serialize)]
struct AgUiEnvelopePayload {
    events: Vec<AgUiEvent>,
}

#[derive(Debug, Clone, Serialize)]
struct McpAppsEnvelopePayload {
    payload: McpAppsSurfacePayload,
}

fn serialize_envelope<P: Serialize>(
    envelope: ToolEnvelope<P>,
) -> Result<Value, adk_core::AdkError> {
    serde_json::to_value(envelope).map_err(|e| {
        adk_core::AdkError::Tool(format!("Failed to serialize protocol envelope: {}", e))
    })
}

pub(crate) fn render_ui_response_with_protocol(
    ui: UiResponse,
    options: &LegacyProtocolOptions,
    default_surface_id: &str,
) -> Result<Value, adk_core::AdkError> {
    let protocol = match options.protocol {
        Some(protocol) => protocol,
        None => {
            return serde_json::to_value(ui)
                .map_err(|e| adk_core::AdkError::Tool(format!("Failed to serialize UI: {}", e)));
        }
    };

    let surface_id = options.resolved_surface_id(default_surface_id);
    let surface = project_ui_response_to_surface(&ui, &surface_id);

    match protocol {
        UiProtocol::A2ui => {
            let jsonl = surface.to_a2ui_jsonl().map_err(|e| {
                adk_core::AdkError::Tool(format!("Failed to encode A2UI JSONL: {}", e))
            })?;
            let envelope = ToolEnvelope::new(
                ToolEnvelopeProtocol::A2ui,
                surface.surface_id,
                A2uiEnvelopePayload {
                    components: surface.components,
                    data_model: surface.data_model,
                    jsonl,
                },
            );
            serialize_envelope(envelope)
        }
        UiProtocol::AgUi => {
            let thread_id = options
                .ag_ui_thread_id
                .clone()
                .unwrap_or_else(|| format!("thread-{}", surface.surface_id));
            let run_id = options
                .ag_ui_run_id
                .clone()
                .unwrap_or_else(|| format!("run-{}", surface.surface_id));
            let events = surface_to_event_stream(&surface, thread_id, run_id);
            let envelope = ToolEnvelope::new(
                ToolEnvelopeProtocol::AgUi,
                surface.surface_id,
                AgUiEnvelopePayload { events },
            );
            serialize_envelope(envelope)
        }
        UiProtocol::McpApps => {
            let mcp_options = match &options.mcp_apps {
                Some(value) => serde_json::from_value::<McpAppsRenderOptions>(value.clone())
                    .map_err(|error| {
                        adk_core::AdkError::Tool(format!(
                            "Invalid mcp_apps options payload: {}",
                            error
                        ))
                    })?,
                None => McpAppsRenderOptions::default(),
            };
            validate_mcp_apps_render_options(&mcp_options)?;
            let payload = surface_to_mcp_apps_payload(&surface, mcp_options);
            let envelope = ToolEnvelope::new(
                ToolEnvelopeProtocol::McpApps,
                surface.surface_id,
                McpAppsEnvelopePayload { payload },
            );
            serialize_envelope(envelope)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Text, TextVariant};

    #[test]
    fn legacy_default_returns_ui_response_json() {
        let ui = UiResponse::new(vec![Component::Text(Text {
            id: None,
            content: "Hello".to_string(),
            variant: TextVariant::Body,
        })]);
        let value = render_ui_response_with_protocol(ui, &LegacyProtocolOptions::default(), "main")
            .expect("legacy render");
        assert!(value.get("components").is_some());
        assert!(value.get("protocol").is_none());
    }

    #[test]
    fn legacy_mcp_apps_returns_protocol_payload() {
        let ui = UiResponse::new(vec![Component::Text(Text {
            id: None,
            content: "Hello".to_string(),
            variant: TextVariant::Body,
        })]);
        let options =
            LegacyProtocolOptions { protocol: Some(UiProtocol::McpApps), ..Default::default() };
        let value = render_ui_response_with_protocol(ui, &options, "main").expect("mcp payload");
        assert_eq!(value["protocol"], "mcp_apps");
        assert_eq!(value["version"], "1.0");
        assert!(value["payload"]["resource"]["uri"].as_str().unwrap().starts_with("ui://"));
    }

    #[test]
    fn legacy_mcp_apps_rejects_invalid_domain_option() {
        let ui = UiResponse::new(vec![Component::Text(Text {
            id: None,
            content: "Hello".to_string(),
            variant: TextVariant::Body,
        })]);
        let options = LegacyProtocolOptions {
            protocol: Some(UiProtocol::McpApps),
            mcp_apps: Some(json!({
                "domain": "ftp://example.com"
            })),
            ..Default::default()
        };
        let value = render_ui_response_with_protocol(ui, &options, "main");
        assert!(value.is_err());
    }
}
