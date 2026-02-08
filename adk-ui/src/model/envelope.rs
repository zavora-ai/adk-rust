use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard envelope protocol marker for render tool outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolEnvelopeProtocol {
    AdkUi,
    A2ui,
    AgUi,
    McpApps,
}

/// Canonical tool output envelope.
///
/// Payload fields are flattened to preserve backward-compatible response shapes
/// while still attaching protocol metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolEnvelope<P> {
    pub protocol: ToolEnvelopeProtocol,
    pub version: String,
    pub surface_id: String,
    #[serde(flatten)]
    pub payload: P,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl<P> ToolEnvelope<P> {
    pub fn new(protocol: ToolEnvelopeProtocol, surface_id: impl Into<String>, payload: P) -> Self {
        Self {
            protocol,
            version: "1.0".to_string(),
            surface_id: surface_id.into(),
            payload,
            meta: None,
        }
    }

    pub fn with_meta(mut self, meta: Option<Value>) -> Self {
        self.meta = meta;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;

    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    struct Payload {
        value: String,
    }

    #[test]
    fn envelope_serializes_flattened_payload() {
        let envelope = ToolEnvelope::new(
            ToolEnvelopeProtocol::A2ui,
            "main",
            Payload { value: "ok".to_string() },
        )
        .with_meta(Some(json!({"trace_id": "abc"})));

        let value = serde_json::to_value(envelope).expect("serialize");
        assert_eq!(value["protocol"], "a2ui");
        assert_eq!(value["surface_id"], "main");
        assert_eq!(value["version"], "1.0");
        assert_eq!(value["value"], "ok");
        assert_eq!(value["meta"]["trace_id"], "abc");
    }
}
