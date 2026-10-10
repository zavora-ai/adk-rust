use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{CacheControlEphemeral, WebFetchToolResultBlockContent};

/// A block containing the result of a web fetch tool operation.
///
/// WebFetchToolResultBlock contains either a successfully fetched document or an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename = "web_fetch_tool_result")]
pub struct WebFetchToolResultBlock {
    /// The content of the web fetch tool result.
    pub content: WebFetchToolResultBlockContent,

    /// The ID of the tool use that this result is for.
    pub tool_use_id: String,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Who made the call, as sent by the API: `{"type": "direct"}`, or
    /// `{"type": "code_execution_20260120", "tool_id": "srvtoolu_..."}` for a call
    /// made from code execution during dynamic filtering. Kept verbatim so replayed
    /// history matches the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<Value>,
}

impl WebFetchToolResultBlock {
    /// Creates a new WebFetchToolResultBlock.
    pub fn new<S: Into<String>>(content: WebFetchToolResultBlockContent, tool_use_id: S) -> Self {
        Self { content, tool_use_id: tool_use_id.into(), cache_control: None, caller: None }
    }

    /// Add a cache control to this web fetch tool result block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Returns true if the web fetch result contains a successful result.
    pub fn has_result(&self) -> bool {
        self.content.is_result()
    }

    /// Returns true if the web fetch result contains an error.
    pub fn has_error(&self) -> bool {
        self.content.is_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        DocumentBlock, DocumentSource, PlainTextSource, WebFetchErrorCode, WebFetchResultContent,
        WebFetchToolResultError,
    };
    use serde_json::Value;

    fn make_text_doc(text: &str) -> DocumentBlock {
        DocumentBlock::new(DocumentSource::PlainText(PlainTextSource::new(text.to_string())))
    }

    /// The response blocks shown in the web fetch tool documentation: a text page with a
    /// title and citations, a PDF, and an error.
    fn documented_blocks() -> Vec<Value> {
        vec![
            serde_json::json!({
                "type": "web_fetch_tool_result",
                "tool_use_id": "srvtoolu_01234567890abcdef",
                "content": {
                    "type": "web_fetch_result",
                    "url": "https://example.com/article",
                    "content": {
                        "type": "document",
                        "source": {
                            "type": "text",
                            "media_type": "text/plain",
                            "data": "Full text content of the article..."
                        },
                        "title": "Article Title",
                        "citations": {"enabled": true}
                    },
                    "retrieved_at": "2025-08-25T10:30:00Z"
                }
            }),
            serde_json::json!({
                "type": "web_fetch_tool_result",
                "tool_use_id": "srvtoolu_02",
                "content": {
                    "type": "web_fetch_result",
                    "url": "https://example.com/paper.pdf",
                    "content": {
                        "type": "document",
                        "source": {
                            "type": "base64",
                            "media_type": "application/pdf",
                            "data": "JVBERi0xLjQKJcOkw7zDtsOfCjIgMCBvYmo..."
                        },
                        "citations": {"enabled": true}
                    },
                    "retrieved_at": "2025-08-25T10:30:02Z"
                }
            }),
            serde_json::json!({
                "type": "web_fetch_tool_result",
                "tool_use_id": "srvtoolu_a93jad",
                "content": {
                    "type": "web_fetch_tool_result_error",
                    "error_code": "url_not_accessible"
                }
            }),
        ]
    }

    #[test]
    fn documented_response_blocks_round_trip_losslessly() {
        for wire in documented_blocks() {
            let block: crate::types::ContentBlock = serde_json::from_value(wire.clone()).unwrap();
            assert!(block.is_web_fetch_tool_result(), "{wire}");
            assert_eq!(serde_json::to_value(&block).unwrap(), wire);
            let text = serde_json::to_string(&block).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), wire);
        }
    }

    #[test]
    fn documented_error_keeps_its_code() {
        let block: WebFetchToolResultBlock =
            serde_json::from_value(documented_blocks().remove(2)).unwrap();
        let error = block.content.as_error().unwrap();
        assert!(error.is_url_not_accessible());
        assert!(!error.is_unknown());
    }

    #[test]
    fn caller_round_trips() {
        let caller =
            serde_json::json!({"type": "code_execution_20260120", "tool_id": "srvtoolu_exec"});
        let block: WebFetchToolResultBlock = serde_json::from_value(serde_json::json!({
            "type": "web_fetch_tool_result",
            "tool_use_id": "srvtoolu_fetch",
            "content": {"type": "web_fetch_tool_result_error", "error_code": "unavailable"},
            "caller": caller
        }))
        .unwrap();
        let mut expected = WebFetchToolResultBlock::new(
            WebFetchToolResultBlockContent::with_error(WebFetchToolResultError::new(
                WebFetchErrorCode::Unavailable,
            )),
            "srvtoolu_fetch",
        );
        expected.caller = Some(caller);
        assert_eq!(block, expected);
        let replayed: WebFetchToolResultBlock =
            serde_json::from_value(serde_json::to_value(&block).unwrap()).unwrap();
        assert_eq!(replayed, block);
    }

    #[test]
    fn result_serialization() {
        let doc = make_text_doc("page content");
        let result = WebFetchResultContent::new("https://example.com", doc);
        let content = WebFetchToolResultBlockContent::with_result(result);
        let block = WebFetchToolResultBlock::new(content, "tool-123");

        let json = serde_json::to_string(&block).unwrap();
        let actual: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(actual["type"], "web_fetch_tool_result");
        assert_eq!(actual["tool_use_id"], "tool-123");
        assert_eq!(actual["content"]["url"], "https://example.com");
    }

    #[test]
    fn error_serialization() {
        let error = WebFetchToolResultError::new(WebFetchErrorCode::Unavailable);
        let content = WebFetchToolResultBlockContent::with_error(error);
        let block = WebFetchToolResultBlock::new(content, "tool-123");

        let json = serde_json::to_string(&block).unwrap();
        let actual: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(actual["type"], "web_fetch_tool_result");
        assert_eq!(actual["tool_use_id"], "tool-123");
        assert_eq!(actual["content"]["error_code"], "unavailable");
    }

    #[test]
    fn deserialization() {
        let doc = make_text_doc("page content");
        let result = WebFetchResultContent::new("https://example.com", doc);
        let content = WebFetchToolResultBlockContent::with_result(result);
        let block = WebFetchToolResultBlock::new(content, "tool-123");

        let json = serde_json::to_string(&block).unwrap();
        let deserialized: WebFetchToolResultBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.tool_use_id, "tool-123");
        assert!(deserialized.has_result());
        assert!(!deserialized.has_error());
        assert!(deserialized.cache_control.is_none());
    }

    #[test]
    fn with_cache_control() {
        let doc = make_text_doc("page content");
        let result = WebFetchResultContent::new("https://example.com", doc);
        let content = WebFetchToolResultBlockContent::with_result(result);
        let cache_control = CacheControlEphemeral::new();
        let block =
            WebFetchToolResultBlock::new(content, "tool-123").with_cache_control(cache_control);

        assert_eq!(block.tool_use_id, "tool-123");
        assert!(block.has_result());
        assert!(block.cache_control.is_some());
    }
}
