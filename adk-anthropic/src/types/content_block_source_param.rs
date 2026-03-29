use serde::{Deserialize, Serialize};

use crate::types::Content;

/// Parameter for a content block source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
#[serde(rename = "content")]
pub struct ContentBlockSourceParam {
    /// The content of the source, which can be either a string or an array of content items.
    pub content: ContentBlockSourceContent,
}

/// The content of a content block source, which can be either a string or an array of content items.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ContentBlockSourceContent {
    /// A simple string content.
    String(String),

    /// An array of content items.
    Array(Vec<Content>),
}

impl ContentBlockSourceParam {
    /// Create a new `ContentBlockSourceParam` with a string content.
    pub fn new_with_string(content: String) -> Self {
        Self { content: ContentBlockSourceContent::String(content) }
    }

    /// Create a new `ContentBlockSourceParam` with string content from a str reference.
    pub fn from_string_ref(content: &str) -> Self {
        Self::new_with_string(content.to_string())
    }

    /// Create a new `ContentBlockSourceParam` with an array of content items.
    pub fn new_with_array(content: Vec<Content>) -> Self {
        Self { content: ContentBlockSourceContent::Array(content) }
    }

    /// Create a new `ContentBlockSourceParam` with a single content item.
    pub fn new_with_item(content: Content) -> Self {
        Self::new_with_array(vec![content])
    }
}

impl std::str::FromStr for ContentBlockSourceParam {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_string_ref(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageBlock, TextBlock, UrlImageSource};
    use serde_json::{json, to_value};

    #[test]
    fn content_block_source_param_with_string() {
        let source = ContentBlockSourceParam::new_with_string("Sample content".to_string());
        let json = to_value(&source).unwrap();

        assert_eq!(
            json,
            json!({
                "content": "Sample content",
                "type": "content"
            })
        );
    }

    #[test]
    fn content_block_source_param_with_array() {
        let text_param = TextBlock::new("Sample text content".to_string());
        let url_source = UrlImageSource::new("https://example.com/image.jpg".to_string());
        let image_param = ImageBlock::new_with_url(url_source);

        let content = vec![Content::Text(text_param), Content::Image(image_param)];

        let source = ContentBlockSourceParam::new_with_array(content);
        let json = to_value(&source).unwrap();

        assert_eq!(
            json,
            json!({
                "content": [
                    {
                        "text": "Sample text content",
                        "type": "text"
                    },
                    {
                        "source": {
                            "url": "https://example.com/image.jpg",
                            "type": "url"
                        },
                        "type": "image"
                    }
                ],
                "type": "content"
            })
        );
    }

    #[test]
    fn content_block_source_param_with_item() {
        let text_param = TextBlock::new("Sample text content".to_string());
        let source = ContentBlockSourceParam::new_with_item(Content::Text(text_param));

        let json = to_value(&source).unwrap();

        assert_eq!(
            json,
            json!({
                "content": [
                    {
                        "text": "Sample text content",
                        "type": "text"
                    }
                ],
                "type": "content"
            })
        );
    }

    #[test]
    fn from_string_ref() {
        let source = ContentBlockSourceParam::from_string_ref("Sample content");

        match &source.content {
            ContentBlockSourceContent::String(content) => {
                assert_eq!(content, "Sample content");
            }
            _ => panic!("Expected String variant"),
        }
    }

    #[test]
    fn from_str() {
        let source = "Sample content".parse::<ContentBlockSourceParam>().unwrap();

        match &source.content {
            ContentBlockSourceContent::String(content) => {
                assert_eq!(content, "Sample content");
            }
            _ => panic!("Expected String variant"),
        }
    }
}
