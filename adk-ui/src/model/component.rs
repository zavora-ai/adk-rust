use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol-neutral component payload wrapper used by canonical surface models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CanonicalComponent(pub Value);

impl CanonicalComponent {
    pub fn id(&self) -> Option<&str> {
        self.0.get("id").and_then(Value::as_str)
    }

    pub fn component_kind(&self) -> Option<&str> {
        self.0
            .get("component")
            .and_then(Value::as_str)
            .or_else(|| self.0.get("type").and_then(Value::as_str))
    }
}

impl From<Value> for CanonicalComponent {
    fn from(value: Value) -> Self {
        Self(value)
    }
}

impl From<CanonicalComponent> for Value {
    fn from(value: CanonicalComponent) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_component_extracts_id_and_kind() {
        let component = CanonicalComponent::from(json!({
            "id": "root",
            "component": "Column"
        }));

        assert_eq!(component.id(), Some("root"));
        assert_eq!(component.component_kind(), Some("Column"));
    }
}
