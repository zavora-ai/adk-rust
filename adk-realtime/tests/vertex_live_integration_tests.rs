//! Integration tests for Vertex AI Live backend.
//!
//! These tests require real Google Cloud credentials and a project with
//! Vertex AI API enabled. They are marked `#[ignore]` and must be run manually.
//!
//! # Required Environment Variables
//!
//! - `GOOGLE_CLOUD_PROJECT` — GCP project ID with Vertex AI API enabled
//! - `GOOGLE_CLOUD_REGION` — GCP region (defaults to `us-central1` if not set)
//! - Application Default Credentials must be configured:
//!   `gcloud auth application-default login`
//!
//! # Running
//!
//! ```bash
//! cargo test -p adk-realtime --features vertex-live \
//!     --test vertex_live_integration_tests -- --ignored
//! ```

#![cfg(feature = "vertex-live")]

use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};

/// Integration test: connect to Vertex AI Live, send text, and verify a
/// `ServerEvent` response is received.
///
/// Validates: Requirements 16.1, 16.4, 16.5
/// Design: D7.1
///
/// Required env vars:
///   - `GOOGLE_CLOUD_PROJECT` — GCP project ID
///   - `GOOGLE_CLOUD_REGION` — GCP region (default: us-central1)
///   - ADC must be configured (`gcloud auth application-default login`)
#[tokio::test]
#[ignore]
async fn test_vertex_live_text_exchange() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        // Read required environment variables
        let project_id = std::env::var("GOOGLE_CLOUD_PROJECT")
            .expect("GOOGLE_CLOUD_PROJECT env var is required");
        let region =
            std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".to_string());

        // Obtain ADC credentials via Builder (synchronous build, no .await)
        let credentials = google_cloud_auth::credentials::Builder::default()
            .build()
            .expect("Failed to obtain Application Default Credentials");

        let backend = GeminiLiveBackend::Vertex { credentials, region, project_id };

        let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");

        let config = RealtimeConfig::default()
            .with_instruction("You are a helpful assistant. Respond briefly.");

        // Connect to Vertex AI Live
        let session = model.connect(config).await.expect("Failed to connect to Vertex AI Live");

        assert!(session.is_connected(), "Session should be connected after successful connect");

        // Send a text message
        session.send_text("Hello, say one word.").await.expect("Failed to send text");

        // Verify we receive at least one ServerEvent response
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
            // Stop after receiving ResponseDone
            if matches!(event, ServerEvent::ResponseDone { .. }) {
                break;
            }
        }

        assert!(
            received_event,
            "Should have received at least one response event from Vertex AI Live"
        );

        session.close().await.expect("Failed to close session");
    })
    .await
    .expect("Test timed out after 30s");
}

/// Integration test: verify Vertex AI Live session reports correct session ID.
///
/// Validates: Requirements 16.1, 16.5
#[tokio::test]
#[ignore]
async fn test_vertex_live_session_id() {
    let timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(timeout, async {
        let project_id = std::env::var("GOOGLE_CLOUD_PROJECT")
            .expect("GOOGLE_CLOUD_PROJECT env var is required");
        let region =
            std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".to_string());

        let credentials = google_cloud_auth::credentials::Builder::default()
            .build()
            .expect("Failed to obtain Application Default Credentials");

        let backend = GeminiLiveBackend::Vertex { credentials, region, project_id };

        let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");

        let config = RealtimeConfig::default();

        let session = model.connect(config).await.expect("Failed to connect to Vertex AI Live");

        // Session ID should be non-empty after connection
        assert!(
            !session.session_id().is_empty(),
            "Session ID should be non-empty after connecting"
        );

        session.close().await.expect("Failed to close session");
    })
    .await
    .expect("Test timed out after 30s");
}
