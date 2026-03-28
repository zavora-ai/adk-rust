use serde::{Deserialize, Serialize};

/// Automatic cache control configuration.
///
/// When set with `type: "auto"`, the server automatically advances the cache
/// breakpoint to the last cacheable block in the conversation on each turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoCacheControl {
    /// The control type (always "auto").
    #[serde(rename = "type")]
    pub control_type: String,
}

impl AutoCacheControl {
    /// Create a new `AutoCacheControl` with type "auto".
    pub fn new() -> Self {
        Self { control_type: "auto".to_string() }
    }
}

impl Default for AutoCacheControl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let control = AutoCacheControl::new();
        let json = serde_json::to_value(&control).unwrap();
        assert_eq!(json, json!({"type": "auto"}));
    }

    #[test]
    fn deserialization() {
        let json = json!({"type": "auto"});
        let control: AutoCacheControl = serde_json::from_value(json).unwrap();
        assert_eq!(control.control_type, "auto");
    }
}
