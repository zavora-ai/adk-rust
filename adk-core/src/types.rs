use serde::{Deserialize, Serialize};

/// Maximum allowed size for inline binary data (10 MB).
/// Prevents accidental or malicious embedding of oversized payloads in Content parts.
pub const MAX_INLINE_DATA_SIZE: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionResponseData {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Text {
        text: String,
    },
    InlineData {
        mime_type: String,
        data: Vec<u8>,
    },
    /// Base64-encoded inline data.
    ///
    /// This keeps canonical base64 payloads intact through request processing so
    /// provider adapters can forward them without decode/re-encode overhead.
    InlineDataBase64 {
        mime_type: String,
        data_base64: String,
    },
    /// File data referenced by URI (URL or cloud storage path).
    ///
    /// This allows referencing external files without embedding the data inline.
    /// Providers that don't support URI-based content can fetch and convert to InlineData.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Part;
    ///
    /// let image_url = Part::FileData {
    ///     mime_type: "image/jpeg".to_string(),
    ///     file_uri: "https://example.com/image.jpg".to_string(),
    /// };
    /// ```
    FileData {
        /// MIME type of the file (e.g., "image/jpeg", "audio/wav")
        mime_type: String,
        /// URI to the file (URL, gs://, etc.)
        file_uri: String,
    },
    FunctionCall {
        name: String,
        args: serde_json::Value,
        /// Tool call ID for OpenAI-style providers. None for Gemini.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    FunctionResponse {
        function_response: FunctionResponseData,
        /// Tool call ID for OpenAI-style providers. None for Gemini.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

impl Content {
    pub fn new(role: impl Into<String>) -> Self {
        Self { role: role.into(), parts: Vec::new() }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.parts.push(Part::Text { text: text.into() });
        self
    }

    /// Add inline binary data (e.g., image bytes).
    ///
    /// # Panics
    /// Panics if `data` exceeds [`MAX_INLINE_DATA_SIZE`] (10 MB).
    pub fn with_inline_data(mut self, mime_type: impl Into<String>, data: Vec<u8>) -> Self {
        assert!(
            data.len() <= MAX_INLINE_DATA_SIZE,
            "Inline data size {} exceeds maximum allowed size of {} bytes",
            data.len(),
            MAX_INLINE_DATA_SIZE
        );
        self.parts.push(Part::InlineData { mime_type: mime_type.into(), data });
        self
    }

    /// Add a file reference by URI (URL or cloud storage path).
    pub fn with_file_uri(
        mut self,
        mime_type: impl Into<String>,
        file_uri: impl Into<String>,
    ) -> Self {
        self.parts.push(Part::FileData { mime_type: mime_type.into(), file_uri: file_uri.into() });
        self
    }
}

impl Part {
    /// Returns the text content if this is a Text part, None otherwise
    pub fn text(&self) -> Option<&str> {
        match self {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        }
    }

    /// Returns the MIME type if this part has one (InlineData or FileData)
    pub fn mime_type(&self) -> Option<&str> {
        match self {
            Part::InlineData { mime_type, .. } => Some(mime_type.as_str()),
            Part::InlineDataBase64 { mime_type, .. } => Some(mime_type.as_str()),
            Part::FileData { mime_type, .. } => Some(mime_type.as_str()),
            _ => None,
        }
    }

    /// Returns the file URI if this is a FileData part
    pub fn file_uri(&self) -> Option<&str> {
        match self {
            Part::FileData { file_uri, .. } => Some(file_uri.as_str()),
            _ => None,
        }
    }

    /// Returns true if this part contains media (image, audio, video)
    pub fn is_media(&self) -> bool {
        matches!(
            self,
            Part::InlineData { .. } | Part::InlineDataBase64 { .. } | Part::FileData { .. }
        )
    }

    /// Returns base64 payload for inline data parts when available.
    ///
    /// `InlineDataBase64` is returned as-is to preserve passthrough payloads.
    pub fn inline_base64(&self) -> Option<&str> {
        match self {
            Part::InlineDataBase64 { data_base64, .. } => Some(data_base64.as_str()),
            _ => None,
        }
    }

    /// Returns raw bytes for inline data parts when available.
    ///
    /// Consumers that need bytes can use this and only decode base64 in their own
    /// path when they receive `InlineDataBase64`.
    pub fn inline_bytes(&self) -> Option<&[u8]> {
        match self {
            Part::InlineData { data, .. } => Some(data.as_slice()),
            _ => None,
        }
    }

    /// Create a new text part
    pub fn text_part(text: impl Into<String>) -> Self {
        Part::Text { text: text.into() }
    }

    /// Create a new inline data part
    ///
    /// # Panics
    /// Panics if `data` exceeds [`MAX_INLINE_DATA_SIZE`] (10 MB).
    pub fn inline_data(mime_type: impl Into<String>, data: Vec<u8>) -> Self {
        assert!(
            data.len() <= MAX_INLINE_DATA_SIZE,
            "Inline data size {} exceeds maximum allowed size of {} bytes",
            data.len(),
            MAX_INLINE_DATA_SIZE
        );
        Part::InlineData { mime_type: mime_type.into(), data }
    }

    /// Create a new base64 inline data part.
    pub fn inline_data_base64(
        mime_type: impl Into<String>,
        data_base64: impl Into<String>,
    ) -> Self {
        Part::InlineDataBase64 { mime_type: mime_type.into(), data_base64: data_base64.into() }
    }

    /// Create a new file data part from URI
    pub fn file_data(mime_type: impl Into<String>, file_uri: impl Into<String>) -> Self {
        Part::FileData { mime_type: mime_type.into(), file_uri: file_uri.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_creation() {
        let content = Content::new("user").with_text("Hello");
        assert_eq!(content.role, "user");
        assert_eq!(content.parts.len(), 1);
    }

    #[test]
    fn test_content_with_inline_data() {
        let content = Content::new("user")
            .with_text("Check this image")
            .with_inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]);
        assert_eq!(content.parts.len(), 2);
        assert!(
            matches!(&content.parts[1], Part::InlineData { mime_type, .. } if mime_type == "image/png")
        );
    }

    #[test]
    fn test_content_with_file_uri() {
        let content = Content::new("user")
            .with_text("Check this image")
            .with_file_uri("image/jpeg", "https://example.com/image.jpg");
        assert_eq!(content.parts.len(), 2);
        assert!(
            matches!(&content.parts[1], Part::FileData { file_uri, .. } if file_uri == "https://example.com/image.jpg")
        );
    }

    #[test]
    fn test_part_serialization() {
        let part = Part::Text { text: "test".to_string() };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("test"));
    }

    #[test]
    fn test_part_file_data_serialization() {
        let part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com/image.jpg".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("image/jpeg"));
        assert!(json.contains("https://example.com/image.jpg"));
    }

    #[test]
    fn test_part_text_accessor() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert_eq!(text_part.text(), Some("hello"));

        let data_part = Part::InlineData { mime_type: "image/png".to_string(), data: vec![] };
        assert_eq!(data_part.text(), None);
    }

    #[test]
    fn test_part_mime_type_accessor() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert_eq!(text_part.mime_type(), None);

        let inline_part = Part::InlineData { mime_type: "image/png".to_string(), data: vec![] };
        assert_eq!(inline_part.mime_type(), Some("image/png"));

        let file_part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com".to_string(),
        };
        assert_eq!(file_part.mime_type(), Some("image/jpeg"));

        let inline_base64_part = Part::InlineDataBase64 {
            mime_type: "application/pdf".to_string(),
            data_base64: "JVBERi0=".to_string(),
        };
        assert_eq!(inline_base64_part.mime_type(), Some("application/pdf"));
    }

    #[test]
    fn test_part_file_uri_accessor() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert_eq!(text_part.file_uri(), None);

        let file_part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com/img.jpg".to_string(),
        };
        assert_eq!(file_part.file_uri(), Some("https://example.com/img.jpg"));
    }

    #[test]
    fn test_part_is_media() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert!(!text_part.is_media());

        let inline_part = Part::InlineData { mime_type: "image/png".to_string(), data: vec![] };
        assert!(inline_part.is_media());

        let file_part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com".to_string(),
        };
        assert!(file_part.is_media());

        let inline_base64_part = Part::InlineDataBase64 {
            mime_type: "application/pdf".to_string(),
            data_base64: "JVBERi0=".to_string(),
        };
        assert!(inline_base64_part.is_media());
    }

    #[test]
    fn test_part_inline_base64_accessor() {
        let inline_base64_part = Part::InlineDataBase64 {
            mime_type: "application/pdf".to_string(),
            data_base64: "JVBERi0=".to_string(),
        };
        assert_eq!(inline_base64_part.inline_base64(), Some("JVBERi0="));
        assert_eq!(inline_base64_part.inline_bytes(), None);

        let inline_part =
            Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x89, 0x50] };
        assert_eq!(inline_part.inline_base64(), None);
        assert_eq!(inline_part.inline_bytes(), Some([0x89, 0x50].as_slice()));
    }

    #[test]
    fn test_part_constructors() {
        let text = Part::text_part("hello");
        assert!(matches!(text, Part::Text { text } if text == "hello"));

        let inline = Part::inline_data("image/png", vec![1, 2, 3]);
        assert!(
            matches!(inline, Part::InlineData { mime_type, data } if mime_type == "image/png" && data == vec![1, 2, 3])
        );

        let file = Part::file_data("image/jpeg", "https://example.com/img.jpg");
        assert!(
            matches!(file, Part::FileData { mime_type, file_uri } if mime_type == "image/jpeg" && file_uri == "https://example.com/img.jpg")
        );

        let inline_base64 = Part::inline_data_base64("application/pdf", "JVBERi0=");
        assert!(
            matches!(inline_base64, Part::InlineDataBase64 { mime_type, data_base64 } if mime_type == "application/pdf" && data_base64 == "JVBERi0=")
        );
    }

    #[test]
    fn test_inline_data_within_limit() {
        // Should succeed: small data
        let data = vec![0u8; 1024];
        let content = Content::new("user").with_inline_data("image/png", data);
        assert_eq!(content.parts.len(), 1);
    }

    #[test]
    fn test_inline_data_at_limit() {
        // Should succeed: exactly at limit
        let data = vec![0u8; MAX_INLINE_DATA_SIZE];
        let part = Part::inline_data("image/png", data);
        assert!(part.is_media());
    }

    #[test]
    #[should_panic(expected = "exceeds maximum allowed size")]
    fn test_inline_data_exceeds_limit_content() {
        let data = vec![0u8; MAX_INLINE_DATA_SIZE + 1];
        let _ = Content::new("user").with_inline_data("image/png", data);
    }

    #[test]
    #[should_panic(expected = "exceeds maximum allowed size")]
    fn test_inline_data_exceeds_limit_part() {
        let data = vec![0u8; MAX_INLINE_DATA_SIZE + 1];
        let _ = Part::inline_data("image/png", data);
    }
}
