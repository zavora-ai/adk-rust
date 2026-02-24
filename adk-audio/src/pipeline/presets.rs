//! Preset pipeline factory functions for common audio topologies.

use std::sync::Arc;

use crate::error::AudioResult;
use crate::pipeline::builder::AudioPipelineBuilder;
use crate::pipeline::handle::PipelineHandle;
use crate::traits::{SttProvider, TtsProvider, VadProcessor};

/// IVR voice agent pipeline: high-aggressiveness VAD, telephony normalization.
///
/// Requires TTS, STT, VAD, and an Agent.
pub fn ivr_pipeline(
    tts: Arc<dyn TtsProvider>,
    stt: Arc<dyn SttProvider>,
    vad: Arc<dyn VadProcessor>,
    agent: Arc<dyn adk_core::Agent>,
) -> AudioResult<PipelineHandle> {
    AudioPipelineBuilder::new().tts(tts).stt(stt).vad(vad).agent(agent).build_voice_agent()
}

/// Podcast production pipeline: TTS with broadcast-quality processing.
pub fn podcast_pipeline(tts: Arc<dyn TtsProvider>) -> AudioResult<PipelineHandle> {
    AudioPipelineBuilder::new().tts(tts).build_tts()
}

/// Meeting transcription pipeline: STT with low-aggressiveness VAD.
pub fn transcription_pipeline(stt: Arc<dyn SttProvider>) -> AudioResult<PipelineHandle> {
    AudioPipelineBuilder::new().stt(stt).build_stt()
}

/// Audio enhancement pipeline: noise suppression, resampling, normalization.
pub fn enhance_pipeline() -> AudioResult<PipelineHandle> {
    AudioPipelineBuilder::new().build_transform()
}
