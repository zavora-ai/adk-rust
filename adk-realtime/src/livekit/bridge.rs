//! Bridge functions for connecting LiveKit audio tracks to a [`RealtimeRunner`].

use futures::StreamExt;
use livekit::track::RemoteAudioTrack;
use livekit::webrtc::audio_stream::native::NativeAudioStream;

use crate::audio::{AudioChunk, AudioFormat};
use crate::error::Result;
use crate::runner::RealtimeRunner;

/// Default sample rate for OpenAI-compatible audio (24kHz).
const DEFAULT_SAMPLE_RATE: i32 = 24000;
/// Gemini-expected sample rate (16kHz).
const GEMINI_SAMPLE_RATE: i32 = 16000;
/// Default number of audio channels (mono).
const DEFAULT_NUM_CHANNELS: i32 = 1;

/// Reads audio frames from a LiveKit [`RemoteAudioTrack`] and sends them as
/// base64-encoded PCM16 audio (24kHz) to the given [`RealtimeRunner`].
///
/// This function runs continuously until the remote track stream ends, at which
/// point it returns `Ok(())`. If sending audio to the runner fails, the error
/// is propagated to the caller.
///
/// # Arguments
///
/// * `track` — The LiveKit remote audio track to read from.
/// * `runner` — The realtime runner to send audio to.
pub async fn bridge_input(track: RemoteAudioTrack, runner: &RealtimeRunner) -> Result<()> {
    let mut stream =
        NativeAudioStream::new(track.rtc_track(), DEFAULT_SAMPLE_RATE, DEFAULT_NUM_CHANNELS);

    while let Some(frame) = stream.next().await {
        // Convert i16 samples to little-endian PCM16 bytes
        let chunk = AudioChunk::from_i16_samples(&frame.data, AudioFormat::pcm16_24khz());
        runner.send_audio(&chunk.to_base64()).await?;
    }

    Ok(())
}

/// Reads audio frames from a LiveKit [`RemoteAudioTrack`], resamples to 16kHz
/// mono PCM16 (Gemini's expected format), and sends them to the given
/// [`RealtimeRunner`].
///
/// This is the Gemini-specific variant of [`bridge_input`]. Use this when the
/// realtime session is connected to a Gemini model that expects 16kHz input.
///
/// # Arguments
///
/// * `track` — The LiveKit remote audio track to read from.
/// * `runner` — The realtime runner to send audio to.
pub async fn bridge_gemini_input(track: RemoteAudioTrack, runner: &RealtimeRunner) -> Result<()> {
    // Request 16kHz mono from LiveKit — it handles resampling for us.
    let mut stream =
        NativeAudioStream::new(track.rtc_track(), GEMINI_SAMPLE_RATE, DEFAULT_NUM_CHANNELS);

    while let Some(frame) = stream.next().await {
        let chunk = AudioChunk::from_i16_samples(&frame.data, AudioFormat::pcm16_16khz());
        runner.send_audio(&chunk.to_base64()).await?;
    }

    Ok(())
}
