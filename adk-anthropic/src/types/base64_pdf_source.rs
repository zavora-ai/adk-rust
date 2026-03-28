use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Represents a base64-encoded PDF source.
///
/// This can be created from either a base64-encoded string or from a file path.
/// The media_type is always "application/pdf".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Base64PdfSource {
    /// The base64-encoded data of the PDF
    pub data: String,

    /// The media type of the file (always "application/pdf")
    #[serde(default = "default_media_type")]
    pub media_type: String,
}

fn default_media_type() -> String {
    "application/pdf".to_string()
}

impl Base64PdfSource {
    /// Create a new Base64PdfSource from a base64-encoded string
    pub fn new(data: String) -> Self {
        Self { data, media_type: default_media_type() }
    }

    /// Create a Base64PdfSource from a file path
    ///
    /// This will read the file and encode it as base64.
    /// The file extension should be ".pdf".
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let path = path.as_ref();

        // Verify file extension is .pdf
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("pdf") => {}
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "File extension must be .pdf",
                ));
            }
        };

        // Read the file
        let mut file = File::open(path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        // Encode as base64
        let data = base64::engine::general_purpose::STANDARD.encode(&buffer);

        Ok(Self { data, media_type: default_media_type() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let source = Base64PdfSource {
            data: "SGVsbG8gV29ybGQ=".to_string(), // "Hello World" in base64
            media_type: "application/pdf".to_string(),
        };

        let json = serde_json::to_value(&source).unwrap();
        let expected = serde_json::json!({
            "data": "SGVsbG8gV29ybGQ=",
            "media_type": "application/pdf"
        });

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({
            "data": "SGVsbG8gV29ybGQ=",
            "media_type": "application/pdf"
        });
        let source: Base64PdfSource = serde_json::from_value(json).unwrap();

        assert_eq!(source.data, "SGVsbG8gV29ybGQ=");
        assert_eq!(source.media_type, "application/pdf");
    }
}
