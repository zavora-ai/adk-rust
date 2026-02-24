//! Audio codec conversion between PCM16 internal format and external formats.

use bytes::Bytes;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;

/// Supported audio formats for encode/decode at transport edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFormat {
    /// Raw PCM-16 LE (internal format, no header).
    #[default]
    Pcm16,
    /// Opus codec (requires `opus` feature).
    Opus,
    /// MP3 format.
    Mp3,
    /// WAV (RIFF) format.
    Wav,
    /// FLAC lossless format.
    Flac,
    /// Ogg container format.
    Ogg,
}

/// Decode encoded bytes into a PCM16 `AudioFrame`.
///
/// Currently supports WAV and raw PCM16. Other formats return
/// `AudioError::Codec`.
pub fn decode(data: &[u8], format: AudioFormat) -> AudioResult<AudioFrame> {
    match format {
        AudioFormat::Pcm16 => {
            // Raw PCM16 — assume 16kHz mono (caller should know the format)
            Ok(AudioFrame::new(Bytes::copy_from_slice(data), 16000, 1))
        }
        AudioFormat::Wav => decode_wav(data),
        _ => Err(AudioError::Codec(format!("decoding {format:?} is not yet supported"))),
    }
}

/// Encode an `AudioFrame` to the target format.
///
/// Currently supports WAV and raw PCM16. Other formats return
/// `AudioError::Codec`.
pub fn encode(frame: &AudioFrame, format: AudioFormat) -> AudioResult<Bytes> {
    match format {
        AudioFormat::Pcm16 => Ok(frame.data.clone()),
        AudioFormat::Wav => encode_wav(frame),
        _ => Err(AudioError::Codec(format!("encoding {format:?} is not yet supported"))),
    }
}

/// Encode an `AudioFrame` as a RIFF WAV file.
fn encode_wav(frame: &AudioFrame) -> AudioResult<Bytes> {
    let data_len = frame.data.len() as u32;
    let channels = frame.channels as u16;
    let sample_rate = frame.sample_rate;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let file_size = 36 + data_len;

    let mut buf = Vec::with_capacity(44 + frame.data.len());
    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    // fmt sub-chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data sub-chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf.extend_from_slice(&frame.data);

    Ok(Bytes::from(buf))
}

/// Decode a RIFF WAV file into an `AudioFrame`.
fn decode_wav(data: &[u8]) -> AudioResult<AudioFrame> {
    if data.len() < 44 {
        return Err(AudioError::Codec("WAV data too short for header".into()));
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(AudioError::Codec("invalid WAV header".into()));
    }
    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);
    if bits_per_sample != 16 {
        return Err(AudioError::Codec(format!(
            "unsupported bits per sample: {bits_per_sample}, expected 16"
        )));
    }
    // Find the data chunk
    let mut offset = 12;
    while offset + 8 <= data.len() {
        let chunk_id = &data[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        if chunk_id == b"data" {
            let start = offset + 8;
            let end = (start + chunk_size).min(data.len());
            let pcm = Bytes::copy_from_slice(&data[start..end]);
            return Ok(AudioFrame::new(pcm, sample_rate, channels as u8));
        }
        offset += 8 + chunk_size;
    }
    Err(AudioError::Codec("WAV data chunk not found".into()))
}
