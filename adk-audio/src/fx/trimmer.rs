//! Silence trimmer.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Trims leading and trailing silence from audio frames.
pub struct SilenceTrimmer {
    threshold: i16,
}

impl SilenceTrimmer {
    /// Create a trimmer with the given silence threshold.
    pub fn new(threshold: i16) -> Self {
        Self { threshold: threshold.abs() }
    }
}

impl Default for SilenceTrimmer {
    fn default() -> Self {
        Self::new(100)
    }
}

#[async_trait]
impl AudioProcessor for SilenceTrimmer {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let samples = frame.samples();
        if samples.is_empty() {
            return Ok(frame.clone());
        }

        let start = samples.iter().position(|&s| s.abs() >= self.threshold).unwrap_or(0);
        let end =
            samples.iter().rposition(|&s| s.abs() >= self.threshold).map(|p| p + 1).unwrap_or(0);

        if start >= end {
            return Ok(AudioFrame::silence(frame.sample_rate, frame.channels, 0));
        }

        let trimmed: Vec<u8> = samples[start..end].iter().flat_map(|&s| s.to_le_bytes()).collect();

        Ok(AudioFrame::new(Bytes::from(trimmed), frame.sample_rate, frame.channels))
    }
}
