//! LiveKit event handler wrapper that publishes model audio to a LiveKit room.

use std::borrow::Cow;

use async_trait::async_trait;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;

use crate::error::{RealtimeError, Result};
use crate::runner::EventHandler;

#[derive(Default)]
struct RemainderState {
    pending_bytes: Vec<u8>,
    item_id: Option<String>,
}

impl RemainderState {
    fn new() -> Self {
        Self { pending_bytes: Vec::new(), item_id: None }
    }

    fn clear_pending_state(&mut self, boundary: &str) {
        if !self.pending_bytes.is_empty() {
            let discarded_bytes = self.pending_bytes.len();
            self.pending_bytes.clear();
            let item_id = self.item_id.take().unwrap_or_else(|| "unknown".to_string());
            tracing::warn!(
                item_id = %item_id,
                discarded_bytes,
                boundary = %boundary,
                "Discarding incomplete PCM16 channel frame at boundary"
            );
        }
        self.item_id = None;
    }

    fn assemble<'a>(
        &mut self,
        audio: &'a [u8],
        item_id: &str,
        channel_frame_bytes: usize,
    ) -> Cow<'a, [i16]> {
        debug_assert!(channel_frame_bytes >= size_of::<i16>());

        if let Some(ref old_id) = self.item_id
            && old_id != item_id
        {
            let discarded_bytes = self.pending_bytes.len();
            self.pending_bytes.clear();
            tracing::warn!(
                item_id = old_id,
                next_item_id = item_id,
                discarded_bytes,
                "Discarding incomplete PCM16 channel frame on item boundary transition"
            );
            self.item_id = None;
        }

        if self.pending_bytes.is_empty() {
            let complete_len = audio.len() - (audio.len() % channel_frame_bytes);
            self.store_remainder(&audio[complete_len..], item_id);
            return decode_pcm16(&audio[..complete_len]);
        }

        let bytes_needed = channel_frame_bytes - self.pending_bytes.len();
        if audio.len() < bytes_needed {
            self.pending_bytes.extend_from_slice(audio);
            return Cow::Owned(Vec::new());
        }

        let remaining = &audio[bytes_needed..];
        let remaining_complete_len = remaining.len() - (remaining.len() % channel_frame_bytes);
        let mut samples =
            Vec::with_capacity((channel_frame_bytes + remaining_complete_len) / size_of::<i16>());

        self.pending_bytes.extend_from_slice(&audio[..bytes_needed]);
        append_pcm16_samples(&mut samples, &self.pending_bytes);
        self.pending_bytes.clear();
        append_pcm16_samples(&mut samples, &remaining[..remaining_complete_len]);
        self.store_remainder(&remaining[remaining_complete_len..], item_id);

        Cow::Owned(samples)
    }

    fn store_remainder(&mut self, remainder: &[u8], item_id: &str) {
        debug_assert!(self.pending_bytes.is_empty());
        if remainder.is_empty() {
            self.item_id = None;
            return;
        }

        self.pending_bytes.extend_from_slice(remainder);
        // Same-item carry already owns this identity; set it only for a new remainder.
        if self.item_id.is_none() {
            self.item_id = Some(item_id.to_string());
        }
    }
}

fn channel_frame_bytes(num_channels: u32) -> Option<usize> {
    usize::try_from(num_channels).ok()?.checked_mul(size_of::<i16>()).filter(|bytes| *bytes != 0)
}

fn decode_pcm16(audio: &[u8]) -> Cow<'_, [i16]> {
    debug_assert!(audio.len().is_multiple_of(size_of::<i16>()));

    #[cfg(target_endian = "little")]
    if let Ok(aligned_slice) = bytemuck::try_cast_slice::<u8, i16>(audio) {
        return Cow::Borrowed(aligned_slice);
    }

    Cow::Owned(
        audio
            .chunks_exact(size_of::<i16>())
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect(),
    )
}

fn append_pcm16_samples(samples: &mut Vec<i16>, audio: &[u8]) {
    debug_assert!(audio.len().is_multiple_of(size_of::<i16>()));
    samples.extend(
        audio.chunks_exact(size_of::<i16>()).map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]])),
    );
}

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
    state: parking_lot::Mutex<RemainderState>,
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
        Self {
            inner,
            audio_source,
            sample_rate,
            num_channels,
            state: parking_lot::Mutex::new(RemainderState::new()),
        }
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
        let Some(channel_frame_bytes) = channel_frame_bytes(self.num_channels) else {
            return Err(RealtimeError::provider(
                "Cannot push audio to LiveKit NativeAudioSource: num_channels is invalid",
            ));
        };
        let samples_cow = self.state.lock().assemble(audio, item_id, channel_frame_bytes);

        if samples_cow.is_empty() {
            return Ok(());
        }

        let samples_per_channel = u32::try_from(samples_cow.len() / self.num_channels as usize)
            .map_err(|_| {
                RealtimeError::provider(
                    "Cannot push audio to LiveKit NativeAudioSource: frame is too large",
                )
            })?;
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
        self.state.lock().clear_pending_state("response_done");
        self.inner.on_response_done().await
    }

    async fn on_response_cancelled(&self) -> Result<()> {
        self.state.lock().clear_pending_state("response_cancelled");
        self.inner.on_response_cancelled().await
    }

    async fn on_error(&self, error: &RealtimeError) -> Result<()> {
        self.state.lock().clear_pending_state("error");
        self.inner.on_error(error).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONO_FRAME_BYTES: usize = size_of::<i16>();
    const STEREO_FRAME_BYTES: usize = 2 * size_of::<i16>();

    #[test]
    fn aligned_complete_frame_uses_borrowed_samples_on_little_endian() {
        let mut state = RemainderState::new();
        let input = [i16::from_ne_bytes([0x01, 0x02]), i16::from_ne_bytes([0x03, 0x04])];
        let samples = state.assemble(bytemuck::cast_slice(&input), "item_a", MONO_FRAME_BYTES);

        assert_eq!(samples.as_ref(), &[0x0201, 0x0403]);
        #[cfg(target_endian = "little")]
        assert!(matches!(samples, Cow::Borrowed(_)));
        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn unaligned_complete_frame_uses_owned_samples() {
        let mut state = RemainderState::new();
        let aligned_words = [
            i16::from_ne_bytes([0x00, 0x01]),
            i16::from_ne_bytes([0x02, 0x03]),
            i16::from_ne_bytes([0x04, 0x00]),
        ];
        let aligned_bytes: &[u8] = bytemuck::cast_slice(&aligned_words);
        let input = &aligned_bytes[1..5];
        let samples = state.assemble(input, "item_a", MONO_FRAME_BYTES);

        assert_eq!(samples.as_ref(), &[0x0201, 0x0403]);
        assert!(matches!(samples, Cow::Owned(_)));
        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn split_pcm16_sample_is_reconstructed_for_same_item() {
        let mut state = RemainderState::new();

        let samples1 = state.assemble(&[0x01, 0x02, 0x03], "item_a", MONO_FRAME_BYTES);
        assert_eq!(samples1.as_ref(), &[0x0201]);
        assert_eq!(state.pending_bytes, [0x03]);
        assert_eq!(state.item_id.as_deref(), Some("item_a"));

        let samples2 = state.assemble(&[0x04], "item_a", MONO_FRAME_BYTES);
        assert_eq!(samples2.as_ref(), &[0x0403]);
        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn same_item_keeps_identity_while_remainder_remains_pending() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01], "item_a", MONO_FRAME_BYTES);
        let samples = state.assemble(&[0x02, 0x03], "item_a", MONO_FRAME_BYTES);

        assert_eq!(samples.as_ref(), &[0x0201]);
        assert_eq!(state.pending_bytes, [0x03]);
        assert_eq!(state.item_id.as_deref(), Some("item_a"));
    }

    #[test]
    fn item_transition_discards_previous_remainder() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01, 0x02, 0x03], "item_a", MONO_FRAME_BYTES);
        let samples2 = state.assemble(&[0x04, 0x05], "item_b", MONO_FRAME_BYTES);

        assert_eq!(samples2.as_ref(), &[0x0504]);
        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn response_done_boundary_clears_remainder() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01, 0x02, 0x03], "item_a", MONO_FRAME_BYTES);
        state.clear_pending_state("response_done");

        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn clearing_pending_state_twice_is_a_no_op() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01], "item_a", MONO_FRAME_BYTES);
        state.clear_pending_state("response_done");
        state.clear_pending_state("response_done");

        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn error_boundary_clears_remainder() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01, 0x02, 0x03], "item_a", MONO_FRAME_BYTES);
        state.clear_pending_state("error");

        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn cancellation_boundary_clears_remainder() {
        let mut state = RemainderState::new();

        state.assemble(&[0x01, 0x02, 0x03], "item_a", MONO_FRAME_BYTES);
        state.clear_pending_state("response_cancelled");

        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn incomplete_stereo_frame_is_carried_without_data_loss() {
        let mut state = RemainderState::new();

        let first =
            state.assemble(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06], "item_a", STEREO_FRAME_BYTES);
        assert_eq!(first.as_ref(), &[0x0201, 0x0403]);
        assert_eq!(state.pending_bytes, [0x05, 0x06]);

        let second = state.assemble(&[0x07, 0x08], "item_a", STEREO_FRAME_BYTES);
        assert_eq!(second.as_ref(), &[0x0605, 0x0807]);
        assert!(state.pending_bytes.is_empty());
        assert_eq!(state.item_id, None);
    }

    #[test]
    fn partial_stereo_frame_waits_until_complete() {
        let mut state = RemainderState::new();

        let first = state.assemble(&[0x01, 0x02, 0x03], "item_a", STEREO_FRAME_BYTES);
        assert!(first.is_empty());
        assert_eq!(state.pending_bytes, [0x01, 0x02, 0x03]);

        let second = state.assemble(&[0x04], "item_a", STEREO_FRAME_BYTES);
        assert_eq!(second.as_ref(), &[0x0201, 0x0403]);
        assert!(state.pending_bytes.is_empty());
    }

    #[test]
    fn arbitrary_chunk_boundaries_preserve_multichannel_stream() {
        let audio: Vec<u8> = (0..48).collect();
        let expected = decode_pcm16(&audio).into_owned();

        for num_channels in 1..=4 {
            let frame_bytes = channel_frame_bytes(num_channels).unwrap();
            for chunk_size in 1..=audio.len() {
                let mut state = RemainderState::new();
                let mut actual = Vec::new();

                for chunk in audio.chunks(chunk_size) {
                    actual.extend_from_slice(state.assemble(chunk, "item_a", frame_bytes).as_ref());
                }

                assert_eq!(
                    actual, expected,
                    "num_channels={num_channels}, chunk_size={chunk_size}"
                );
                assert!(
                    state.pending_bytes.is_empty(),
                    "num_channels={num_channels}, chunk_size={chunk_size}"
                );
            }
        }
    }

    #[test]
    fn zero_channels_has_no_frame_size() {
        assert_eq!(channel_frame_bytes(0), None);
    }
}
