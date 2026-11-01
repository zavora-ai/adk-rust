//! Speech-to-text provider trait and response types.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::metering::AudioBilling;

/// Options for speech-to-text transcription.
///
/// # Example
///
/// ```
/// use adk_audio::SttOptions;
///
/// let options = SttOptions::default()
///     .with_language("en-US")
///     .with_keywords(["Muse", "ADK-Rust"])
///     .with_language_bias(["en", "sw"])
///     .with_diarize(true);
/// assert_eq!(options.keywords, ["Muse", "ADK-Rust"]);
/// assert!(options.diarize);
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
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
    /// Domain vocabulary the provider should favour, for providers that take keyword hints.
    pub keywords: Vec<String>,
    /// BCP-47 languages the audio is likely in, for providers that take several.
    pub language_bias: Vec<String>,
}

impl SttOptions {
    /// Sets the language hint.
    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }

    /// Turns speaker diarization on or off.
    #[must_use]
    pub fn with_diarize(mut self, diarize: bool) -> Self {
        self.diarize = diarize;
        self
    }

    /// Turns per-word timestamps on or off.
    #[must_use]
    pub fn with_word_timestamps(mut self, word_timestamps: bool) -> Self {
        self.word_timestamps = word_timestamps;
        self
    }

    /// Turns smart formatting on or off.
    #[must_use]
    pub fn with_smart_format(mut self, smart_format: bool) -> Self {
        self.smart_format = smart_format;
        self
    }

    /// Sets the model hint.
    #[must_use]
    pub fn with_model_hint(mut self, model: impl Into<String>) -> Self {
        self.model_hint = Some(model.into());
        self
    }

    /// Sets the keyword hints.
    #[must_use]
    pub fn with_keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the languages the audio is likely in.
    #[must_use]
    pub fn with_language_bias(
        mut self,
        languages: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.language_bias = languages.into_iter().map(Into::into).collect();
        self
    }
}

/// One event of a live transcription ([`SttProvider::transcribe_stream`]).
///
/// | Event | Meaning |
/// |-------|---------|
/// | [`Started`](Self::Started) | The provider accepted the session |
/// | [`SpeechStarted`](Self::SpeechStarted) | Speech began; later events with the same `turn_id` belong to it |
/// | [`Partial`](Self::Partial) | The current hypothesis for the turn, in full; a later `Partial` or the `Final` replaces it |
/// | [`Final`](Self::Final) | Text the provider will not revise; at most one per turn for providers that number turns |
/// | [`SpeechEnded`](Self::SpeechEnded) | Speech stopped |
/// | [`Speaker`](Self::Speaker) | The speaker changed (diarization) |
/// | [`Progress`](Self::Progress) | How much audio the provider has processed |
/// | [`Usage`](Self::Usage) | The audio the session is billed for, its billing basis |
///
/// A provider that streams deltas reports them as cumulative `Partial` text, so a consumer
/// always replaces the displayed hypothesis rather than appending to it.
///
/// # Example
///
/// ```
/// use adk_audio::{Transcript, TranscriptEvent};
///
/// let events = [
///     TranscriptEvent::Partial { turn_id: Some(1), text: "hello wor".into() },
///     TranscriptEvent::Final {
///         turn_id: Some(1),
///         transcript: Transcript { text: "hello world".into(), ..Default::default() },
///     },
/// ];
/// let finals: Vec<&str> = events
///     .iter()
///     .filter_map(|event| match event {
///         TranscriptEvent::Final { transcript, .. } => Some(transcript.text.as_str()),
///         _ => None,
///     })
///     .collect();
/// assert_eq!(finals, ["hello world"]);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TranscriptEvent {
    /// The provider accepted the session.
    Started {
        /// The provider's session id, when it reports one.
        session_id: Option<String>,
    },
    /// Speech began.
    SpeechStarted {
        /// The turn the speech opens, when the provider numbers turns.
        turn_id: Option<u32>,
        /// Where in the audio it began, in milliseconds, when reported.
        offset_ms: Option<u64>,
    },
    /// The current hypothesis for a turn, in full.
    Partial {
        /// The turn, when the provider numbers turns.
        turn_id: Option<u32>,
        /// The hypothesis.
        text: String,
    },
    /// Text the provider will not revise.
    Final {
        /// The turn, when the provider numbers turns.
        turn_id: Option<u32>,
        /// The transcript.
        transcript: Transcript,
    },
    /// Speech stopped.
    SpeechEnded {
        /// The turn the silence closes, when the provider numbers turns.
        turn_id: Option<u32>,
        /// Where in the audio it stopped, in milliseconds, when reported.
        offset_ms: Option<u64>,
    },
    /// The speaker changed.
    Speaker {
        /// The provider's label for the speaker.
        label: String,
        /// Where in the audio the change happened, in milliseconds, when reported.
        offset_ms: Option<u64>,
    },
    /// Audio the provider has processed so far, in milliseconds.
    Progress {
        /// Milliseconds processed.
        processed_ms: u64,
    },
    /// The audio the session is billed for, in milliseconds.
    Usage {
        /// Milliseconds billed.
        audio_ms: u64,
    },
}

/// A live transcription: the events of [`SttProvider::transcribe_stream`].
pub type TranscriptStream = Pin<Box<dyn Stream<Item = AudioResult<TranscriptEvent>> + Send>>;

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

    /// Transcribes a stream of audio frames as they arrive, yielding [`TranscriptEvent`]s.
    ///
    /// The default, for a provider without streaming support, returns
    /// [`AudioError::Stt`] instead of an empty stream. Dropping the returned stream stops the
    /// provider's work for it, including any task that forwards `audio`, and releases
    /// `audio`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] when the provider does not support streaming or cannot
    /// open the stream.
    async fn transcribe_stream(
        &self,
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        opts: &SttOptions,
    ) -> AudioResult<TranscriptStream> {
        let _ = (audio, opts);
        Err(AudioError::Stt {
            provider: "stt".into(),
            message: "this provider does not support streaming transcription; use transcribe"
                .into(),
        })
    }

    /// The vendor and model this provider's calls are billed under, for metering them
    /// against the current run ([`crate::metering`]). `None`, the default, for a provider
    /// that bills nothing, such as a local model.
    fn billing(&self) -> Option<AudioBilling> {
        None
    }
}
