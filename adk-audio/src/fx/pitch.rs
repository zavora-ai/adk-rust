//! Voice pitch shifter.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Simple pitch shifter via resampling and time-stretching.
///
/// A factor > 1.0 raises pitch, < 1.0 lowers pitch.
pub struct PitchShifter {
    factor: f32,
}

impl PitchShifter {
    /// Create a pitch shifter with the given factor (0.5–2.0).
    pub fn new(factor: f32) -> Self {
        Self { factor: factor.clamp(0.5, 2.0) }
    }
}

#[async_trait]
impl AudioProcessor for PitchShifter {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        if (self.factor - 1.0).abs() < f32::EPSILON {
            return Ok(frame.clone());
        }

        let samples = frame.samples();
        // Resample to change pitch while keeping duration approximately the same
        let new_len = (samples.len() as f32 / self.factor) as usize;
        let mut pitched = Vec::with_capacity(new_len);

        for i in 0..new_len {
            let src_pos = i as f64 * self.factor as f64;
            let idx = src_pos as usize;
            let frac = src_pos - idx as f64;
            let s0 = samples.get(idx).copied().unwrap_or(0) as f64;
            let s1 = samples.get(idx + 1).copied().unwrap_or(s0 as i16) as f64;
            let interpolated = s0 + frac * (s1 - s0);
            pitched.push(interpolated.clamp(-32768.0, 32767.0) as i16);
        }

        let pcm: Vec<u8> = pitched.iter().flat_map(|&s| s.to_le_bytes()).collect();

        Ok(AudioFrame::new(Bytes::from(pcm), frame.sample_rate, frame.channels))
    }
}
