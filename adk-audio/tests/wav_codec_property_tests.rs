//! Property P5: WAV Codec Round-Trip
//!
//! *For any* `AudioFrame` with valid PCM16 data, encoding to WAV and decoding
//! back SHALL produce an `AudioFrame` with identical data, sample_rate, and channels.
//!
//! **Validates: Requirement 20**

use adk_audio::{AudioFormat, AudioFrame, decode, encode};
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

    /// P5: WAV round-trip preserves data
    #[test]
    fn prop_wav_roundtrip(
        data in arb_pcm_data(500),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let original = AudioFrame::new(data, sr, ch);
        let encoded = encode(&original, AudioFormat::Wav).unwrap();
        let decoded = decode(&encoded, AudioFormat::Wav).unwrap();

        prop_assert_eq!(&decoded.data, &original.data);
        prop_assert_eq!(decoded.sample_rate, original.sample_rate);
        prop_assert_eq!(decoded.channels, original.channels);
    }

    /// P5.2: WAV encoding produces valid RIFF header
    #[test]
    fn prop_wav_valid_header(
        data in arb_pcm_data(100),
        sr in arb_sample_rate(),
        ch in arb_channels(),
    ) {
        let frame = AudioFrame::new(data, sr, ch);
        let encoded = encode(&frame, AudioFormat::Wav).unwrap();
        let bytes = encoded.as_ref();

        // RIFF header
        prop_assert_eq!(&bytes[0..4], b"RIFF");
        prop_assert_eq!(&bytes[8..12], b"WAVE");
        // fmt chunk
        prop_assert_eq!(&bytes[12..16], b"fmt ");
        // data chunk
        prop_assert_eq!(&bytes[36..40], b"data");
    }
}
