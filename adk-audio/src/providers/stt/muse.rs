//! Meta Muse speech-to-text (`muse-voice-transcribe-1.0`) over `adk-model`'s Muse clients
//! (feature `muse`).

use std::pin::Pin;

use adk_model::muse::{
    AsrEncoding, AsrEvent, AsrMode, MuseAsrClient, MuseConfig, MuseRealtimeAsrClient,
    RealtimeAsrOptions, TranscribeOptions,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt};

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::traits::{
    Speaker, SttOptions, SttProvider, Transcript, TranscriptEvent, TranscriptStream,
};

/// Speech-to-text with Meta's `muse-voice-transcribe-1.0`: file transcription for
/// [`transcribe`](SttProvider::transcribe) and the realtime WebSocket for
/// [`transcribe_stream`](SttProvider::transcribe_stream).
///
/// | Live event | From |
/// |------------|------|
/// | `Started` | The acknowledged handshake |
/// | `SpeechStarted`, `SpeechEnded` | `speechStart`, `speechEnd` |
/// | `Partial` | Each cumulative `transcript` |
/// | `Final` | `speechComplete` per turn ([`AsrMode::Endpointing`], [`AsrMode::Diarization`]), or the final `transcript` ([`AsrMode::PushToTalk`]) |
/// | `Speaker` | `speaker` (diarization) |
/// | `Progress` | `audioProgress` |
/// | `Usage` | The audio processed when the session closed normally |
///
/// Live audio frames must be 16-bit PCM mono at the encoding's rate (16 kHz unless
/// [`with_encoding`](Self::with_encoding) says otherwise). `SttOptions::diarize` selects
/// diarization; `keywords` and `language_bias` (or `language`) become the session's hints.
/// A session lasts at most 60 minutes: the limit ends the stream with an
/// [`AudioError::Stt`] naming `model.muse.session_limit`, and the client does not
/// reconnect. The Muse clients meter their own calls against the current run, so
/// [`billing`](SttProvider::billing) is `None` and `TranscribeTool` does not meter them a
/// second time.
///
/// # Example
///
/// ```rust,no_run
/// use adk_audio::{MuseStt, SttOptions, SttProvider};
///
/// # async fn demo(audio: adk_audio::AudioFrame) -> adk_audio::AudioResult<()> {
/// let stt = MuseStt::from_env()?;
/// let transcript = stt.transcribe(&audio, &SttOptions::default().with_keywords(["ADK-Rust"])).await?;
/// println!("{}", transcript.text);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct MuseStt {
    batch: std::sync::Arc<MuseAsrClient>,
    live: MuseRealtimeAsrClient,
    mode: AsrMode,
    encoding: AsrEncoding,
}

impl std::fmt::Debug for MuseStt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuseStt")
            .field("mode", &self.mode)
            .field("encoding", &self.encoding)
            .finish_non_exhaustive()
    }
}

impl MuseStt {
    /// A provider over `config`; the model defaults to `muse-voice-transcribe-1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] when a Muse client cannot be built.
    pub fn new(config: MuseConfig) -> AudioResult<Self> {
        let batch = MuseAsrClient::new(config.clone()).map_err(muse_error)?;
        let live = MuseRealtimeAsrClient::new(config).map_err(muse_error)?;
        Ok(Self {
            batch: std::sync::Arc::new(batch),
            live,
            mode: AsrMode::default(),
            encoding: AsrEncoding::default(),
        })
    }

    /// A provider keyed from `META_API_KEY`, or `MODEL_API_KEY`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] when neither variable is set.
    pub fn from_env() -> AudioResult<Self> {
        let key = std::env::var("META_API_KEY")
            .or_else(|_| std::env::var("MODEL_API_KEY"))
            .map_err(|_| AudioError::Stt {
                provider: "muse".into(),
                message: "META_API_KEY not set".into(),
            })?;
        Self::new(MuseConfig::new(key, adk_model::muse::MUSE_VOICE_MODEL))
    }

    /// Sets when live turn boundaries are reported; `SttOptions::diarize` overrides it with
    /// [`AsrMode::Diarization`].
    #[must_use]
    pub fn with_mode(mut self, mode: AsrMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the encoding of live audio frames.
    #[must_use]
    pub fn with_encoding(mut self, encoding: AsrEncoding) -> Self {
        self.encoding = encoding;
        self
    }
}

/// An `adk-model` error as the audio error a caller of [`SttProvider`] receives, its code
/// kept in the message.
fn muse_error(error: adk_core::AdkError) -> AudioError {
    AudioError::Stt {
        provider: "muse".into(),
        message: format!("{} ({})", error.message, error.code),
    }
}

/// The languages a session is biased to: `language_bias`, or `language` alone.
fn languages(opts: &SttOptions) -> Vec<String> {
    if opts.language_bias.is_empty() {
        opts.language.iter().cloned().collect()
    } else {
        opts.language_bias.clone()
    }
}

/// The [`TranscriptEvent`] a realtime event becomes in `mode`; `None` for an event the mode
/// reports twice (a final `transcript` beside `speechComplete`) or one added later.
fn transcript_event(event: AsrEvent, mode: AsrMode) -> Option<TranscriptEvent> {
    let final_of = |turn_id, text| TranscriptEvent::Final {
        turn_id,
        transcript: Transcript { text, ..Transcript::default() },
    };
    Some(match event {
        AsrEvent::Started { session_id } => {
            TranscriptEvent::Started { session_id: Some(session_id) }
        }
        AsrEvent::SpeechStart { turn_id, audio_processed_ms } => {
            TranscriptEvent::SpeechStarted { turn_id: Some(turn_id), offset_ms: audio_processed_ms }
        }
        AsrEvent::Transcript { turn_id, text, is_final: true, .. }
            if mode == AsrMode::PushToTalk =>
        {
            final_of(turn_id, text)
        }
        AsrEvent::Transcript { turn_id, text, .. } => TranscriptEvent::Partial { turn_id, text },
        AsrEvent::SpeechEnd { turn_id } => {
            TranscriptEvent::SpeechEnded { turn_id: Some(turn_id), offset_ms: None }
        }
        AsrEvent::SpeechComplete { turn_id, text } => final_of(Some(turn_id), text),
        AsrEvent::Speaker { label } => TranscriptEvent::Speaker { label, offset_ms: None },
        AsrEvent::AudioProgress { audio_processed_ms } => {
            TranscriptEvent::Progress { processed_ms: audio_processed_ms }
        }
        AsrEvent::Usage { audio_ms } => TranscriptEvent::Usage { audio_ms },
        _ => return None,
    })
}

#[async_trait]
impl SttProvider for MuseStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        let wav = super::frame_to_wav_bytes(audio)?;
        let options = TranscribeOptions {
            mode: opts.diarize.then(|| AsrMode::Diarization.as_str().to_string()),
            language_bias: languages(opts),
            keywords: opts.keywords.clone(),
        };
        let transcript = self
            .batch
            .transcribe(wav.to_vec(), "audio.wav", "audio/wav", options)
            .await
            .map_err(muse_error)?;
        let mut labels: Vec<String> = Vec::new();
        for label in transcript.segments.iter().filter_map(|segment| segment.speaker.clone()) {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        let speakers = labels
            .into_iter()
            .zip(0_u32..)
            .map(|(label, id)| Speaker { id, label: Some(label) })
            .collect();
        Ok(Transcript { text: transcript.text, speakers, ..Transcript::default() })
    }

    async fn transcribe_stream(
        &self,
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        opts: &SttOptions,
    ) -> AudioResult<TranscriptStream> {
        let mode = if opts.diarize { AsrMode::Diarization } else { self.mode };
        let options = RealtimeAsrOptions::default()
            .with_mode(mode)
            .with_encoding(self.encoding)
            .with_keywords(opts.keywords.clone())
            .with_language_bias(languages(opts));
        let events = self
            .live
            .clone()
            .with_options(options)
            .transcribe(audio.map(|frame| frame.data))
            .await
            .map_err(muse_error)?;
        Ok(Box::pin(events.filter_map(move |event| async move {
            match event {
                Ok(event) => transcript_event(event, mode).map(Ok),
                Err(error) => Some(Err(muse_error(error))),
            }
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(event: Option<TranscriptEvent>) -> Option<(&'static str, String)> {
        match event? {
            TranscriptEvent::Partial { text, .. } => Some(("partial", text)),
            TranscriptEvent::Final { transcript, .. } => Some(("final", transcript.text)),
            other => Some(("other", format!("{other:?}"))),
        }
    }

    #[test]
    fn endpointing_finals_come_from_speech_complete_and_push_to_talk_from_the_final_transcript() {
        let final_transcript = || AsrEvent::Transcript {
            turn_id: Some(1),
            text: "hello".into(),
            is_final: true,
            audio_processed_ms: None,
        };
        let complete = || AsrEvent::SpeechComplete { turn_id: 1, text: "hello".into() };
        assert_eq!(
            text_of(transcript_event(final_transcript(), AsrMode::Endpointing)),
            Some(("partial", "hello".into()))
        );
        assert_eq!(
            text_of(transcript_event(complete(), AsrMode::Endpointing)),
            Some(("final", "hello".into()))
        );
        assert_eq!(
            text_of(transcript_event(final_transcript(), AsrMode::PushToTalk)),
            Some(("final", "hello".into()))
        );
    }

    #[test]
    fn the_language_hint_stands_in_for_an_empty_bias() {
        assert_eq!(languages(&SttOptions::default().with_language("sw")), ["sw"]);
        assert_eq!(
            languages(&SttOptions::default().with_language("sw").with_language_bias(["en", "fr"])),
            ["en", "fr"]
        );
        assert!(languages(&SttOptions::default()).is_empty());
    }

    #[test]
    fn errors_keep_the_muse_code() {
        let error = muse_error(adk_core::AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::Unavailable,
            "model.muse.session_limit",
            "the Muse realtime session closed with code 1011: Max session duration reached",
        ));
        assert!(
            matches!(&error, AudioError::Stt { provider, message } if provider == "muse" && message.contains("model.muse.session_limit")),
            "{error}"
        );
    }
}
