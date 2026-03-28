use serde::{Deserialize, Serialize};

/// A source for a PDF from a URL.
///
/// This type is used to provide a PDF to the model from a URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UrlPdfSource {
    /// The URL of the PDF.
    pub url: String,
}

impl UrlPdfSource {
    /// Creates a new UrlPdfSource with the specified URL.
    pub fn new<S: Into<String>>(url: S) -> Self {
        Self { url: url.into() }
    }

    /// Checks if the URL has a PDF file extension.
    pub fn has_pdf_extension(&self) -> bool {
        self.url.to_lowercase().ends_with(".pdf")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let source = UrlPdfSource { url: "https://example.com/document.pdf".to_string() };

        let json = serde_json::to_value(&source).unwrap();
        let expected = serde_json::json!({"url": "https://example.com/document.pdf"});

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({"url": "https://example.com/document.pdf"});
        let source: UrlPdfSource = serde_json::from_value(json).unwrap();

        assert_eq!(source.url, "https://example.com/document.pdf");
    }

    #[test]
    fn has_pdf_extension() {
        let source = UrlPdfSource::new("https://example.com/document.pdf");
        assert!(source.has_pdf_extension());

        let source = UrlPdfSource::new("https://example.com/document.PDF");
        assert!(source.has_pdf_extension());

        let source = UrlPdfSource::new("https://example.com/document.docx");
        assert!(!source.has_pdf_extension());
    }
}
