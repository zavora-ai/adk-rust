//! Property-based tests for LiveKit event handler delegation.
//!
//! **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
//! *For any* event data (text string, transcript string, timestamp, or error), calling a
//! non-audio method (`on_text`, `on_transcript`, `on_speech_started`, `on_speech_stopped`,
//! `on_response_done`, `on_error`) on a `LiveKitEventHandler` wrapping a recording
//! `EventHandler` SHALL produce the same recorded call on the inner handler as calling
//! the inner handler directly.
//! **Validates: Requirements 4.1**

#![cfg(feature = "livekit")]

use std::sync::Arc;

use adk_realtime::error::RealtimeError;
use adk_realtime::livekit::LiveKitEventHandler;
use adk_realtime::runner::EventHandler;
use async_trait::async_trait;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use proptest::prelude::*;
use tokio::sync::Mutex;

/// Represents a recorded call to the inner EventHandler.
#[derive(Debug, Clone, PartialEq)]
enum RecordedCall {
    OnText { text: String, item_id: String },
    OnTranscript { transcript: String, item_id: String },
    OnSpeechStarted { audio_start_ms: u64 },
    OnSpeechStopped { audio_end_ms: u64 },
    OnResponseDone,
    OnError { message: String },
}

/// A recording EventHandler that stores all calls for later inspection.
#[derive(Debug, Clone)]
struct RecordingHandler {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl RecordingHandler {
    fn new() -> Self {
        Self { calls: Arc::new(Mutex::new(Vec::new())) }
    }

    async fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().await.clone()
    }
}

#[async_trait]
impl EventHandler for RecordingHandler {
    async fn on_audio(&self, _audio: &[u8], _item_id: &str) -> adk_realtime::Result<()> {
        // Not recorded here — we only test non-audio delegation
        Ok(())
    }

    async fn on_text(&self, text: &str, item_id: &str) -> adk_realtime::Result<()> {
        self.calls
            .lock()
            .await
            .push(RecordedCall::OnText { text: text.to_string(), item_id: item_id.to_string() });
        Ok(())
    }

    async fn on_transcript(&self, transcript: &str, item_id: &str) -> adk_realtime::Result<()> {
        self.calls.lock().await.push(RecordedCall::OnTranscript {
            transcript: transcript.to_string(),
            item_id: item_id.to_string(),
        });
        Ok(())
    }

    async fn on_speech_started(&self, audio_start_ms: u64) -> adk_realtime::Result<()> {
        self.calls.lock().await.push(RecordedCall::OnSpeechStarted { audio_start_ms });
        Ok(())
    }

    async fn on_speech_stopped(&self, audio_end_ms: u64) -> adk_realtime::Result<()> {
        self.calls.lock().await.push(RecordedCall::OnSpeechStopped { audio_end_ms });
        Ok(())
    }

    async fn on_response_done(&self) -> adk_realtime::Result<()> {
        self.calls.lock().await.push(RecordedCall::OnResponseDone);
        Ok(())
    }

    async fn on_error(&self, error: &RealtimeError) -> adk_realtime::Result<()> {
        self.calls.lock().await.push(RecordedCall::OnError { message: error.to_string() });
        Ok(())
    }
}

/// Creates a `LiveKitEventHandler` wrapping a `RecordingHandler` for testing.
fn create_test_handler() -> (LiveKitEventHandler<RecordingHandler>, RecordingHandler) {
    let inner = RecordingHandler::new();
    let inner_clone = inner.clone();
    let audio_source = NativeAudioSource::new(
        AudioSourceOptions::default(),
        24000, // sample rate
        1,     // mono
        100,   // queue_size_ms
    );
    let handler = LiveKitEventHandler::new(inner, audio_source, 24000, 1);
    (handler, inner_clone)
}

/// Creates a standalone `RecordingHandler` for direct-call comparison.
fn create_direct_handler() -> RecordingHandler {
    RecordingHandler::new()
}

/// Generator for arbitrary non-empty strings (event text/transcripts).
fn arb_non_empty_string() -> impl Strategy<Value = String> {
    ".{1,200}".prop_filter("must be non-empty", |s| !s.is_empty())
}

/// Generator for arbitrary timestamps (u64).
fn arb_timestamp() -> impl Strategy<Value = u64> {
    0u64..=u64::MAX
}

/// Helper to run an async block in a tokio runtime for proptest.
fn run_async<F: std::future::Future<Output = ()>>(f: F) {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* text string and item_id, calling `on_text` on a `LiveKitEventHandler`
    /// SHALL produce the same recorded call on the inner handler as calling the inner
    /// handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_text_delegation(
        text in arb_non_empty_string(),
        item_id in arb_non_empty_string()
    ) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            // Call via LiveKitEventHandler
            lk_handler.on_text(&text, &item_id).await.unwrap();
            // Call directly on inner handler
            direct.on_text(&text, &item_id).await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_text delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* transcript string and item_id, calling `on_transcript` on a
    /// `LiveKitEventHandler` SHALL produce the same recorded call on the inner handler
    /// as calling the inner handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_transcript_delegation(
        transcript in arb_non_empty_string(),
        item_id in arb_non_empty_string()
    ) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            lk_handler.on_transcript(&transcript, &item_id).await.unwrap();
            direct.on_transcript(&transcript, &item_id).await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_transcript delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* timestamp, calling `on_speech_started` on a `LiveKitEventHandler`
    /// SHALL produce the same recorded call on the inner handler as calling the inner
    /// handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_speech_started_delegation(ts in arb_timestamp()) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            lk_handler.on_speech_started(ts).await.unwrap();
            direct.on_speech_started(ts).await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_speech_started delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* timestamp, calling `on_speech_stopped` on a `LiveKitEventHandler`
    /// SHALL produce the same recorded call on the inner handler as calling the inner
    /// handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_speech_stopped_delegation(ts in arb_timestamp()) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            lk_handler.on_speech_stopped(ts).await.unwrap();
            direct.on_speech_stopped(ts).await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_speech_stopped delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* invocation, calling `on_response_done` on a `LiveKitEventHandler`
    /// SHALL produce the same recorded call on the inner handler as calling the inner
    /// handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_response_done_delegation(_ in 0..100u32) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            lk_handler.on_response_done().await.unwrap();
            direct.on_response_done().await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_response_done delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }

    /// **Feature: realtime-audio-transport, Property 2: LiveKit Event Handler Delegation**
    /// *For any* error context string, calling `on_error` on a `LiveKitEventHandler`
    /// SHALL produce the same recorded call on the inner handler as calling the inner
    /// handler directly.
    /// **Validates: Requirements 4.1**
    #[test]
    fn prop_on_error_delegation(ctx in arb_non_empty_string()) {
        run_async(async {
            let (lk_handler, lk_inner) = create_test_handler();
            let direct = create_direct_handler();

            let error = RealtimeError::livekit(&ctx);
            lk_handler.on_error(&error).await.unwrap();
            direct.on_error(&error).await.unwrap();

            let lk_calls = lk_inner.recorded_calls().await;
            let direct_calls = direct.recorded_calls().await;

            assert_eq!(
                lk_calls, direct_calls,
                "on_error delegation mismatch: lk={:?}, direct={:?}",
                lk_calls, direct_calls
            );
        });
    }
}
