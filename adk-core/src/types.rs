use serde::{Deserialize, Serialize};

/// Maximum allowed size for inline binary data (10 MB).
/// Prevents accidental or malicious embedding of oversized payloads in Content parts.
pub const MAX_INLINE_DATA_SIZE: usize = 10 * 1024 * 1024;

/// Data part for inline binary content in a function response.
///
/// Carries a MIME type and raw binary payload for images, audio, PDFs, etc.
///
/// # Example
///
/// ```rust
/// use adk_core::InlineDataPart;
///
/// let part = InlineDataPart {
///     mime_type: "image/png".to_string(),
///     data: vec![0x89, 0x50, 0x4E, 0x47],
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineDataPart {
    /// MIME type of the inline data (e.g., "image/png", "audio/wav").
    pub mime_type: String,
    /// Raw binary data.
    pub data: Vec<u8>,
}

/// Data part for file references in a function response.
///
/// Carries a MIME type and URI (URL or cloud storage path) for external files.
///
/// # Example
///
/// ```rust
/// use adk_core::FileDataPart;
///
/// let part = FileDataPart {
///     mime_type: "application/pdf".to_string(),
///     file_uri: "gs://my-bucket/report.pdf".to_string(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDataPart {
    /// MIME type of the file (e.g., "application/pdf").
    pub mime_type: String,
    /// URI to the file (URL, gs://, etc.).
    pub file_uri: String,
}

/// Data for a function (tool) response, including optional multimodal parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionResponseData {
    /// Name of the function that produced this response.
    pub name: String,
    /// The JSON response payload from the function.
    pub response: serde_json::Value,
    /// Optional inline binary data parts (images, audio, PDFs).
    /// Each part is validated against [`MAX_INLINE_DATA_SIZE`] on construction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_data: Vec<InlineDataPart>,
    /// Optional file data references (URIs to external files).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_data: Vec<FileDataPart>,
}

impl FunctionResponseData {
    /// Create with JSON response only (backward-compatible).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::FunctionResponseData;
    ///
    /// let frd = FunctionResponseData::new("my_tool", serde_json::json!({"status": "ok"}));
    /// assert!(frd.inline_data.is_empty());
    /// assert!(frd.file_data.is_empty());
    /// ```
    pub fn new(name: impl Into<String>, response: serde_json::Value) -> Self {
        Self { name: name.into(), response, inline_data: Vec::new(), file_data: Vec::new() }
    }

    /// Create with JSON response and inline data parts.
    ///
    /// # Panics
    ///
    /// Panics if any inline data part exceeds [`MAX_INLINE_DATA_SIZE`] (10 MB).
    pub fn with_inline_data(
        name: impl Into<String>,
        response: serde_json::Value,
        inline_data: Vec<InlineDataPart>,
    ) -> Self {
        for part in &inline_data {
            assert!(
                part.data.len() <= MAX_INLINE_DATA_SIZE,
                "Inline data size {} exceeds maximum allowed size of {MAX_INLINE_DATA_SIZE} bytes",
                part.data.len(),
            );
        }
        Self { name: name.into(), response, inline_data, file_data: Vec::new() }
    }

    /// Create with JSON response and file data references.
    pub fn with_file_data(
        name: impl Into<String>,
        response: serde_json::Value,
        file_data: Vec<FileDataPart>,
    ) -> Self {
        Self { name: name.into(), response, inline_data: Vec::new(), file_data }
    }

    /// Create with JSON response, inline data, and file data.
    ///
    /// # Panics
    ///
    /// Panics if any inline data part exceeds [`MAX_INLINE_DATA_SIZE`] (10 MB).
    pub fn with_multimodal(
        name: impl Into<String>,
        response: serde_json::Value,
        inline_data: Vec<InlineDataPart>,
        file_data: Vec<FileDataPart>,
    ) -> Self {
        for part in &inline_data {
            assert!(
                part.data.len() <= MAX_INLINE_DATA_SIZE,
                "Inline data size {} exceeds maximum allowed size of {MAX_INLINE_DATA_SIZE} bytes",
                part.data.len(),
            );
        }
        Self { name: name.into(), response, inline_data, file_data }
    }

    /// Construct from a tool's return value, preserving multimodal parts if present.
    ///
    /// If `value` is a JSON object containing `inline_data` or `file_data` arrays
    /// (matching the `FunctionResponseData` schema), the multimodal parts are extracted
    /// and the `response` field is used as the JSON payload. Otherwise, the entire
    /// `value` is used as a plain JSON response (backward-compatible).
    ///
    /// This allows tools to return multimodal data by including `inline_data` and/or
    /// `file_data` in their JSON return value.
    pub fn from_tool_result(name: impl Into<String>, value: serde_json::Value) -> Self {
        let name = name.into();
        // Check if the value has multimodal fields
        if let serde_json::Value::Object(ref map) = value {
            let has_inline = map.get("inline_data").is_some_and(|v| v.is_array());
            let has_file = map.get("file_data").is_some_and(|v| v.is_array());
            if has_inline || has_file {
                // Try to deserialize the multimodal parts
                let inline_data: Vec<InlineDataPart> = map
                    .get("inline_data")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let file_data: Vec<FileDataPart> = map
                    .get("file_data")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                if !inline_data.is_empty() || !file_data.is_empty() {
                    // Extract the response field, or use the remaining fields as response
                    let response = map.get("response").cloned().unwrap_or_else(|| {
                        let mut clean = map.clone();
                        clean.remove("inline_data");
                        clean.remove("file_data");
                        clean.remove("name");
                        serde_json::Value::Object(clean)
                    });
                    return Self { name, response, inline_data, file_data };
                }
            }
        }
        // Fallback: plain JSON response
        Self::new(name, value)
    }
}

/// A message in a conversation, consisting of a role and content parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    /// The role of this content's author (e.g., "user", "model", "function").
    pub role: String,
    /// The parts that make up this content (text, data, function calls, etc.).
    pub parts: Vec<Part>,
}

/// A single part of a [`Content`] message.
///
/// Parts can be text, binary data, file references, function calls/responses,
/// thinking traces, or server-side tool interactions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    /// Thinking/reasoning trace from a thinking-capable model.
    ///
    /// Must be placed before `Text` in the enum so that `#[serde(untagged)]`
    /// deserialization matches `{"thinking": "..."}` before falling through to `Text`.
    Thinking {
        /// The reasoning/thinking text.
        thinking: String,
        /// Optional cryptographic signature for thought verification.
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Plain text content.
    Text {
        /// The text content.
        text: String,
    },
    /// Inline binary data (images, audio, etc.).
    InlineData {
        /// MIME type of the data.
        mime_type: String,
        /// Raw binary data.
        data: Vec<u8>,
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
    /// A function (tool) call from the model.
    FunctionCall {
        /// Name of the function to call.
        name: String,
        /// Arguments as a JSON value.
        args: serde_json::Value,
        /// Tool call ID for OpenAI-style providers. None for Gemini.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Thought signature for Gemini 3 series models.
        /// Must be preserved and relayed back in conversation history
        /// during multi-turn function calling.
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    /// A function (tool) response.
    #[serde(rename_all = "camelCase")]
    FunctionResponse {
        /// The function response data.
        function_response: FunctionResponseData,
        /// Tool call ID for OpenAI-style providers. None for Gemini.
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// Server-side tool call data from Gemini 3 built-in tool invocations.
    /// Stored as opaque JSON to avoid coupling core types to provider-specific schemas.
    ServerToolCall {
        /// Opaque JSON payload for the server tool call.
        server_tool_call: serde_json::Value,
    },
    /// Server-side tool response data from Gemini 3 built-in tool invocations.
    /// Stored as opaque JSON to avoid coupling core types to provider-specific schemas.
    ServerToolResponse {
        /// Opaque JSON payload for the server tool response.
        server_tool_response: serde_json::Value,
    },
}

impl Content {
    /// Returns `true` if any part of this content is a function (tool) call.
    ///
    /// Useful for deciding response semantics — e.g. a model turn that emits
    /// tool calls is **not** complete, since tool results must still be
    /// processed and sent back.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Content, Part};
    ///
    /// let mut content = Content::new("model").with_text("calling a tool");
    /// assert!(!content.has_function_calls());
    ///
    /// content.parts.push(Part::FunctionCall {
    ///     name: "get_weather".to_string(),
    ///     args: serde_json::json!({}),
    ///     id: None,
    ///     thought_signature: None,
    /// });
    /// assert!(content.has_function_calls());
    /// ```
    pub fn has_function_calls(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::FunctionCall { .. }))
    }

    /// Creates a new empty content with the given role.
    pub fn new(role: impl Into<String>) -> Self {
        Self { role: role.into(), parts: Vec::new() }
    }

    /// Appends a text part to this content.
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

    /// Add a thinking/reasoning trace part.
    pub fn with_thinking(mut self, thinking: impl Into<String>) -> Self {
        self.parts.push(Part::Thinking { thinking: thinking.into(), signature: None });
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

    /// Returns true if this part is a Thinking variant
    pub fn is_thinking(&self) -> bool {
        matches!(self, Part::Thinking { .. })
    }

    /// Returns the thinking text content if this is a Thinking part, None otherwise
    pub fn thinking_text(&self) -> Option<&str> {
        match self {
            Part::Thinking { thinking, .. } => Some(thinking.as_str()),
            _ => None,
        }
    }

    /// Returns the MIME type if this part has one (InlineData or FileData)
    pub fn mime_type(&self) -> Option<&str> {
        match self {
            Part::InlineData { mime_type, .. } => Some(mime_type.as_str()),
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
        matches!(self, Part::InlineData { .. } | Part::FileData { .. })
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

    #[test]
    fn test_thinking_variant_accessors() {
        let part = Part::Thinking {
            thinking: "step by step".to_string(),
            signature: Some("sig123".to_string()),
        };
        assert!(part.is_thinking());
        assert_eq!(part.thinking_text(), Some("step by step"));
        assert_eq!(part.text(), None);
    }

    #[test]
    fn test_non_thinking_variant_accessors() {
        let text = Part::Text { text: "hello".to_string() };
        assert!(!text.is_thinking());
        assert_eq!(text.thinking_text(), None);

        let data = Part::InlineData { mime_type: "image/png".to_string(), data: vec![] };
        assert!(!data.is_thinking());
        assert_eq!(data.thinking_text(), None);
    }

    #[test]
    fn test_content_with_thinking() {
        let content = Content::new("model").with_thinking("Let me reason about this");
        assert_eq!(content.parts.len(), 1);
        assert!(matches!(
            &content.parts[0],
            Part::Thinking { thinking, signature } if thinking == "Let me reason about this" && signature.is_none()
        ));
    }

    #[test]
    fn test_thinking_serialization_round_trip() {
        let part = Part::Thinking {
            thinking: "reasoning here".to_string(),
            signature: Some("sig".to_string()),
        };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_thinking_without_signature_serialization() {
        let part = Part::Thinking { thinking: "reasoning".to_string(), signature: None };
        let json = serde_json::to_string(&part).unwrap();
        // signature should be omitted from JSON
        assert!(!json.contains("signature"));
        let deserialized: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_thinking_does_not_deserialize_as_text() {
        let json = r#"{"thinking": "some reasoning"}"#;
        let part: Part = serde_json::from_str(json).unwrap();
        assert!(part.is_thinking());
        assert_eq!(part.thinking_text(), Some("some reasoning"));
        assert_eq!(part.text(), None);
    }

    #[test]
    fn test_text_does_not_deserialize_as_thinking() {
        let json = r#"{"text": "hello world"}"#;
        let part: Part = serde_json::from_str(json).unwrap();
        assert!(!part.is_thinking());
        assert_eq!(part.text(), Some("hello world"));
    }

    #[test]
    fn test_server_tool_call_round_trip() {
        let payload = serde_json::json!({
            "toolCallId": "tc_001",
            "toolName": "google_search",
            "args": {"query": "Rust programming"}
        });
        let part = Part::ServerToolCall { server_tool_call: payload.clone() };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
        assert!(json.contains("server_tool_call"));

        // Accessors return None/false for server tool call
        assert_eq!(part.text(), None);
        assert_eq!(part.mime_type(), None);
        assert_eq!(part.file_uri(), None);
        assert!(!part.is_media());
        assert!(!part.is_thinking());
        assert_eq!(part.thinking_text(), None);
    }

    #[test]
    fn test_server_tool_response_round_trip() {
        let payload = serde_json::json!({
            "toolCallId": "tc_001",
            "output": {"results": [{"title": "Rust Lang", "url": "https://rust-lang.org"}]}
        });
        let part = Part::ServerToolResponse { server_tool_response: payload.clone() };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: Part = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
        assert!(json.contains("server_tool_response"));

        // Accessors return None/false for server tool response
        assert_eq!(part.text(), None);
        assert_eq!(part.mime_type(), None);
        assert_eq!(part.file_uri(), None);
        assert!(!part.is_media());
        assert!(!part.is_thinking());
        assert_eq!(part.thinking_text(), None);
    }

    // ===== Multimodal FunctionResponseData tests =====

    #[test]
    fn test_inline_data_part_serde_round_trip() {
        let part = InlineDataPart { mime_type: "image/png".to_string(), data: vec![1, 2, 3, 4] };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: InlineDataPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_file_data_part_serde_round_trip() {
        let part = FileDataPart {
            mime_type: "application/pdf".to_string(),
            file_uri: "gs://bucket/report.pdf".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: FileDataPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_function_response_data_new_constructor() {
        let frd = FunctionResponseData::new("my_tool", serde_json::json!({"status": "ok"}));
        assert_eq!(frd.name, "my_tool");
        assert_eq!(frd.response, serde_json::json!({"status": "ok"}));
        assert!(frd.inline_data.is_empty());
        assert!(frd.file_data.is_empty());
    }

    #[test]
    fn test_function_response_data_with_inline_data() {
        let inline =
            vec![InlineDataPart { mime_type: "image/png".to_string(), data: vec![0x89, 0x50] }];
        let frd = FunctionResponseData::with_inline_data(
            "chart_tool",
            serde_json::json!({"ok": true}),
            inline.clone(),
        );
        assert_eq!(frd.inline_data, inline);
        assert!(frd.file_data.is_empty());
    }

    #[test]
    fn test_function_response_data_with_file_data() {
        let files = vec![FileDataPart {
            mime_type: "application/pdf".to_string(),
            file_uri: "gs://bucket/doc.pdf".to_string(),
        }];
        let frd = FunctionResponseData::with_file_data(
            "doc_tool",
            serde_json::json!({"ok": true}),
            files.clone(),
        );
        assert!(frd.inline_data.is_empty());
        assert_eq!(frd.file_data, files);
    }

    #[test]
    fn test_function_response_data_with_multimodal() {
        let inline =
            vec![InlineDataPart { mime_type: "image/png".to_string(), data: vec![1, 2, 3] }];
        let files = vec![FileDataPart {
            mime_type: "audio/wav".to_string(),
            file_uri: "https://example.com/audio.wav".to_string(),
        }];
        let frd = FunctionResponseData::with_multimodal(
            "multi_tool",
            serde_json::json!({}),
            inline.clone(),
            files.clone(),
        );
        assert_eq!(frd.inline_data, inline);
        assert_eq!(frd.file_data, files);
    }

    #[test]
    fn test_json_only_function_response_data_serializes_without_multimodal_keys() {
        let frd = FunctionResponseData::new("tool", serde_json::json!({"result": 42}));
        let json = serde_json::to_string(&frd).unwrap();
        assert!(!json.contains("inline_data"));
        assert!(!json.contains("file_data"));
        // Should only have name and response
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert!(map.contains_key("name"));
        assert!(map.contains_key("response"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_function_response_data_serde_round_trip_with_multimodal() {
        let frd = FunctionResponseData::with_multimodal(
            "tool",
            serde_json::json!({"status": "ok"}),
            vec![InlineDataPart { mime_type: "image/png".to_string(), data: vec![1, 2] }],
            vec![FileDataPart {
                mime_type: "application/pdf".to_string(),
                file_uri: "gs://b/f.pdf".to_string(),
            }],
        );
        let json = serde_json::to_string(&frd).unwrap();
        let deserialized: FunctionResponseData = serde_json::from_str(&json).unwrap();
        assert_eq!(frd, deserialized);
    }

    #[test]
    #[should_panic(expected = "exceeds maximum allowed size")]
    fn test_with_inline_data_panics_on_oversized() {
        let oversized = vec![InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![0u8; MAX_INLINE_DATA_SIZE + 1],
        }];
        let _ = FunctionResponseData::with_inline_data("tool", serde_json::json!({}), oversized);
    }

    #[test]
    #[should_panic(expected = "exceeds maximum allowed size")]
    fn test_with_multimodal_panics_on_oversized() {
        let oversized = vec![InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![0u8; MAX_INLINE_DATA_SIZE + 1],
        }];
        let _ =
            FunctionResponseData::with_multimodal("tool", serde_json::json!({}), oversized, vec![]);
    }

    // ===== from_tool_result tests =====

    #[test]
    fn test_from_tool_result_plain_json() {
        let value = serde_json::json!({"temperature": 72, "unit": "F"});
        let frd = FunctionResponseData::from_tool_result("weather", value.clone());
        assert_eq!(frd.name, "weather");
        assert_eq!(frd.response, value);
        assert!(frd.inline_data.is_empty());
        assert!(frd.file_data.is_empty());
    }

    #[test]
    fn test_from_tool_result_with_inline_data() {
        let value = serde_json::json!({
            "response": {"status": "ok", "description": "chart generated"},
            "inline_data": [
                {"mime_type": "image/png", "data": [0x89, 0x50, 0x4E, 0x47]}
            ]
        });
        let frd = FunctionResponseData::from_tool_result("chart_tool", value);
        assert_eq!(frd.name, "chart_tool");
        assert_eq!(
            frd.response,
            serde_json::json!({"status": "ok", "description": "chart generated"})
        );
        assert_eq!(frd.inline_data.len(), 1);
        assert_eq!(frd.inline_data[0].mime_type, "image/png");
        assert_eq!(frd.inline_data[0].data, vec![0x89, 0x50, 0x4E, 0x47]);
        assert!(frd.file_data.is_empty());
    }

    #[test]
    fn test_from_tool_result_with_file_data() {
        let value = serde_json::json!({
            "response": {"doc_id": "report-2024"},
            "file_data": [
                {"mime_type": "application/pdf", "file_uri": "gs://bucket/report.pdf"}
            ]
        });
        let frd = FunctionResponseData::from_tool_result("doc_tool", value);
        assert_eq!(frd.name, "doc_tool");
        assert_eq!(frd.file_data.len(), 1);
        assert_eq!(frd.file_data[0].file_uri, "gs://bucket/report.pdf");
    }

    #[test]
    fn test_from_tool_result_with_both() {
        let value = serde_json::json!({
            "response": {"ok": true},
            "inline_data": [{"mime_type": "image/png", "data": [1, 2]}],
            "file_data": [{"mime_type": "application/pdf", "file_uri": "gs://b/f.pdf"}]
        });
        let frd = FunctionResponseData::from_tool_result("multi", value);
        assert_eq!(frd.inline_data.len(), 1);
        assert_eq!(frd.file_data.len(), 1);
    }

    #[test]
    fn test_from_tool_result_empty_arrays_treated_as_plain() {
        // Empty inline_data/file_data arrays should not trigger multimodal extraction
        let value = serde_json::json!({
            "response": {"ok": true},
            "inline_data": [],
            "file_data": []
        });
        let frd = FunctionResponseData::from_tool_result("tool", value.clone());
        // Falls through to plain JSON since arrays are empty after deserialization
        assert!(frd.inline_data.is_empty());
        assert!(frd.file_data.is_empty());
    }

    #[test]
    fn test_from_tool_result_no_response_field_uses_remaining() {
        // If there's no explicit "response" field, the remaining fields become the response
        let value = serde_json::json!({
            "status": "ok",
            "inline_data": [{"mime_type": "image/png", "data": [1]}]
        });
        let frd = FunctionResponseData::from_tool_result("tool", value);
        assert_eq!(frd.inline_data.len(), 1);
        assert_eq!(frd.response, serde_json::json!({"status": "ok"}));
    }
}
