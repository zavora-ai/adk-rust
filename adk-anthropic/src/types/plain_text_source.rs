use serde::{Deserialize, Serialize};

/// A plain text source parameter for content blocks.
///
/// This represents plain text data that can be used as a source in content blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlainTextSource {
    /// The plain text data.
    pub data: String,

    /// The media type, which is always "text/plain".
    #[serde(rename = "media_type")]
    pub media_type: String,
}

impl PlainTextSource {
    /// Create a new `PlainTextSource` with the given text data.
    pub fn new(data: String) -> Self {
        Self { data, media_type: "text/plain".to_string() }
    }

    /// Create a new `PlainTextSource` from a string reference.
    pub fn from_string_ref(data: &str) -> Self {
        Self::new(data.to_string())
    }
}

impl std::str::FromStr for PlainTextSource {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_string_ref(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn plain_text_source_serialization() {
        let source = PlainTextSource::new("Sample text content".to_string());
        let json = to_value(&source).unwrap();

        assert_eq!(
            json,
            json!({
                "data": "Sample text content",
                "media_type": "text/plain"
            })
        );
    }

    #[test]
    fn plain_text_source_from_string_ref() {
        let source = PlainTextSource::from_string_ref("Sample text content");
        assert_eq!(source.data, "Sample text content");
        assert_eq!(source.media_type, "text/plain");
    }

    #[test]
    fn from_str() {
        let source = "Sample text content".parse::<PlainTextSource>().unwrap();
        assert_eq!(source.data, "Sample text content");
        assert_eq!(source.media_type, "text/plain");
    }
}
