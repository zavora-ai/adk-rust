//! EBU R128 loudness normalizer.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Loudness normalizer targeting EBU R128 standard.
///
/// Default target is -23 LUFS (broadcast standard).
pub struct LoudnessNormalizer {
    target_lufs: f64,
}

impl LoudnessNormalizer {
    /// Create a normalizer with the default -23 LUFS target.
    pub fn new() -> Self {
        Self { target_lufs: -23.0 }
    }

    /// Create a normalizer with a custom LUFS target.
    pub fn with_target(target_lufs: f64) -> Self {
        Self { target_lufs }
    }
}

impl Default for LoudnessNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioProcessor for LoudnessNormalizer {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let samples = frame.samples();
        if samples.is_empty() {
            return Ok(frame.clone());
        }

        // Compute RMS loudness (simplified LUFS approximation)
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        let rms = (sum_sq / samples.len() as f64).sqrt();
        if rms < 1.0 {
            return Ok(frame.clone());
        }

        let current_lufs = 20.0 * (rms / 32768.0).log10();
        let gain_db = self.target_lufs - current_lufs;
        let gain_linear = 10.0_f64.powf(gain_db / 20.0);

        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let scaled = (s as f64 * gain_linear) as i32;
                let clamped = scaled.clamp(-32768, 32767) as i16;
                clamped.to_le_bytes()
            })
            .collect();

        Ok(AudioFrame::new(Bytes::from(pcm), frame.sample_rate, frame.channels))
    }
}
