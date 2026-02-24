//! Text-to-speech provider trait and request types.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::codec::AudioFormat;
use crate::error::AudioResult;
use crate::frame::AudioFrame;

/// Emotion hint for TTS synthesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emotion {
    /// Neutral tone.
    Neutral,
    /// Happy / upbeat tone.
    Happy,
    /// Sad / somber tone.
    Sad,
    /// Angry / forceful tone.
    Angry,
    /// Whispered / quiet tone.
    Whisper,
    /// Excited / energetic tone.
    Excited,
    /// Calm / soothing tone.
    Calm,
}

/// Descriptor for an available voice.
#[derive(Debug, Clone)]
pub struct Voice {
    /// Provider-specific voice identifier.
    pub id: String,
    /// Human-readable voice name.
    pub name: String,
    /// BCP-47 language code.
    pub language: String,
    /// Optional gender label.
    pub gender: Option<String>,
}

/// Request parameters for TTS synthesis.
#[derive(Debug, Clone)]
pub struct TtsRequest {
    /// Text to synthesize.
    pub text: String,
    /// Voice identifier.
    pub voice: String,
    /// Optional BCP-47 language code.
    pub language: Option<String>,
    /// Speaking speed multiplier (0.5–2.0, default 1.0).
    pub speed: f32,
    /// Optional pitch adjustment.
    pub pitch: Option<f32>,
    /// Optional emotion hint.
    pub emotion: Option<Emotion>,
    /// Desired output format (internal use; providers output PCM16).
    pub output_format: AudioFormat,
}

impl Default for TtsRequest {
    fn default() -> Self {
        Self {
            text: String::new(),
            voice: String::new(),
            language: None,
            speed: 1.0,
            pitch: None,
            emotion: None,
            output_format: AudioFormat::Pcm16,
        }
    }
}

/// Unified trait for text-to-speech providers.
///
/// Implementors include cloud services (ElevenLabs, OpenAI, Gemini, Cartesia)
/// and local models (MLX Kokoro, ONNX Kokoro).
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize text to a single audio frame (batch mode).
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame>;

    /// Synthesize text as a stream of audio frames (streaming mode).
    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>>;

    /// List available voices for this provider.
    fn voice_catalog(&self) -> &[Voice];
}
