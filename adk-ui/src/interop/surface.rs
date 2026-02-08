use crate::a2ui::{
    A2uiMessage, CreateSurface, CreateSurfaceMessage, UpdateComponents, UpdateComponentsMessage,
    UpdateDataModel, UpdateDataModelMessage, encode_jsonl,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Supported UI interoperability protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiProtocol {
    #[default]
    A2ui,
    AgUi,
    McpApps,
}

/// Protocol-neutral UI surface representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSurface {
    pub surface_id: String,
    pub catalog_id: String,
    pub components: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_model: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<Value>,
    pub send_data_model: bool,
}

impl UiSurface {
    pub fn new(
        surface_id: impl Into<String>,
        catalog_id: impl Into<String>,
        components: Vec<Value>,
    ) -> Self {
        Self {
            surface_id: surface_id.into(),
            catalog_id: catalog_id.into(),
            components,
            data_model: None,
            theme: None,
            send_data_model: true,
        }
    }

    pub fn with_data_model(mut self, data_model: Option<Value>) -> Self {
        self.data_model = data_model;
        self
    }

    pub fn with_theme(mut self, theme: Option<Value>) -> Self {
        self.theme = theme;
        self
    }

    pub fn with_send_data_model(mut self, send_data_model: bool) -> Self {
        self.send_data_model = send_data_model;
        self
    }

    pub fn to_a2ui_messages(&self) -> Vec<A2uiMessage> {
        let mut messages = vec![A2uiMessage::CreateSurface(CreateSurfaceMessage {
            create_surface: CreateSurface {
                surface_id: self.surface_id.clone(),
                catalog_id: self.catalog_id.clone(),
                theme: self.theme.clone(),
                send_data_model: Some(self.send_data_model),
            },
        })];

        if let Some(data_model) = self.data_model.clone() {
            messages.push(A2uiMessage::UpdateDataModel(UpdateDataModelMessage {
                update_data_model: UpdateDataModel {
                    surface_id: self.surface_id.clone(),
                    path: Some("/".to_string()),
                    value: Some(data_model),
                },
            }));
        }

        messages.push(A2uiMessage::UpdateComponents(UpdateComponentsMessage {
            update_components: UpdateComponents {
                surface_id: self.surface_id.clone(),
                components: self.components.clone(),
            },
        }));

        messages
    }

    pub fn to_a2ui_jsonl(&self) -> Result<String, serde_json::Error> {
        encode_jsonl(self.to_a2ui_messages())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn surface_to_a2ui_messages_emits_expected_order() {
        let surface = UiSurface::new(
            "main",
            "catalog",
            vec![json!({"id":"root","component":{"Column":{"children":[]}}})],
        )
        .with_data_model(Some(json!({"ok": true})));

        let messages = surface.to_a2ui_messages();
        assert_eq!(messages.len(), 3);

        let first = serde_json::to_value(&messages[0]).unwrap();
        let second = serde_json::to_value(&messages[1]).unwrap();
        let third = serde_json::to_value(&messages[2]).unwrap();

        assert!(first.get("createSurface").is_some());
        assert!(second.get("updateDataModel").is_some());
        assert!(third.get("updateComponents").is_some());
    }

    #[test]
    fn surface_to_a2ui_jsonl_serializes() {
        let surface = UiSurface::new(
            "main",
            "catalog",
            vec![json!({"id":"root","component":{"Column":{"children":[]}}})],
        );
        let jsonl = surface.to_a2ui_jsonl().unwrap();
        assert!(jsonl.contains("createSurface"));
        assert!(jsonl.contains("updateComponents"));
    }
}
