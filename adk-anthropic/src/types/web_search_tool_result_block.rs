use serde::{Deserialize, Serialize};

use crate::types::{CacheControlEphemeral, WebSearchToolResultBlockContent};

/// A block containing the results of a web search tool operation.
///
/// WebSearchToolResultBlock contains either a list of search results or an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename = "web_search_tool_result")]
pub struct WebSearchToolResultBlock {
    /// The content of the web search tool result.
    pub content: WebSearchToolResultBlockContent,

    /// The ID of the tool use that this result is for.
    pub tool_use_id: String,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl WebSearchToolResultBlock {
    /// Creates a new WebSearchToolResultBlock.
    pub fn new<S: Into<String>>(content: WebSearchToolResultBlockContent, tool_use_id: S) -> Self {
        Self { content, tool_use_id: tool_use_id.into(), cache_control: None }
    }

    /// Creates a new WebSearchToolResultBlock with results.
    pub fn new_with_results<S: Into<String>>(
        results: Vec<crate::types::WebSearchResultBlock>,
        tool_use_id: S,
    ) -> Self {
        Self::new(WebSearchToolResultBlockContent::with_results(results), tool_use_id)
    }

    /// Creates a new WebSearchToolResultBlock with an error.
    pub fn new_with_error<S: Into<String>>(
        error: crate::types::WebSearchToolResultError,
        tool_use_id: S,
    ) -> Self {
        Self::new(WebSearchToolResultBlockContent::with_error(error), tool_use_id)
    }

    /// Add a cache control to this web search tool result block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Returns true if the web search result contains successful results.
    pub fn has_results(&self) -> bool {
        self.content.is_results()
    }

    /// Returns true if the web search result contains an error.
    pub fn has_error(&self) -> bool {
        self.content.is_error()
    }

    /// Returns the number of search results, or 0 if this is an error result.
    pub fn result_count(&self) -> usize {
        self.content.result_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{WebSearchErrorCode, WebSearchResultBlock, WebSearchToolResultError};
    use serde_json::Value;

    #[test]
    fn results_serialization() {
        let results = vec![
            WebSearchResultBlock::new(
                "encrypted-data-1",
                "Example Page 1",
                "https://example.com/page1",
            )
            .with_page_age("2 days ago".to_string()),
        ];

        let content = WebSearchToolResultBlockContent::with_results(results);
        let block = WebSearchToolResultBlock::new(content, "tool-123");

        let json = serde_json::to_string(&block).unwrap();

        // Parse both the actual and expected JSON to Values for comparison
        // This avoids issues with key ordering
        let actual: Value = serde_json::from_str(&json).unwrap();
        let expected: Value = serde_json::from_str(
            r#"{"type":"web_search_tool_result","content":[{"type":"web_search_result","encrypted_content":"encrypted-data-1","page_age":"2 days ago","title":"Example Page 1","url":"https://example.com/page1"}],"tool_use_id":"tool-123"}"#
        ).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn error_serialization() {
        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let content = WebSearchToolResultBlockContent::with_error(error);
        let block = WebSearchToolResultBlock::new(content, "tool-123");

        let json = serde_json::to_string(&block).unwrap();

        // Parse both the actual and expected JSON to Values for comparison
        let actual: Value = serde_json::from_str(&json).unwrap();
        let expected: Value = serde_json::from_str(
            r#"{"type":"web_search_tool_result","content":{"error_code":"invalid_tool_input"},"tool_use_id":"tool-123"}"#
        ).unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn deserialization() {
        let json = r#"{"content":[{"type":"web_search_result","encrypted_content":"encrypted-data-1","page_age":"2 days ago","title":"Example Page 1","url":"https://example.com/page1"}],"tool_use_id":"tool-123","type":"web_search_tool_result"}"#;
        let block: WebSearchToolResultBlock = serde_json::from_str(json).unwrap();

        assert_eq!(block.tool_use_id, "tool-123");
        assert!(block.has_results());
        assert!(!block.has_error());
        assert_eq!(block.result_count(), 1);
    }

    #[test]
    fn new_with_results() {
        let results = vec![
            WebSearchResultBlock::new(
                "encrypted-data-1",
                "Example Page 1",
                "https://example.com/page1",
            )
            .with_page_age("2 days ago".to_string()),
        ];

        let block = WebSearchToolResultBlock::new_with_results(results, "tool-123");

        assert_eq!(block.tool_use_id, "tool-123");
        assert!(block.has_results());
        assert!(!block.has_error());
        assert_eq!(block.result_count(), 1);
        assert!(block.cache_control.is_none());
    }

    #[test]
    fn new_with_error() {
        let error = WebSearchToolResultError { error_code: WebSearchErrorCode::InvalidToolInput };

        let block = WebSearchToolResultBlock::new_with_error(error, "tool-123");

        assert_eq!(block.tool_use_id, "tool-123");
        assert!(!block.has_results());
        assert!(block.has_error());
        assert_eq!(block.result_count(), 0);
        assert!(block.cache_control.is_none());
    }

    #[test]
    fn with_cache_control() {
        let results = vec![
            WebSearchResultBlock::new(
                "encrypted-data-1",
                "Example Page 1",
                "https://example.com/page1",
            )
            .with_page_age("2 days ago".to_string()),
        ];

        let cache_control = CacheControlEphemeral::new();
        let block = WebSearchToolResultBlock::new_with_results(results, "tool-123")
            .with_cache_control(cache_control);

        assert_eq!(block.tool_use_id, "tool-123");
        assert!(block.has_results());
        assert!(block.cache_control.is_some());
    }
}
