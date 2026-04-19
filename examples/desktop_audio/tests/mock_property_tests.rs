//! Property-based tests for MockVad and config validation.

use adk_audio::{AudioFrame, CaptureConfig, VadConfig, VadMode};
use bytes::Bytes;
use desktop_audio_example::MockVad;
use proptest::prelude::*;

/// Create an AudioFrame from raw i16 samples.
fn make_frame(samples: &[i16]) -> AudioFrame {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    AudioFrame::new(Bytes::from(data), 16000, 1)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: desktop-audio-examples, Property 1: MockVad Amplitude Classification**
    /// *For any* AudioFrame with known sample values and *for any* non-negative threshold,
    /// `MockVad::is_speech()` SHALL return `true` if and only if at least one sample has
    /// absolute value >= threshold.
    /// **Validates: Requirements 2.1**
    #[test]
    fn prop_mock_vad_amplitude_classification(
        samples in prop::collection::vec(any::<i16>(), 1..64),
        threshold in 0i16..=i16::MAX,
    ) {
        let vad = MockVad { threshold };
        let frame = make_frame(&samples);

        let expected = samples.iter().any(|&s| (s as i32).abs() >= threshold as i32);
        let actual = adk_audio::VadProcessor::is_speech(&vad, &frame);

        prop_assert_eq!(actual, expected);
    }

    /// **Feature: desktop-audio-examples, Property 2: CaptureConfig Validation Completeness**
    /// *For any* CaptureConfig where at least one of sample_rate, channels, or frame_duration_ms
    /// is zero, `validate()` SHALL return Err. For any CaptureConfig where all three are non-zero,
    /// `validate()` SHALL return Ok(()).
    /// **Validates: Requirements 8**
    #[test]
    fn prop_capture_config_validation(
        sample_rate in 0u32..=48000,
        channels in 0u8..=2,
        frame_duration_ms in 0u32..=100,
    ) {
        let config = CaptureConfig { sample_rate, channels, frame_duration_ms };
        let result = config.validate();

        if sample_rate == 0 || channels == 0 || frame_duration_ms == 0 {
            prop_assert!(result.is_err());
        } else {
            prop_assert!(result.is_ok());
        }
    }

    /// **Feature: desktop-audio-examples, Property 3: VadConfig Validation Completeness**
    /// *For any* VadConfig where silence_threshold_ms or speech_threshold_ms is zero,
    /// `validate()` SHALL return Err. For any VadConfig where both are non-zero,
    /// `validate()` SHALL return Ok(()).
    /// **Validates: Requirements 8**
    #[test]
    fn prop_vad_config_validation(
        silence_threshold_ms in 0u32..=2000,
        speech_threshold_ms in 0u32..=2000,
        mode_idx in 0usize..2,
    ) {
        let mode = if mode_idx == 0 { VadMode::HandsFree } else { VadMode::PushToTalk };
        let config = VadConfig { mode, silence_threshold_ms, speech_threshold_ms };
        let result = config.validate();

        if silence_threshold_ms == 0 || speech_threshold_ms == 0 {
            prop_assert!(result.is_err());
        } else {
            prop_assert!(result.is_ok());
        }
    }
}
