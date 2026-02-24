//! Property P6: Mixer Volume Scaling
//!
//! *For any* single-track Mixer with volume V and input samples S, the output
//! samples SHALL equal `(S * V).clamp(-32768, 32767)`. Volume 0.0 → silence.
//! Volume 1.0 → original samples.
//!
//! **Validates: Requirement 22**

use adk_audio::{AudioFrame, Mixer};
use bytes::Bytes;
use proptest::prelude::*;

fn arb_volume() -> impl Strategy<Value = f32> {
    prop_oneof![Just(0.0f32), Just(0.5f32), Just(1.0f32), (0.0f32..1.0f32),]
}

fn arb_pcm_samples(max: usize) -> impl Strategy<Value = Vec<i16>> {
    proptest::collection::vec(any::<i16>(), 1..max)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P6.1: Volume 0.0 produces silence
    #[test]
    fn prop_volume_zero_silence(samples in arb_pcm_samples(200)) {
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = AudioFrame::new(Bytes::from(pcm), 16000, 1);

        let mut mixer = Mixer::new(16000);
        mixer.add_track("test", 0.0);
        mixer.push_frame("test", frame);
        let mixed = mixer.mix().unwrap();

        for &s in mixed.samples() {
            prop_assert_eq!(s, 0i16, "expected silence with volume 0.0");
        }
    }

    /// P6.2: Volume 1.0 preserves original samples
    #[test]
    fn prop_volume_one_identity(samples in arb_pcm_samples(200)) {
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = AudioFrame::new(Bytes::from(pcm), 16000, 1);

        let mut mixer = Mixer::new(16000);
        mixer.add_track("test", 1.0);
        mixer.push_frame("test", frame.clone());
        let mixed = mixer.mix().unwrap();

        let original = frame.samples();
        let result = mixed.samples();
        prop_assert_eq!(result.len(), original.len());
        for (i, (&o, &r)) in original.iter().zip(result.iter()).enumerate() {
            // Volume 1.0: (s as f32 * 1.0) as i32 may differ by ±1 due to float rounding
            let expected = (o as f32 * 1.0) as i32;
            let clamped = expected.clamp(-32768, 32767) as i16;
            prop_assert_eq!(r, clamped, "mismatch at sample {}: original={}, result={}", i, o, r);
        }
    }

    /// P6.3: Volume scaling is correct
    #[test]
    fn prop_volume_scaling(samples in arb_pcm_samples(200), volume in arb_volume()) {
        let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let frame = AudioFrame::new(Bytes::from(pcm), 16000, 1);

        let mut mixer = Mixer::new(16000);
        mixer.add_track("test", volume);
        mixer.push_frame("test", frame.clone());
        let mixed = mixer.mix().unwrap();

        let original = frame.samples();
        let result = mixed.samples();
        prop_assert_eq!(result.len(), original.len());
        for (i, (&o, &r)) in original.iter().zip(result.iter()).enumerate() {
            let expected = ((o as f32 * volume) as i32).clamp(-32768, 32767) as i16;
            prop_assert_eq!(r, expected, "mismatch at sample {}: original={}, volume={}, expected={}, got={}", i, o, volume, expected, r);
        }
    }
}
