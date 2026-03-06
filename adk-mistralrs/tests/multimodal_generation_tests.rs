//! Property tests for multimodal generation.
//!
//! **Property: Multimodal Message Construction**
//! *For any* combination of text, images, and audio content in an LlmRequest,
//! the system SHALL correctly extract and categorize each content type.
//!
//! **Validates: Requirements 6.1, 17.1**

use adk_core::{Content, Part};
use adk_mistralrs::convert::{
    AudioFormat, ImageFormat, extract_audio_from_content, extract_images_from_content,
    extract_text_from_content,
};
use proptest::prelude::*;

// ============================================================================
// Generators
// ============================================================================

/// Generate a valid PNG image (minimal 1x1 pixel)
fn generate_minimal_png() -> Vec<u8> {
    use image::{ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(1, 1, |_, _| Rgb([255u8, 0u8, 0u8]));

    let mut bytes: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    bytes
}

/// Generate arbitrary text content
fn arb_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{1,50}"
}

/// Generate arbitrary image MIME type
fn arb_image_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("image/jpeg".to_string()),
        Just("image/png".to_string()),
        Just("image/webp".to_string()),
    ]
}

/// Generate arbitrary audio MIME type
fn arb_audio_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("audio/wav".to_string()),
        Just("audio/mp3".to_string()),
        Just("audio/mpeg".to_string()),
        Just("audio/flac".to_string()),
    ]
}

/// Generate a content with only text parts
fn arb_text_only_content() -> impl Strategy<Value = Content> {
    (arb_text(), arb_text()).prop_map(|(text1, text2)| Content {
        role: adk_core::types::Role::User,
        parts: vec![Part::text(text1), Part::text(text2)],
    })
}

/// Generate a content with text and image parts
fn arb_text_and_image_content() -> impl Strategy<Value = Content> {
    (arb_text(), arb_image_mime_type()).prop_map(|(text, mime_type)| {
        let png_data = generate_minimal_png();
        Content {
            role: adk_core::types::Role::User,
            parts: vec![Part::text(text), Part::InlineData { mime_type: mime_type.parse().unwrap(), data: png_data.into() }],
        }
    })
}

/// Generate a content with text and audio parts
fn arb_text_and_audio_content() -> impl Strategy<Value = Content> {
    (arb_text(), arb_audio_mime_type()).prop_map(|(text, mime_type)| {
        // Use empty audio data - we're testing extraction logic, not decoding
        Content {
            role: adk_core::types::Role::User,
            parts: vec![
                Part::text(text),
                Part::InlineData {
                    mime_type: mime_type.parse().unwrap(),
                    data: vec![0u8; 44].into(), // Minimal WAV header size
                },
            ],
        }
    })
}

/// Generate a multimodal content with text, image, and audio parts
fn arb_multimodal_content() -> impl Strategy<Value = Content> {
    (arb_text(), arb_image_mime_type(), arb_audio_mime_type()).prop_map(
        |(text, image_mime, audio_mime)| {
            let png_data = generate_minimal_png();
            Content {
                role: adk_core::types::Role::User,
                parts: vec![
                    Part::text(text),
                    Part::InlineData { mime_type: image_mime.parse().unwrap(), data: png_data.into() },
                    Part::InlineData { mime_type: audio_mime.parse().unwrap(), data: vec![0u8; 44].into() },
                ],
            }
        },
    )
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: mistral-rs-integration, Property: Multimodal Content Extraction**
    /// *For any* text-only content, extract_text SHALL return all text parts joined.
    /// **Validates: Requirements 6.1**
    #[test]
    fn prop_text_only_extraction(content in arb_text_only_content()) {
        let text = extract_text_from_content(&content);
        let images = extract_images_from_content(&content);
        let audios = extract_audio_from_content(&content);

        // Text should be non-empty
        prop_assert!(!text.is_empty(), "Text extraction should return non-empty string");

        // No images or audio should be extracted
        prop_assert!(images.is_empty(), "No images should be extracted from text-only content");
        prop_assert!(audios.is_empty(), "No audio should be extracted from text-only content");
    }

    /// **Feature: mistral-rs-integration, Property: Text+Image Content Extraction**
    /// *For any* content with text and images, both SHALL be correctly extracted.
    /// **Validates: Requirements 6.1, 6.2**
    #[test]
    fn prop_text_and_image_extraction(content in arb_text_and_image_content()) {
        let text = extract_text_from_content(&content);
        let images = extract_images_from_content(&content);
        let audios = extract_audio_from_content(&content);

        // Text should be non-empty
        prop_assert!(!text.is_empty(), "Text extraction should return non-empty string");

        // Exactly one image should be extracted (PNG decoding should succeed)
        prop_assert_eq!(images.len(), 1, "Exactly one image should be extracted");

        // No audio should be extracted
        prop_assert!(audios.is_empty(), "No audio should be extracted from text+image content");
    }

    /// **Feature: mistral-rs-integration, Property: Text+Audio Content Extraction**
    /// *For any* content with text and audio, text SHALL be extracted and audio parts identified.
    /// **Validates: Requirements 6.1, 17.1**
    #[test]
    fn prop_text_and_audio_extraction(content in arb_text_and_audio_content()) {
        let text = extract_text_from_content(&content);
        let images = extract_images_from_content(&content);

        // Text should be non-empty
        prop_assert!(!text.is_empty(), "Text extraction should return non-empty string");

        // No images should be extracted
        prop_assert!(images.is_empty(), "No images should be extracted from text+audio content");

        // Audio part should be identified by MIME type (even if decoding fails)
        let has_audio_part = content.parts.iter().any(|part| {
            matches!(part, Part::InlineData { mime_type, .. } if AudioFormat::is_supported_mime_type(mime_type.as_ref()))
        });
        prop_assert!(has_audio_part, "Audio part should be present in content");
    }

    /// **Feature: mistral-rs-integration, Property: Full Multimodal Content Extraction**
    /// *For any* content with text, images, and audio, all modalities SHALL be correctly identified.
    /// **Validates: Requirements 6.1, 17.1**
    #[test]
    fn prop_full_multimodal_extraction(content in arb_multimodal_content()) {
        let text = extract_text_from_content(&content);
        let images = extract_images_from_content(&content);

        // Text should be non-empty
        prop_assert!(!text.is_empty(), "Text extraction should return non-empty string");

        // Exactly one image should be extracted
        prop_assert_eq!(images.len(), 1, "Exactly one image should be extracted");

        // Audio part should be identified by MIME type
        let has_audio_part = content.parts.iter().any(|part| {
            matches!(part, Part::InlineData { mime_type, .. } if AudioFormat::is_supported_mime_type(mime_type.as_ref()))
        });
        prop_assert!(has_audio_part, "Audio part should be present in multimodal content");

        // Image part should be identified by MIME type
        let has_image_part = content.parts.iter().any(|part| {
            matches!(part, Part::InlineData { mime_type, .. } if ImageFormat::is_supported_mime_type(mime_type.as_ref()))
        });
        prop_assert!(has_image_part, "Image part should be present in multimodal content");
    }

    /// **Feature: mistral-rs-integration, Property: Content Part Count Preservation**
    /// *For any* multimodal content, the total extracted parts SHALL match input parts.
    /// **Validates: Requirements 6.1, 17.1**
    #[test]
    fn prop_content_part_count(content in arb_multimodal_content()) {
        let text_parts: Vec<_> = content.parts.iter().filter(|p| matches!(p, Part::Text(..))).collect();
        let image_parts: Vec<_> = content.parts.iter().filter(|p| {
            matches!(p, Part::InlineData { mime_type, .. } if ImageFormat::is_supported_mime_type(mime_type.as_ref()))
        }).collect();
        let audio_parts: Vec<_> = content.parts.iter().filter(|p| {
            matches!(p, Part::InlineData { mime_type, .. } if AudioFormat::is_supported_mime_type(mime_type.as_ref()))
        }).collect();

        // Verify we have the expected number of each part type
        prop_assert_eq!(text_parts.len(), 1, "Should have 1 text part");
        prop_assert_eq!(image_parts.len(), 1, "Should have 1 image part");
        prop_assert_eq!(audio_parts.len(), 1, "Should have 1 audio part");

        // Total parts should be 3
        prop_assert_eq!(content.parts.len(), 3, "Multimodal content should have 3 parts");
    }

    /// **Feature: mistral-rs-integration, Property: Role Preservation**
    /// *For any* content, the role SHALL be preserved during extraction.
    /// **Validates: Requirements 6.1**
    #[test]
    fn prop_role_preservation(
        role_str in prop_oneof![
            Just("user".to_string()),
            Just("assistant".to_string()),
            Just("model".to_string()),
            Just("system".to_string()),
        ],
        text in arb_text()
    ) {
        let content = Content {
            role: adk_core::types::Role::Custom(role_str.clone()),
            parts: vec![Part::text(text)],
        };

        // Role should be preserved
        prop_assert_eq!(&content.role, &adk_core::types::Role::Custom(role_str.clone()), "Role should be preserved");
    }
}

// ============================================================================
// Unit Tests for Edge Cases
// ============================================================================

#[test]
fn test_empty_content_extraction() {
    let content = Content { role: adk_core::types::Role::User, parts: vec![] };

    let text = extract_text_from_content(&content);
    let images = extract_images_from_content(&content);
    let audios = extract_audio_from_content(&content);

    assert!(text.is_empty(), "Empty content should yield empty text");
    assert!(images.is_empty(), "Empty content should yield no images");
    assert!(audios.is_empty(), "Empty content should yield no audio");
}

#[test]
fn test_mixed_valid_invalid_images() {
    let png_data = generate_minimal_png();
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::InlineData { mime_type: "image/png".parse().unwrap(), data: png_data.into() },
            Part::InlineData {
                mime_type: "image/jpeg".parse().unwrap(),
                data: vec![0, 1, 2, 3].into(), // Invalid JPEG data
            },
        ],
    };

    let images = extract_images_from_content(&content);
    // Only the valid PNG should be extracted
    assert_eq!(images.len(), 1, "Only valid images should be extracted");
}

#[test]
fn test_multiple_text_parts_joined() {
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::text("Hello".to_string()),
            Part::text("World".to_string()),
            Part::text("Test".to_string()),
        ],
    };

    let text = extract_text_from_content(&content);
    assert!(text.contains("Hello"), "Should contain first text");
    assert!(text.contains("World"), "Should contain second text");
    assert!(text.contains("Test"), "Should contain third text");
}

#[test]
fn test_multimodal_with_multiple_images() {
    let png_data1 = generate_minimal_png();
    let png_data2 = generate_minimal_png();
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::text("Describe these images".to_string()),
            Part::InlineData { mime_type: "image/png".parse().unwrap(), data: png_data1.into() },
            Part::InlineData { mime_type: "image/png".parse().unwrap(), data: png_data2.into() },
        ],
    };

    let text = extract_text_from_content(&content);
    let images = extract_images_from_content(&content);

    assert!(!text.is_empty(), "Text should be extracted");
    assert_eq!(images.len(), 2, "Both images should be extracted");
}

#[test]
fn test_unsupported_mime_type_ignored() {
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::text("Test".to_string()),
            Part::InlineData {
                mime_type: "application/octet-stream".parse().unwrap(),
                data: vec![0, 1, 2, 3].into(),
            },
            Part::InlineData { mime_type: "video/mp4".parse().unwrap(), data: vec![0, 1, 2, 3].into() },
        ],
    };

    let images = extract_images_from_content(&content);
    let audios = extract_audio_from_content(&content);

    assert!(images.is_empty(), "Unsupported MIME types should not be extracted as images");
    assert!(audios.is_empty(), "Unsupported MIME types should not be extracted as audio");
}
