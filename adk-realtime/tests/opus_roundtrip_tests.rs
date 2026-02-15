//! Property-based tests for Opus codec lossy round-trip.
//!
//! **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
//! *For any* valid PCM16 audio buffer at 24kHz mono, encoding to Opus then
//! decoding back to PCM16 SHALL produce an output with the same number of samples.
//! The output sample rate SHALL match the configured sample rate.
//! **Validates: Requirements 11.1, 11.2, 11.3**
//!
//! Requires the `openai-webrtc` feature and `cmake` installed (for audiopus).
//! Run: `cargo test -p adk-realtime --features openai-webrtc --test opus_roundtrip_tests`

#![cfg(feature = "openai-webrtc")]

use adk_realtime::openai::OpusCodec;
use audiopus::{Channels, SampleRate};
use proptest::prelude::*;

/// Generator for valid Opus frame sizes at 24kHz.
///
/// Opus supports frame durations of 2.5, 5, 10, 20, 40, and 60 ms.
/// At 24kHz: 120, 240, 480, 960, 1920, 2880 samples respectively.
fn arb_frame_size() -> impl Strategy<Value = usize> {
    prop_oneof![
        Just(480),  // 10ms — most common
        Just(960),  // 20ms — common
        Just(1920), // 40ms
        Just(2880), // 60ms
        Just(120),  // 2.5ms
        Just(240),  // 5ms
    ]
}

/// Generator for PCM16 audio samples of a valid Opus frame size at 24kHz mono.
fn arb_pcm16_frame() -> impl Strategy<Value = Vec<i16>> {
    arb_frame_size().prop_flat_map(|size| proptest::collection::vec(any::<i16>(), size..=size))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
    /// *For any* valid PCM16 audio buffer at 24kHz mono with a valid Opus frame size,
    /// encoding to Opus then decoding back to PCM16 SHALL produce an output buffer
    /// with the same number of samples as the input.
    /// **Validates: Requirements 11.1, 11.2, 11.3**
    #[test]
    fn prop_roundtrip_preserves_sample_count(pcm in arb_pcm16_frame()) {
        let mut codec = OpusCodec::new(24000, 1)
            .expect("Failed to create OpusCodec at 24kHz mono");

        let input_len = pcm.len();

        let encoded = codec.encode(&pcm)
            .expect("Opus encode failed for valid PCM16 input");
        let decoded = codec.decode(&encoded)
            .expect("Opus decode failed for valid encoded data");

        prop_assert_eq!(
            decoded.len(),
            input_len,
            "Round-trip sample count mismatch: input {} samples, output {} samples",
            input_len,
            decoded.len()
        );
    }

    /// **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
    /// *For any* valid PCM16 audio buffer at 24kHz mono, encoding to Opus SHALL
    /// produce a non-empty output.
    /// **Validates: Requirements 11.1**
    #[test]
    fn prop_encode_produces_nonempty_output(pcm in arb_pcm16_frame()) {
        let mut codec = OpusCodec::new(24000, 1)
            .expect("Failed to create OpusCodec at 24kHz mono");

        let encoded = codec.encode(&pcm)
            .expect("Opus encode failed for valid PCM16 input");

        prop_assert!(
            !encoded.is_empty(),
            "Opus encode produced empty output for {} input samples",
            pcm.len()
        );
    }

    /// **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
    /// *For any* valid PCM16 audio buffer at 24kHz mono, decoding the encoded Opus
    /// data SHALL produce a non-empty output.
    /// **Validates: Requirements 11.2**
    #[test]
    fn prop_decode_produces_nonempty_output(pcm in arb_pcm16_frame()) {
        let mut codec = OpusCodec::new(24000, 1)
            .expect("Failed to create OpusCodec at 24kHz mono");

        let encoded = codec.encode(&pcm)
            .expect("Opus encode failed");
        let decoded = codec.decode(&encoded)
            .expect("Opus decode failed");

        prop_assert!(
            !decoded.is_empty(),
            "Opus decode produced empty output for encoded data of {} bytes",
            encoded.len()
        );
    }
}

/// **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
/// The codec sample rate SHALL match the configured sample rate (24kHz).
/// **Validates: Requirements 11.2**
#[test]
fn test_sample_rate_matches_configured() {
    let codec = OpusCodec::new(24000, 1).expect("Failed to create OpusCodec at 24kHz mono");

    assert_eq!(
        codec.sample_rate(),
        SampleRate::Hz24000,
        "Codec sample rate does not match configured 24kHz"
    );
}

/// **Feature: realtime-audio-transport, Property 3: Opus Codec Lossy Round-Trip**
/// The codec channel count SHALL match the configured channel count (mono).
/// **Validates: Requirements 11.3**
#[test]
fn test_channel_count_matches_configured() {
    let codec = OpusCodec::new(24000, 1).expect("Failed to create OpusCodec at 24kHz mono");

    assert_eq!(codec.channels(), Channels::Mono, "Codec channels do not match configured mono");
}
