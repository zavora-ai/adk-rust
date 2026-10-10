use serde::{Deserialize, Serialize, Serializer};

use crate::types::{DocumentBlock, WebFetchToolResultError};

/// The content of a successfully fetched URL.
///
/// Carries the fetched document, its source URL, and the retrieval timestamp.
/// Field names match the live API: `content` holds the document, `type` is always
/// `"web_fetch_result"` and is preserved for lossless round-tripping on `pause_turn` resend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebFetchResultContent {
    /// Type discriminator, always `"web_fetch_result"`. Preserved so the block round-trips
    /// faithfully when echoed back to the API on a `pause_turn` continuation.
    #[serde(rename = "type", default = "web_fetch_result_type")]
    r#type: String,

    /// The URL that was fetched.
    pub url: String,

    /// The fetched document content. Named `content` in the API response, where it is a
    /// `document` content block; it serializes with `"type": "document"`, which the API
    /// requires when the block is sent back in conversation history.
    #[serde(rename = "content", serialize_with = "serialize_document_block")]
    pub document: DocumentBlock,

    /// ISO 8601 timestamp of when the URL was fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
}

fn web_fetch_result_type() -> String {
    "web_fetch_result".to_string()
}

fn serialize_document_block<S: Serializer>(
    document: &DocumentBlock,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Tagged<'a> {
        #[serde(rename = "type")]
        block_type: &'static str,
        #[serde(flatten)]
        document: &'a DocumentBlock,
    }
    Tagged { block_type: "document", document }.serialize(serializer)
}

impl WebFetchResultContent {
    /// Creates a new successful fetch result.
    pub fn new(url: impl Into<String>, document: DocumentBlock) -> Self {
        Self { r#type: web_fetch_result_type(), url: url.into(), document, retrieved_at: None }
    }

    /// Sets the retrieval timestamp.
    pub fn with_retrieved_at(mut self, retrieved_at: impl Into<String>) -> Self {
        self.retrieved_at = Some(retrieved_at.into());
        self
    }
}

/// Content of a web fetch tool result.
///
/// This can either be a successful fetch result, an error, or an unrecognised shape from a future
/// API version. The `Unknown` variant acts as a catch-all so deserialization never fails: its
/// `Value` preserves the raw JSON for lossless round-tripping on `pause_turn` resend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WebFetchToolResultBlockContent {
    /// A successfully fetched document. Boxed to keep the enum size manageable.
    Result(Box<WebFetchResultContent>),

    /// An error that occurred during the web fetch.
    Error(WebFetchToolResultError),

    /// An unrecognised content shape (future API versions or undocumented variants). Stored as raw
    /// JSON so the block survives deserialization and round-trips faithfully.
    Unknown(serde_json::Value),
}

impl WebFetchToolResultBlockContent {
    /// Creates a new WebFetchToolResultBlockContent with a successful fetch result.
    pub fn with_result(result: WebFetchResultContent) -> Self {
        Self::Result(Box::new(result))
    }

    /// Creates a new WebFetchToolResultBlockContent with an error.
    pub fn with_error(error: WebFetchToolResultError) -> Self {
        Self::Error(error)
    }

    /// Returns true if the content is a successful result.
    pub fn is_result(&self) -> bool {
        matches!(self, WebFetchToolResultBlockContent::Result(_))
    }

    /// Returns true if the content is an error.
    pub fn is_error(&self) -> bool {
        matches!(self, WebFetchToolResultBlockContent::Error(_))
    }

    /// Returns true if the content is an unrecognised shape.
    pub fn is_unknown(&self) -> bool {
        matches!(self, WebFetchToolResultBlockContent::Unknown(_))
    }

    /// Returns a reference to the fetch result if this is a Result variant, or None otherwise.
    pub fn as_result(&self) -> Option<&WebFetchResultContent> {
        match self {
            WebFetchToolResultBlockContent::Result(result) => Some(result),
            _ => None,
        }
    }

    /// Returns a reference to the error if this is an Error variant, or None otherwise.
    pub fn as_error(&self) -> Option<&WebFetchToolResultError> {
        match self {
            WebFetchToolResultBlockContent::Error(error) => Some(error),
            _ => None,
        }
    }

    /// Returns a reference to the raw JSON value if this is an Unknown variant, or None otherwise.
    pub fn as_unknown(&self) -> Option<&serde_json::Value> {
        match self {
            WebFetchToolResultBlockContent::Unknown(value) => Some(value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PlainTextSource, WebFetchErrorCode};

    fn make_text_doc(text: &str) -> DocumentBlock {
        DocumentBlock::new(crate::types::DocumentSource::PlainText(PlainTextSource::new(
            text.to_string(),
        )))
    }

    #[test]
    fn result_serialization_includes_type_and_content_fields() {
        let doc = make_text_doc("hello world");
        let result = WebFetchResultContent::new("https://example.com", doc)
            .with_retrieved_at("2025-09-10T00:00:00Z");
        let content = WebFetchToolResultBlockContent::with_result(result);
        let json = serde_json::to_string(&content).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "web_fetch_result");
        assert_eq!(value["url"], "https://example.com");
        assert_eq!(value["retrieved_at"], "2025-09-10T00:00:00Z");
        // API field name is "content", not "document"
        assert_eq!(value["content"]["type"], "document");
        assert!(value["document"].is_null());
    }

    #[test]
    fn error_serialization() {
        let error = WebFetchToolResultError::new(WebFetchErrorCode::Unavailable);
        let content = WebFetchToolResultBlockContent::with_error(error);
        let json = serde_json::to_string(&content).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type": "web_fetch_tool_result_error", "error_code": "unavailable"})
        );
    }

    #[test]
    fn error_deserialization() {
        let json = r#"{"error_code":"url_not_allowed"}"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_error());
        assert!(!content.is_result());
        assert!(!content.is_unknown());
    }

    #[test]
    fn live_api_success_shape_deserializes_to_result() {
        // Actual shape observed from the live Anthropic API (2026-06-12). The "content" field
        // holds a DocumentBlock and the outer "type" tag is "web_fetch_result".
        let json = r#"{"type":"web_fetch_result","url":"https://example.com","retrieved_at":"2026-06-12T07:18:15.285763","content":{"type":"document","source":{"type":"text","media_type":"text/plain","data":"page content"}}}"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_result(), "expected Result, got {content:?}");
        assert!(!content.is_error());
        assert!(!content.is_unknown());
        let result = content.as_result().unwrap();
        assert_eq!(result.url, "https://example.com");
    }

    #[test]
    fn live_api_success_shape_round_trips() {
        // Round-trip must preserve both "type" tags so the block can be echoed back verbatim.
        let json = r#"{"type":"web_fetch_result","url":"https://example.com","retrieved_at":"2026-06-12T00:00:00Z","content":{"type":"document","source":{"type":"text","media_type":"text/plain","data":"hello"}}}"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_result());
        assert_eq!(
            serde_json::to_value(&content).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
    }

    #[test]
    fn stored_result_without_document_type_gains_it_on_replay() {
        // History persisted before the document tag was serialized still deserializes and
        // is replayed in the shape the API requires.
        let json = r#"{"type":"web_fetch_result","url":"https://example.com","content":{"source":{"type":"text","media_type":"text/plain","data":"hello"}}}"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_result());
        assert_eq!(serde_json::to_value(&content).unwrap()["content"]["type"], "document");
    }

    #[test]
    fn result_deserialization_roundtrip() {
        let doc = make_text_doc("page content");
        let result = WebFetchResultContent::new("https://example.com", doc);
        let serialized = serde_json::to_string(&result).unwrap();
        let deserialized: WebFetchToolResultBlockContent =
            serde_json::from_str(&serialized).unwrap();
        assert!(deserialized.is_result());
        assert!(!deserialized.is_error());
        assert!(!deserialized.is_unknown());
    }

    #[test]
    fn unknown_shape_deserializes_to_unknown_variant() {
        // An unrecognised content shape must not cause a hard deserialization failure.
        let json = r#"[{"type":"text","text":"fetched page content"}]"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_unknown());
        assert!(!content.is_result());
        assert!(!content.is_error());
    }

    #[test]
    fn unknown_variant_round_trips_raw_json() {
        let json = r#"{"some_future_field":"value"}"#;
        let content: WebFetchToolResultBlockContent = serde_json::from_str(json).unwrap();
        assert!(content.is_unknown());
        let serialized = serde_json::to_string(&content).unwrap();
        let round_tripped: WebFetchToolResultBlockContent =
            serde_json::from_str(&serialized).unwrap();
        assert_eq!(content, round_tripped);
    }
}
