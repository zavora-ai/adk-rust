//! Pipeline input, output, and control types.

use crate::frame::AudioFrame;
use crate::traits::Transcript;

/// Messages that can be sent into a pipeline.
pub enum PipelineInput {
    /// Raw audio data.
    Audio(AudioFrame),
    /// Text input (bypasses STT).
    Text(String),
    /// Control message.
    Control(PipelineControl),
}

/// Pipeline control commands.
pub enum PipelineControl {
    /// Shut down the pipeline gracefully.
    Stop,
    /// Pause processing.
    Pause,
    /// Resume processing.
    Resume,
}

/// Messages produced by a pipeline.
pub enum PipelineOutput {
    /// Synthesized or processed audio.
    Audio(AudioFrame),
    /// Transcription result.
    Transcript(Transcript),
    /// Agent text response (before TTS).
    AgentText(String),
    /// Pipeline performance metrics.
    Metrics(PipelineMetrics),
}

/// Real-time latency and quality metrics from pipeline stages.
#[derive(Debug, Clone, Default)]
pub struct PipelineMetrics {
    /// TTS synthesis latency in milliseconds.
    pub tts_latency_ms: f64,
    /// STT transcription latency in milliseconds.
    pub stt_latency_ms: f64,
    /// LLM agent reasoning latency in milliseconds.
    pub llm_latency_ms: f64,
    /// Total audio processed in milliseconds.
    pub total_audio_ms: u64,
    /// Ratio of speech frames to total frames (0.0–1.0).
    pub vad_speech_ratio: f32,
}
