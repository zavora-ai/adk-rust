//! Bridge between adk-realtime transport and adk-audio pipelines.
//!
//! Converts between realtime transport audio events and pipeline
//! `PipelineInput`/`PipelineOutput` messages. Requires the `livekit` feature.

use std::pin::Pin;

use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::pipeline::{PipelineInput, PipelineOutput};

/// Bridge connecting `adk-realtime` audio streams to `adk-audio` pipelines.
pub struct RealtimeBridge {
    sample_rate: u32,
    channels: u16,
}

impl RealtimeBridge {
    /// Create a new bridge with the given audio parameters.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self { sample_rate, channels }
    }

    /// Convert a stream of base64-encoded PCM16 audio deltas into pipeline inputs.
    pub fn from_realtime(
        &self,
        audio_deltas: Pin<Box<dyn Stream<Item = String> + Send>>,
    ) -> Pin<Box<dyn Stream<Item = AudioResult<PipelineInput>> + Send>> {
        use base64::Engine;
        use futures::StreamExt;

        let sample_rate = self.sample_rate;
        let channels = self.channels;

        let stream = async_stream::stream! {
            let mut deltas = audio_deltas;
            while let Some(b64) = deltas.next().await {
                match base64::engine::general_purpose::STANDARD.decode(&b64) {
                    Ok(pcm_bytes) => {
                        if pcm_bytes.len() >= 2 {
                            let frame = AudioFrame::new(pcm_bytes, sample_rate, channels as u8);
                            yield Ok(PipelineInput::Audio(frame));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("base64 decode failed in realtime bridge: {e}");
                        yield Err(AudioError::Codec(format!("base64-pcm16 decode failed: {e}")));
                    }
                }
            }
        };
        Box::pin(stream)
    }

    /// Convert pipeline output audio frames into base64-encoded PCM16 strings.
    pub fn to_realtime(
        &self,
        pipeline_output: Pin<Box<dyn Stream<Item = PipelineOutput> + Send>>,
    ) -> Pin<Box<dyn Stream<Item = String> + Send>> {
        use base64::Engine;
        use futures::StreamExt;

        let stream = async_stream::stream! {
            let mut outputs = pipeline_output;
            while let Some(output) = outputs.next().await {
                if let PipelineOutput::Audio(frame) = output {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&frame.data);
                    yield b64;
                }
            }
        };
        Box::pin(stream)
    }
}
