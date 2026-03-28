use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::CacheControlEphemeral;

/// A block representing a server-side tool use request from the model.
///
/// ServerToolUseBlocks indicate the model wants to use a server-side tool (like web search).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerToolUseBlock {
    /// A unique identifier for this tool use request.
    pub id: String,

    /// The input data for the tool, can be any valid JSON.
    pub input: Value,

    /// The name of the server tool being invoked.
    /// Currently only "web_search" is supported.
    #[serde(default = "default_name")]
    pub name: String,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

fn default_name() -> String {
    "web_search".to_string()
}

impl ServerToolUseBlock {
    /// Creates a new ServerToolUseBlock with the specified id and input.
    /// The name is set to "web_search" as that's the only supported server tool.
    pub fn new<S: Into<String>>(id: S, input: Value) -> Self {
        Self { id: id.into(), input, name: default_name(), cache_control: None }
    }

    /// Add a cache control to this server tool use block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Creates a new web search ServerToolUseBlock with the specified id and query.
    pub fn new_web_search<S1: Into<String>, S2: Into<String>>(id: S1, query: S2) -> Self {
        let input = serde_json::json!({
            "query": query.into()
        });

        Self::new(id, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn server_tool_use_block_serialization() {
        let input_json = serde_json::json!({
            "query": "weather in San Francisco"
        });

        let block = ServerToolUseBlock::new("tool_123", input_json);

        let json = serde_json::to_string(&block).unwrap();
        let expected =
            r#"{"id":"tool_123","input":{"query":"weather in San Francisco"},"name":"web_search"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn new_web_search() {
        let block = ServerToolUseBlock::new_web_search("tool_123", "weather in San Francisco");

        let json = serde_json::to_string(&block).unwrap();
        let expected =
            r#"{"id":"tool_123","input":{"query":"weather in San Francisco"},"name":"web_search"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json =
            r#"{"id":"tool_123","input":{"query":"weather in San Francisco"},"name":"web_search"}"#;
        let block: ServerToolUseBlock = serde_json::from_str(json).unwrap();

        assert_eq!(block.id, "tool_123");
        assert_eq!(block.name, "web_search");

        let expected_input = serde_json::json!({
            "query": "weather in San Francisco"
        });
        assert_eq!(block.input, expected_input);
    }

    #[test]
    fn server_tool_use_block_with_cache_control() {
        let input = json!({
            "query": "weather in San Francisco"
        });

        let cache_control = CacheControlEphemeral::new();
        let block =
            ServerToolUseBlock::new("server_tool_1", input).with_cache_control(cache_control);

        let json = to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({
                "id": "server_tool_1",
                "input": {
                    "query": "weather in San Francisco"
                },
                "name": "web_search",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }
}
