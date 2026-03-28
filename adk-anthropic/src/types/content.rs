use serde::{Deserialize, Serialize};

use crate::types::{ImageBlock, TextBlock};

/// A content type that can be either a text block or an image block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum Content {
    /// A text block content.
    #[serde(rename = "text")]
    Text(TextBlock),

    /// An image block content.
    #[serde(rename = "image")]
    Image(ImageBlock),
}

impl From<TextBlock> for Content {
    fn from(text_block: TextBlock) -> Self {
        Content::Text(text_block)
    }
}

impl From<ImageBlock> for Content {
    fn from(image_block: ImageBlock) -> Self {
        Content::Image(image_block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn content_text_serialization() {
        let text_block = TextBlock::new("Sample text content".to_string());
        let content = Content::Text(text_block);

        let json = to_value(&content).unwrap();
        assert_eq!(
            json,
            json!({
                "text": "Sample text content",
                "type": "text"
            })
        );
    }

    #[test]
    fn content_image_serialization() {
        let base64_source = crate::types::Base64ImageSource::new(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==".to_string(),
            crate::types::ImageMediaType::Png,
        );
        let image_block = ImageBlock::new_with_base64(base64_source);
        let content = Content::Image(image_block);

        let json = to_value(&content).unwrap();
        assert_eq!(
            json,
            json!({
                "source": {
                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
                    "media_type": "image/png",
                    "type": "base64"
                },
                "type": "image"
            })
        );
    }

    #[test]
    fn content_deserialization() {
        // Test text deserialization
        let json = json!({
            "text": "Sample text content",
            "type": "text"
        });

        let content: Content = serde_json::from_value(json).unwrap();
        match content {
            Content::Text(text_block) => {
                assert_eq!(text_block.text, "Sample text content");
            }
            _ => panic!("Expected Text variant"),
        }

        // Test image deserialization
        let json = json!({
            "source": {
                "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==",
                "media_type": "image/png",
                "type": "base64"
            },
            "type": "image"
        });

        let content: Content = serde_json::from_value(json).unwrap();
        match content {
            Content::Image(_) => {
                // Image deserialization successful
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn from_text_block() {
        let text_block = TextBlock::new("Test content".to_string());
        let content = Content::from(text_block.clone());

        match content {
            Content::Text(block) => {
                assert_eq!(block.text, "Test content");
            }
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn from_image_block() {
        let base64_source = crate::types::Base64ImageSource::new(
            "data".to_string(),
            crate::types::ImageMediaType::Png,
        );
        let image_block = ImageBlock::new_with_base64(base64_source);
        let content = Content::from(image_block);

        match content {
            Content::Image(_) => {
                // Conversion successful
            }
            _ => panic!("Expected Image variant"),
        }
    }
}
