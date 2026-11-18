use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::CacheControlEphemeral;

/// A block representing a tool use request from the model.
///
/// ToolUseBlocks indicate the model wants to use a specific tool with certain inputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolUseBlock {
    /// A unique identifier for this tool use request.
    pub id: String,

    /// The input data for the tool, can be any valid JSON.
    pub input: Value,

    /// The name of the tool being invoked.
    pub name: String,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// The client toolset the call belongs to (`computer` or `browser`), with `name`
    /// naming the member. `None` for every other tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset_name: Option<String>,
}

impl ToolUseBlock {
    /// Creates a new ToolUseBlock with the specified id, name, and input.
    pub fn new<S1: Into<String>, S2: Into<String>>(id: S1, name: S2, input: Value) -> Self {
        Self { id: id.into(), name: name.into(), input, cache_control: None, toolset_name: None }
    }

    /// A call to `member` of the client toolset `toolset`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_anthropic::{COMPUTER_TOOLSET, ToolUseBlock};
    /// use serde_json::json;
    ///
    /// let block = ToolUseBlock::member("toolu_1", COMPUTER_TOOLSET, "left_click", json!({ "coordinate": [5, 9] }));
    /// assert_eq!(
    ///     serde_json::to_value(&block).unwrap(),
    ///     json!({
    ///         "id": "toolu_1",
    ///         "name": "left_click",
    ///         "toolset_name": "computer",
    ///         "input": { "coordinate": [5, 9] }
    ///     })
    /// );
    /// ```
    pub fn member(
        id: impl Into<String>,
        toolset: impl Into<String>,
        member: impl Into<String>,
        input: Value,
    ) -> Self {
        Self::new(id, member, input).with_toolset_name(toolset)
    }

    /// Marks the call as a member of the client toolset `toolset`.
    pub fn with_toolset_name(mut self, toolset: impl Into<String>) -> Self {
        self.toolset_name = Some(toolset.into());
        self
    }

    /// Add a cache control to this tool use block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn tool_use_block_serialization() {
        let input_json = json!({
            "query": "weather in San Francisco",
            "limit": 5
        });

        let block = ToolUseBlock::new("tool_123", "search", input_json.clone());

        let json = serde_json::to_value(&block).unwrap();
        let expected = serde_json::json!({
            "id": "tool_123",
            "input": input_json,
            "name": "search"
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn tool_use_block_with_cache_control() {
        let input_json = json!({
            "query": "weather in San Francisco",
            "limit": 5
        });

        let cache_control = CacheControlEphemeral::new();
        let block =
            ToolUseBlock::new("tool_123", "search", input_json).with_cache_control(cache_control);

        let json = to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({
                "id": "tool_123",
                "input": {
                    "query": "weather in San Francisco",
                    "limit": 5
                },
                "name": "search",
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({
            "id": "tool_123",
            "input": {
                "query": "weather in San Francisco",
                "limit": 5
            },
            "name": "search"
        });
        let block: ToolUseBlock = serde_json::from_value(json).unwrap();

        assert_eq!(block.id, "tool_123");
        assert_eq!(block.name, "search");

        let expected_input = json!({
            "query": "weather in San Francisco",
            "limit": 5
        });
        assert_eq!(block.input, expected_input);
    }
}
