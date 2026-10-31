use crate::error::{AdkError, ErrorCategory, ErrorComponent, Result};
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
///     uri: None,
///     annotations: None,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InlineDataPart {
    /// MIME type of the inline data (e.g., "image/png", "audio/wav").
    pub mime_type: String,
    /// Raw binary data.
    pub data: Vec<u8>,
    /// Optional source URI associated with the inline payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Optional protocol or presentation annotations preserved with the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
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
///     annotations: None,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDataPart {
    /// MIME type of the file (e.g., "application/pdf").
    pub mime_type: String,
    /// URI to the file (URL, gs://, etc.).
    pub file_uri: String,
    /// Optional protocol or presentation annotations preserved with the reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
}

/// Text contents of an embedded resource.
///
/// Carries a source URI, an optional MIME type, and an inline UTF-8 text payload.
/// Mirrors the MCP / ACP text embedded-resource block. The text is stored and
/// transmitted verbatim (no base64 encoding).
///
/// # Example
///
/// ```rust
/// use adk_core::TextResourceContents;
///
/// let contents = TextResourceContents {
///     uri: "file:///project/src/main.rs".to_string(),
///     mime_type: Some("text/x-rust".to_string()),
///     text: "fn main() {}".to_string(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextResourceContents {
    /// Source URI of the resource (e.g., `file:///path`, `https://...`).
    pub uri: String,
    /// Optional MIME type of the resource (e.g., "text/x-rust", "text/markdown").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The inline UTF-8 text payload.
    pub text: String,
}

impl TextResourceContents {
    /// Create text resource contents from a URI and text payload.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::TextResourceContents;
    ///
    /// let contents = TextResourceContents::new("file:///notes.txt", None, "hello");
    /// assert_eq!(contents.uri, "file:///notes.txt");
    /// ```
    pub fn new(uri: impl Into<String>, mime_type: Option<String>, text: impl Into<String>) -> Self {
        Self { uri: uri.into(), mime_type, text: text.into() }
    }
}

/// Binary contents of an embedded resource.
///
/// Carries a source URI, an optional MIME type, and a raw binary payload.
/// Mirrors the MCP / ACP blob embedded-resource block. On the wire the bytes are
/// base64-encoded by protocol layers, but they are stored here as raw bytes.
///
/// Construct via [`BlobResourceContents::new`], which validates the payload
/// against [`MAX_INLINE_DATA_SIZE`].
///
/// # Example
///
/// ```rust
/// use adk_core::BlobResourceContents;
///
/// let contents = BlobResourceContents::new(
///     "file:///logo.png",
///     Some("image/png".to_string()),
///     vec![0x89, 0x50, 0x4E, 0x47],
/// )
/// .unwrap();
/// assert_eq!(contents.uri, "file:///logo.png");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobResourceContents {
    /// Source URI of the resource (e.g., `file:///path`, `https://...`).
    pub uri: String,
    /// Optional MIME type of the resource (e.g., "image/png", "application/pdf").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Raw binary data.
    pub data: Vec<u8>,
}

impl BlobResourceContents {
    /// Create binary resource contents, validating the payload size.
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] with category [`ErrorCategory::InvalidInput`] if
    /// `data` exceeds [`MAX_INLINE_DATA_SIZE`] (10 MB). Unlike the panicking
    /// `InlineData` helpers, this constructor rejects oversized payloads without
    /// truncating.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::BlobResourceContents;
    ///
    /// let ok = BlobResourceContents::new("file:///a.bin", None, vec![1, 2, 3]);
    /// assert!(ok.is_ok());
    /// ```
    pub fn new(uri: impl Into<String>, mime_type: Option<String>, data: Vec<u8>) -> Result<Self> {
        if data.len() > MAX_INLINE_DATA_SIZE {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                "core.embedded_resource.too_large",
                format!(
                    "embedded resource blob size {} exceeds maximum allowed size of {MAX_INLINE_DATA_SIZE} bytes",
                    data.len(),
                ),
            ));
        }
        Ok(Self { uri: uri.into(), mime_type, data })
    }
}

/// A complete resource embedded in a message: a source URI plus inline text or
/// binary contents.
///
/// Mirrors the MCP / ACP embedded-resource content block. Serialized untagged and
/// disambiguated by the distinct `text` (for [`Text`](Self::Text)) versus `data`
/// (for [`Blob`](Self::Blob)) fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddedResource {
    /// Text resource contents (verbatim UTF-8 payload).
    Text(TextResourceContents),
    /// Binary resource contents (raw bytes; base64-encoded on the wire).
    Blob(BlobResourceContents),
}

impl EmbeddedResource {
    /// Returns the source URI of the embedded resource.
    pub fn uri(&self) -> &str {
        match self {
            EmbeddedResource::Text(t) => &t.uri,
            EmbeddedResource::Blob(b) => &b.uri,
        }
    }

    /// Returns the MIME type of the embedded resource, if present.
    pub fn mime_type(&self) -> Option<&str> {
        match self {
            EmbeddedResource::Text(t) => t.mime_type.as_deref(),
            EmbeddedResource::Blob(b) => b.mime_type.as_deref(),
        }
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        /// Optional source URI associated with the inline payload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        /// Optional protocol or presentation annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<serde_json::Value>,
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
    ///     annotations: None,
    /// };
    /// ```
    FileData {
        /// MIME type of the file (e.g., "image/jpeg", "audio/wav")
        mime_type: String,
        /// URI to the file (URL, gs://, etc.)
        file_uri: String,
        /// Optional protocol or presentation annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<serde_json::Value>,
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
        /// Optional protocol or presentation annotations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<serde_json::Value>,
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
    /// A complete resource embedded in a message: a source URI plus inline text
    /// or binary contents. Mirrors the MCP / ACP embedded-resource block.
    ///
    /// The `resource` key is unique among all `Part` variants, so untagged
    /// deserialization matches this variant unambiguously. This variant is
    /// additive: `Content` values serialized before it existed deserialize
    /// unchanged.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{EmbeddedResource, Part, TextResourceContents};
    ///
    /// let part = Part::EmbeddedResource {
    ///     resource: EmbeddedResource::Text(TextResourceContents::new(
    ///         "file:///main.rs",
    ///         Some("text/x-rust".to_string()),
    ///         "fn main() {}",
    ///     )),
    /// };
    /// assert_eq!(part.embedded_resource().map(EmbeddedResource::uri), Some("file:///main.rs"));
    /// ```
    EmbeddedResource {
        /// The embedded resource (text or binary contents).
        resource: EmbeddedResource,
    },
}

/// Prefix of the ID an agent gives a function call that its provider sent without one.
///
/// Providers such as Ollama issue tool calls without IDs. [`Content::assign_call_ids`] gives
/// each such call a generated ID, so the call, its response and any confirmation request for
/// it share one identity in the session, and [`Content::strip_generated_call_ids`] removes
/// these IDs again from the contents sent to a model, so a provider never receives an ID it
/// did not issue.
pub const GENERATED_CALL_ID_PREFIX: &str = "adk-call-";

impl Content {
    /// Gives each function call without an ID a generated one, prefixed with
    /// [`GENERATED_CALL_ID_PREFIX`]. Calls that carry an ID keep it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Content, GENERATED_CALL_ID_PREFIX, Part};
    ///
    /// let call = |id: Option<&str>| Part::FunctionCall {
    ///     name: "transfer".to_string(),
    ///     args: serde_json::json!({}),
    ///     id: id.map(str::to_string),
    ///     thought_signature: None,
    /// };
    /// let mut content = Content { role: "model".into(), parts: vec![call(None), call(Some("c1"))] };
    /// content.assign_call_ids();
    ///
    /// let ids: Vec<_> = content
    ///     .parts
    ///     .iter()
    ///     .filter_map(|part| match part {
    ///         Part::FunctionCall { id, .. } => id.clone(),
    ///         _ => None,
    ///     })
    ///     .collect();
    /// assert!(ids[0].starts_with(GENERATED_CALL_ID_PREFIX));
    /// assert_eq!(ids[1], "c1");
    /// ```
    pub fn assign_call_ids(&mut self) {
        for part in &mut self.parts {
            if let Part::FunctionCall { id: id @ None, .. } = part {
                *id = Some(format!("{GENERATED_CALL_ID_PREFIX}{}", uuid::Uuid::new_v4().simple()));
            }
        }
    }

    /// Removes the IDs [`assign_call_ids`](Self::assign_call_ids) generated from this
    /// content's function calls and function responses, leaving provider-issued IDs.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Content, FunctionResponseData, Part};
    ///
    /// let mut content = Content {
    ///     role: "function".into(),
    ///     parts: vec![Part::FunctionResponse {
    ///         function_response: FunctionResponseData::new("transfer", serde_json::json!({})),
    ///         id: Some("adk-call-1f2e".into()),
    ///         annotations: None,
    ///     }],
    /// };
    /// content.strip_generated_call_ids();
    /// assert!(matches!(&content.parts[0], Part::FunctionResponse { id: None, .. }));
    /// ```
    pub fn strip_generated_call_ids(&mut self) {
        for part in &mut self.parts {
            if let Part::FunctionCall { id, .. } | Part::FunctionResponse { id, .. } = part
                && id.as_deref().is_some_and(|id| id.starts_with(GENERATED_CALL_ID_PREFIX))
            {
                *id = None;
            }
        }
    }

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

    /// Returns `true` if this content carries input from its author: a part other than
    /// blank text or a function response.
    ///
    /// A run that only resumes held tool calls sends a user turn of blank text, and a
    /// client's custom tool results arrive as user-role function responses; neither is a
    /// message for input guardrails to screen.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Content;
    ///
    /// assert!(Content::new("user").with_text("hello").carries_user_input());
    /// assert!(!Content::new("user").with_text("  ").carries_user_input());
    /// assert!(!Content::new("user").carries_user_input());
    /// ```
    pub fn carries_user_input(&self) -> bool {
        self.parts.iter().any(|part| match part {
            Part::Text { text } => !text.trim().is_empty(),
            Part::FunctionResponse { .. } => false,
            Part::Thinking { .. }
            | Part::InlineData { .. }
            | Part::FileData { .. }
            | Part::FunctionCall { .. }
            | Part::ServerToolCall { .. }
            | Part::ServerToolResponse { .. }
            | Part::EmbeddedResource { .. } => true,
        })
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
        self.parts.push(Part::InlineData {
            mime_type: mime_type.into(),
            data,
            uri: None,
            annotations: None,
        });
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
        self.parts.push(Part::FileData {
            mime_type: mime_type.into(),
            file_uri: file_uri.into(),
            annotations: None,
        });
        self
    }

    /// Add an embedded resource part (a source URI plus inline text or binary
    /// contents).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{Content, EmbeddedResource, TextResourceContents};
    ///
    /// let content = Content::new("user").with_embedded_resource(EmbeddedResource::Text(
    ///     TextResourceContents::new("file:///readme.md", Some("text/markdown".to_string()), "# Hi"),
    /// ));
    /// assert_eq!(content.parts.len(), 1);
    /// ```
    pub fn with_embedded_resource(mut self, resource: EmbeddedResource) -> Self {
        self.parts.push(Part::EmbeddedResource { resource });
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

    /// Returns the source URI for inline or referenced file data.
    pub fn uri(&self) -> Option<&str> {
        match self {
            Part::InlineData { uri, .. } => uri.as_deref(),
            Part::FileData { file_uri, .. } => Some(file_uri.as_str()),
            _ => None,
        }
    }

    /// Returns protocol or presentation annotations attached to this part.
    pub fn annotations(&self) -> Option<&serde_json::Value> {
        match self {
            Part::InlineData { annotations, .. }
            | Part::FileData { annotations, .. }
            | Part::FunctionResponse { annotations, .. } => annotations.as_ref(),
            _ => None,
        }
    }

    /// Returns true if this part contains media (image, audio, video)
    pub fn is_media(&self) -> bool {
        matches!(self, Part::InlineData { .. } | Part::FileData { .. })
    }

    /// Returns the embedded resource if this is an `EmbeddedResource` part, None otherwise.
    pub fn embedded_resource(&self) -> Option<&EmbeddedResource> {
        match self {
            Part::EmbeddedResource { resource } => Some(resource),
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
        Part::InlineData { mime_type: mime_type.into(), data, uri: None, annotations: None }
    }

    /// Create a new file data part from URI
    pub fn file_data(mime_type: impl Into<String>, file_uri: impl Into<String>) -> Self {
        Part::FileData { mime_type: mime_type.into(), file_uri: file_uri.into(), annotations: None }
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
            annotations: None,
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("image/jpeg"));
        assert!(json.contains("https://example.com/image.jpg"));
    }

    #[test]
    fn test_part_text_accessor() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert_eq!(text_part.text(), Some("hello"));

        let data_part = Part::inline_data("image/png", vec![]);
        assert_eq!(data_part.text(), None);
    }

    #[test]
    fn test_part_mime_type_accessor() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert_eq!(text_part.mime_type(), None);

        let inline_part = Part::inline_data("image/png", vec![]);
        assert_eq!(inline_part.mime_type(), Some("image/png"));

        let file_part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com".to_string(),
            annotations: None,
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
            annotations: None,
        };
        assert_eq!(file_part.file_uri(), Some("https://example.com/img.jpg"));
    }

    #[test]
    fn test_part_is_media() {
        let text_part = Part::Text { text: "hello".to_string() };
        assert!(!text_part.is_media());

        let inline_part = Part::inline_data("image/png", vec![]);
        assert!(inline_part.is_media());

        let file_part = Part::FileData {
            mime_type: "image/jpeg".to_string(),
            file_uri: "https://example.com".to_string(),
            annotations: None,
        };
        assert!(file_part.is_media());
    }

    #[test]
    fn test_part_constructors() {
        let text = Part::text_part("hello");
        assert!(matches!(text, Part::Text { text } if text == "hello"));

        let inline = Part::inline_data("image/png", vec![1, 2, 3]);
        assert!(
            matches!(inline, Part::InlineData { mime_type, data, .. } if mime_type == "image/png" && data == vec![1, 2, 3])
        );

        let file = Part::file_data("image/jpeg", "https://example.com/img.jpg");
        assert!(
            matches!(file, Part::FileData { mime_type, file_uri, .. } if mime_type == "image/jpeg" && file_uri == "https://example.com/img.jpg")
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

        let data = Part::inline_data("image/png", vec![]);
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
        let part = InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![1, 2, 3, 4],
            uri: Some("file:///screenshots/result.png".to_string()),
            annotations: Some(serde_json::json!({"audience": ["user"], "priority": 0.8})),
        };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: InlineDataPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn test_file_data_part_serde_round_trip() {
        let part = FileDataPart {
            mime_type: "application/pdf".to_string(),
            file_uri: "gs://bucket/report.pdf".to_string(),
            annotations: Some(serde_json::json!({"audience": ["assistant"]})),
        };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: FileDataPart = serde_json::from_str(&json).unwrap();
        assert_eq!(part, deserialized);
    }

    #[test]
    fn part_metadata_survives_session_serialization() {
        let annotations = serde_json::json!({"audience": ["user"], "priority": 0.8});
        let parts = vec![
            Part::InlineData {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
                uri: Some("file:///screenshots/result.png".to_string()),
                annotations: Some(annotations.clone()),
            },
            Part::FileData {
                mime_type: "application/pdf".to_string(),
                file_uri: "file:///reports/result.pdf".to_string(),
                annotations: Some(annotations.clone()),
            },
            Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "render_report",
                    serde_json::json!({"ok": true}),
                ),
                id: Some("call-1".to_string()),
                annotations: Some(annotations.clone()),
            },
        ];

        let encoded = serde_json::to_value(&parts).unwrap();
        let decoded: Vec<Part> = serde_json::from_value(encoded).unwrap();

        assert_eq!(decoded, parts);
        assert_eq!(decoded[0].uri(), Some("file:///screenshots/result.png"));
        assert_eq!(decoded[0].annotations(), Some(&annotations));
        assert_eq!(decoded[1].annotations(), Some(&annotations));
        assert_eq!(decoded[2].annotations(), Some(&annotations));
    }

    #[test]
    fn part_metadata_fields_default_when_deserializing_existing_payloads() {
        let inline: Part = serde_json::from_value(serde_json::json!({
            "mime_type": "image/png",
            "data": [1, 2, 3]
        }))
        .unwrap();
        assert_eq!(inline.uri(), None);
        assert_eq!(inline.annotations(), None);

        let file: Part = serde_json::from_value(serde_json::json!({
            "mime_type": "application/pdf",
            "file_uri": "file:///reports/result.pdf"
        }))
        .unwrap();
        assert_eq!(file.annotations(), None);
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
        let inline = vec![InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![0x89, 0x50],
            uri: None,
            annotations: None,
        }];
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
            annotations: None,
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
        let inline = vec![InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![1, 2, 3],
            uri: None,
            annotations: None,
        }];
        let files = vec![FileDataPart {
            mime_type: "audio/wav".to_string(),
            file_uri: "https://example.com/audio.wav".to_string(),
            annotations: None,
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
            vec![InlineDataPart {
                mime_type: "image/png".to_string(),
                data: vec![1, 2],
                uri: None,
                annotations: None,
            }],
            vec![FileDataPart {
                mime_type: "application/pdf".to_string(),
                file_uri: "gs://b/f.pdf".to_string(),
                annotations: None,
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
            uri: None,
            annotations: None,
        }];
        let _ = FunctionResponseData::with_inline_data("tool", serde_json::json!({}), oversized);
    }

    #[test]
    #[should_panic(expected = "exceeds maximum allowed size")]
    fn test_with_multimodal_panics_on_oversized() {
        let oversized = vec![InlineDataPart {
            mime_type: "image/png".to_string(),
            data: vec![0u8; MAX_INLINE_DATA_SIZE + 1],
            uri: None,
            annotations: None,
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
