//! Noise suppressor.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Simple noise gate / suppressor.
///
/// Attenuates samples below a configurable threshold.
pub struct NoiseSuppressor {
    threshold: i16,
}

impl NoiseSuppressor {
    /// Create a noise suppressor with the given threshold (0–32767).
    pub fn new(threshold: i16) -> Self {
        Self { threshold: threshold.abs() }
    }
}

impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self::new(200)
    }
}

#[async_trait]
impl AudioProcessor for NoiseSuppressor {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let samples = frame.samples();
        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let out = if s.abs() < self.threshold { 0i16 } else { s };
                out.to_le_bytes()
            })
            .collect();

        Ok(AudioFrame::new(Bytes::from(pcm), frame.sample_rate, frame.channels))
    }
}
