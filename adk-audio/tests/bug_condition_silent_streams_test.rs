//! Bug Condition Exploration Test — Silent Empty Streams
//!
//! **Property 1 (Bug 2): Unimplemented streams return explicit errors**
//!
//! A provider without streaming support SHALL return
//! `Err(AudioError::Stt { .. })` from `transcribe_stream()` with a message that
//! says streaming is not yet implemented and names `transcribe` as the
//! alternative. `Ok(empty_stream)` reads as "no speech" and hides the gap.
//!
//! Every `SttProvider` in the crate is accounted for:
//!
//! | Provider | `transcribe_stream` | Covered by |
//! |----------|---------------------|------------|
//! | `AssemblyAiStt` | error | this file |
//! | `GeminiStt` | error | this file |
//! | `WhisperApiStt` | error | this file |
//! | `MlxSttProvider` (`mlx`) | error | this file |
//! | `DeepgramStt` | WebSocket stream | `src/providers/stt/deepgram.rs` tests, against a scripted local server |
//! | `OnnxSttProvider` (`onnx`) | one transcript per 30-second segment | needs downloaded model files; it transcribes the audio it receives |
//!
//! **Validates: Requirements 1.3, 1.4, 1.5, 1.6**

use adk_audio::error::AudioError;
use adk_audio::frame::AudioFrame;
use adk_audio::traits::{SttOptions, SttProvider};

const UNSUPPORTED: &str =
    "streaming transcription not yet implemented; call `transcribe` with the buffered audio";

/// Asserts `transcribe_stream` fails with `AudioError::Stt` for `provider` and `message`,
/// for audio that is not empty.
async fn assert_streaming_unsupported(stt: &dyn SttProvider, provider: &str, message: &str) {
    let audio = futures::stream::iter([AudioFrame::new(vec![1_u8; 320], 16_000, 1)]);
    match stt.transcribe_stream(Box::pin(audio), &SttOptions::default()).await {
        Err(AudioError::Stt { provider: actual_provider, message: actual_message }) => {
            assert_eq!((actual_provider.as_str(), actual_message.as_str()), (provider, message));
        }
        Err(other) => panic!("expected AudioError::Stt, got: {other}"),
        Ok(_) => panic!(
            "BUG CONFIRMED: {provider} transcribe_stream() returned Ok(stream) instead of Err. \
             This is the silent stream bug."
        ),
    }
}

#[tokio::test]
async fn bug_condition_assemblyai_transcribe_stream_returns_error() {
    adk_core::ensure_crypto_provider();
    let stt = adk_audio::AssemblyAiStt::with_api_key("test-key".to_string());
    assert_streaming_unsupported(&stt, "assemblyai", UNSUPPORTED).await;
}

#[tokio::test]
async fn bug_condition_gemini_transcribe_stream_returns_error() {
    adk_core::ensure_crypto_provider();
    let stt = adk_audio::GeminiStt::new("test-key");
    assert_streaming_unsupported(
        &stt,
        "gemini",
        &format!("{UNSUPPORTED}, or use the Gemini Live API through `adk-realtime` for live audio"),
    )
    .await;
}

#[tokio::test]
async fn bug_condition_whisper_api_transcribe_stream_returns_error() {
    adk_core::ensure_crypto_provider();
    let stt = adk_audio::WhisperApiStt::with_api_key("test-key".to_string());
    assert_streaming_unsupported(&stt, "whisper", UNSUPPORTED).await;
}

#[cfg(feature = "mlx")]
#[tokio::test]
async fn bug_condition_mlx_transcribe_stream_returns_error() {
    let stt = adk_audio::MlxSttProvider::with_dummy();
    assert_streaming_unsupported(&stt, "MLX", UNSUPPORTED).await;
}

/// A provider that implements only batch transcription.
struct BatchOnly;

#[async_trait::async_trait]
impl SttProvider for BatchOnly {
    async fn transcribe(
        &self,
        _audio: &AudioFrame,
        _opts: &SttOptions,
    ) -> adk_audio::AudioResult<adk_audio::Transcript> {
        Ok(adk_audio::Transcript::default())
    }
}

#[tokio::test]
async fn a_provider_without_streaming_fails_by_default() {
    assert_streaming_unsupported(
        &BatchOnly,
        "stt",
        "this provider does not support streaming transcription; use transcribe",
    )
    .await;
}
