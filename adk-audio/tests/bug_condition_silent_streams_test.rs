//! Bug Condition Exploration Test — Silent Empty Streams
//!
//! **Property 1 (Bug 2): Unimplemented streams return explicit errors**
//!
//! For each provider in `{"assemblyai", "deepgram", "MLX"}`, calling
//! `transcribe_stream()` SHALL return `Err(AudioError::Stt { .. })` with a
//! message containing "not yet implemented".
//!
//! On unfixed code, all three return `Ok(empty_stream)`, so the assertion
//! fails. On fixed code, all three return the expected error.
//!
//! **Validates: Requirements 1.3, 1.4, 1.5, 1.6**

use adk_audio::error::AudioError;
use adk_audio::traits::{SttOptions, SttProvider};

fn init_crypto() {
    adk_core::ensure_crypto_provider();
}

// ---------------------------------------------------------------------------
// AssemblyAI
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bug_condition_assemblyai_transcribe_stream_returns_error() {
    init_crypto();
    let provider = adk_audio::AssemblyAiStt::with_api_key("test-key".to_string());

    let result = provider
        .transcribe_stream(Box::pin(futures::stream::empty()), &SttOptions::default())
        .await;

    match result {
        Err(AudioError::Stt { provider, message }) => {
            assert_eq!(provider, "assemblyai");
            assert!(
                message.contains("not yet implemented"),
                "expected 'not yet implemented' in message, got: {message}"
            );
        }
        Err(other) => panic!("expected AudioError::Stt, got: {other}"),
        Ok(_) => panic!(
            "BUG CONFIRMED: transcribe_stream() returned Ok(empty_stream) instead of Err. \
             This is the silent stream bug."
        ),
    }
}

// ---------------------------------------------------------------------------
// Deepgram — streaming is now implemented, so it attempts a real WebSocket
// connection. With a fake API key it returns a connection/auth error, which
// is correct behavior (not the silent-stream bug).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bug_condition_deepgram_transcribe_stream_returns_error() {
    init_crypto();
    let provider = adk_audio::DeepgramStt::with_api_key("test-key".to_string());

    let result = provider
        .transcribe_stream(Box::pin(futures::stream::empty()), &SttOptions::default())
        .await;

    match result {
        Err(AudioError::Stt { provider, message }) => {
            assert_eq!(provider, "deepgram");
            // With a fake key, Deepgram returns a WebSocket connection error (401).
            assert!(
                message.contains("WebSocket connection failed"),
                "expected WebSocket connection error, got: {message}"
            );
        }
        Err(other) => panic!("expected AudioError::Stt, got: {other}"),
        Ok(_) => {
            // Streaming is implemented — getting Ok means the connection succeeded,
            // which shouldn't happen with a fake key.
            panic!("unexpected Ok with fake API key");
        }
    }
}

// ---------------------------------------------------------------------------
// MLX
// ---------------------------------------------------------------------------

#[cfg(feature = "mlx")]
#[tokio::test]
async fn bug_condition_mlx_transcribe_stream_returns_error() {
    let provider = adk_audio::MlxSttProvider::with_dummy();

    let result = provider
        .transcribe_stream(Box::pin(futures::stream::empty()), &SttOptions::default())
        .await;

    match result {
        Err(AudioError::Stt { provider, message }) => {
            assert_eq!(provider, "MLX");
            assert!(
                message.contains("not yet implemented"),
                "expected 'not yet implemented' in message, got: {message}"
            );
        }
        Err(other) => panic!("expected AudioError::Stt, got: {other}"),
        Ok(_) => panic!(
            "BUG CONFIRMED: transcribe_stream() returned Ok(empty_stream) instead of Err. \
             This is the silent stream bug."
        ),
    }
}
