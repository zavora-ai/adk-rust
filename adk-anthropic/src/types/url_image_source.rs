use serde::{Deserialize, Serialize};

/// A source for an image from a URL.
///
/// This type is used to provide an image to the model from a URL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UrlImageSource {
    /// The URL of the image.
    pub url: String,
}

impl UrlImageSource {
    /// Creates a new UrlImageSource with the specified URL.
    pub fn new<S: Into<String>>(url: S) -> Self {
        Self { url: url.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization() {
        let source = UrlImageSource { url: "https://example.com/image.jpg".to_string() };

        let json = serde_json::to_value(&source).unwrap();
        let expected = serde_json::json!({"url": "https://example.com/image.jpg"});

        assert_eq!(json, expected);
    }

    #[test]
    fn deserialization() {
        let json = serde_json::json!({"url": "https://example.com/image.jpg"});
        let source: UrlImageSource = serde_json::from_value(json).unwrap();

        assert_eq!(source.url, "https://example.com/image.jpg");
    }
}
