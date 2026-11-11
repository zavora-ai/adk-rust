//! Property tests for the shared ACP `ContentBlock` <-> `adk_core::Part` mapping.
//!
//! These validate correctness property **P4 — Content mapping round-trip**: for
//! any `ContentBlock` in {text, image, audio, embedded-text, embedded-blob},
//! `block_to_part` followed by `part_to_block` preserves the MIME type and
//! payload (URI preserved for embedded resources).

use adk_acp::content::{block_to_part, part_to_block};
use agent_client_protocol::v1::schema::{
    AudioContent, BlobResourceContents, ContentBlock, EmbeddedResource, EmbeddedResourceResource,
    ImageContent, TextContent, TextResourceContents,
};
use base64::{Engine as _, engine::general_purpose};
use proptest::prelude::*;

/// Non-empty ASCII text that is stable across a JSON/serde round-trip.
fn arb_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.,!?/-]{1,120}"
}

/// Raw binary payloads kept small so the checked blob constructor accepts them.
fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..512)
}

fn arb_uri() -> impl Strategy<Value = String> {
    "file:///[a-z0-9_/-]{1,40}\\.[a-z]{1,5}"
}

fn arb_image_mime() -> impl Strategy<Value = String> {
    prop_oneof![Just("image/png".to_string()), Just("image/jpeg".to_string())]
}

fn arb_audio_mime() -> impl Strategy<Value = String> {
    prop_oneof![Just("audio/mp3".to_string()), Just("audio/wav".to_string())]
}

fn arb_optional_mime() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        Just(Some("text/markdown".to_string())),
        Just(Some("application/octet-stream".to_string())),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// **Feature: acp-v1-full-support, Property P4: Content mapping round-trip (text)**
    /// *For any* text content block, `block_to_part` then `part_to_block`
    /// preserves the text payload.
    /// **Validates: Requirements 2.1, 2.2**
    #[test]
    fn prop_text_block_round_trips(text in arb_text()) {
        let block = ContentBlock::Text(TextContent::new(text.clone()));
        let part = block_to_part(&block).expect("text maps");
        let round_tripped = part_to_block(&part).expect("text maps back");
        match round_tripped {
            ContentBlock::Text(content) => prop_assert_eq!(content.text, text),
            other => prop_assert!(false, "expected text block, got {:?}", other),
        }
    }

    /// **Feature: acp-v1-full-support, Property P4: Content mapping round-trip (image)**
    /// *For any* image content block, `block_to_part` then `part_to_block`
    /// preserves the MIME type and decoded payload.
    /// **Validates: Requirements 2.1, 2.2, 6.1**
    #[test]
    fn prop_image_block_round_trips(
        bytes in arb_bytes(),
        mime in arb_image_mime(),
    ) {
        let encoded = general_purpose::STANDARD.encode(&bytes);
        let block = ContentBlock::Image(ImageContent::new(encoded, mime.clone()));
        let part = block_to_part(&block).expect("image maps");
        let round_tripped = part_to_block(&part).expect("image maps back");
        match round_tripped {
            ContentBlock::Image(content) => {
                prop_assert_eq!(content.mime_type, mime);
                let decoded = general_purpose::STANDARD.decode(content.data).expect("valid base64");
                prop_assert_eq!(decoded, bytes);
            }
            other => prop_assert!(false, "expected image block, got {:?}", other),
        }
    }

    /// **Feature: acp-v1-full-support, Property P4: Content mapping round-trip (audio)**
    /// *For any* audio content block, `block_to_part` then `part_to_block`
    /// preserves the MIME type and decoded payload.
    /// **Validates: Requirements 2.1, 2.2, 6.2**
    #[test]
    fn prop_audio_block_round_trips(
        bytes in arb_bytes(),
        mime in arb_audio_mime(),
    ) {
        let encoded = general_purpose::STANDARD.encode(&bytes);
        let block = ContentBlock::Audio(AudioContent::new(encoded, mime.clone()));
        let part = block_to_part(&block).expect("audio maps");
        let round_tripped = part_to_block(&part).expect("audio maps back");
        match round_tripped {
            ContentBlock::Audio(content) => {
                prop_assert_eq!(content.mime_type, mime);
                let decoded = general_purpose::STANDARD.decode(content.data).expect("valid base64");
                prop_assert_eq!(decoded, bytes);
            }
            other => prop_assert!(false, "expected audio block, got {:?}", other),
        }
    }

    /// **Feature: acp-v1-full-support, Property P4: Content mapping round-trip (embedded text)**
    /// *For any* text embedded-resource block, `block_to_part` then
    /// `part_to_block` preserves the URI, MIME type, and text payload.
    /// **Validates: Requirements 2.1, 2.2, 2.4**
    #[test]
    fn prop_embedded_text_round_trips(
        text in arb_text(),
        uri in arb_uri(),
        mime in arb_optional_mime(),
    ) {
        let mut acp_text = TextResourceContents::new(text.clone(), uri.clone());
        acp_text = acp_text.mime_type(mime.clone());
        let block = ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(acp_text),
        ));
        let part = block_to_part(&block).expect("embedded text maps");
        let round_tripped = part_to_block(&part).expect("embedded text maps back");
        match round_tripped {
            ContentBlock::Resource(resource) => match resource.resource {
                EmbeddedResourceResource::TextResourceContents(content) => {
                    prop_assert_eq!(content.uri, uri);
                    prop_assert_eq!(content.mime_type, mime);
                    prop_assert_eq!(content.text, text);
                }
                other => prop_assert!(false, "expected text resource, got {:?}", other),
            },
            other => prop_assert!(false, "expected resource block, got {:?}", other),
        }
    }

    /// **Feature: acp-v1-full-support, Property P4: Content mapping round-trip (embedded blob)**
    /// *For any* blob embedded-resource block, `block_to_part` then
    /// `part_to_block` preserves the URI, MIME type, and decoded payload.
    /// **Validates: Requirements 2.1, 2.2, 2.5**
    #[test]
    fn prop_embedded_blob_round_trips(
        bytes in arb_bytes(),
        uri in arb_uri(),
        mime in arb_optional_mime(),
    ) {
        let encoded = general_purpose::STANDARD.encode(&bytes);
        let mut acp_blob = BlobResourceContents::new(encoded, uri.clone());
        acp_blob = acp_blob.mime_type(mime.clone());
        let block = ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::BlobResourceContents(acp_blob),
        ));
        let part = block_to_part(&block).expect("embedded blob maps");
        let round_tripped = part_to_block(&part).expect("embedded blob maps back");
        match round_tripped {
            ContentBlock::Resource(resource) => match resource.resource {
                EmbeddedResourceResource::BlobResourceContents(content) => {
                    prop_assert_eq!(content.uri, uri);
                    prop_assert_eq!(content.mime_type, mime);
                    let decoded =
                        general_purpose::STANDARD.decode(content.blob).expect("valid base64");
                    prop_assert_eq!(decoded, bytes);
                }
                other => prop_assert!(false, "expected blob resource, got {:?}", other),
            },
            other => prop_assert!(false, "expected resource block, got {:?}", other),
        }
    }
}
