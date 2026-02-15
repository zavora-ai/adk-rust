//! Integration tests for OpenAI WebRTC transport.
//!
//! These tests require a valid OpenAI API key with realtime access and
//! `cmake` installed (for the `audiopus` Opus codec build). They are marked
//! `#[ignore]` and must be run manually.
//!
//! # Required Environment Variables
//!
//! - `OPENAI_API_KEY` — OpenAI API key with realtime API access
//!
//! # System Requirements
//!
//! - `cmake` must be installed (`brew install cmake` / `apt install cmake`)
//!
//! # Running
//!
//! ```bash
//! cargo test -p adk-realtime --features openai-webrtc \
//!     --test openai_webrtc_integration_tests -- --ignored
//! ```

#![cfg(feature = "openai-webrtc")]

use adk_realtime::openai::{OpenAIRealtimeModel, OpenAITransport};
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};

/// Integration test: connect to OpenAI via WebRTC, send text, and verify
/// a response event is received on the data channel.
///
/// Validates: Requirements 16.3, 16.4, 16.5
/// Design: D7.2
///
/// Required env vars:
///   - `OPENAI_API_KEY` — OpenAI API key with realtime access
#[tokio::test]
#[ignore]
async fn test_openai_webrtc_text_exchange() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY env var is required");

        let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17")
            .with_transport(OpenAITransport::WebRTC);

        let config = RealtimeConfig::default()
            .with_instruction("You are a helpful assistant. Respond briefly.")
            .with_voice("alloy");

        // Connect via WebRTC (SDP signaling happens internally)
        let session = model.connect(config).await.expect("Failed to connect via OpenAI WebRTC");

        assert!(
            session.is_connected(),
            "Session should be connected after successful WebRTC handshake"
        );

        // Send a text message over the data channel
        session
            .send_text("Hello, say one word.")
            .await
            .expect("Failed to send text over data channel");

        // Verify we receive at least one response event
        let mut received_event = false;
        while let Some(event_result) = session.next_event().await {
            let event = event_result.expect("Received error event from server");
            match &event {
                ServerEvent::AudioDelta { .. }
                | ServerEvent::TextDelta { .. }
                | ServerEvent::TranscriptDelta { .. }
                | ServerEvent::ResponseDone { .. } => {
                    received_event = true;
                }
                _ => {}
            }
            if matches!(event, ServerEvent::ResponseDone { .. }) {
                break;
            }
        }

        assert!(received_event, "Should have received at least one response event via WebRTC");

        session.close().await.expect("Failed to close WebRTC session");
    })
    .await
    .expect("Test timed out after 30s");
}

/// Integration test: connect via WebRTC, send audio, and verify a response.
///
/// Validates: Requirements 16.3, 16.4, 16.5
/// Design: D7.2
///
/// Required env vars:
///   - `OPENAI_API_KEY` — OpenAI API key with realtime access
#[tokio::test]
#[ignore]
async fn test_openai_webrtc_audio_roundtrip() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY env var is required");

        let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17")
            .with_transport(OpenAITransport::WebRTC);

        let config = RealtimeConfig::default()
            .with_instruction("You are a helpful assistant.")
            .with_voice("alloy");

        let session = model.connect(config).await.expect("Failed to connect via OpenAI WebRTC");

        // Create a short PCM16 audio chunk (480 samples of silence at 24kHz = 20ms)
        let silence_pcm: Vec<i16> = vec![0i16; 480];
        let pcm_bytes: Vec<u8> = silence_pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let audio_chunk = adk_realtime::audio::AudioChunk::pcm16_24khz(pcm_bytes);

        // Send audio and commit to trigger processing
        session
            .send_audio(&audio_chunk)
            .await
            .expect("Failed to send audio over WebRTC media track");

        session.commit_audio().await.expect("Failed to commit audio buffer");

        // We may or may not get a meaningful response from silence,
        // but the session should remain connected and not error out.
        assert!(session.is_connected(), "Session should remain connected after sending audio");

        session.close().await.expect("Failed to close WebRTC session");
    })
    .await
    .expect("Test timed out after 30s");
}

/// Integration test: verify WebRTC session reports a valid session ID.
///
/// Validates: Requirements 16.3, 16.5
#[tokio::test]
#[ignore]
async fn test_openai_webrtc_session_id() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY env var is required");

        let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17")
            .with_transport(OpenAITransport::WebRTC);

        let config = RealtimeConfig::default();

        let session = model.connect(config).await.expect("Failed to connect via OpenAI WebRTC");

        assert!(
            !session.session_id().is_empty(),
            "Session ID should be non-empty after WebRTC connection"
        );

        session.close().await.expect("Failed to close WebRTC session");
    })
    .await
    .expect("Test timed out after 30s");
}
