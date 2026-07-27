//! Integration test: memory injection at session start.
//!
//! Verifies that `IntegratedRealtimeRunner::connect()` queries `MemoryService`
//! when `inject_memory_context` is enabled and skips the query when disabled.
//!
//! **Validates: Requirement 3.3**

#![cfg(feature = "integration")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_core::{Content, Result as AdkResult};
use adk_memory::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
use adk_realtime::audio::AudioFormat;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::error::Result;
use adk_realtime::integration::{IntegratedRealtimeRunnerBuilder, IntegrationConfig};
use adk_realtime::model::{BoxedModel, RealtimeModel};
use adk_realtime::session::BoxedSession;
use async_trait::async_trait;
use chrono::Utc;

// ─── Mock RealtimeModel ──────────────────────────────────────────────────────

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
        // Simulate connection failure — no real WebSocket endpoint.
        Err(adk_realtime::error::RealtimeError::connection(
            "mock transport: no connection available",
        ))
    }
}

fn mock_model() -> BoxedModel {
    Arc::new(MockRealtimeModel) as BoxedModel
}

// ─── Tracking MemoryService ──────────────────────────────────────────────────

/// A `MemoryService` implementation that tracks whether `search` was called
/// and returns pre-populated memory entries.
struct TrackingMemoryService {
    search_called: AtomicBool,
    entries: Vec<MemoryEntry>,
}

impl TrackingMemoryService {
    fn new(entries: Vec<MemoryEntry>) -> Self {
        Self { search_called: AtomicBool::new(false), entries }
    }

    fn was_search_called(&self) -> bool {
        self.search_called.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl MemoryService for TrackingMemoryService {
    async fn add_session(
        &self,
        _app_name: &str,
        _user_id: &str,
        _session_id: &str,
        _entries: Vec<MemoryEntry>,
    ) -> AdkResult<()> {
        Ok(())
    }

    async fn search(&self, _req: SearchRequest) -> AdkResult<SearchResponse> {
        self.search_called.store(true, Ordering::SeqCst);
        Ok(SearchResponse { memories: self.entries.clone() })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_injection_enabled_queries_memory_on_connect() {
    // Pre-populate memory entries
    let entries = vec![
        MemoryEntry {
            content: Content::new("model").with_text("User prefers concise answers"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("model").with_text("User is working on a Rust project"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
    ];

    let memory_service = Arc::new(TrackingMemoryService::new(entries));

    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-1")
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: true,
            max_memory_injection: 10,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    // connect() will query memory BEFORE calling runner.connect(),
    // which will fail because our mock model returns a connection error.
    let result = runner.connect().await;

    // The connect call should fail (from the mock transport),
    // but memory should have been queried before that failure.
    assert!(result.is_err(), "connect should fail due to mock transport");
    assert!(
        memory_service.was_search_called(),
        "MemoryService::search should be called when inject_memory_context is enabled"
    );
}

#[tokio::test]
async fn test_memory_injection_disabled_does_not_query_memory_on_connect() {
    let entries = vec![MemoryEntry {
        content: Content::new("model").with_text("Some stored context"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];

    let memory_service = Arc::new(TrackingMemoryService::new(entries));

    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-2")
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: false,
            max_memory_injection: 10,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    let result = runner.connect().await;

    assert!(result.is_err(), "connect should fail due to mock transport");
    assert!(
        !memory_service.was_search_called(),
        "MemoryService::search should NOT be called when inject_memory_context is disabled"
    );
}

#[tokio::test]
async fn test_memory_injection_with_no_memory_service_configured() {
    // When no memory service is configured, connect should not panic.
    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-3")
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: true,
            max_memory_injection: 10,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    let result = runner.connect().await;

    // Should fail from the mock transport, not from missing memory service.
    assert!(result.is_err(), "connect should fail due to mock transport");
}

// ─── Injection, not just retrieval ───────────────────────────────────────────
//
// `connect` fetched the prior session into `_session` and dropped it, and logged "injecting
// memory entries into session context" next to a comment saying injection was a future
// enhancement. A resumed session therefore began with neither history nor memory while the
// logs said otherwise, and the tests above only proved the *query* happened.

#[tokio::test]
async fn recalled_memory_reaches_the_system_instruction() {
    let entries = vec![MemoryEntry {
        content: Content::new("model").with_text("User prefers concise answers"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];
    let memory_service = Arc::new(TrackingMemoryService::new(entries));

    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-1")
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: true,
            max_memory_injection: 10,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    // The transport fails, but injection happens before the connection attempt, so the
    // instruction the session *would* have been created with is observable.
    let _ = runner.connect().await;

    let instruction = runner.instruction().await.unwrap_or_default();
    assert!(
        instruction.contains("User prefers concise answers"),
        "recalled memory must reach the provider instruction, not just the logs: {instruction:?}"
    );
    assert!(
        instruction.contains("Relevant recalled context"),
        "the carried block must be labelled so the model can tell it from the task: {instruction:?}"
    );
}

#[tokio::test]
async fn nothing_is_injected_when_memory_injection_is_disabled() {
    let entries = vec![MemoryEntry {
        content: Content::new("model").with_text("User prefers concise answers"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];
    let memory_service = Arc::new(TrackingMemoryService::new(entries));

    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-1")
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: false,
            max_memory_injection: 10,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    let _ = runner.connect().await;

    let instruction = runner.instruction().await.unwrap_or_default();
    assert!(
        !instruction.contains("User prefers concise answers"),
        "the opt-out must be respected: {instruction:?}"
    );
}

#[tokio::test]
async fn the_injection_bound_is_respected() {
    // More entries than the bound allows; the instruction must carry at most the bound.
    let entries: Vec<MemoryEntry> = (0..10)
        .map(|index| MemoryEntry {
            content: Content::new("model").with_text(format!("fact number {index}")),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        })
        .collect();
    let memory_service = Arc::new(TrackingMemoryService::new(entries));

    let runner = IntegratedRealtimeRunnerBuilder::new()
        .model(mock_model())
        .identity("test-app", "user-1", "session-1")
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig {
            persist_transcripts: false,
            store_to_memory: false,
            inject_memory_context: true,
            max_memory_injection: 3,
            max_history_injection: 20,
        })
        .build()
        .expect("builder should succeed");

    let _ = runner.connect().await;

    let instruction = runner.instruction().await.unwrap_or_default();
    let carried =
        (0..10).filter(|index| instruction.contains(&format!("fact number {index}"))).count();
    assert!(
        carried <= 3,
        "the instruction is sent once and counts against context, so the bound must hold: \
         carried {carried}"
    );
    assert!(carried > 0, "something must be carried: {instruction:?}");
}
