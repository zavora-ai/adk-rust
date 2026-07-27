//! Unit tests for `IntegratedRealtimeRunnerBuilder` validation.
//!
//! Tests that:
//! - Missing model returns a config error
//! - Missing identity returns a config error
//! - Minimal valid build (model + identity) succeeds
//! - Full build with all services configured succeeds
//!
//! **Validates: Requirement 6.7**

#![cfg(feature = "integration")]

use std::sync::Arc;

use adk_realtime::audio::AudioFormat;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::error::Result;
use adk_realtime::integration::{IntegratedRealtimeRunnerBuilder, IntegrationConfig};
use adk_realtime::model::{BoxedModel, RealtimeModel};
use adk_realtime::session::BoxedSession;
use async_trait::async_trait;

// ─── Mock RealtimeModel ──────────────────────────────────────────────────────

/// A no-op model for testing builder validation.
/// `connect()` is never actually called in these tests — we only exercise `build()`.
struct MockRealtimeModel;

#[async_trait]
impl RealtimeModel for MockRealtimeModel {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model_id(&self) -> &str {
        "mock-model-v1"
    }

    fn supports_realtime(&self) -> bool {
        true
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }

    fn available_voices(&self) -> Vec<&str> {
        vec!["default"]
    }

    async fn connect(&self, _config: RealtimeConfig) -> Result<BoxedSession> {
        // Should never be called during builder validation tests.
        unimplemented!("MockRealtimeModel::connect should not be called in builder tests")
    }
}

fn mock_model() -> BoxedModel {
    Arc::new(MockRealtimeModel) as BoxedModel
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_missing_model_returns_config_error() {
    let result = IntegratedRealtimeRunnerBuilder::new().identity("app", "user", "sess").build();

    assert!(result.is_err());
    let err = result.err().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("Model is required"), "expected 'Model is required' in error, got: {msg}");
}

#[test]
fn test_missing_identity_returns_config_error() {
    let result = IntegratedRealtimeRunnerBuilder::new().model(mock_model()).build();

    assert!(result.is_err());
    let err = result.err().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("Identity"), "expected 'Identity' in error, got: {msg}");
}

#[test]
fn test_successful_build_with_minimal_config() {
    let result = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("my-app", "user-1", "session-abc")
        .build();

    assert!(
        result.is_ok(),
        "minimal build (model + identity) should succeed, got: {:?}",
        result.err()
    );
}

#[test]
fn test_successful_build_with_all_services_configured() {
    use adk_memory::InMemoryMemoryService;
    use adk_plugin::EnhancedPluginManager;
    use adk_session::InMemorySessionService;

    let session_service = Arc::new(InMemorySessionService::new());
    let memory_service = Arc::new(InMemoryMemoryService::new());
    let plugin_manager = Arc::new(EnhancedPluginManager::new(vec![]));

    let result = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("my-app", "user-1", "session-abc")
        .session_service(session_service)
        .memory_service(memory_service)
        .plugin_manager(plugin_manager)
        .integration_config(IntegrationConfig {
            persist_transcripts: true,
            store_to_memory: true,
            inject_memory_context: false,
            max_memory_injection: 5,
            max_history_injection: 20,
        })
        .build();

    assert!(result.is_ok(), "full build with all services should succeed, got: {:?}", result.err());
}
