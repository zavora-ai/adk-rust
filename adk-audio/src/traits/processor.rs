//! Audio processor trait and FxChain composition.

use async_trait::async_trait;

use crate::error::AudioResult;
use crate::frame::AudioFrame;

/// Trait for stateless or stateful DSP transforms on audio frames.
///
/// Implementors include normalizers, resamplers, noise suppressors,
/// compressors, and the `FxChain` itself (enabling nested chains).
#[async_trait]
pub trait AudioProcessor: Send + Sync {
    /// Process a single audio frame, returning the transformed result.
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame>;
}

/// An ordered chain of `AudioProcessor` stages applied in series.
///
/// The output of stage N becomes the input to stage N+1.
/// An empty chain returns the input frame unchanged.
///
/// # Example
///
/// ```ignore
/// let chain = FxChain::new()
///     .push(normalizer)
///     .push(resampler);
/// let output = chain.process(&input).await?;
/// ```
pub struct FxChain {
    stages: Vec<Box<dyn AudioProcessor>>,
}

impl FxChain {
    /// Create an empty FxChain.
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Append a processing stage to the chain.
    pub fn push(mut self, processor: impl AudioProcessor + 'static) -> Self {
        self.stages.push(Box::new(processor));
        self
    }

    /// Returns the number of stages in the chain.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    /// Returns true if the chain has no stages.
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

impl Default for FxChain {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioProcessor for FxChain {
    async fn process(&self, frame: &AudioFrame) -> AudioResult<AudioFrame> {
        let mut current = frame.clone();
        for stage in &self.stages {
            current = stage.process(&current).await?;
        }
        Ok(current)
    }
}
