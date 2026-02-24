//! Dynamic range compressor.

use async_trait::async_trait;
use bytes::Bytes;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::traits::AudioProcessor;

/// Dynamic range compressor.
///
/// Reduces the volume of loud sounds above the threshold by the given ratio.
pub struct DynamicRangeCompressor {
    threshold: f32,
    ratio: f32,
}

impl DynamicRangeCompressor {
    /// Create a compressor with threshold (0.0–1.0) and ratio (e.g. 4.0 = 4:1).
    pub fn new(threshold: f32, ratio: f32) -> Self {
        Self { threshold: threshold.clamp(0.0, 1.0), ratio: ratio.max(1.0) }
    }
}

impl Default for DynamicRangeCompressor {
    fn default() -> Self {
        Self::new(0.5, 4.0)
    }
}

#[async_trait]
impl AudioProcessor for DynamicRangeCompressor {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let threshold_abs = self.threshold * 32767.0;
        let samples = frame.samples();
        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|&s| {
                let abs = (s as f32).abs();
                let out = if abs > threshold_abs {
                    let excess = abs - threshold_abs;
                    let compressed = threshold_abs + excess / self.ratio;
                    let sign = if s >= 0 { 1.0 } else { -1.0 };
                    (compressed * sign) as i32
                } else {
                    s as i32
                };
                let clamped = out.clamp(-32768, 32767) as i16;
                clamped.to_le_bytes()
            })
            .collect();

        Ok(AudioFrame::new(Bytes::from(pcm), frame.sample_rate, frame.channels))
    }
}
