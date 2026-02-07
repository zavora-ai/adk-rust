use super::{
    McpAppsRenderOptions, UiProtocol, UiSurface, surface_to_event_stream, surface_to_mcp_apps_payload,
};
use serde_json::{Value, json};

/// Shared interface for converting canonical UI surfaces into protocol payloads.
pub trait UiProtocolAdapter {
    fn protocol(&self) -> UiProtocol;
    fn from_canonical(&self, surface: &UiSurface) -> Result<Value, adk_core::AdkError>;

    fn validate(&self, _payload: &Value) -> Result<(), adk_core::AdkError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct A2uiAdapter;

impl UiProtocolAdapter for A2uiAdapter {
    fn protocol(&self) -> UiProtocol {
        UiProtocol::A2ui
    }

    fn from_canonical(&self, surface: &UiSurface) -> Result<Value, adk_core::AdkError> {
        let jsonl = surface.to_a2ui_jsonl().map_err(|error| {
            adk_core::AdkError::Tool(format!("Failed to encode A2UI JSONL: {}", error))
        })?;
        Ok(json!({
            "protocol": "a2ui",
            "surface_id": surface.surface_id,
            "components": surface.components,
            "data_model": surface.data_model,
            "jsonl": jsonl,
        }))
    }

    fn validate(&self, payload: &Value) -> Result<(), adk_core::AdkError> {
        if payload.get("jsonl").and_then(Value::as_str).is_none() {
            return Err(adk_core::AdkError::Tool(
                "A2UI payload validation failed: missing jsonl string".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AgUiAdapter {
    thread_id: String,
    run_id: String,
}

impl AgUiAdapter {
    pub fn new(thread_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self { thread_id: thread_id.into(), run_id: run_id.into() }
    }
}

impl UiProtocolAdapter for AgUiAdapter {
    fn protocol(&self) -> UiProtocol {
        UiProtocol::AgUi
    }

    fn from_canonical(&self, surface: &UiSurface) -> Result<Value, adk_core::AdkError> {
        let events = surface_to_event_stream(surface, self.thread_id.clone(), self.run_id.clone());
        Ok(json!({
            "protocol": "ag_ui",
            "surface_id": surface.surface_id,
            "events": events,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct McpAppsAdapter {
    options: McpAppsRenderOptions,
}

impl McpAppsAdapter {
    pub fn new(options: McpAppsRenderOptions) -> Self {
        Self { options }
    }
}

impl UiProtocolAdapter for McpAppsAdapter {
    fn protocol(&self) -> UiProtocol {
        UiProtocol::McpApps
    }

    fn from_canonical(&self, surface: &UiSurface) -> Result<Value, adk_core::AdkError> {
        let payload = surface_to_mcp_apps_payload(surface, self.options.clone());
        Ok(json!({
            "protocol": "mcp_apps",
            "surface_id": surface.surface_id,
            "payload": payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_surface() -> UiSurface {
        UiSurface::new(
            "main",
            "catalog",
            vec![json!({"id":"root","component":"Column","children":[]})],
        )
    }

    #[test]
    fn a2ui_adapter_emits_jsonl_payload() {
        let adapter = A2uiAdapter;
        let payload = adapter.from_canonical(&test_surface()).expect("a2ui payload");
        adapter.validate(&payload).expect("a2ui validate");
        assert_eq!(payload["protocol"], "a2ui");
        assert!(payload["jsonl"].as_str().unwrap().contains("createSurface"));
    }

    #[test]
    fn ag_ui_adapter_emits_event_stream_payload() {
        let adapter = AgUiAdapter::new("thread-main", "run-main");
        let payload = adapter.from_canonical(&test_surface()).expect("ag ui payload");
        assert_eq!(payload["protocol"], "ag_ui");
        assert_eq!(payload["events"][0]["type"], "RUN_STARTED");
    }

    #[test]
    fn mcp_apps_adapter_emits_resource_payload() {
        let adapter = McpAppsAdapter::new(McpAppsRenderOptions::default());
        let payload = adapter.from_canonical(&test_surface()).expect("mcp payload");
        assert_eq!(payload["protocol"], "mcp_apps");
        assert!(payload["payload"]["resource"]["uri"].as_str().unwrap().starts_with("ui://"));
    }
}
