//! Data types for documents, chunks, and search results.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A source document containing text content, metadata, and provenance.
///
/// `source_uri` and `title` say where the text came from. Every chunker copies them onto
/// each [`Chunk`], every vector store returns them with search results, and
/// [`RagTool`](crate::RagTool) gives them to the model, so an answer can cite the
/// document. `metadata` is stored verbatim; no key is reserved.
///
/// # Example
///
/// ```rust
/// use adk_rag::Document;
///
/// let document = Document::new("refunds", "Refunds are issued within 30 days.")
///     .with_source_uri("https://example.com/policies/refunds")
///     .with_title("Refund policy")
///     .with_metadata("section", "billing");
///
/// assert_eq!(document.source_uri.as_deref(), Some("https://example.com/policies/refunds"));
/// assert_eq!(document.metadata["section"], "billing");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct Document {
    /// Unique identifier for the document.
    pub id: String,
    /// The text content of the document.
    pub text: String,
    /// Key-value metadata associated with the document.
    pub metadata: HashMap<String, String>,
    /// URI of the original source, such as a web page or file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    /// Human-readable title of the original source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Document {
    /// Creates a document with no metadata and no provenance.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::Document;
    ///
    /// let document = Document::new("intro", "Rust is a systems programming language.");
    /// assert!(document.metadata.is_empty());
    /// assert_eq!(document.source_uri, None);
    /// ```
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            metadata: HashMap::new(),
            source_uri: None,
            title: None,
        }
    }

    /// Adds one metadata entry, replacing any earlier value for `key`.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Sets the URI of the original source.
    #[must_use]
    pub fn with_source_uri(mut self, source_uri: impl Into<String>) -> Self {
        self.source_uri = Some(source_uri.into());
        self
    }

    /// Sets the title of the original source.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// A segment of a [`Document`] with its vector embedding.
///
/// A chunk carries its document's `source_uri` and `title`, which every vector store
/// persists and returns as fields of their own.
///
/// # Example
///
/// ```rust
/// use adk_rag::{Chunk, Document};
///
/// let document = Document::new("refunds", "Refunds are issued within 30 days.")
///     .with_source_uri("https://example.com/policies/refunds");
/// let chunk = Chunk::from_document(&document, "refunds_0", "Refunds are issued")
///     .with_embedding(vec![0.1, 0.2]);
///
/// assert_eq!(chunk.document_id, "refunds");
/// assert_eq!(chunk.source_uri, document.source_uri);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct Chunk {
    /// Unique identifier for the chunk.
    pub id: String,
    /// The text content of the chunk.
    pub text: String,
    /// The vector embedding for this chunk's text.
    pub embedding: Vec<f32>,
    /// Key-value metadata inherited from the parent document plus chunk-specific fields.
    pub metadata: HashMap<String, String>,
    /// The ID of the parent [`Document`].
    pub document_id: String,
    /// URI of the parent document's original source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    /// Title of the parent document's original source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Chunk {
    /// Creates a chunk with no embedding, metadata, or provenance.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_rag::Chunk;
    ///
    /// let chunk = Chunk::new("doc_0", "doc", "first chunk").with_embedding(vec![1.0, 0.0]);
    /// assert_eq!(chunk.embedding, [1.0, 0.0]);
    /// ```
    pub fn new(
        id: impl Into<String>,
        document_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            embedding: Vec::new(),
            metadata: HashMap::new(),
            document_id: document_id.into(),
            source_uri: None,
            title: None,
        }
    }

    /// Creates a chunk of `document` that inherits its ID, metadata, source URI, and title.
    ///
    /// A [`Chunker`](crate::Chunker) builds its chunks with this constructor, so provenance
    /// reaches the vector store.
    pub fn from_document(
        document: &Document,
        id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            embedding: Vec::new(),
            metadata: document.metadata.clone(),
            document_id: document.id.clone(),
            source_uri: document.source_uri.clone(),
            title: document.title.clone(),
        }
    }

    /// Sets the embedding vector.
    #[must_use]
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = embedding;
        self
    }

    /// Adds one metadata entry, replacing any earlier value for `key`.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Sets the URI of the original source.
    #[must_use]
    pub fn with_source_uri(mut self, source_uri: impl Into<String>) -> Self {
        self.source_uri = Some(source_uri.into());
        self
    }

    /// Sets the title of the original source.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// A retrieved [`Chunk`] paired with a relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The retrieved chunk.
    pub chunk: Chunk,
    /// The similarity score (higher is more relevant).
    pub score: f32,
}
