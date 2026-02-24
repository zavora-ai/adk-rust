//! Sample rate resampler using rubato.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Sample rate resampler (8000–96000 Hz).
///
/// Uses linear interpolation for resampling. For production use,
/// the `rubato` crate provides higher-quality sinc interpolation.
pub struct Resampler {
    target_sample_rate: u32,
}

impl Resampler {
    /// Create a resampler targeting the given sample rate.
    pub fn new(target_sample_rate: u32) -> Self {
        Self { target_sample_rate }
    }
}

#[async_trait]
impl AudioProcessor for Resampler {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        if frame.sample_rate == self.target_sample_rate {
            return Ok(frame.clone());
        }
        if !(8000..=96000).contains(&self.target_sample_rate) {
            return Err(AudioError::Fx(format!(
                "target sample rate {} out of range 8000–96000",
                self.target_sample_rate
            )));
        }

        let samples = frame.samples();
        let ratio = self.target_sample_rate as f64 / frame.sample_rate as f64;
        let new_len = (samples.len() as f64 * ratio) as usize;

        let mut resampled = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let src_pos = i as f64 / ratio;
            let idx = src_pos as usize;
            let frac = src_pos - idx as f64;
            let s0 = samples.get(idx).copied().unwrap_or(0) as f64;
            let s1 = samples.get(idx + 1).copied().unwrap_or(s0 as i16) as f64;
            let interpolated = s0 + frac * (s1 - s0);
            resampled.push(interpolated.clamp(-32768.0, 32767.0) as i16);
        }

        let pcm: Vec<u8> = resampled.iter().flat_map(|&s| s.to_le_bytes()).collect();

        Ok(AudioFrame::new(Bytes::from(pcm), self.target_sample_rate, frame.channels))
    }
}
