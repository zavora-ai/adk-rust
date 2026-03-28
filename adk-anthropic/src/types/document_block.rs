use serde::{Deserialize, Serialize};

use crate::types::{
    Base64PdfSource, CacheControlEphemeral, CitationsConfig, ContentBlockSourceParam, FileSource,
    PlainTextSource, UrlPdfSource,
};

/// The source type for a document block, which can be one of several types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum DocumentSource {
    /// A Base64 encoded PDF source.
    #[serde(rename = "base64")]
    Base64Pdf(Base64PdfSource),

    /// A plain text source.
    #[serde(rename = "text")]
    PlainText(PlainTextSource),

    /// A content block source.
    #[serde(rename = "content")]
    ContentBlock(ContentBlockSourceParam),

    /// A URL PDF source.
    #[serde(rename = "url")]
    UrlPdf(UrlPdfSource),

    /// A file source referencing a server-side file.
    #[serde(rename = "file")]
    File(FileSource),
}

impl From<Base64PdfSource> for DocumentSource {
    fn from(source: Base64PdfSource) -> Self {
        DocumentSource::Base64Pdf(source)
    }
}

impl From<PlainTextSource> for DocumentSource {
    fn from(source: PlainTextSource) -> Self {
        DocumentSource::PlainText(source)
    }
}

impl From<ContentBlockSourceParam> for DocumentSource {
    fn from(source: ContentBlockSourceParam) -> Self {
        DocumentSource::ContentBlock(source)
    }
}

impl From<UrlPdfSource> for DocumentSource {
    fn from(source: UrlPdfSource) -> Self {
        DocumentSource::UrlPdf(source)
    }
}

impl From<FileSource> for DocumentSource {
    fn from(source: FileSource) -> Self {
        DocumentSource::File(source)
    }
}

/// Parameters for a document block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentBlock {
    /// The source of the document.
    pub source: DocumentSource,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Configuration for citations in this document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<CitationsConfig>,

    /// Optional context for the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    /// Optional title for the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl DocumentBlock {
    /// Create a new `DocumentBlock` with the given source.
    pub fn new(source: DocumentSource) -> Self {
        Self { source, cache_control: None, citations: None, context: None, title: None }
    }

    /// Create a new `DocumentBlock` with a Base64 PDF source.
    pub fn new_with_base64_pdf(source: Base64PdfSource) -> Self {
        Self::new(DocumentSource::Base64Pdf(source))
    }

    /// Create a new `DocumentBlock` with a plain text source.
    pub fn new_with_plain_text(source: PlainTextSource) -> Self {
        Self::new(DocumentSource::PlainText(source))
    }

    /// Create a new `DocumentBlock` with a content block source.
    pub fn new_with_content_block(source: ContentBlockSourceParam) -> Self {
        Self::new(DocumentSource::ContentBlock(source))
    }

    /// Create a new `DocumentBlock` with a URL PDF source.
    pub fn new_with_url_pdf(source: UrlPdfSource) -> Self {
        Self::new(DocumentSource::UrlPdf(source))
    }

    /// Add a cache control to this document block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }

    /// Add citations configuration to this document block.
    pub fn with_citations(mut self, citations: CitationsConfig) -> Self {
        self.citations = Some(citations);
        self
    }

    /// Add context to this document block.
    pub fn with_context(mut self, context: String) -> Self {
        self.context = Some(context);
        self
    }

    /// Add a title to this document block.
    pub fn with_title(mut self, title: String) -> Self {
        self.title = Some(title);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn document_block_with_base64_pdf() {
        let base64_source =
            Base64PdfSource::new("data:application/pdf;base64,JVBERi0xLjcKJeLjz9MKN".to_string());

        let document_block = DocumentBlock::new_with_base64_pdf(base64_source);
        let json = to_value(&document_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "base64",
                    "data": "data:application/pdf;base64,JVBERi0xLjcKJeLjz9MKN",
                    "media_type": "application/pdf"
                }
            })
        );
    }

    #[test]
    fn document_block_with_plain_text() {
        let text_source = PlainTextSource::new("Sample text content".to_string());

        let document_block = DocumentBlock::new_with_plain_text(text_source);
        let json = to_value(&document_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "text",
                    "data": "Sample text content",
                    "media_type": "text/plain"
                }
            })
        );
    }

    #[test]
    fn document_block_with_content_block() {
        let content_source = ContentBlockSourceParam::from_string_ref("Sample content");

        let document_block = DocumentBlock::new_with_content_block(content_source);
        let json = to_value(&document_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "content",
                    "content": "Sample content"
                }
            })
        );
    }

    #[test]
    fn document_block_with_url_pdf() {
        let url_source = UrlPdfSource::new("https://example.com/document.pdf".to_string());

        let document_block = DocumentBlock::new_with_url_pdf(url_source);
        let json = to_value(&document_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "url",
                    "url": "https://example.com/document.pdf"
                }
            })
        );
    }

    #[test]
    fn document_block_with_all_fields() {
        let url_source = UrlPdfSource::new("https://example.com/document.pdf".to_string());
        let cache_control = CacheControlEphemeral::new();
        let citations = CitationsConfig::enabled();

        let document_block = DocumentBlock::new_with_url_pdf(url_source)
            .with_cache_control(cache_control)
            .with_citations(citations)
            .with_context("Document context".to_string())
            .with_title("Document Title".to_string());

        let json = to_value(&document_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "url",
                    "url": "https://example.com/document.pdf"
                },
                "cache_control": {
                    "type": "ephemeral"
                },
                "citations": {
                    "enabled": true
                },
                "context": "Document context",
                "title": "Document Title"
            })
        );
    }
}
