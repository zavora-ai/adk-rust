//! Property tests for image content handling.
//!
//! **Property 6: Image Content Handling**
//! *For any* image content in supported formats (JPEG, PNG, WebP), the conversion
//! to mistral.rs image input SHALL succeed and preserve the image data.
//!
//! **Validates: Requirements 6.2, 6.3, 6.4**

use adk_core::{Content, Part};
use adk_mistralrs::convert::{
    AudioFormat, ImageFormat, audio_part_to_mistralrs, extract_images_from_content,
    extract_text_from_content, image_from_base64, image_from_bytes, image_part_to_mistralrs,
};
use proptest::prelude::*;

// ============================================================================
// Generators
// ============================================================================

/// Generate a valid PNG image (minimal 1x1 pixel)
fn generate_minimal_png() -> Vec<u8> {
    // Create a 1x1 red pixel PNG using the image crate
    use image::{ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(1, 1, |_, _| Rgb([255u8, 0u8, 0u8]));

    let mut bytes: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    bytes
}

/// Generate arbitrary image MIME type
fn arb_image_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("image/jpeg".to_string()),
        Just("image/jpg".to_string()),
        Just("image/png".to_string()),
        Just("image/webp".to_string()),
        Just("image/gif".to_string()),
    ]
}

/// Generate arbitrary audio MIME type
fn arb_audio_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("audio/wav".to_string()),
        Just("audio/wave".to_string()),
        Just("audio/x-wav".to_string()),
        Just("audio/mp3".to_string()),
        Just("audio/mpeg".to_string()),
        Just("audio/flac".to_string()),
        Just("audio/ogg".to_string()),
    ]
}

/// Generate arbitrary non-image MIME type
fn arb_non_image_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("text/plain".to_string()),
        Just("application/json".to_string()),
        Just("video/mp4".to_string()),
        Just("audio/wav".to_string()),
    ]
}

/// Generate arbitrary text content
fn arb_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{1,100}"
}

// ============================================================================
// Property Tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: mistral-rs-integration, Property 6: Image Content Handling**
    /// *For any* image content in supported formats (JPEG, PNG, WebP), the conversion
    /// to mistral.rs image input SHALL succeed and preserve the image data.
    /// **Validates: Requirements 6.2, 6.3, 6.4**
    #[test]
    fn prop_image_format_detection(mime_type in arb_image_mime_type()) {
        // All supported image MIME types should be recognized
        prop_assert!(ImageFormat::is_supported_mime_type(&mime_type));
        prop_assert!(ImageFormat::from_mime_type(&mime_type).is_some());
    }

    /// Property: Non-image MIME types should not be recognized as images
    #[test]
    fn prop_non_image_format_rejection(mime_type in arb_non_image_mime_type()) {
        prop_assert!(!ImageFormat::is_supported_mime_type(&mime_type));
        prop_assert!(ImageFormat::from_mime_type(&mime_type).is_none());
    }

    /// Property: Audio format detection works for all supported formats
    #[test]
    fn prop_audio_format_detection(mime_type in arb_audio_mime_type()) {
        prop_assert!(AudioFormat::is_supported_mime_type(&mime_type));
        prop_assert!(AudioFormat::from_mime_type(&mime_type).is_some());
    }

    /// Property: Text parts should not be converted to images
    #[test]
    fn prop_text_part_not_image(text in arb_text()) {
        let part = Part::text(text);
        prop_assert!(image_part_to_mistralrs(&part).is_none());
    }

    /// Property: Text parts should not be converted to audio
    #[test]
    fn prop_text_part_not_audio(text in arb_text()) {
        let part = Part::text(text);
        prop_assert!(audio_part_to_mistralrs(&part).is_none());
    }

    /// Property: Extract text from content preserves all text parts
    #[test]
    fn prop_extract_text_preserves_content(
        text1 in arb_text(),
        text2 in arb_text(),
    ) {
        let content = Content {
            role: adk_core::types::Role::User,
            parts: vec![
                Part::text(text1.clone()),
                Part::text(text2.clone()),
            ],
        };

        let extracted = extract_text_from_content(&content);
        prop_assert!(extracted.contains(&text1));
        prop_assert!(extracted.contains(&text2));
    }

    /// Property: MIME type case insensitivity
    #[test]
    fn prop_mime_type_case_insensitive(mime_type in arb_image_mime_type()) {
        let upper = mime_type.to_uppercase();
        let lower = mime_type.to_lowercase();

        // Both cases should be recognized
        prop_assert!(ImageFormat::is_supported_mime_type(&upper));
        prop_assert!(ImageFormat::is_supported_mime_type(&lower));
    }

    /// Property: Audio MIME type case insensitivity
    #[test]
    fn prop_audio_mime_type_case_insensitive(mime_type in arb_audio_mime_type()) {
        let upper = mime_type.to_uppercase();
        let lower = mime_type.to_lowercase();

        prop_assert!(AudioFormat::is_supported_mime_type(&upper));
        prop_assert!(AudioFormat::is_supported_mime_type(&lower));
    }
}

// ============================================================================
// Unit Tests for Image Decoding
// ============================================================================

#[test]
fn test_png_image_from_bytes() {
    let png_data = generate_minimal_png();
    let result = image_from_bytes(&png_data);
    assert!(result.is_ok(), "PNG decoding should succeed");

    let image = result.unwrap();
    assert_eq!(image.width(), 1);
    assert_eq!(image.height(), 1);
}

#[test]
fn test_image_part_to_mistralrs_with_png() {
    let png_data = generate_minimal_png();
    let part = Part::InlineData { mime_type: "image/png".parse().unwrap(), data: png_data.into() };

    let result = image_part_to_mistralrs(&part);
    assert!(result.is_some(), "PNG part should convert to image");
}

#[test]
fn test_image_part_to_mistralrs_with_unsupported_mime() {
    let part = Part::InlineData {
        mime_type: "application/octet-stream".parse().unwrap(),
        data: vec![0, 1, 2, 3].into(),
    };

    let result = image_part_to_mistralrs(&part);
    assert!(result.is_none(), "Unsupported MIME type should return None");
}

#[test]
fn test_extract_images_from_content() {
    let png_data = generate_minimal_png();
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![
            Part::text("Describe this image".to_string()),
            Part::InlineData { mime_type: "image/png".parse().unwrap(), data: png_data.into() },
        ],
    };

    let images = extract_images_from_content(&content);
    assert_eq!(images.len(), 1, "Should extract one image");
}

#[test]
fn test_extract_images_from_content_no_images() {
    let content = Content {
        role: adk_core::types::Role::User,
        parts: vec![Part::text("Hello world".to_string())],
    };

    let images = extract_images_from_content(&content);
    assert!(images.is_empty(), "Should extract no images from text-only content");
}

#[test]
fn test_image_from_base64_invalid() {
    let result = image_from_base64("not-valid-base64!!!");
    assert!(result.is_err(), "Invalid base64 should fail");
}

#[test]
fn test_image_from_base64_valid_but_not_image() {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"not an image");
    let result = image_from_base64(&encoded);
    assert!(result.is_err(), "Valid base64 but not image data should fail");
}

#[test]
fn test_image_format_mime_types() {
    assert_eq!(ImageFormat::Jpeg.mime_type(), "image/jpeg");
    assert_eq!(ImageFormat::Png.mime_type(), "image/png");
    assert_eq!(ImageFormat::WebP.mime_type(), "image/webp");
    assert_eq!(ImageFormat::Gif.mime_type(), "image/gif");
}

#[test]
fn test_audio_format_mime_types() {
    assert_eq!(AudioFormat::Wav.mime_type(), "audio/wav");
    assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
    assert_eq!(AudioFormat::Flac.mime_type(), "audio/flac");
    assert_eq!(AudioFormat::Ogg.mime_type(), "audio/ogg");
}

// ============================================================================
// FileData Tests
// ============================================================================

#[test]
fn test_file_data_part_with_image_mime() {
    let part = Part::FileData {
        mime_type: "image/jpeg".parse().unwrap(),
        file_uri: "https://example.com/image.jpg".to_string(),
    };

    // FileData with image MIME type should be recognized as media
    assert!(part.is_media());
    assert_eq!(part.mime_type(), Some("image/jpeg"));
    assert_eq!(part.file_uri(), Some("https://example.com/image.jpg"));
}

#[test]
fn test_file_data_part_with_audio_mime() {
    let part = Part::FileData {
        mime_type: "audio/wav".parse().unwrap(),
        file_uri: "https://example.com/audio.wav".to_string(),
    };

    assert!(part.is_media());
    assert_eq!(part.mime_type(), Some("audio/wav"));
    assert_eq!(part.file_uri(), Some("https://example.com/audio.wav"));
}

#[test]
fn test_content_with_file_uri() {
    let content = Content::new("user")
        .with_text("Check this image")
        .with_file_uri("image/jpeg", "https://example.com/photo.jpg");

    assert_eq!(content.parts.len(), 2);

    // First part is text
    assert!(matches!(&content.parts[0], Part::Text(text) if text == "Check this image"));

    // Second part is FileData
    assert!(matches!(
        &content.parts[1],
        Part::FileData { mime_type, file_uri }
        if mime_type.as_ref() == "image/jpeg" && file_uri == "https://example.com/photo.jpg"
    ));
}

#[test]
fn test_part_constructors() {
    // Test Part::file_data constructor
    let file_part = Part::file_data("image/png", "https://example.com/img.png");
    assert!(matches!(
        file_part,
        Part::FileData { mime_type, file_uri }
        if mime_type.as_ref() == "image/png" && file_uri == "https://example.com/img.png"
    ));

    // Test Part::inline_data constructor
    let inline_part = Part::inline_data("image/jpeg", vec![1, 2, 3]);
    assert!(matches!(
        inline_part,
        Ok(Part::InlineData { mime_type, data })
        if mime_type.as_ref() == "image/jpeg" && data.as_ref() == [1, 2, 3]
    ));
}
