//! LiveKit event handler wrapper that publishes model audio to a LiveKit room.

use std::borrow::Cow;

use async_trait::async_trait;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;

use crate::error::{RealtimeError, Result};
use crate::runner::EventHandler;

/// Wraps an inner [`EventHandler`] and intercepts `on_audio` to push PCM16 data
/// to a LiveKit [`NativeAudioSource`].
///
/// All non-audio event methods are delegated to the inner handler without modification.
/// If pushing audio to the `NativeAudioSource` fails, the error is logged via
/// `tracing::warn` and processing continues — audio push failures are never propagated.
pub struct LiveKitEventHandler<H: EventHandler> {
    inner: H,
    audio_source: NativeAudioSource,
    sample_rate: u32,
    num_channels: u32,
}

impl<H: EventHandler> LiveKitEventHandler<H> {
    /// Create a new `LiveKitEventHandler` wrapping the given inner handler.
    ///
    /// # Arguments
    ///
    /// * `inner` — The inner event handler to delegate to.
    /// * `audio_source` — The LiveKit native audio source to push model audio to.
    /// * `sample_rate` — Sample rate of the audio (e.g., 24000 for OpenAI, 16000 for Gemini).
    /// * `num_channels` — Number of audio channels (typically 1 for mono).
    pub fn new(
        inner: H,
        audio_source: NativeAudioSource,
        sample_rate: u32,
        num_channels: u32,
    ) -> Self {
        Self { inner, audio_source, sample_rate, num_channels }
    }
}

#[async_trait]
impl<H: EventHandler> EventHandler for LiveKitEventHandler<H> {
    async fn on_audio(&self, audio: &[u8], item_id: &str) -> Result<()> {
        // Forward to inner handler first
        self.inner.on_audio(audio, item_id).await?;

        // Convert PCM bytes to i16 samples and push to LiveKit
        let samples: &[i16] = bytemuck::cast_slice(audio);
        let samples_per_channel = samples.len() as u32 / self.num_channels;
        let frame = AudioFrame {
            data: Cow::Borrowed(samples),
            sample_rate: self.sample_rate,
            num_channels: self.num_channels,
            samples_per_channel,
        };
        if let Err(e) = self.audio_source.capture_frame(&frame).await {
            tracing::warn!(error = %e, "Failed to push audio to LiveKit NativeAudioSource");
        }
        Ok(())
    }

    async fn on_text(&self, text: &str, item_id: &str) -> Result<()> {
        self.inner.on_text(text, item_id).await
    }

    async fn on_transcript(&self, transcript: &str, item_id: &str) -> Result<()> {
        self.inner.on_transcript(transcript, item_id).await
    }

    async fn on_speech_started(&self, audio_start_ms: u64) -> Result<()> {
        self.inner.on_speech_started(audio_start_ms).await
    }

    async fn on_speech_stopped(&self, audio_end_ms: u64) -> Result<()> {
        self.inner.on_speech_stopped(audio_end_ms).await
    }

    async fn on_response_done(&self) -> Result<()> {
        self.inner.on_response_done().await
    }

    async fn on_error(&self, error: &RealtimeError) -> Result<()> {
        self.inner.on_error(error).await
    }
}
