//! Property P1: AudioFrame PCM16 Validity
//!
//! *For any* `AudioFrame` constructed via `AudioFrame::new`, the `data` field
//! SHALL have an even number of bytes, `duration_ms` SHALL be correctly computed,
//! and `samples()` SHALL return a slice of length `data.len() / 2`.
//!
//! **Validates: Requirement 1**

use adk_audio::{AudioFrame, merge_frames};
use proptest::prelude::*;

fn arb_sample_rate() -> impl Strategy<Value = u32> {
    prop_oneof![Just(8000u32), Just(16000u32), Just(24000u32), Just(44100u32), Just(48000u32),]
}

fn arb_channels() -> impl Strategy<Value = u8> {
    prop_oneof![Just(1u8), Just(2u8)]
}

fn arb_pcm_data(max_samples: usize) -> impl Strategy<Value = Vec<u8>> {
    (1..max_samples).prop_flat_map(|n| proptest::collection::vec(any::<u8>(), n * 2..=n * 2))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P1.1: data length is always even
    #[test]
    fn prop_data_length_even(
        data in arb_pcm_data(500),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let frame = AudioFrame::new(data, sr, ch);
        prop_assert_eq!(frame.data.len() % 2, 0);
    }

    /// P1.2: duration_ms is correctly computed
    #[test]
    fn prop_duration_correct(
        data in arb_pcm_data(500),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let frame = AudioFrame::new(data.clone(), sr, ch);
        let samples_per_channel = data.len() / 2 / ch as usize;
        let expected_ms = (samples_per_channel as u64 * 1000 / sr as u64) as u32;
        prop_assert_eq!(frame.duration_ms, expected_ms);
    }

    /// P1.3: samples() returns correct length
    #[test]
    fn prop_samples_length(
        data in arb_pcm_data(500),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let frame = AudioFrame::new(data.clone(), sr, ch);
        prop_assert_eq!(frame.samples().len(), data.len() / 2);
    }

    /// P1.4: silence has all-zero samples
    #[test]
    fn prop_silence_is_zero(
        sr in arb_sample_rate(),
        ch in arb_channels(),
        dur in 1u32..500,
    ) {
        let frame = AudioFrame::silence(sr, ch, dur);
        for &s in frame.samples() {
            prop_assert_eq!(s, 0i16);
        }
    }

    /// P1.5: merge_frames preserves total data
    #[test]
    fn prop_merge_preserves_data(
        d1 in arb_pcm_data(200),
        d2 in arb_pcm_data(200),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let f1 = AudioFrame::new(d1.clone(), sr, ch);
        let f2 = AudioFrame::new(d2.clone(), sr, ch);
        let merged = merge_frames(&[f1, f2]);
        prop_assert_eq!(merged.data.len(), d1.len() + d2.len());
        prop_assert_eq!(merged.sample_rate, sr);
        prop_assert_eq!(merged.channels, ch);
    }
}
