//! Bidirectional mapping between ACP `ContentBlock`s and [`adk_core::Part`]s.
//!
//! This module is the single source of truth for translating content in both
//! directions, so the server prompt parser, the server streamer, and the client
//! all share one implementation. It is available regardless of the `server`
//! feature so the client direction can reuse it.
//!
//! # Mapping
//!
//! | ACP `ContentBlock`                     | [`adk_core::Part`]                              |
//! |----------------------------------------|-------------------------------------------------|
//! | `Text`                                 | `Part::Text`                                    |
//! | `Image { data, mime_type, uri? }`      | `Part::InlineData { mime_type, data }` (uri dropped) |
//! | `Audio { data, mime_type }`            | `Part::InlineData { mime_type, data }`          |
//! | `ResourceLink`                         | `Part::Text` (human-readable reference)         |
//! | `ResourceLink` (outbound)              | `Part::FileData { mime_type, file_uri }`        |
//! | `Resource { resource: Text }`          | `Part::EmbeddedResource(Text)`                  |
//! | `Resource { resource: Blob }`          | `Part::EmbeddedResource(Blob)` (base64 decode)  |
//!
//! Binary payloads (image, audio, and blob embedded resources) are base64-encoded
//! on the wire and decoded to raw bytes internally. Text embedded resources are
//! preserved verbatim without base64 encoding.

use adk_core::{
    BlobResourceContents, Content, EmbeddedResource, MAX_INLINE_DATA_SIZE, Part,
    TextResourceContents,
};
use agent_client_protocol::schema::v1::{
    AudioContent, BlobResourceContents as AcpBlobResourceContents, ContentBlock,
    EmbeddedResource as AcpEmbeddedResource, EmbeddedResourceResource, ImageContent, ResourceLink,
    TextContent, TextResourceContents as AcpTextResourceContents,
};
use base64::{Engine as _, engine::general_purpose};

use crate::error::AcpError;

const MAX_BASE64_ENCODED_SIZE: usize = MAX_INLINE_DATA_SIZE.div_ceil(3) * 4;

/// Convert an inbound ACP [`ContentBlock`] into an [`adk_core::Part`].
///
/// Binary content (image, audio, and blob embedded resources) is base64-decoded
/// to raw bytes and limited to [`MAX_INLINE_DATA_SIZE`]. Text and text embedded
/// resources are preserved verbatim.
///
/// # Errors
///
/// Returns [`AcpError::Protocol`] if a base64 payload is malformed, if binary
/// content exceeds the maximum inline size, or if the content block type is not
/// supported by this mapping.
///
/// # Example
///
/// ```rust
/// use adk_acp::content::block_to_part;
/// use adk_core::Part;
/// use agent_client_protocol::schema::v1::{ContentBlock, TextContent};
///
/// let block = ContentBlock::Text(TextContent::new("hello"));
/// let part = block_to_part(&block).unwrap();
/// assert!(matches!(part, Part::Text { text } if text == "hello"));
/// ```
pub fn block_to_part(block: &ContentBlock) -> Result<Part, AcpError> {
    match block {
        ContentBlock::Text(text) => Ok(Part::Text { text: text.text.clone() }),
        ContentBlock::Image(image) => {
            let data = decode_base64(&image.data)?;
            Ok(Part::InlineData { mime_type: image.mime_type.clone(), data })
        }
        ContentBlock::Audio(audio) => {
            let data = decode_base64(&audio.data)?;
            Ok(Part::InlineData { mime_type: audio.mime_type.clone(), data })
        }
        ContentBlock::ResourceLink(link) => Ok(Part::Text { text: resource_link_text(link) }),
        ContentBlock::Resource(resource) => embedded_resource_to_part(resource),
        _ => Err(AcpError::Protocol(
            "prompt contains a content type this agent did not advertise".into(),
        )),
    }
}

/// Convert an outbound [`adk_core::Part`] into an ACP [`ContentBlock`].
///
/// Returns [`None`] for parts that have no ACP content-block representation
/// (function calls, thinking traces, and server-tool parts),
/// leaving the caller to map those to the appropriate `SessionUpdate` variant.
///
/// Binary inline data is re-encoded as base64. An inline-data part with an
/// `audio/*` MIME type maps to an [`AudioContent`] block and an `image/*` MIME
/// type maps to an [`ImageContent`] block. Other inline MIME types and binary
/// payloads larger than [`MAX_INLINE_DATA_SIZE`] have no safe ACP content-block
/// representation and return [`None`]. File-data parts map to
/// [`ResourceLink`]s, preserving their URI and MIME type without fetching
/// external content.
///
/// # Example
///
/// ```rust
/// use adk_acp::content::part_to_block;
/// use adk_core::Part;
/// use agent_client_protocol::schema::v1::ContentBlock;
///
/// let part = Part::Text { text: "hi".into() };
/// assert!(matches!(part_to_block(&part), Some(ContentBlock::Text(_))));
/// ```
pub fn part_to_block(part: &Part) -> Option<ContentBlock> {
    match part {
        Part::Text { text } => Some(ContentBlock::Text(TextContent::new(text.clone()))),
        Part::InlineData { data, .. } if data.len() > MAX_INLINE_DATA_SIZE => None,
        Part::InlineData { mime_type, data } if mime_type.starts_with("audio/") => {
            let encoded = general_purpose::STANDARD.encode(data);
            Some(ContentBlock::Audio(AudioContent::new(encoded, mime_type.clone())))
        }
        Part::InlineData { mime_type, data } if mime_type.starts_with("image/") => {
            let encoded = general_purpose::STANDARD.encode(data);
            Some(ContentBlock::Image(ImageContent::new(encoded, mime_type.clone())))
        }
        Part::InlineData { .. } => None,
        Part::FileData { mime_type, file_uri } => {
            let name = file_uri
                .rsplit('/')
                .find(|segment| !segment.is_empty())
                .unwrap_or(file_uri)
                .to_string();
            Some(ContentBlock::ResourceLink(
                ResourceLink::new(name, file_uri.clone()).mime_type(Some(mime_type.clone())),
            ))
        }
        Part::EmbeddedResource { resource } => embedded_resource_to_block(resource),
        _ => None,
    }
}

/// Convert an outbound ADK [`Content`] into the ACP [`ContentBlock`]s that carry
/// it in a prompt.
///
/// Each [`Part`] is routed through [`part_to_block`]. Parts that have no ACP
/// content-block representation (function calls, thinking traces, and
/// server-tool parts) map to [`None`] and are skipped, so text
/// and other representable content is always transmitted rather than dropped.
///
/// This is the client-direction counterpart to the server prompt parser: it lets
/// an ACP client send images, audio, and embedded resources as their matching
/// ACP blocks instead of collapsing everything to text.
///
/// # Example
///
/// ```rust
/// use adk_acp::content::content_to_blocks;
/// use adk_core::{Content, Part};
/// use agent_client_protocol::schema::v1::ContentBlock;
///
/// let mut content = Content::new("user");
/// content.parts.push(Part::Text { text: "describe this".into() });
/// content.parts.push(Part::InlineData { mime_type: "image/png".into(), data: vec![1, 2, 3] });
///
/// let blocks = content_to_blocks(&content);
/// assert_eq!(blocks.len(), 2);
/// assert!(matches!(blocks[0], ContentBlock::Text(_)));
/// assert!(matches!(blocks[1], ContentBlock::Image(_)));
/// ```
pub fn content_to_blocks(content: &Content) -> Vec<ContentBlock> {
    content.parts.iter().filter_map(part_to_block).collect()
}

/// Render a [`ResourceLink`] as a human-readable text reference.
///
/// Mirrors the previous inline behavior in the server prompt parser so that
/// resource links continue to reach the agent as text.
fn resource_link_text(link: &ResourceLink) -> String {
    let description =
        link.description.as_deref().map(|value| format!(" — {value}")).unwrap_or_default();
    format!("Referenced resource: {} ({}){description}", link.name, link.uri)
}

/// Map an inbound ACP embedded resource to an [`adk_core::Part::EmbeddedResource`].
fn embedded_resource_to_part(resource: &AcpEmbeddedResource) -> Result<Part, AcpError> {
    let mapped = match &resource.resource {
        EmbeddedResourceResource::TextResourceContents(text) => EmbeddedResource::Text(
            TextResourceContents::new(text.uri.clone(), text.mime_type.clone(), text.text.clone()),
        ),
        EmbeddedResourceResource::BlobResourceContents(blob) => {
            let data = decode_base64(&blob.blob)?;
            EmbeddedResource::Blob(
                BlobResourceContents::new(blob.uri.clone(), blob.mime_type.clone(), data)
                    .map_err(|error| AcpError::Protocol(error.to_string()))?,
            )
        }
        _ => {
            return Err(AcpError::Protocol("unsupported embedded resource contents".into()));
        }
    };
    Ok(Part::EmbeddedResource { resource: mapped })
}

/// Map an outbound [`adk_core::EmbeddedResource`] to an ACP embedded-resource block.
fn embedded_resource_to_block(resource: &EmbeddedResource) -> Option<ContentBlock> {
    let inner = match resource {
        EmbeddedResource::Text(text) => EmbeddedResourceResource::TextResourceContents(
            AcpTextResourceContents::new(text.text.clone(), text.uri.clone())
                .mime_type(text.mime_type.clone()),
        ),
        EmbeddedResource::Blob(blob) if blob.data.len() > MAX_INLINE_DATA_SIZE => return None,
        EmbeddedResource::Blob(blob) => {
            let encoded = general_purpose::STANDARD.encode(&blob.data);
            EmbeddedResourceResource::BlobResourceContents(
                AcpBlobResourceContents::new(encoded, blob.uri.clone())
                    .mime_type(blob.mime_type.clone()),
            )
        }
    };
    Some(ContentBlock::Resource(AcpEmbeddedResource::new(inner)))
}

/// Decode a base64 payload with an allocation bound derived from the core inline-data limit.
fn decode_base64(data: &str) -> Result<Vec<u8>, AcpError> {
    if data.len() > MAX_BASE64_ENCODED_SIZE {
        return Err(AcpError::Protocol(format!(
            "base64 content encoded length {} exceeds the maximum {MAX_BASE64_ENCODED_SIZE} characters for a {MAX_INLINE_DATA_SIZE}-byte decoded payload",
            data.len(),
        )));
    }

    let decoded = general_purpose::STANDARD
        .decode(data)
        .map_err(|error| AcpError::Protocol(format!("invalid base64 content: {error}")))?;
    if decoded.len() > MAX_INLINE_DATA_SIZE {
        return Err(AcpError::Protocol(format!(
            "decoded base64 content size {} exceeds the maximum {MAX_INLINE_DATA_SIZE} bytes",
            decoded.len(),
        )));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_binary_part_size(part: Part, expected: usize) {
        match part {
            Part::InlineData { data, .. } => assert_eq!(data.len(), expected),
            Part::EmbeddedResource { resource: EmbeddedResource::Blob(blob) } => {
                assert_eq!(blob.data.len(), expected);
            }
            other => panic!("expected binary part, got {other:?}"),
        }
    }

    fn assert_decoded_oversize_rejected(block: &ContentBlock) {
        let error = block_to_part(block).expect_err("oversized binary content must be rejected");
        assert!(
            error.to_string().contains("decoded base64 content size"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn text_block_maps_to_text_part() {
        let block = ContentBlock::Text(TextContent::new("hello world"));
        let part = block_to_part(&block).expect("text maps");
        assert!(matches!(part, Part::Text { text } if text == "hello world"));
    }

    #[test]
    fn image_block_decodes_base64_to_inline_data() {
        let raw = vec![0x89, 0x50, 0x4E, 0x47];
        let encoded = general_purpose::STANDARD.encode(&raw);
        let block = ContentBlock::Image(ImageContent::new(encoded, "image/png"));
        let part = block_to_part(&block).expect("image maps");
        assert!(
            matches!(part, Part::InlineData { mime_type, data } if mime_type == "image/png" && data == raw)
        );
    }

    #[test]
    fn audio_block_decodes_base64_to_inline_data() {
        let raw = vec![1u8, 2, 3, 4, 5];
        let encoded = general_purpose::STANDARD.encode(&raw);
        let block = ContentBlock::Audio(AudioContent::new(encoded, "audio/mp3"));
        let part = block_to_part(&block).expect("audio maps");
        assert!(
            matches!(part, Part::InlineData { mime_type, data } if mime_type == "audio/mp3" && data == raw)
        );
    }

    #[test]
    fn resource_link_maps_to_text_reference() {
        let link = ResourceLink::new("main.rs", "file:///main.rs").description("entry point");
        let block = ContentBlock::ResourceLink(link);
        let part = block_to_part(&block).expect("resource link maps");
        match part {
            Part::Text { text } => {
                assert!(text.contains("main.rs"));
                assert!(text.contains("file:///main.rs"));
                assert!(text.contains("entry point"));
            }
            other => panic!("expected text part, got {other:?}"),
        }
    }

    #[test]
    fn text_embedded_resource_preserves_payload_without_base64() {
        let inner = EmbeddedResourceResource::TextResourceContents(
            AcpTextResourceContents::new("fn main() {}", "file:///main.rs")
                .mime_type(Some("text/x-rust".to_string())),
        );
        let block = ContentBlock::Resource(AcpEmbeddedResource::new(inner));
        let part = block_to_part(&block).expect("text resource maps");
        match part {
            Part::EmbeddedResource { resource: EmbeddedResource::Text(text) } => {
                assert_eq!(text.uri, "file:///main.rs");
                assert_eq!(text.mime_type.as_deref(), Some("text/x-rust"));
                assert_eq!(text.text, "fn main() {}");
            }
            other => panic!("expected text embedded resource, got {other:?}"),
        }
    }

    #[test]
    fn blob_embedded_resource_decodes_base64() {
        let raw = vec![10u8, 20, 30, 40];
        let encoded = general_purpose::STANDARD.encode(&raw);
        let inner = EmbeddedResourceResource::BlobResourceContents(
            AcpBlobResourceContents::new(encoded, "file:///logo.png")
                .mime_type(Some("image/png".to_string())),
        );
        let block = ContentBlock::Resource(AcpEmbeddedResource::new(inner));
        let part = block_to_part(&block).expect("blob resource maps");
        match part {
            Part::EmbeddedResource { resource: EmbeddedResource::Blob(blob) } => {
                assert_eq!(blob.uri, "file:///logo.png");
                assert_eq!(blob.mime_type.as_deref(), Some("image/png"));
                assert_eq!(blob.data, raw);
            }
            other => panic!("expected blob embedded resource, got {other:?}"),
        }
    }

    #[test]
    fn malformed_base64_is_rejected_for_every_binary_block() {
        let image = ContentBlock::Image(ImageContent::new("not*base64", "image/png"));
        assert!(block_to_part(&image).is_err());

        let audio = ContentBlock::Audio(AudioContent::new("not*base64", "audio/wav"));
        assert!(block_to_part(&audio).is_err());

        let blob = EmbeddedResourceResource::BlobResourceContents(AcpBlobResourceContents::new(
            "not*base64",
            "file:///data.bin",
        ));
        assert!(block_to_part(&ContentBlock::Resource(AcpEmbeddedResource::new(blob))).is_err());
    }

    #[test]
    fn binary_blocks_accept_the_exact_inline_data_limit() {
        let image = ContentBlock::Image(ImageContent::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE]),
            "image/png",
        ));
        assert_binary_part_size(
            block_to_part(&image).expect("image at the limit maps"),
            MAX_INLINE_DATA_SIZE,
        );

        let audio = ContentBlock::Audio(AudioContent::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE]),
            "audio/wav",
        ));
        assert_binary_part_size(
            block_to_part(&audio).expect("audio at the limit maps"),
            MAX_INLINE_DATA_SIZE,
        );

        let blob = EmbeddedResourceResource::BlobResourceContents(AcpBlobResourceContents::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE]),
            "file:///data.bin",
        ));
        assert_binary_part_size(
            block_to_part(&ContentBlock::Resource(AcpEmbeddedResource::new(blob)))
                .expect("blob at the limit maps"),
            MAX_INLINE_DATA_SIZE,
        );
    }

    #[test]
    fn binary_blocks_reject_a_decoded_payload_over_the_inline_data_limit() {
        let image = ContentBlock::Image(ImageContent::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE + 1]),
            "image/png",
        ));
        assert_decoded_oversize_rejected(&image);

        let audio = ContentBlock::Audio(AudioContent::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE + 1]),
            "audio/wav",
        ));
        assert_decoded_oversize_rejected(&audio);

        let blob = EmbeddedResourceResource::BlobResourceContents(AcpBlobResourceContents::new(
            general_purpose::STANDARD.encode(vec![0_u8; MAX_INLINE_DATA_SIZE + 1]),
            "file:///data.bin",
        ));
        assert_decoded_oversize_rejected(&ContentBlock::Resource(AcpEmbeddedResource::new(blob)));
    }

    #[test]
    fn impossible_encoded_size_is_rejected_before_base64_decode() {
        let block = ContentBlock::Image(ImageContent::new(
            "A".repeat(MAX_BASE64_ENCODED_SIZE + 1),
            "image/png",
        ));
        let error = block_to_part(&block).expect_err("impossible encoded size must be rejected");
        assert!(
            error.to_string().contains("base64 content encoded length"),
            "unexpected error: {error}",
        );
    }

    #[test]
    fn text_part_maps_to_text_block() {
        let part = Part::Text { text: "hi".into() };
        assert!(matches!(part_to_block(&part), Some(ContentBlock::Text(_))));
    }

    #[test]
    fn inline_audio_part_maps_to_audio_block() {
        let part = Part::InlineData { mime_type: "audio/wav".into(), data: vec![1, 2, 3] };
        match part_to_block(&part) {
            Some(ContentBlock::Audio(audio)) => {
                assert_eq!(audio.mime_type, "audio/wav");
                assert_eq!(general_purpose::STANDARD.decode(audio.data).unwrap(), vec![1, 2, 3]);
            }
            other => panic!("expected audio block, got {other:?}"),
        }
    }

    #[test]
    fn inline_image_part_maps_to_image_block() {
        let part = Part::InlineData { mime_type: "image/png".into(), data: vec![9, 8, 7] };
        match part_to_block(&part) {
            Some(ContentBlock::Image(image)) => {
                assert_eq!(image.mime_type, "image/png");
                assert_eq!(general_purpose::STANDARD.decode(image.data).unwrap(), vec![9, 8, 7]);
            }
            other => panic!("expected image block, got {other:?}"),
        }
    }

    #[test]
    fn outbound_binary_mapping_rejects_unsupported_or_oversized_payloads() {
        let unsupported =
            Part::InlineData { mime_type: "application/octet-stream".into(), data: vec![1, 2, 3] };
        assert!(part_to_block(&unsupported).is_none());

        let oversized = Part::InlineData {
            mime_type: "image/png".into(),
            data: vec![0; MAX_INLINE_DATA_SIZE + 1],
        };
        assert!(part_to_block(&oversized).is_none());

        let oversized_blob = Part::EmbeddedResource {
            resource: EmbeddedResource::Blob(BlobResourceContents {
                uri: "file:///data.bin".into(),
                mime_type: Some("application/octet-stream".into()),
                data: vec![0; MAX_INLINE_DATA_SIZE + 1],
            }),
        };
        assert!(part_to_block(&oversized_blob).is_none());
    }

    #[test]
    fn file_data_part_maps_to_resource_link() {
        let part = Part::FileData {
            mime_type: "application/pdf".into(),
            file_uri: "https://example.com/reports/quarterly.pdf".into(),
        };
        match part_to_block(&part) {
            Some(ContentBlock::ResourceLink(link)) => {
                assert_eq!(link.name, "quarterly.pdf");
                assert_eq!(link.uri, "https://example.com/reports/quarterly.pdf");
                assert_eq!(link.mime_type.as_deref(), Some("application/pdf"));
            }
            other => panic!("expected resource-link block, got {other:?}"),
        }
    }

    #[test]
    fn function_call_part_has_no_block_representation() {
        let part = Part::FunctionCall {
            name: "tool".into(),
            args: serde_json::json!({}),
            id: None,
            thought_signature: None,
        };
        assert!(part_to_block(&part).is_none());
    }

    #[test]
    fn embedded_text_part_round_trips_to_block_and_back() {
        let part = Part::EmbeddedResource {
            resource: EmbeddedResource::Text(TextResourceContents::new(
                "file:///notes.md",
                Some("text/markdown".to_string()),
                "# Notes",
            )),
        };
        let block = part_to_block(&part).expect("maps to block");
        let round_tripped = block_to_part(&block).expect("maps back");
        assert_eq!(round_tripped, part);
    }

    #[test]
    fn content_to_blocks_maps_rich_content_and_skips_unrepresentable_parts() {
        let mut content = Content::new("user");
        content.parts.push(Part::Text { text: "describe these".into() });
        content.parts.push(Part::InlineData { mime_type: "image/png".into(), data: vec![1, 2, 3] });
        content.parts.push(Part::InlineData { mime_type: "audio/wav".into(), data: vec![4, 5, 6] });
        content.parts.push(Part::FileData {
            mime_type: "application/pdf".into(),
            file_uri: "file:///reports/result.pdf".into(),
        });
        content.parts.push(Part::EmbeddedResource {
            resource: EmbeddedResource::Text(TextResourceContents::new(
                "file:///notes.md",
                Some("text/markdown".to_string()),
                "# Notes",
            )),
        });
        // Not representable as a ContentBlock — must be skipped, not transmitted.
        content.parts.push(Part::FunctionCall {
            name: "tool".into(),
            args: serde_json::json!({}),
            id: None,
            thought_signature: None,
        });

        let blocks = content_to_blocks(&content);
        assert_eq!(blocks.len(), 5, "the function-call part must be skipped");
        assert!(matches!(blocks[0], ContentBlock::Text(_)));
        assert!(matches!(blocks[1], ContentBlock::Image(_)));
        assert!(matches!(blocks[2], ContentBlock::Audio(_)));
        assert!(matches!(blocks[3], ContentBlock::ResourceLink(_)));
        assert!(matches!(blocks[4], ContentBlock::Resource(_)));
    }

    #[test]
    fn content_to_blocks_preserves_text() {
        let mut content = Content::new("user");
        content.parts.push(Part::Text { text: "just text".into() });
        let blocks = content_to_blocks(&content);
        match &blocks[..] {
            [ContentBlock::Text(text)] => assert_eq!(text.text, "just text"),
            other => panic!("expected a single text block, got {other:?}"),
        }
    }

    #[test]
    fn embedded_blob_part_round_trips_to_block_and_back() {
        let part = Part::EmbeddedResource {
            resource: EmbeddedResource::Blob(
                BlobResourceContents::new(
                    "file:///data.bin",
                    Some("application/octet-stream".to_string()),
                    vec![0, 1, 2, 3, 255],
                )
                .unwrap(),
            ),
        };
        let block = part_to_block(&part).expect("maps to block");
        let round_tripped = block_to_part(&block).expect("maps back");
        assert_eq!(round_tripped, part);
    }
}
