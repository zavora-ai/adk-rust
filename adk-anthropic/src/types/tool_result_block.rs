use serde::{Deserialize, Serialize};

use crate::types::{CacheControlEphemeral, Content};

/// A block containing the result of a tool execution.
///
/// ToolResultBlock represents the output from executing a tool that was previously
/// requested via a ToolUseBlock. It contains the tool's response, which can be
/// either successful output or an error indication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename = "tool_result")]
pub struct ToolResultBlock {
    /// The ID of the tool use that this result is for.
    ///
    /// This must match the ID from the corresponding ToolUseBlock.
    #[serde(rename = "tool_use_id")]
    pub tool_use_id: String,

    /// Create a cache control breakpoint at this content block.
    ///
    /// When set, this creates an ephemeral cache point that can be reused
    /// in subsequent requests to avoid reprocessing the tool result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// The content of the tool result, which can be either a string or an array of content items.
    ///
    /// This contains the actual output from the tool execution. It can be simple text
    /// or structured content including images and formatted text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultBlockContent>,

    /// Whether this tool result represents an error.
    ///
    /// When true, indicates that the tool execution failed and the content
    /// contains error information rather than successful output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// The content of a tool result block, which can be either a string or an array of content items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ToolResultBlockContent {
    /// A simple string content.
    String(String),

    /// An array of content items.
    Array(Vec<Content>),
}

impl From<String> for ToolResultBlockContent {
    fn from(value: String) -> Self {
        ToolResultBlockContent::String(value)
    }
}

impl From<&str> for ToolResultBlockContent {
    fn from(value: &str) -> Self {
        ToolResultBlockContent::String(value.to_string())
    }
}

impl ToolResultBlock {
    /// Create a new `ToolResultBlock` with the given tool use ID.
    pub fn new(tool_use_id: String) -> Self {
        Self { tool_use_id, cache_control: None, content: None, is_error: None }
    }

    /// Add a cache control to this tool result block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Add string content to this tool result block.
    pub fn with_string_content(mut self, content: String) -> Self {
        self.content = Some(ToolResultBlockContent::String(content));
        self
    }

    /// Add array content to this tool result block.
    pub fn with_array_content(mut self, content: Vec<Content>) -> Self {
        self.content = Some(ToolResultBlockContent::Array(content));
        self
    }

    /// Add a single text content item to this tool result block.
    pub fn with_text_content(mut self, text: crate::types::TextBlock) -> Self {
        let content = match self.content {
            Some(ToolResultBlockContent::Array(mut items)) => {
                items.push(Content::Text(text));
                ToolResultBlockContent::Array(items)
            }
            Some(ToolResultBlockContent::String(s)) => ToolResultBlockContent::Array(vec![
                Content::Text(crate::types::TextBlock::new(s)),
                Content::Text(text),
            ]),
            None => ToolResultBlockContent::Array(vec![Content::Text(text)]),
        };
        self.content = Some(content);
        self
    }

    /// Set this tool result block as an error.
    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = Some(is_error);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn tool_result_block_with_string_content() {
        let block = ToolResultBlock::new("tool_1".to_string())
            .with_string_content("Result of tool execution".to_string());

        let json = to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({
                "tool_use_id": "tool_1",
                "type": "tool_result",
                "content": "Result of tool execution"
            })
        );
    }

    #[test]
    fn tool_result_block_with_array_content() {
        let text_param = crate::types::TextBlock::new("Sample text content".to_string());
        let content = vec![Content::Text(text_param)];

        let block = ToolResultBlock::new("tool_1".to_string()).with_array_content(content);

        let json = to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({
                "tool_use_id": "tool_1",
                "type": "tool_result",
                "content": [
                    {
                        "text": "Sample text content",
                        "type": "text"
                    }
                ]
            })
        );
    }

    #[test]
    fn tool_result_block_with_error() {
        let block = ToolResultBlock::new("tool_1".to_string())
            .with_string_content("Error executing tool".to_string())
            .with_error(true);

        let json = to_value(&block).unwrap();
        assert_eq!(
            json,
            json!({
                "tool_use_id": "tool_1",
                "type": "tool_result",
                "content": "Error executing tool",
                "is_error": true
            })
        );
    }

    #[test]
    fn tool_result_block_deserialization() {
        let json = json!({
            "tool_use_id": "tool_1",
            "type": "tool_result",
            "content": "Result of tool execution",
            "is_error": false
        });

        let block: ToolResultBlock = serde_json::from_value(json).unwrap();
        assert_eq!(block.tool_use_id, "tool_1");

        match &block.content {
            Some(ToolResultBlockContent::String(s)) => {
                assert_eq!(s, "Result of tool execution");
            }
            _ => panic!("Expected String variant"),
        }

        assert_eq!(block.is_error, Some(false));
    }

    #[test]
    fn tool_result_block_content_from_string() {
        // Test From<String> trait
        let content: ToolResultBlockContent = "Test content".to_string().into();
        match content {
            ToolResultBlockContent::String(s) => assert_eq!(s, "Test content"),
            _ => panic!("Expected String variant"),
        }

        // Test From<&str> trait
        let content: ToolResultBlockContent = "Another test".into();
        match content {
            ToolResultBlockContent::String(s) => assert_eq!(s, "Another test"),
            _ => panic!("Expected String variant"),
        }
    }
}
