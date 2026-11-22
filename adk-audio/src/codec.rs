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
    /// Opus codec (not yet implemented).
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

impl AudioFormat {
    /// Returns `true` if this format has a working `encode()` implementation.
    ///
    /// Currently only `Pcm16` and `Wav` support encoding.
    pub fn supports_encode(&self) -> bool {
        match self {
            AudioFormat::Pcm16 => true,
            AudioFormat::Wav => true,
            AudioFormat::Opus => false,
            AudioFormat::Mp3 => false,
            AudioFormat::Flac => false,
            AudioFormat::Ogg => false,
        }
    }

    /// Returns `true` if this format has a working `decode()` implementation.
    ///
    /// Currently only `Pcm16` and `Wav` support decoding.
    pub fn supports_decode(&self) -> bool {
        match self {
            AudioFormat::Pcm16 => true,
            AudioFormat::Wav => true,
            AudioFormat::Opus => false,
            AudioFormat::Mp3 => false,
            AudioFormat::Flac => false,
            AudioFormat::Ogg => false,
        }
    }
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
///
/// Walks the RIFF chunks for `fmt ` and `data` rather than reading fixed offsets, so a file
/// with other chunks first — CoreAudio writes `JUNK` before `fmt ` and `FLLR` before `data`
/// — decodes like a canonical 44-byte header.
fn decode_wav(data: &[u8]) -> AudioResult<AudioFrame> {
    if data.len() < 12 {
        return Err(AudioError::Codec("WAV data too short for header".into()));
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(AudioError::Codec("invalid WAV header".into()));
    }
    // (sample_rate, channels) from the `fmt ` chunk, which precedes `data`.
    let mut format = None;
    let mut offset = 12usize;
    while let Some(header) = offset.checked_add(8).and_then(|end| data.get(offset..end)) {
        let chunk_size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let start = offset + 8;
        // A streamed WAV can declare a placeholder size; the bytes present count.
        let end = start.saturating_add(chunk_size).min(data.len());
        let body = &data[start..end];
        match &header[0..4] {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(AudioError::Codec(format!(
                        "WAV fmt chunk too short: {} bytes, expected at least 16",
                        body.len()
                    )));
                }
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let bits_per_sample = u16::from_le_bytes([body[14], body[15]]);
                if bits_per_sample != 16 {
                    return Err(AudioError::Codec(format!(
                        "unsupported bits per sample: {bits_per_sample}, expected 16"
                    )));
                }
                let channels = u8::try_from(channels).ok().filter(|&c| c > 0).ok_or_else(|| {
                    AudioError::Codec(format!(
                        "unsupported channel count: {channels}, expected 1 to 255"
                    ))
                })?;
                format = Some((sample_rate, channels));
            }
            b"data" => {
                let (sample_rate, channels) = format.ok_or_else(|| {
                    AudioError::Codec("WAV data chunk comes before the fmt chunk".into())
                })?;
                return Ok(AudioFrame::new(Bytes::copy_from_slice(body), sample_rate, channels));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        offset = start.saturating_add(chunk_size).saturating_add(chunk_size % 2);
    }
    Err(AudioError::Codec(
        if format.is_some() { "WAV data chunk not found" } else { "WAV fmt chunk not found" }
            .into(),
    ))
}
