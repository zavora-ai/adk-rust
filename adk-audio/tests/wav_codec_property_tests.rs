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

/// A RIFF chunk: id, little-endian size, body, and a pad byte when the size is odd.
fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + body.len() + 1);
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A 16-byte PCM `fmt ` chunk body.
fn fmt_body(sample_rate: u32, channels: u16, bits_per_sample: u16) -> Vec<u8> {
    let block_align = channels * bits_per_sample / 8;
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&sample_rate.to_le_bytes());
    body.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    body.extend_from_slice(&block_align.to_le_bytes());
    body.extend_from_slice(&bits_per_sample.to_le_bytes());
    body
}

/// A RIFF/WAVE file around `chunks`, laid out in order.
fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut out = Vec::with_capacity(12 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&body);
    out
}

/// The layout macOS `say -o out.wav --data-format=LEI16@24000` writes: `JUNK`, `fmt `,
/// `FLLR`, then `data`.
#[test]
fn decodes_a_coreaudio_wav_with_junk_and_fllr_chunks() {
    let pcm: Vec<u8> = (0u8..64).collect();
    let wav = riff(&[
        chunk(b"JUNK", &[0; 28]),
        chunk(b"fmt ", &fmt_body(24_000, 1, 16)),
        chunk(b"FLLR", &[0; 4008]),
        chunk(b"data", &pcm),
    ]);

    let decoded = decode(&wav, AudioFormat::Wav).unwrap();

    assert_eq!(decoded, AudioFrame::new(pcm, 24_000, 1));
}

/// An odd-sized chunk carries a pad byte that is not part of the next chunk's header.
#[test]
fn skips_the_pad_byte_after_an_odd_sized_chunk() {
    let pcm = vec![1u8, 2, 3, 4];
    let wav = riff(&[
        chunk(b"fmt ", &fmt_body(16_000, 2, 16)),
        chunk(b"LIST", b"INFOISFT\x05\x00\x00\x00adk!\x00"),
        chunk(b"data", &pcm),
    ]);

    let decoded = decode(&wav, AudioFormat::Wav).unwrap();

    assert_eq!(decoded, AudioFrame::new(pcm, 16_000, 2));
}

#[test]
fn rejects_wavs_it_cannot_decode() {
    let pcm = vec![0u8; 8];
    let cases = [
        (
            riff(&[chunk(b"fmt ", &fmt_body(8_000, 1, 8)), chunk(b"data", &pcm)]),
            "unsupported bits per sample: 8, expected 16",
        ),
        (
            riff(&[
                chunk(b"JUNK", &[0; 28]),
                chunk(b"fmt ", &fmt_body(8_000, 1, 24)),
                chunk(b"data", &pcm),
            ]),
            "unsupported bits per sample: 24, expected 16",
        ),
        (
            riff(&[chunk(b"fmt ", &fmt_body(8_000, 0, 16)), chunk(b"data", &pcm)]),
            "unsupported channel count: 0, expected 1 to 255",
        ),
        (
            riff(&[chunk(b"data", &pcm), chunk(b"fmt ", &fmt_body(8_000, 1, 16))]),
            "WAV data chunk comes before the fmt chunk",
        ),
        (
            riff(&[chunk(b"fmt ", &[0; 12]), chunk(b"data", &pcm)]),
            "WAV fmt chunk too short: 12 bytes, expected at least 16",
        ),
        (riff(&[chunk(b"fmt ", &fmt_body(8_000, 1, 16))]), "WAV data chunk not found"),
        (riff(&[chunk(b"JUNK", &[0; 28])]), "WAV fmt chunk not found"),
        (b"RIFF\x00\x00\x00\x00AVI ".to_vec(), "invalid WAV header"),
        (b"RIFF".to_vec(), "WAV data too short for header"),
    ];
    for (wav, expected) in cases {
        let err = decode(&wav, AudioFormat::Wav).unwrap_err();
        assert_eq!(err.to_string(), format!("Codec error: {expected}"));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// P5.3: chunks before `fmt ` and between `fmt ` and `data`, of any size, do not change the
    /// decoded frame.
    #[test]
    fn prop_wav_decode_skips_unknown_chunks(
        data in arb_pcm_data(200),
        sr in arb_sample_rate(),
        ch in arb_channels(),
        before in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), 0..3),
        between in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), 0..3),
    ) {
        let mut chunks: Vec<Vec<u8>> = before.iter().map(|body| chunk(b"JUNK", body)).collect();
        chunks.push(chunk(b"fmt ", &fmt_body(sr, u16::from(ch), 16)));
        chunks.extend(between.iter().map(|body| chunk(b"FLLR", body)));
        chunks.push(chunk(b"data", &data));

        let decoded = decode(&riff(&chunks), AudioFormat::Wav).unwrap();

        prop_assert_eq!(decoded, AudioFrame::new(data, sr, ch));
    }
}
