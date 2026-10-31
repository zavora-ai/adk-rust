//! Speech-to-text provider trait and response types.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::AudioResult;
use crate::frame::AudioFrame;
use crate::metering::AudioBilling;

/// Options for speech-to-text transcription.
#[derive(Debug, Clone, Default)]
pub struct SttOptions {
    /// Optional BCP-47 language hint.
    pub language: Option<String>,
    /// Enable speaker diarization.
    pub diarize: bool,
    /// Include per-word timestamps.
    pub word_timestamps: bool,
    /// Apply smart formatting (punctuation, casing).
    pub smart_format: bool,
    /// Optional model hint for the provider.
    pub model_hint: Option<String>,
}

/// A transcription result.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    /// Full transcribed text.
    pub text: String,
    /// Per-word details with timestamps.
    pub words: Vec<Word>,
    /// Identified speakers.
    pub speakers: Vec<Speaker>,
    /// Overall confidence score (0.0–1.0).
    pub confidence: f32,
    /// Detected language (BCP-47).
    pub language_detected: Option<String>,
}

/// A single word with timing and confidence.
#[derive(Debug, Clone)]
pub struct Word {
    /// The word text.
    pub text: String,
    /// Start time in milliseconds.
    pub start_ms: u32,
    /// End time in milliseconds.
    pub end_ms: u32,
    /// Word-level confidence (0.0–1.0).
    pub confidence: f32,
    /// Speaker ID if diarization is enabled.
    pub speaker: Option<u32>,
}

/// An identified speaker.
#[derive(Debug, Clone)]
pub struct Speaker {
    /// Numeric speaker identifier.
    pub id: u32,
    /// Optional human-readable label.
    pub label: Option<String>,
}

/// Unified trait for speech-to-text providers.
///
/// Implementors include cloud services (Whisper API, Deepgram, AssemblyAI)
/// and local models (MLX Whisper).
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe a single audio frame (batch mode).
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript>;

    /// Transcribe a stream of audio frames (streaming mode).
    ///
    /// A provider without streaming support returns [`AudioError::Stt`](crate::AudioError::Stt)
    /// instead of an empty stream. Dropping the returned stream stops the provider's work for
    /// it, including any task that forwards `audio`, and releases `audio`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`](crate::AudioError::Stt) when the provider does not support
    /// streaming or cannot open the stream.
    async fn transcribe_stream(
        &self,
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>>;

    /// The vendor and model this provider's calls are billed under, for metering them
    /// against the current run ([`crate::metering`]). `None`, the default, for a provider
    /// that bills nothing, such as a local model.
    fn billing(&self) -> Option<AudioBilling> {
        None
    }
}
