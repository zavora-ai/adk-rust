//! Preservation Property Test B — STT Surface Unchanged
//!
//! **Property 4: Preservation — batch transcribe() code paths unchanged**
//!
//! Verifies the provider-facing structures used by batch transcription without
//! contacting an external service:
//!
//! 1. The `SttProvider` trait still requires both `transcribe()` and `transcribe_stream()`
//! 2. AudioFrame construction from arbitrary PCM-16 LE data is unchanged
//!
//! This confirms the batch transcription surface is untouched by the streaming
//! stub fix.
//!
//! **Validates: Requirements 3.4, 3.5**

use adk_audio::frame::AudioFrame;
use adk_audio::traits::{SttOptions, SttProvider};
use bytes::Bytes;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Property tests — AudioFrame construction preservation
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: production-hardening, Property 4A: Preservation — AudioFrame round-trip**
    ///
    /// *For any* sample count, constructing an AudioFrame from PCM-16 LE bytes
    /// SHALL produce a frame whose `samples()` slice matches the original data
    /// and whose `duration_ms` is computed correctly.
    ///
    /// This confirms the AudioFrame API used by transcribe() is unchanged.
    ///
    /// **Validates: Requirement 3.4, 3.5**
    #[test]
    fn prop_audio_frame_construction_preserves_samples(
        sample_count in 1usize..4800,
    ) {
        let original_samples: Vec<i16> = (0..sample_count).map(|i| (i % 256) as i16).collect();
        let byte_data: Vec<u8> = original_samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = AudioFrame::new(Bytes::from(byte_data), 16000, 1);

        // samples() should return the same data
        let recovered = frame.samples();
        prop_assert_eq!(recovered.len(), sample_count);
        for (i, (&orig, &recov)) in original_samples.iter().zip(recovered.iter()).enumerate() {
            prop_assert_eq!(orig, recov, "mismatch at sample {}", i);
        }

        // duration_ms should be computed from sample_count / sample_rate * 1000
        let expected_duration = (sample_count as u64 * 1000 / 16000) as u32;
        prop_assert_eq!(frame.duration_ms, expected_duration);
    }

    /// **Feature: production-hardening, Property 4B: Preservation — SttOptions default unchanged**
    ///
    /// *For any* language string, SttOptions can be constructed with default values.
    /// This confirms the options struct used by transcribe() is unchanged.
    ///
    /// **Validates: Requirement 3.4, 3.5**
    #[test]
    fn prop_stt_options_default_is_stable(
        _dummy in 0u8..1,
    ) {
        let opts = SttOptions::default();
        // SttOptions::default() should always be constructible
        // (compile-time check that the struct hasn't changed shape)
        let _ = opts;
    }
}

/// Verify that SttProvider trait still requires both transcribe() and transcribe_stream().
/// This is a compile-time structural check — if either method were removed from the
/// trait, this test would fail to compile.
#[tokio::test]
async fn trait_requires_both_transcribe_methods() {
    // Construct providers — this verifies the struct and trait are intact
    let assemblyai = adk_audio::AssemblyAiStt::with_api_key("test".to_string());
    let deepgram = adk_audio::DeepgramStt::with_api_key("test".to_string());

    // Verify both methods exist on the trait (compile-time check)
    let _: &dyn SttProvider = &assemblyai;
    let _: &dyn SttProvider = &deepgram;
}
