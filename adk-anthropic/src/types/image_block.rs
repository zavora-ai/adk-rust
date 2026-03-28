use serde::{Deserialize, Serialize};

use crate::types::{Base64ImageSource, CacheControlEphemeral, FileSource, UrlImageSource};

/// The source type for an image block, which can be either Base64 encoded, a URL, or a file reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ImageSource {
    /// A Base64 encoded image source.
    #[serde(rename = "base64")]
    Base64(Base64ImageSource),

    /// A URL image source.
    #[serde(rename = "url")]
    Url(UrlImageSource),

    /// A file source referencing a server-side file.
    #[serde(rename = "file")]
    File(FileSource),
}

/// Parameters for an image block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageBlock {
    /// The source of the image.
    pub source: ImageSource,

    /// Create a cache control breakpoint at this content block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl ImageBlock {
    /// Create a new `ImageBlock` with the given source.
    pub fn new(source: ImageSource) -> Self {
        Self { source, cache_control: None }
    }

    /// Create a new `ImageBlock` with a Base64 image source.
    pub fn new_with_base64(source: Base64ImageSource) -> Self {
        Self::new(ImageSource::Base64(source))
    }

    /// Create a new `ImageBlock` with a URL image source.
    pub fn new_with_url(source: UrlImageSource) -> Self {
        Self::new(ImageSource::Url(source))
    }

    /// Add a cache control to this image block.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::base64_image_source::ImageMediaType;
    use serde_json::{json, to_value};

    #[test]
    fn image_block_with_base64() {
        let base64_source = Base64ImageSource::new(
            "data:image/jpeg;base64,SGVsbG8gd29ybGQ=".to_string(),
            ImageMediaType::Jpeg,
        );

        let image_block = ImageBlock::new_with_base64(base64_source);
        let json = to_value(&image_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "base64",
                    "data": "data:image/jpeg;base64,SGVsbG8gd29ybGQ=",
                    "media_type": "image/jpeg"
                }
            })
        );
    }

    #[test]
    fn image_block_with_url() {
        let url_source = UrlImageSource::new("https://example.com/image.jpg".to_string());

        let image_block = ImageBlock::new_with_url(url_source);
        let json = to_value(&image_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "url",
                    "url": "https://example.com/image.jpg"
                }
            })
        );
    }

    #[test]
    fn image_block_with_cache_control() {
        let url_source = UrlImageSource::new("https://example.com/image.jpg".to_string());
        let cache_control = CacheControlEphemeral::new();

        let image_block = ImageBlock::new_with_url(url_source).with_cache_control(cache_control);

        let json = to_value(&image_block).unwrap();

        assert_eq!(
            json,
            json!({
                "source": {
                    "type": "url",
                    "url": "https://example.com/image.jpg"
                },
                "cache_control": {
                    "type": "ephemeral"
                }
            })
        );
    }
}
