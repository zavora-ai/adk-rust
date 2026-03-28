use serde::{Deserialize, Serialize};

use crate::types::CacheControlEphemeral;

/// Parameters for the Bash tool type, version 20250124.
///
/// This tool allows the AI to execute bash commands via the API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolBash20250124 {
    /// Name of the tool. This is how the tool will be called by the model and in `tool_use` blocks.
    ///
    /// Always set to "bash".
    #[serde(default = "default_name")]
    pub name: String,

    /// Create a cache control breakpoint at this content block.
    /// If provided, this instructs the API to not cache this tool or its results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

fn default_name() -> String {
    "bash".to_string()
}

impl ToolBash20250124 {
    /// Creates a new Bash tool parameter object with default settings.
    pub fn new() -> Self {
        Self { name: default_name(), cache_control: None }
    }

    /// Sets the cache control to ephemeral for this tool.
    pub fn with_ephemeral_cache_control(mut self) -> Self {
        self.cache_control = Some(CacheControlEphemeral::new());
        self
    }
}

impl Default for ToolBash20250124 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn tool_bash_param_minimal() {
        let param = ToolBash20250124::new();
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "name": "bash"
            })
        );
    }

    #[test]
    fn tool_bash_param_with_cache_control() {
        let param = ToolBash20250124::new().with_ephemeral_cache_control();

        let json = to_value(&param).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "bash",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn tool_bash_param_deserialization() {
        let json = json!({
            "name": "bash",
            "cache_control": {
                "type": "ephemeral"
            }
        });

        let param: ToolBash20250124 = serde_json::from_value(json).unwrap();
        assert_eq!(param.name, "bash");
        assert!(param.cache_control.is_some());
    }
}
