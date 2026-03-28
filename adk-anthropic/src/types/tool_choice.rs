use serde::{Deserialize, Serialize};

/// Configuration for Claude's tool choice behavior.
///
/// This can be one of the following:
/// - "auto": Let the model decide if and when to use tools
/// - "any": Allow the model to use any available tool
/// - "tool": Force the model to use a specific named tool
/// - "none": Do not use any tools
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// Automatic tool choice
    Auto {
        /// Whether to disable parallel tool use.
        ///
        /// Defaults to `false`. If set to `true`, the model will output at most one tool use.
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },

    /// Any tool choice
    Any {
        /// Whether to disable parallel tool use.
        ///
        /// Defaults to `false`. If set to `true`, the model will output exactly one tool use.
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },

    /// Specific tool choice
    Tool {
        /// The name of the tool to use.
        name: String,

        /// Whether to disable parallel tool use.
        ///
        /// Defaults to `false`. If set to `true`, the model will output exactly one tool use.
        #[serde(skip_serializing_if = "Option::is_none")]
        disable_parallel_tool_use: Option<bool>,
    },

    /// No tools
    None,
}

impl ToolChoice {
    /// Create a new `ToolChoice` with auto mode.
    pub fn auto() -> Self {
        Self::Auto { disable_parallel_tool_use: None }
    }

    /// Create a new `ToolChoice` with auto mode, specifying whether to disable parallel tool use.
    pub fn auto_with_disable_parallel(disable: bool) -> Self {
        Self::Auto { disable_parallel_tool_use: Some(disable) }
    }

    /// Create a new `ToolChoice` allowing any tool.
    pub fn any() -> Self {
        Self::Any { disable_parallel_tool_use: None }
    }

    /// Create a new `ToolChoice` allowing any tool, specifying whether to disable parallel tool use.
    pub fn any_with_disable_parallel(disable: bool) -> Self {
        Self::Any { disable_parallel_tool_use: Some(disable) }
    }

    /// Create a new `ToolChoice` with a specific named tool.
    pub fn tool(name: impl Into<String>) -> Self {
        Self::Tool { name: name.into(), disable_parallel_tool_use: None }
    }

    /// Create a new `ToolChoice` with a specific named tool, specifying whether to disable parallel tool use.
    pub fn tool_with_disable_parallel(name: impl Into<String>, disable: bool) -> Self {
        Self::Tool { name: name.into(), disable_parallel_tool_use: Some(disable) }
    }

    /// Create a new `ToolChoice` with no tools.
    pub fn none() -> Self {
        Self::None
    }
}

impl Default for ToolChoice {
    fn default() -> Self {
        Self::auto()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn tool_choice_auto() {
        let param = ToolChoice::auto();
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "type": "auto"
            })
        );
    }

    #[test]
    fn tool_choice_any() {
        let param = ToolChoice::any();
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "type": "any"
            })
        );
    }

    #[test]
    fn tool_choice_tool() {
        let param = ToolChoice::tool("my_tool");
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "name": "my_tool",
                "type": "tool"
            })
        );
    }

    #[test]
    fn tool_choice_none() {
        let param = ToolChoice::none();
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "type": "none"
            })
        );
    }

    #[test]
    fn tool_choice_auto_with_disable_parallel() {
        let param = ToolChoice::auto_with_disable_parallel(true);
        let json = to_value(&param).unwrap();

        assert_eq!(
            json,
            json!({
                "type": "auto",
                "disable_parallel_tool_use": true
            })
        );
    }

    #[test]
    fn tool_choice_deserialization_auto() {
        let json = json!({
            "type": "auto",
            "disable_parallel_tool_use": true
        });

        let param: ToolChoice = serde_json::from_value(json).unwrap();
        match param {
            ToolChoice::Auto { disable_parallel_tool_use } => {
                assert_eq!(disable_parallel_tool_use, Some(true));
            }
            _ => panic!("Expected Auto variant"),
        }
    }

    #[test]
    fn tool_choice_deserialization_tool() {
        let json = json!({
            "name": "my_tool",
            "type": "tool",
            "disable_parallel_tool_use": true
        });

        let param: ToolChoice = serde_json::from_value(json).unwrap();
        match param {
            ToolChoice::Tool { name, disable_parallel_tool_use } => {
                assert_eq!(name, "my_tool");
                assert_eq!(disable_parallel_tool_use, Some(true));
            }
            _ => panic!("Expected Tool variant"),
        }
    }
}
