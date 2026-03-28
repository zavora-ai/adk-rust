use serde::{Deserialize, Serialize};

use crate::types::{
    CodeExecutionResultBlock, DocumentBlock, ImageBlock, ProgrammaticToolUseBlock,
    RedactedThinkingBlock, ServerToolUseBlock, TextBlock, ThinkingBlock, ToolResultBlock,
    ToolUseBlock, WebSearchToolResultBlock,
};

/// A block of content in a message.
///
/// This enum represents the different types of content blocks that can be included
/// in a message's content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// A block of text content
    #[serde(rename = "text")]
    Text(TextBlock),

    /// An image block
    #[serde(rename = "image")]
    Image(ImageBlock),

    /// A block representing a tool use request
    #[serde(rename = "tool_use")]
    ToolUse(ToolUseBlock),

    /// A block representing a server-side tool use request
    #[serde(rename = "server_tool_use")]
    ServerToolUse(ServerToolUseBlock),

    /// A web search tool result block
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult(WebSearchToolResultBlock),

    /// A tool result block
    #[serde(rename = "tool_result")]
    ToolResult(ToolResultBlock),

    /// A document block
    #[serde(rename = "document")]
    Document(DocumentBlock),

    /// A block containing model thinking
    #[serde(rename = "thinking")]
    Thinking(ThinkingBlock),

    /// A block containing redacted thinking data
    #[serde(rename = "redacted_thinking")]
    RedactedThinking(RedactedThinkingBlock),

    /// A code execution result block
    #[serde(rename = "code_execution_result")]
    CodeExecutionResult(CodeExecutionResultBlock),

    /// A programmatic tool use block from code execution
    #[serde(rename = "programmatic_tool_use")]
    ProgrammaticToolUse(ProgrammaticToolUseBlock),
}

impl ContentBlock {
    /// Returns true if this block is a text block
    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text(_))
    }

    /// Returns true if this block is an image block
    pub fn is_image(&self) -> bool {
        matches!(self, ContentBlock::Image(_))
    }

    /// Returns true if this block is a tool use block
    pub fn is_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ToolUse(_))
    }

    /// Returns true if this block is a server tool use block
    pub fn is_server_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ServerToolUse(_))
    }

    /// Returns true if this block is a web search tool result block
    pub fn is_web_search_tool_result(&self) -> bool {
        matches!(self, ContentBlock::WebSearchToolResult(_))
    }

    /// Returns true if this block is a tool result block
    pub fn is_tool_result(&self) -> bool {
        matches!(self, ContentBlock::ToolResult(_))
    }

    /// Returns true if this block is a document block
    pub fn is_document(&self) -> bool {
        matches!(self, ContentBlock::Document(_))
    }

    /// Returns true if this block is a thinking block
    pub fn is_thinking(&self) -> bool {
        matches!(self, ContentBlock::Thinking(_))
    }

    /// Returns true if this block is a redacted thinking block
    pub fn is_redacted_thinking(&self) -> bool {
        matches!(self, ContentBlock::RedactedThinking(_))
    }

    /// Returns a reference to the inner TextBlock if this is a Text variant,
    /// or None otherwise.
    pub fn as_text(&self) -> Option<&TextBlock> {
        match self {
            ContentBlock::Text(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner ImageBlock if this is an Image variant,
    /// or None otherwise.
    pub fn as_image(&self) -> Option<&ImageBlock> {
        match self {
            ContentBlock::Image(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner ToolUseBlock if this is a ToolUse variant,
    /// or None otherwise.
    pub fn as_tool_use(&self) -> Option<&ToolUseBlock> {
        match self {
            ContentBlock::ToolUse(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner ServerToolUseBlock if this is a ServerToolUse variant,
    /// or None otherwise.
    pub fn as_server_tool_use(&self) -> Option<&ServerToolUseBlock> {
        match self {
            ContentBlock::ServerToolUse(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner WebSearchToolResultBlock if this is a WebSearchToolResult variant,
    /// or None otherwise.
    pub fn as_web_search_tool_result(&self) -> Option<&WebSearchToolResultBlock> {
        match self {
            ContentBlock::WebSearchToolResult(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner ToolResultBlock if this is a ToolResult variant,
    /// or None otherwise.
    pub fn as_tool_result(&self) -> Option<&ToolResultBlock> {
        match self {
            ContentBlock::ToolResult(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner DocumentBlock if this is a Document variant,
    /// or None otherwise.
    pub fn as_document(&self) -> Option<&DocumentBlock> {
        match self {
            ContentBlock::Document(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner ThinkingBlock if this is a Thinking variant,
    /// or None otherwise.
    pub fn as_thinking(&self) -> Option<&ThinkingBlock> {
        match self {
            ContentBlock::Thinking(block) => Some(block),
            _ => None,
        }
    }

    /// Returns a reference to the inner RedactedThinkingBlock if this is a RedactedThinking variant,
    /// or None otherwise.
    pub fn as_redacted_thinking(&self) -> Option<&RedactedThinkingBlock> {
        match self {
            ContentBlock::RedactedThinking(block) => Some(block),
            _ => None,
        }
    }

    /// Returns true if this block is a code execution result block
    pub fn is_code_execution_result(&self) -> bool {
        matches!(self, ContentBlock::CodeExecutionResult(_))
    }

    /// Returns a reference to the inner CodeExecutionResultBlock if this is a CodeExecutionResult variant,
    /// or None otherwise.
    pub fn as_code_execution_result(&self) -> Option<&CodeExecutionResultBlock> {
        match self {
            ContentBlock::CodeExecutionResult(block) => Some(block),
            _ => None,
        }
    }

    /// Returns true if this block is a programmatic tool use block
    pub fn is_programmatic_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ProgrammaticToolUse(_))
    }

    /// Returns a reference to the inner ProgrammaticToolUseBlock if this is a ProgrammaticToolUse variant,
    /// or None otherwise.
    pub fn as_programmatic_tool_use(&self) -> Option<&ProgrammaticToolUseBlock> {
        match self {
            ContentBlock::ProgrammaticToolUse(block) => Some(block),
            _ => None,
        }
    }
}

/// Helper methods to create ContentBlock variants
impl From<TextBlock> for ContentBlock {
    fn from(block: TextBlock) -> Self {
        ContentBlock::Text(block)
    }
}

impl From<ImageBlock> for ContentBlock {
    fn from(block: ImageBlock) -> Self {
        ContentBlock::Image(block)
    }
}

impl From<ToolUseBlock> for ContentBlock {
    fn from(block: ToolUseBlock) -> Self {
        ContentBlock::ToolUse(block)
    }
}

impl From<ServerToolUseBlock> for ContentBlock {
    fn from(block: ServerToolUseBlock) -> Self {
        ContentBlock::ServerToolUse(block)
    }
}

impl From<WebSearchToolResultBlock> for ContentBlock {
    fn from(block: WebSearchToolResultBlock) -> Self {
        ContentBlock::WebSearchToolResult(block)
    }
}

impl From<ToolResultBlock> for ContentBlock {
    fn from(block: ToolResultBlock) -> Self {
        ContentBlock::ToolResult(block)
    }
}

impl From<DocumentBlock> for ContentBlock {
    fn from(block: DocumentBlock) -> Self {
        ContentBlock::Document(block)
    }
}

impl From<ThinkingBlock> for ContentBlock {
    fn from(block: ThinkingBlock) -> Self {
        ContentBlock::Thinking(block)
    }
}

impl From<RedactedThinkingBlock> for ContentBlock {
    fn from(block: RedactedThinkingBlock) -> Self {
        ContentBlock::RedactedThinking(block)
    }
}

impl From<CodeExecutionResultBlock> for ContentBlock {
    fn from(block: CodeExecutionResultBlock) -> Self {
        ContentBlock::CodeExecutionResult(block)
    }
}

impl From<ProgrammaticToolUseBlock> for ContentBlock {
    fn from(block: ProgrammaticToolUseBlock) -> Self {
        ContentBlock::ProgrammaticToolUse(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_serialization() {
        let text_block = TextBlock::new("This is some text content.");
        let content_block = ContentBlock::from(text_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let expected = r#"{"type":"text","text":"This is some text content."}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn tool_use_block_serialization() {
        let input_json = serde_json::json!({
            "query": "weather in San Francisco",
            "limit": 5
        });

        let tool_block = ToolUseBlock::new("tool_123", "search", input_json);
        let content_block = ContentBlock::from(tool_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&json).unwrap();
        let expected: serde_json::Value = serde_json::json!({
            "type": "tool_use",
            "id": "tool_123",
            "input": {"limit": 5, "query": "weather in San Francisco"},
            "name": "search"
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn server_tool_use_block_serialization() {
        let server_block =
            ServerToolUseBlock::new_web_search("tool_123", "weather in San Francisco");
        let content_block = ContentBlock::from(server_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let expected = r#"{"type":"server_tool_use","id":"tool_123","input":{"query":"weather in San Francisco"},"name":"web_search"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn thinking_block_serialization() {
        let thinking_block = ThinkingBlock::new(
            "Let me think through this problem step by step...",
            "abc123signature",
        );
        let content_block = ContentBlock::from(thinking_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let expected = r#"{"type":"thinking","signature":"abc123signature","thinking":"Let me think through this problem step by step..."}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn redacted_thinking_block_serialization() {
        let redacted_block = RedactedThinkingBlock::new("encoded-thinking-data-123");
        let content_block = ContentBlock::from(redacted_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let expected = r#"{"type":"redacted_thinking","data":"encoded-thinking-data-123"}"#;

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"text":"This is some text content.","type":"text"}"#;
        let content_block: ContentBlock = serde_json::from_str(json).unwrap();

        assert!(content_block.is_text());
        assert!(!content_block.is_tool_use());

        if let ContentBlock::Text(block) = content_block {
            assert_eq!(block.text, "This is some text content.");
        } else {
            panic!("Expected TextBlock");
        }

        let json = r#"{"id":"tool_123","input":{"query":"weather in San Francisco"},"name":"web_search","type":"server_tool_use"}"#;
        let content_block: ContentBlock = serde_json::from_str(json).unwrap();

        assert!(content_block.is_server_tool_use());
        assert!(!content_block.is_text());

        if let ContentBlock::ServerToolUse(block) = content_block {
            assert_eq!(block.id, "tool_123");
            assert_eq!(block.name, "web_search");
        } else {
            panic!("Expected ServerToolUseBlock");
        }
    }

    #[test]
    fn as_methods() {
        let text_block = TextBlock::new("This is some text content.");
        let content_block = ContentBlock::from(text_block);

        assert!(content_block.as_text().is_some());
        assert!(content_block.as_image().is_none());
        assert!(content_block.as_tool_use().is_none());
        assert!(content_block.as_server_tool_use().is_none());
        assert!(content_block.as_web_search_tool_result().is_none());
        assert!(content_block.as_tool_result().is_none());
        assert!(content_block.as_document().is_none());
        assert!(content_block.as_thinking().is_none());
        assert!(content_block.as_redacted_thinking().is_none());

        let text_ref = content_block.as_text().unwrap();
        assert_eq!(text_ref.text, "This is some text content.");
    }

    #[test]
    fn image_block_serialization() {
        let image_source =
            crate::types::UrlImageSource::new("https://example.com/image.jpg".to_string());
        let image_block = ImageBlock::new_with_url(image_source);
        let content_block = ContentBlock::from(image_block);

        let json = serde_json::to_string(&content_block).unwrap();
        let expected =
            r#"{"type":"image","source":{"type":"url","url":"https://example.com/image.jpg"}}"#;

        assert_eq!(json, expected);
    }
}
