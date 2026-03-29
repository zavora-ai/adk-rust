use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-model capability flags.
///
/// Known boolean fields are typed; unknown fields are preserved in `extra`
/// for forward compatibility.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Whether the model supports extended thinking.
    #[serde(default)]
    pub supports_thinking: bool,
    /// Whether the model supports vision (image inputs).
    #[serde(default)]
    pub supports_vision: bool,
    /// Whether the model supports extended context windows.
    #[serde(default)]
    pub supports_extended_context: bool,
    /// Whether the model supports computer use tools.
    #[serde(default)]
    pub supports_computer_use: bool,
    /// Whether the model supports PDF document inputs.
    #[serde(default)]
    pub supports_pdf: bool,
    /// Catch-all for unknown capability fields.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let caps = ModelCapabilities {
            supports_thinking: true,
            supports_vision: true,
            supports_extended_context: false,
            supports_computer_use: false,
            supports_pdf: true,
            extra: HashMap::new(),
        };
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(
            json,
            json!({
                "supports_thinking": true,
                "supports_vision": true,
                "supports_extended_context": false,
                "supports_computer_use": false,
                "supports_pdf": true
            })
        );
    }

    #[test]
    fn deserialization_with_unknown_fields() {
        let json = json!({
            "supports_thinking": true,
            "supports_vision": false,
            "supports_extended_context": false,
            "supports_computer_use": false,
            "supports_pdf": false,
            "supports_future_feature": true,
            "max_batch_size": 100
        });
        let caps: ModelCapabilities = serde_json::from_value(json).unwrap();
        assert!(caps.supports_thinking);
        assert_eq!(caps.extra.get("supports_future_feature"), Some(&json!(true)));
        assert_eq!(caps.extra.get("max_batch_size"), Some(&json!(100)));
    }

    #[test]
    fn roundtrip_with_extra_fields() {
        let mut extra = HashMap::new();
        extra.insert("new_cap".to_string(), json!(true));
        let caps = ModelCapabilities { supports_thinking: true, extra, ..Default::default() };
        let json = serde_json::to_value(&caps).unwrap();
        let deserialized: ModelCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps, deserialized);
    }
}
