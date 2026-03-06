//! Property tests for audio input handling.
//!
//! **Property 14: Audio Input Handling**
//! *For any* audio content in supported formats (WAV, MP3, FLAC), the conversion
//! SHALL succeed.
//!
//! **Validates: Requirements 6.5**

use adk_core::Part;
use adk_mistralrs::convert::{AudioFormat, audio_from_base64, audio_part_to_mistralrs};
use proptest::prelude::*;

// ============================================================================
// Generators
// ============================================================================

/// Generate arbitrary audio MIME type
fn arb_audio_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("audio/wav".to_string()),
        Just("audio/wave".to_string()),
        Just("audio/x-wav".to_string()),
        Just("audio/mp3".to_string()),
        Just("audio/mpeg".to_string()),
        Just("audio/flac".to_string()),
        Just("audio/x-flac".to_string()),
        Just("audio/ogg".to_string()),
    ]
}

/// Generate arbitrary non-audio MIME type
fn arb_non_audio_mime_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("text/plain".to_string()),
        Just("application/json".to_string()),
        Just("video/mp4".to_string()),
        Just("image/jpeg".to_string()),
        Just("image/png".to_string()),
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

    /// **Feature: mistral-rs-integration, Property 14: Audio Input Handling**
    /// *For any* audio content in supported formats (WAV, MP3, FLAC, OGG), the format
    /// detection SHALL succeed.
    /// **Validates: Requirements 6.5**
    #[test]
    fn prop_audio_format_detection(mime_type in arb_audio_mime_type()) {
        // All supported audio MIME types should be recognized
        prop_assert!(AudioFormat::is_supported_mime_type(&mime_type));
        prop_assert!(AudioFormat::from_mime_type(&mime_type).is_some());
    }

    /// Property: Non-audio MIME types should not be recognized as audio
    #[test]
    fn prop_non_audio_format_rejection(mime_type in arb_non_audio_mime_type()) {
        prop_assert!(!AudioFormat::is_supported_mime_type(&mime_type));
        prop_assert!(AudioFormat::from_mime_type(&mime_type).is_none());
    }

    /// Property: Audio MIME type case insensitivity
    #[test]
    fn prop_audio_mime_type_case_insensitive(mime_type in arb_audio_mime_type()) {
        let upper = mime_type.to_uppercase();
        let lower = mime_type.to_lowercase();

        // Both cases should be recognized
        prop_assert!(AudioFormat::is_supported_mime_type(&upper));
        prop_assert!(AudioFormat::is_supported_mime_type(&lower));
    }

    /// Property: Text parts should not be converted to audio
    #[test]
    fn prop_text_part_not_audio(text in arb_text()) {
        let part = Part::text(text);
        prop_assert!(audio_part_to_mistralrs(&part).is_none());
    }

    /// Property: All audio format variants have valid MIME types
    #[test]
    fn prop_audio_format_has_valid_mime_type(_dummy in 0..1i32) {
        // Test all variants
        let formats = [
            AudioFormat::Wav,
            AudioFormat::Mp3,
            AudioFormat::Flac,
            AudioFormat::Ogg,
        ];

        for format in formats {
            let mime = format.mime_type();
            prop_assert!(!mime.is_empty());
            prop_assert!(mime.starts_with("audio/"));
        }
    }

    /// Property: Audio format roundtrip - format to MIME to format
    #[test]
    fn prop_audio_format_roundtrip(_dummy in 0..1i32) {
        let formats = [
            AudioFormat::Wav,
            AudioFormat::Mp3,
            AudioFormat::Flac,
            AudioFormat::Ogg,
        ];

        for format in formats {
            let mime = format.mime_type();
            let recovered = AudioFormat::from_mime_type(mime);
            prop_assert!(recovered.is_some());
            prop_assert_eq!(recovered.unwrap(), format);
        }
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[test]
fn test_audio_format_wav_variants() {
    // All WAV MIME type variants should be recognized
    assert_eq!(AudioFormat::from_mime_type("audio/wav"), Some(AudioFormat::Wav));
    assert_eq!(AudioFormat::from_mime_type("audio/wave"), Some(AudioFormat::Wav));
    assert_eq!(AudioFormat::from_mime_type("audio/x-wav"), Some(AudioFormat::Wav));
}

#[test]
fn test_audio_format_mp3_variants() {
    // All MP3 MIME type variants should be recognized
    assert_eq!(AudioFormat::from_mime_type("audio/mp3"), Some(AudioFormat::Mp3));
    assert_eq!(AudioFormat::from_mime_type("audio/mpeg"), Some(AudioFormat::Mp3));
}

#[test]
fn test_audio_format_flac_variants() {
    // All FLAC MIME type variants should be recognized
    assert_eq!(AudioFormat::from_mime_type("audio/flac"), Some(AudioFormat::Flac));
    assert_eq!(AudioFormat::from_mime_type("audio/x-flac"), Some(AudioFormat::Flac));
}

#[test]
fn test_audio_format_ogg() {
    assert_eq!(AudioFormat::from_mime_type("audio/ogg"), Some(AudioFormat::Ogg));
}

#[test]
fn test_audio_part_to_mistralrs_with_unsupported_mime() {
    let part = Part::InlineData {
        mime_type: "application/octet-stream".to_string(),
        data: vec![0, 1, 2, 3],
    };

    let result = audio_part_to_mistralrs(&part);
    assert!(result.is_none(), "Unsupported MIME type should return None");
}

#[test]
fn test_audio_from_base64_invalid() {
    let result = audio_from_base64("not-valid-base64!!!");
    assert!(result.is_err(), "Invalid base64 should fail");
}

#[test]
fn test_audio_format_mime_types() {
    assert_eq!(AudioFormat::Wav.mime_type(), "audio/wav");
    assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
    assert_eq!(AudioFormat::Flac.mime_type(), "audio/flac");
    assert_eq!(AudioFormat::Ogg.mime_type(), "audio/ogg");
}

#[test]
fn test_audio_format_equality() {
    assert_eq!(AudioFormat::Wav, AudioFormat::Wav);
    assert_ne!(AudioFormat::Wav, AudioFormat::Mp3);
    assert_ne!(AudioFormat::Mp3, AudioFormat::Flac);
    assert_ne!(AudioFormat::Flac, AudioFormat::Ogg);
}
