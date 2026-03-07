//! Integration tests for LiveKit WebRTC bridge.
//!
//! These tests require a running LiveKit server and valid credentials.
//! They are marked `#[ignore]` and must be run manually.
//!
//! # Required Environment Variables
//!
//! - `LIVEKIT_URL` — LiveKit server WebSocket URL (e.g., `ws://localhost:7880`)
//! - `LIVEKIT_API_KEY` — LiveKit API key
//! - `LIVEKIT_API_SECRET` — LiveKit API secret
//!
//! # Running
//!
//! ```bash
//! cargo test -p adk-realtime --features livekit \
//!     --test livekit_openai_integration_tests -- --ignored
//! ```

#![cfg(feature = "livekit")]

use std::sync::Arc;

use adk_realtime::error::RealtimeError;
use adk_realtime::livekit::LiveKitEventHandler;
use adk_realtime::runner::EventHandler;
use async_trait::async_trait;
use tokio::sync::Mutex;

/// A recording event handler that captures all forwarded events for verification.
struct RecordingEventHandler {
    audio_calls: Arc<Mutex<Vec<Vec<u8>>>>,
    text_calls: Arc<Mutex<Vec<(String, String)>>>,
    transcript_calls: Arc<Mutex<Vec<(String, String)>>>,
    speech_started_calls: Arc<Mutex<Vec<u64>>>,
    speech_stopped_calls: Arc<Mutex<Vec<u64>>>,
    response_done_count: Arc<Mutex<u32>>,
    error_calls: Arc<Mutex<Vec<String>>>,
}

impl RecordingEventHandler {
    fn new() -> Self {
        Self {
            audio_calls: Arc::new(Mutex::new(Vec::new())),
            text_calls: Arc::new(Mutex::new(Vec::new())),
            transcript_calls: Arc::new(Mutex::new(Vec::new())),
            speech_started_calls: Arc::new(Mutex::new(Vec::new())),
            speech_stopped_calls: Arc::new(Mutex::new(Vec::new())),
            response_done_count: Arc::new(Mutex::new(0)),
            error_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl EventHandler for RecordingEventHandler {
    async fn on_audio(&self, audio: &[u8], _item_id: &str) -> adk_realtime::Result<()> {
        self.audio_calls.lock().await.push(audio.to_vec());
        Ok(())
    }

    async fn on_text(&self, text: &str, item_id: &str) -> adk_realtime::Result<()> {
        self.text_calls.lock().await.push((text.to_string(), item_id.to_string()));
        Ok(())
    }

    async fn on_transcript(&self, transcript: &str, item_id: &str) -> adk_realtime::Result<()> {
        self.transcript_calls.lock().await.push((transcript.to_string(), item_id.to_string()));
        Ok(())
    }

    async fn on_speech_started(&self, audio_start_ms: u64) -> adk_realtime::Result<()> {
        self.speech_started_calls.lock().await.push(audio_start_ms);
        Ok(())
    }

    async fn on_speech_stopped(&self, audio_end_ms: u64) -> adk_realtime::Result<()> {
        self.speech_stopped_calls.lock().await.push(audio_end_ms);
        Ok(())
    }

    async fn on_response_done(&self) -> adk_realtime::Result<()> {
        *self.response_done_count.lock().await += 1;
        Ok(())
    }

    async fn on_error(&self, error: &RealtimeError) -> adk_realtime::Result<()> {
        self.error_calls.lock().await.push(error.to_string());
        Ok(())
    }
}

/// Integration test: create a `LiveKitEventHandler`, simulate audio events,
/// and verify the inner handler receives forwarded calls.
///
/// This test does NOT require a LiveKit server — it verifies the event handler
/// delegation logic using a `NativeAudioSource` from the LiveKit SDK.
///
/// Validates: Requirements 16.2, 16.4, 16.5
/// Design: D7.1
///
/// Required env vars:
///   - `LIVEKIT_URL` — LiveKit server URL (for full bridge tests)
///   - `LIVEKIT_API_KEY` — LiveKit API key
///   - `LIVEKIT_API_SECRET` — LiveKit API secret
#[tokio::test]
#[ignore]
async fn test_livekit_event_handler_audio_forwarding() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        // Create a NativeAudioSource for the handler
        let audio_source = livekit::webrtc::audio_source::native::NativeAudioSource::new(
            livekit::webrtc::audio_source::AudioSourceOptions::default(),
            24000,
            1,
            100, // queue_size_ms
        );

        let inner = RecordingEventHandler::new();
        let audio_calls = Arc::clone(&inner.audio_calls);
        let text_calls = Arc::clone(&inner.text_calls);

        let handler = LiveKitEventHandler::new(inner, audio_source, 24000, 1);

        // Simulate audio event — PCM16 samples (must be even number of bytes)
        let pcm_audio: Vec<u8> = vec![0u8; 960]; // 480 samples of silence
        handler.on_audio(&pcm_audio, "item_1").await.expect("on_audio should succeed");

        // Verify inner handler received the audio
        let recorded_audio = audio_calls.lock().await;
        assert_eq!(
            recorded_audio.len(),
            1,
            "Inner handler should have received exactly one audio call"
        );
        assert_eq!(
            recorded_audio[0], pcm_audio,
            "Inner handler should receive the same audio bytes"
        );
        drop(recorded_audio);

        // Simulate text event — should be delegated to inner handler
        handler.on_text("Hello from model", "item_2").await.expect("on_text should succeed");

        let recorded_text = text_calls.lock().await;
        assert_eq!(
            recorded_text.len(),
            1,
            "Inner handler should have received exactly one text call"
        );
        assert_eq!(recorded_text[0].0, "Hello from model");
        assert_eq!(recorded_text[0].1, "item_2");
    })
    .await
    .expect("Test timed out after 30s");
}

/// Integration test: verify all non-audio events are delegated correctly.
///
/// Validates: Requirements 16.2, 16.4, 16.5
#[tokio::test]
#[ignore]
async fn test_livekit_event_handler_non_audio_delegation() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        let audio_source = livekit::webrtc::audio_source::native::NativeAudioSource::new(
            livekit::webrtc::audio_source::AudioSourceOptions::default(),
            24000,
            1,
            100, // queue_size_ms
        );

        let inner = RecordingEventHandler::new();
        let transcript_calls = Arc::clone(&inner.transcript_calls);
        let speech_started_calls = Arc::clone(&inner.speech_started_calls);
        let speech_stopped_calls = Arc::clone(&inner.speech_stopped_calls);
        let response_done_count = Arc::clone(&inner.response_done_count);
        let error_calls = Arc::clone(&inner.error_calls);

        let handler = LiveKitEventHandler::new(inner, audio_source, 24000, 1);

        // Test transcript delegation
        handler
            .on_transcript("test transcript", "item_t")
            .await
            .expect("on_transcript should succeed");
        assert_eq!(transcript_calls.lock().await.len(), 1);

        // Test speech_started delegation
        handler.on_speech_started(1000).await.expect("on_speech_started should succeed");
        assert_eq!(speech_started_calls.lock().await[0], 1000);

        // Test speech_stopped delegation
        handler.on_speech_stopped(2000).await.expect("on_speech_stopped should succeed");
        assert_eq!(speech_stopped_calls.lock().await[0], 2000);

        // Test response_done delegation
        handler.on_response_done().await.expect("on_response_done should succeed");
        assert_eq!(*response_done_count.lock().await, 1);

        // Test error delegation
        let error = RealtimeError::connection("test error");
        handler.on_error(&error).await.expect("on_error should succeed");
        assert_eq!(error_calls.lock().await.len(), 1);
    })
    .await
    .expect("Test timed out after 30s");
}
