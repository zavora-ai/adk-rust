use serde::{Deserialize, Serialize};

use crate::types::CacheControlEphemeral;

/// Parameters for the Text Editor tool type, version 20250728.
///
/// This tool allows the AI to perform text editing operations via the API.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolTextEditor20250728 {
    /// Name of the tool. This is how the tool will be called by the model and in `tool_use` blocks.
    ///
    /// Always set to "str_replace_based_edit_tool".
    #[serde(default = "default_name")]
    pub name: String,

    /// Create a cache control breakpoint at this content block.
    /// If provided, this instructs the API to not cache this tool or its results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Maximum characters to display when viewing a file.
    /// If not specified, defaults to showing the full file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_characters: Option<i32>,
}

fn default_name() -> String {
    "str_replace_based_edit_tool".to_string()
}

impl ToolTextEditor20250728 {
    /// Creates a new Text Editor tool parameter object with default settings.
    pub fn new() -> Self {
        Self { name: default_name(), cache_control: None, max_characters: None }
    }

    /// Sets the cache control to ephemeral for this tool.
    pub fn with_ephemeral_cache_control(mut self) -> Self {
        self.cache_control = Some(CacheControlEphemeral::new());
        self
    }

    /// Sets the maximum characters to display when viewing a file.
    pub fn with_max_characters(mut self, max_characters: i32) -> Self {
        self.max_characters = Some(max_characters);
        self
    }
}

impl Default for ToolTextEditor20250728 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn tool_text_editor_param_minimal() {
        let param = ToolTextEditor20250728::new();
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "name": "str_replace_based_edit_tool"
            })
        );
    }

    #[test]
    fn tool_text_editor_param_with_cache_control() {
        let param = ToolTextEditor20250728::new().with_ephemeral_cache_control();

        let json = to_value(&param).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "str_replace_based_edit_tool",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn tool_text_editor_param_with_max_characters() {
        let param =
            ToolTextEditor20250728::new().with_max_characters(1000).with_ephemeral_cache_control();

        let json = to_value(&param).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "str_replace_based_edit_tool",
                "cache_control": {
                    "type": "ephemeral"
                },
                "max_characters": 1000
            })
        );
    }

    #[test]
    fn tool_text_editor_param_deserialization() {
        let json = json!({
            "name": "str_replace_based_edit_tool",
            "cache_control": {
                "type": "ephemeral"
            },
            "max_characters": 2000
        });

        let param: ToolTextEditor20250728 = serde_json::from_value(json).unwrap();
        assert_eq!(param.name, "str_replace_based_edit_tool");
        assert!(param.cache_control.is_some());
        assert_eq!(param.max_characters, Some(2000));
    }
}
