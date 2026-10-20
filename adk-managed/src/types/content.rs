//! Content block types for managed agent messages.
//!
//! Defines [`ContentBlock`], the union type for message content within
//! the managed agent runtime.

use serde::{Deserialize, Serialize};

/// Content within a message. Forward-compatible: unknown types are opaque.
///
/// Each variant serializes with a `"type"` discriminator tag using snake_case
/// naming. The enum is `#[non_exhaustive]` to allow additive evolution.
///
/// # Wire Shapes
///
/// ```json
/// {"type": "text", "text": "Hello, world!"}
/// {"type": "image", "source": {"url": "https://example.com/img.png"}}
/// {"type": "file", "file_id": "file_abc123"}
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::ContentBlock;
///
/// let block = ContentBlock::Text { text: "Hello".to_string() };
/// let json = serde_json::to_string(&block).unwrap();
/// assert!(json.contains(r#""type":"text""#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentBlock {
    /// Plain text content.
    Text {
        /// The text content.
        text: String,
    },
    /// Image content.
    Image {
        /// Image source descriptor (opaque JSON value for forward-compatibility).
        source: serde_json::Value,
    },
    /// File reference.
    File {
        /// The file identifier.
        file_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_text_serialization_round_trip() {
        let block = ContentBlock::Text { text: "Hello, world!".to_string() };

        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(
            serialized,
            json!({
                "type": "text",
                "text": "Hello, world!"
            })
        );

        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::Text { text } => assert_eq!(text, "Hello, world!"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn test_image_serialization_round_trip() {
        let source = json!({"url": "https://example.com/img.png", "media_type": "image/png"});
        let block = ContentBlock::Image { source: source.clone() };

        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(
            serialized,
            json!({
                "type": "image",
                "source": {"url": "https://example.com/img.png", "media_type": "image/png"}
            })
        );

        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::Image { source: deserialized_source } => {
                assert_eq!(deserialized_source, source);
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn test_file_serialization_round_trip() {
        let block = ContentBlock::File { file_id: "file_abc123".to_string() };

        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(
            serialized,
            json!({
                "type": "file",
                "file_id": "file_abc123"
            })
        );

        let deserialized: ContentBlock = serde_json::from_value(serialized).unwrap();
        match deserialized {
            ContentBlock::File { file_id } => assert_eq!(file_id, "file_abc123"),
            _ => panic!("Expected File variant"),
        }
    }

    #[test]
    fn test_text_from_json_string() {
        let json_str = r#"{"type": "text", "text": "sample text"}"#;
        let block: ContentBlock = serde_json::from_str(json_str).unwrap();
        match block {
            ContentBlock::Text { text } => assert_eq!(text, "sample text"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn test_image_from_json_string() {
        let json_str =
            r#"{"type": "image", "source": {"url": "https://cdn.example.com/photo.jpg"}}"#;
        let block: ContentBlock = serde_json::from_str(json_str).unwrap();
        match block {
            ContentBlock::Image { source } => {
                assert_eq!(source["url"], "https://cdn.example.com/photo.jpg");
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn test_file_from_json_string() {
        let json_str = r#"{"type": "file", "file_id": "file_xyz789"}"#;
        let block: ContentBlock = serde_json::from_str(json_str).unwrap();
        match block {
            ContentBlock::File { file_id } => assert_eq!(file_id, "file_xyz789"),
            _ => panic!("Expected File variant"),
        }
    }

    #[test]
    fn test_unknown_type_rejected() {
        let json_str = r#"{"type": "video", "url": "https://example.com/vid.mp4"}"#;
        let result: Result<ContentBlock, _> = serde_json::from_str(json_str);
        assert!(result.is_err(), "Unknown type should be rejected");
    }

    #[test]
    fn test_vec_content_blocks_round_trip() {
        let blocks = vec![
            ContentBlock::Text { text: "Here is an image:".to_string() },
            ContentBlock::Image { source: json!({"url": "https://example.com/img.png"}) },
            ContentBlock::File { file_id: "attachment_001".to_string() },
        ];

        let serialized = serde_json::to_value(&blocks).unwrap();
        let deserialized: Vec<ContentBlock> = serde_json::from_value(serialized).unwrap();

        assert_eq!(deserialized.len(), 3);
        match &deserialized[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Here is an image:"),
            _ => panic!("Expected Text"),
        }
        match &deserialized[1] {
            ContentBlock::Image { source } => {
                assert_eq!(source["url"], "https://example.com/img.png");
            }
            _ => panic!("Expected Image"),
        }
        match &deserialized[2] {
            ContentBlock::File { file_id } => assert_eq!(file_id, "attachment_001"),
            _ => panic!("Expected File"),
        }
    }

    #[test]
    fn test_debug_impl() {
        let block = ContentBlock::Text { text: "test".to_string() };
        let debug_str = format!("{block:?}");
        assert!(debug_str.contains("Text"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_clone_impl() {
        let block = ContentBlock::Image { source: json!({"url": "https://example.com/img.png"}) };
        let cloned = block.clone();
        let original_json = serde_json::to_value(&block).unwrap();
        let cloned_json = serde_json::to_value(&cloned).unwrap();
        assert_eq!(original_json, cloned_json);
    }
}
