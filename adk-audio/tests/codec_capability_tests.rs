//! Property tests for AudioFormat capability query methods.
//!
//! Validates that `supports_encode()` and `supports_decode()` are consistent
//! with the actual `encode()` and `decode()` implementations.
//!
//! **Validates: Requirements 2.11, 2.12, 3.11, 3.12, 3.13**

use adk_audio::{AudioFormat, AudioFrame, decode, encode};
use proptest::prelude::*;

/// Strategy that produces any `AudioFormat` variant.
fn arb_audio_format() -> impl Strategy<Value = AudioFormat> {
    prop_oneof![
        Just(AudioFormat::Pcm16),
        Just(AudioFormat::Opus),
        Just(AudioFormat::Mp3),
        Just(AudioFormat::Wav),
        Just(AudioFormat::Flac),
        Just(AudioFormat::Ogg),
    ]
}

/// Create a valid PCM16 AudioFrame suitable for encoding.
fn valid_pcm16_frame() -> AudioFrame {
    // 10 samples of silence at 16kHz mono = 20 bytes
    AudioFrame::silence(16000, 1, 10)
}

/// Create valid encoded data suitable for decoding in a given format.
/// For Pcm16, raw PCM bytes work. For Wav, we encode a frame first.
fn valid_encoded_data(format: AudioFormat) -> Vec<u8> {
    match format {
        AudioFormat::Pcm16 => {
            // Raw PCM16 LE bytes — 10 samples of silence
            vec![0u8; 20]
        }
        AudioFormat::Wav => {
            // Encode a valid frame to WAV first
            let frame = valid_pcm16_frame();
            encode(&frame, AudioFormat::Wav).unwrap().to_vec()
        }
        // For unsupported formats, provide some arbitrary bytes.
        // decode() should return Err regardless of content.
        _ => vec![0u8; 64],
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: pro-hardening, Property: supports_encode consistency**
    /// *For any* `AudioFormat` variant, if `supports_encode()` returns `true`
    /// then `encode()` with a valid frame must return `Ok`; if `false` then
    /// `encode()` must return `Err`.
    /// **Validates: Requirements 2.11, 3.11, 3.12**
    #[test]
    fn prop_supports_encode_matches_encode(format in arb_audio_format()) {
        let frame = valid_pcm16_frame();
        let result = encode(&frame, format);

        if format.supports_encode() {
            prop_assert!(
                result.is_ok(),
                "supports_encode() returned true for {format:?} but encode() failed: {:?}",
                result.err()
            );
        } else {
            prop_assert!(
                result.is_err(),
                "supports_encode() returned false for {format:?} but encode() succeeded"
            );
        }
    }

    /// **Feature: pro-hardening, Property: supports_decode consistency**
    /// *For any* `AudioFormat` variant, if `supports_decode()` returns `true`
    /// then `decode()` with valid data must return `Ok`; if `false` then
    /// `decode()` must return `Err`.
    /// **Validates: Requirements 2.12, 3.11, 3.12**
    #[test]
    fn prop_supports_decode_matches_decode(format in arb_audio_format()) {
        let data = valid_encoded_data(format);
        let result = decode(&data, format);

        if format.supports_decode() {
            prop_assert!(
                result.is_ok(),
                "supports_decode() returned true for {format:?} but decode() failed: {:?}",
                result.err()
            );
        } else {
            prop_assert!(
                result.is_err(),
                "supports_decode() returned false for {format:?} but decode() succeeded"
            );
        }
    }
}

/// **Feature: pro-hardening, Test: all variants have capability answer**
/// Enumerate all 6 `AudioFormat` variants and assert each returns a definite
/// `true` or `false` for both `supports_encode()` and `supports_decode()`
/// (no panics).
/// **Validates: Requirements 2.11, 2.12, 3.13**
#[test]
fn test_all_variants_have_capability_answer() {
    let all_formats = [
        AudioFormat::Pcm16,
        AudioFormat::Opus,
        AudioFormat::Mp3,
        AudioFormat::Wav,
        AudioFormat::Flac,
        AudioFormat::Ogg,
    ];

    for format in &all_formats {
        // These calls must not panic — they return bool
        let can_encode: bool = format.supports_encode();
        let can_decode: bool = format.supports_decode();

        // Verify expected capabilities
        match format {
            AudioFormat::Pcm16 | AudioFormat::Wav => {
                assert!(can_encode, "{format:?} should support encoding");
                assert!(can_decode, "{format:?} should support decoding");
            }
            AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Flac | AudioFormat::Ogg => {
                assert!(!can_encode, "{format:?} should NOT support encoding");
                assert!(!can_decode, "{format:?} should NOT support decoding");
            }
        }
    }
}
