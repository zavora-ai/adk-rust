use serde::{Deserialize, Serialize};

use crate::types::{
    ToolBash20241022, ToolBash20250124, ToolParam, ToolTextEditor20250124, ToolTextEditor20250429,
    ToolTextEditor20250728, WebSearchTool20250305,
};

/// Union type for different tool parameter types.
///
/// This type represents a union of different tool types that can be used with Claude, including:
/// - Custom tools
/// - Bash tools
/// - Text editor tools
/// - Web search tools
///
/// The API accepts any of these tool types when tools are provided to Claude.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolUnionParam {
    /// A custom tool with a defined schema
    #[serde(rename = "custom")]
    CustomTool(ToolParam),

    /// A bash tool for executing shell commands (version 20241022)
    #[serde(rename = "bash_20241022")]
    Bash20241022(ToolBash20241022),

    /// A bash tool for executing shell commands (version 20250124)
    #[serde(rename = "bash_20250124")]
    Bash20250124(ToolBash20250124),

    /// A text editor tool for making changes to text
    #[serde(rename = "text_editor_20250124")]
    TextEditor20250124(ToolTextEditor20250124),

    /// A text editor tool for making changes to text (version 20250429)
    #[serde(rename = "text_editor_20250429")]
    TextEditor20250429(ToolTextEditor20250429),

    /// A text editor tool for making changes to text (version 20250728)
    #[serde(rename = "text_editor_20250728")]
    TextEditor20250728(ToolTextEditor20250728),

    /// A web search tool for retrieving information from the internet
    #[serde(rename = "web_search_20250305")]
    WebSearch20250305(WebSearchTool20250305),
}

impl ToolUnionParam {
    /// Creates a new custom tool
    pub fn new_custom_tool(name: String, input_schema: serde_json::Value) -> Self {
        Self::CustomTool(ToolParam::new(name, input_schema))
    }

    /// Creates a new bash tool (version 20241022)
    pub fn new_bash_20241022_tool() -> Self {
        Self::Bash20241022(ToolBash20241022::new())
    }

    /// Creates a new bash tool (version 20250124)
    pub fn new_bash_tool() -> Self {
        Self::Bash20250124(ToolBash20250124::new())
    }

    /// Creates a new text editor tool
    pub fn new_text_editor_tool() -> Self {
        Self::TextEditor20250124(ToolTextEditor20250124::new())
    }

    /// Creates a new text editor tool (version 20250429)
    pub fn new_text_editor_20250429_tool() -> Self {
        Self::TextEditor20250429(ToolTextEditor20250429::new())
    }

    /// Creates a new text editor tool (version 20250728)
    pub fn new_text_editor_20250728_tool() -> Self {
        Self::TextEditor20250728(ToolTextEditor20250728::new())
    }

    /// Creates a new web search tool
    pub fn new_web_search_tool() -> Self {
        Self::WebSearch20250305(WebSearchTool20250305::new())
    }

    /// Check if this tool has strict mode enabled.
    ///
    /// Only custom tools can have strict mode enabled. All other tool types
    /// return `false`.
    pub fn is_strict(&self) -> bool {
        match self {
            Self::CustomTool(tool) => tool.strict == Some(true),
            // Built-in tools don't support strict mode
            Self::Bash20241022(_)
            | Self::Bash20250124(_)
            | Self::TextEditor20250124(_)
            | Self::TextEditor20250429(_)
            | Self::TextEditor20250728(_)
            | Self::WebSearch20250305(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CacheControlEphemeral, UserLocation};
    use serde_json::{json, to_value};

    #[test]
    fn custom_tool() {
        let input_schema = json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                }
            }
        });

        let custom_tool = ToolParam::new("search".to_string(), input_schema)
            .with_description("Search for information".to_string())
            .with_cache_control(CacheControlEphemeral::new());

        let tool = ToolUnionParam::CustomTool(custom_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        }
                    }
                },
                "name": "search",
                "cache_control": {
                    "type": "ephemeral"
                },
                "description": "Search for information",
                "type": "custom"
            })
        );
    }

    #[test]
    fn bash_20241022_tool() {
        let bash_tool = ToolBash20241022::new().with_ephemeral_cache_control();
        let tool = ToolUnionParam::Bash20241022(bash_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "bash",
                "type": "bash_20241022",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn bash_20250124_tool() {
        let bash_tool = ToolBash20250124::new().with_ephemeral_cache_control();
        let tool = ToolUnionParam::Bash20250124(bash_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "bash",
                "type": "bash_20250124",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn text_editor_tool() {
        let text_editor_tool = ToolTextEditor20250124::new().with_ephemeral_cache_control();
        let tool = ToolUnionParam::TextEditor20250124(text_editor_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "str_replace_editor",
                "type": "text_editor_20250124",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn text_editor_20250429_tool() {
        let text_editor_tool = ToolTextEditor20250429::new().with_ephemeral_cache_control();
        let tool = ToolUnionParam::TextEditor20250429(text_editor_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "str_replace_based_edit_tool",
                "type": "text_editor_20250429",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn text_editor_20250728_tool() {
        let text_editor_tool =
            ToolTextEditor20250728::new().with_max_characters(5000).with_ephemeral_cache_control();
        let tool = ToolUnionParam::TextEditor20250728(text_editor_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "str_replace_based_edit_tool",
                "type": "text_editor_20250728",
                "cache_control": {
                    "type": "ephemeral"
                },
                "max_characters": 5000
            })
        );
    }

    #[test]
    fn web_search_tool() {
        let user_location = UserLocation::new().with_city("San Francisco").with_country("US");

        let web_search_tool = WebSearchTool20250305::new()
            .with_allowed_domains(vec!["example.com".to_string(), "example.org".to_string()])
            .with_max_uses(5)
            .with_user_location(user_location)
            .with_cache_control(CacheControlEphemeral::new());

        let tool = ToolUnionParam::WebSearch20250305(web_search_tool);

        let json = to_value(&tool).unwrap();
        assert_eq!(
            json,
            json!({
                "name": "web_search",
                "type": "web_search_20250305",
                "allowed_domains": ["example.com", "example.org"],
                "cache_control": {
                    "type": "ephemeral"
                },
                "max_uses": 5,
                "user_location": {
                    "type": "approximate",
                    "city": "San Francisco",
                    "country": "US"
                }
            })
        );
    }

    #[test]
    fn deserialization() {
        // Test custom tool deserialization
        let json = json!({
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                }
            },
            "name": "search",
            "description": "Search for information",
            "type": "custom"
        });

        let tool: ToolUnionParam = serde_json::from_value(json).unwrap();
        match tool {
            ToolUnionParam::CustomTool(t) => {
                assert_eq!(t.name, "search");
                assert_eq!(t.description, Some("Search for information".to_string()));
            }
            _ => panic!("Expected CustomTool variant"),
        }

        // Test bash 20241022 tool deserialization
        let json = json!({
            "name": "bash",
            "type": "bash_20241022"
        });

        let tool: ToolUnionParam = serde_json::from_value(json).unwrap();
        match tool {
            ToolUnionParam::Bash20241022(t) => {
                assert_eq!(t.name, "bash");
            }
            _ => panic!("Expected Bash20241022 variant"),
        }

        // Test bash 20250124 tool deserialization
        let json = json!({
            "name": "bash",
            "type": "bash_20250124"
        });

        let tool: ToolUnionParam = serde_json::from_value(json).unwrap();
        match tool {
            ToolUnionParam::Bash20250124(t) => {
                assert_eq!(t.name, "bash");
            }
            _ => panic!("Expected Bash20250124 variant"),
        }
    }
}
