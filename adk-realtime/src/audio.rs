//! Audio format definitions and utilities.

use serde::{Deserialize, Serialize};

/// Audio encoding formats supported by realtime APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AudioEncoding {
    /// 16-bit PCM audio (most common).
    #[serde(rename = "pcm16")]
    #[default]
    Pcm16,
    /// G.711 μ-law encoding.
    #[serde(rename = "g711_ulaw")]
    G711Ulaw,
    /// G.711 A-law encoding.
    #[serde(rename = "g711_alaw")]
    G711Alaw,
}

impl std::fmt::Display for AudioEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pcm16 => write!(f, "pcm16"),
            Self::G711Ulaw => write!(f, "g711_ulaw"),
            Self::G711Alaw => write!(f, "g711_alaw"),
        }
    }
}

/// Complete audio format specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    /// Sample rate in Hz (e.g., 24000, 16000, 8000).
    pub sample_rate: u32,
    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u8,
    /// Bits per sample.
    pub bits_per_sample: u8,
    /// Audio encoding format.
    pub encoding: AudioEncoding,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::pcm16_24khz()
    }
}

impl AudioFormat {
    /// Create a new audio format specification.
    pub fn new(
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        encoding: AudioEncoding,
    ) -> Self {
        Self { sample_rate, channels, bits_per_sample, encoding }
    }

    /// Standard PCM16 format at 24kHz (OpenAI default).
    pub fn pcm16_24khz() -> Self {
        Self {
            sample_rate: 24000,
            channels: 1,
            bits_per_sample: 16,
            encoding: AudioEncoding::Pcm16,
        }
    }

    /// PCM16 format at 16kHz (Gemini input default).
    pub fn pcm16_16khz() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            bits_per_sample: 16,
            encoding: AudioEncoding::Pcm16,
        }
    }

    /// G.711 μ-law format at 8kHz (telephony standard).
    pub fn g711_ulaw() -> Self {
        Self {
            sample_rate: 8000,
            channels: 1,
            bits_per_sample: 8,
            encoding: AudioEncoding::G711Ulaw,
        }
    }

    /// G.711 A-law format at 8kHz (telephony standard).
    pub fn g711_alaw() -> Self {
        Self {
            sample_rate: 8000,
            channels: 1,
            bits_per_sample: 8,
            encoding: AudioEncoding::G711Alaw,
        }
    }

    /// Calculate bytes per second for this format.
    pub fn bytes_per_second(&self) -> u32 {
        self.sample_rate * self.channels as u32 * (self.bits_per_sample / 8) as u32
    }

    /// Calculate duration in milliseconds for a given number of bytes.
    pub fn duration_ms(&self, bytes: usize) -> f64 {
        let bytes_per_ms = self.bytes_per_second() as f64 / 1000.0;
        bytes as f64 / bytes_per_ms
    }
}

/// Audio chunk with format information.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Raw audio data.
    pub data: Vec<u8>,
    /// Audio format of this chunk.
    pub format: AudioFormat,
}

impl AudioChunk {
    /// Create a new audio chunk.
    pub fn new(data: Vec<u8>, format: AudioFormat) -> Self {
        Self { data, format }
    }

    /// Create a PCM16 24kHz audio chunk (OpenAI format).
    pub fn pcm16_24khz(data: Vec<u8>) -> Self {
        Self::new(data, AudioFormat::pcm16_24khz())
    }

    /// Create a PCM16 16kHz audio chunk (Gemini input format).
    pub fn pcm16_16khz(data: Vec<u8>) -> Self {
        Self::new(data, AudioFormat::pcm16_16khz())
    }

    /// Get duration of this audio chunk in milliseconds.
    pub fn duration_ms(&self) -> f64 {
        self.format.duration_ms(self.data.len())
    }

    /// Encode audio data as base64.
    pub fn to_base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }

    /// Decode audio data from base64.
    pub fn from_base64(encoded: &str, format: AudioFormat) -> Result<Self, base64::DecodeError> {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        Ok(Self::new(data, format))
    }

    /// Create an AudioChunk from i16 samples (converts to PCM16 little-endian bytes).
    ///
    /// This is useful when working with audio APIs (like LiveKit) that provide
    /// samples as `i16` slices rather than raw byte buffers.
    pub fn from_i16_samples(samples: &[i16], format: AudioFormat) -> Self {
        let mut data = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            data.extend_from_slice(&sample.to_le_bytes());
        }
        Self::new(data, format)
    }

    /// Convert the audio data to a vector of i16 samples (assuming PCM16 little-endian).
    ///
    /// Returns an error string if the data length is not even (not valid PCM16).
    pub fn to_i16_samples(&self) -> Result<Vec<i16>, String> {
        if self.data.len() % 2 != 0 {
            return Err(format!(
                "Invalid data length for PCM16: {} (must be even)",
                self.data.len()
            ));
        }
        let mut samples = Vec::with_capacity(self.data.len() / 2);
        for chunk in self.data.chunks_exact(2) {
            samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
        }
        Ok(samples)
    }
}

/// Buffers audio samples until a target duration is reached.
///
/// Smart buffering (e.g., 200ms) is essential for AI voice services to:
/// 1. **Reduce Network Overhead**: Aggregating small frames into larger chunks
///    drastically reduces packet rate, lowering CPU usage and bandwidth overhead.
/// 2. **Improve Model Performance**: Provides sufficient context for Voice Activity
///    Detection (VAD) to distinguish speech from noise.
/// 3. **Resist Jitter**: Smooths out network jitter common in mobile networks.
/// 4. **Latency Trade-off**: Maintains a real-time feel while gaining stability.
#[derive(Debug, Clone)]
pub struct SmartAudioBuffer {
    buffer: Vec<i16>,
    sample_rate: u32,
    target_duration_ms: u32,
}

impl SmartAudioBuffer {
    /// Create a new smart audio buffer.
    pub fn new(sample_rate: u32, target_duration_ms: u32) -> Self {
        Self { buffer: Vec::new(), sample_rate, target_duration_ms }
    }

    /// Push new samples into the buffer.
    pub fn push(&mut self, samples: &[i16]) {
        self.buffer.extend_from_slice(samples);
    }

    fn should_flush(&self) -> bool {
        let duration_ms = (self.buffer.len() as f64 / self.sample_rate as f64) * 1000.0;

        duration_ms >= self.target_duration_ms as f64
    }

    /// Flush the buffer if the target duration has been reached.
    pub fn flush(&mut self) -> Option<Vec<i16>> {
        if self.should_flush() { Some(std::mem::take(&mut self.buffer)) } else { None }
    }

    /// Flush any remaining samples in the buffer.
    pub fn flush_remaining(&mut self) -> Option<Vec<i16>> {
        if self.buffer.is_empty() { None } else { Some(std::mem::take(&mut self.buffer)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smart_audio_buffer_flush_threshold() {
        let sample_rate = 1000;
        let target_ms = 100;
        // 1000 samples/sec -> 1 sample = 1ms.
        // target 100ms -> 100 samples.

        let mut buffer = SmartAudioBuffer::new(sample_rate, target_ms);

        // Push 50 samples (50ms)
        buffer.push(&[0; 50]);
        assert!(buffer.flush().is_none());

        // Push 49 samples (total 99ms)
        buffer.push(&[0; 49]);
        assert!(buffer.flush().is_none());

        // Push 1 sample (total 100ms)
        buffer.push(&[0; 1]);
        let flushed = buffer.flush();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 100);
        assert!(buffer.buffer.is_empty());
    }

    #[test]
    fn test_smart_audio_buffer_flush_remaining() {
        let sample_rate = 1000;
        let target_ms = 100;
        let mut buffer = SmartAudioBuffer::new(sample_rate, target_ms);

        buffer.push(&[0; 50]);
        assert!(buffer.flush().is_none());

        let remaining = buffer.flush_remaining();
        assert!(remaining.is_some());
        assert_eq!(remaining.unwrap().len(), 50);
        assert!(buffer.buffer.is_empty());
    }

    #[test]
    fn test_smart_audio_buffer_empty_flush() {
        let mut buffer = SmartAudioBuffer::new(1000, 100);
        assert!(buffer.flush().is_none());
        assert!(buffer.flush_remaining().is_none());
    }

    #[test]
    fn test_audio_format_bytes_per_second() {
        let pcm16_24k = AudioFormat::pcm16_24khz();
        assert_eq!(pcm16_24k.bytes_per_second(), 48000); // 24000 * 1 * 2

        let pcm16_16k = AudioFormat::pcm16_16khz();
        assert_eq!(pcm16_16k.bytes_per_second(), 32000); // 16000 * 1 * 2
    }

    #[test]
    fn test_audio_format_duration() {
        let format = AudioFormat::pcm16_24khz();
        // 48000 bytes = 1 second
        let duration = format.duration_ms(48000);
        assert!((duration - 1000.0).abs() < 0.001);
    }

    #[test]
    fn test_audio_chunk_base64() {
        let original = AudioChunk::pcm16_24khz(vec![0, 1, 2, 3, 4, 5]);
        let encoded = original.to_base64();
        let decoded = AudioChunk::from_base64(&encoded, AudioFormat::pcm16_24khz()).unwrap();
        assert_eq!(original.data, decoded.data);
    }

    #[test]
    fn test_i16_samples_roundtrip() {
        let samples: Vec<i16> = vec![0, 1, -1, 32767, -32768, 1000, -1000];
        let chunk = AudioChunk::from_i16_samples(&samples, AudioFormat::pcm16_24khz());
        let recovered = chunk.to_i16_samples().unwrap();
        assert_eq!(samples, recovered);
    }

    #[test]
    fn test_i16_samples_empty() {
        let chunk = AudioChunk::from_i16_samples(&[], AudioFormat::pcm16_24khz());
        assert!(chunk.data.is_empty());
        assert_eq!(chunk.to_i16_samples().unwrap(), Vec::<i16>::new());
    }

    #[test]
    fn test_i16_samples_odd_bytes_error() {
        let chunk = AudioChunk::pcm16_24khz(vec![0, 1, 2]); // 3 bytes = invalid PCM16
        assert!(chunk.to_i16_samples().is_err());
    }
}
