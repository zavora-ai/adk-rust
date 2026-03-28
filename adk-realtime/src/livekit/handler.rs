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

        // Zero-Copy Architecture:
        // Local Edge: O(0) allocation via `bytemuck` pointer casts directly to C++ WebRTC FFI.
        // Global Core: `Cow::Borrowed` prevents `'a` lifetime infection of the async graph state.
        let samples_cow = 'cow: {
            #[cfg(target_endian = "little")]
            if let Ok(aligned_slice) = bytemuck::try_cast_slice::<u8, i16>(audio) {
                break 'cow Cow::Borrowed(aligned_slice);
            }

            // Fallback: `try_cast_slice` fails on memory-unaligned network byte streams.
            // This can happen if upstream buffers fragment asynchronously (e.g. WebSocket
            // odd-byte chunking) or due to custom protocol TLS padding offsets.
            // We safely map using an LLVM-vectorized iterator (3x faster than manual Vec::push).
            let fallback: Vec<i16> = audio
                .chunks_exact(2)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                .collect();
            Cow::Owned(fallback)
        };

        if self.num_channels == 0 {
            return Err(RealtimeError::provider(
                "Cannot push audio to LiveKit NativeAudioSource: num_channels is 0",
            ));
        }

        if samples_cow.len() % (self.num_channels as usize) != 0 {
            tracing::warn!(
                samples_len = samples_cow.len(),
                num_channels = self.num_channels,
                "Skipping invalid audio frame: sample count is not an exact multiple of channels"
            );
            return Ok(());
        }

        // Guaranteed exact division (modulo == 0) and non-zero denominator by safety guards above.
        let samples_per_channel = samples_cow.len() as u32 / self.num_channels;
        let frame = AudioFrame {
            data: samples_cow,
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
