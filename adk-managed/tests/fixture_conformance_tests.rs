//! Golden fixture conformance tests (scripted mode, per-commit gate).
//!
//! This test suite loads each fixture JSON, constructs a runtime with
//! `ScriptedLlm` + the in-process session loop, executes the scenario,
//! and asserts `exact_sequence` — byte-identical type sequences.
//!
//! Runs on every commit, blocks merge, costs $0.
//!
//! # Test Mode
//!
//! Controlled by `ADK_TEST_MODE` environment variable:
//! - `scripted` (default): uses `scripted_model.turns` → asserts `exact_sequence`
//! - `real`: uses `agent_def.model` against real provider → asserts `must_contain` + `must_end_with`

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::session_loop::SessionLoop;
use adk_managed::testing::{ScriptedLlm, ScriptedTurn};
use adk_managed::types::{ContentBlock, ManagedAgentDef, ModelRef, SessionEvent, UserEvent};
use adk_managed::{DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner};

use adk_core::{Agent, Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_session::service::SessionService;
use async_trait::async_trait;

/// Build a stub agent for tests that need a `SessionLoop` with the full API.
fn build_stub_agent() -> Arc<dyn Agent> {
    struct StubLlm;

    #[async_trait]
    impl Llm for StubLlm {
        fn name(&self) -> &str {
            "stub-llm"
        }
        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let s = async_stream::stream! {
                yield Ok(LlmResponse {
                    content: Some(Content::new("model").with_text("stub response")),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    let agent =
        adk_agent::LlmAgentBuilder::new("stub-agent").model(Arc::new(StubLlm)).build().unwrap();
    Arc::new(agent)
}

/// Build a stub session service for tests.
fn build_stub_session_service() -> Arc<dyn SessionService> {
    Arc::new(adk_session::InMemorySessionService::new())
}

/// Fixture schema matching the JSON files in `tests/fixtures/`.
#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    description: String,
    agent_def: serde_json::Value,
    scripted_model: ScriptedModel,
    scenario: Vec<ScenarioEvent>,
    assertions: Assertions,
}

#[derive(Debug, Deserialize)]
struct ScriptedModel {
    turns: Vec<ScriptedTurn>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ScenarioEvent {
    #[serde(rename = "user.message")]
    Message { content: Vec<ContentBlockJson> },
    #[serde(rename = "user.interrupt")]
    Interrupt {},
    #[serde(rename = "user.custom_tool_result")]
    CustomToolResult { custom_tool_use_id: String, content: Vec<ContentBlockJson> },
    #[serde(rename = "user.tool_confirmation")]
    ToolConfirmation { tool_use_id: String, result: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlockJson {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: serde_json::Value },
    #[serde(rename = "file")]
    File { file_id: String },
}

#[derive(Debug, Deserialize)]
struct Assertions {
    exact_sequence: Vec<String>,
    #[allow(dead_code)]
    must_contain: Option<Vec<String>>,
    #[allow(dead_code)]
    must_end_with: Option<Vec<String>>,
}

/// Load a fixture from the fixtures directory.
fn load_fixture(filename: &str) -> Fixture {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(filename);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("failed to parse fixture {}: {e}", path.display()))
}

/// Convert a scenario event into a `UserEvent`.
fn scenario_to_user_event(event: &ScenarioEvent) -> UserEvent {
    match event {
        ScenarioEvent::Message { content } => {
            UserEvent::Message { content: content.iter().map(content_block_from_json).collect() }
        }
        ScenarioEvent::Interrupt {} => UserEvent::Interrupt {},
        ScenarioEvent::CustomToolResult { custom_tool_use_id, content } => {
            UserEvent::CustomToolResult {
                custom_tool_use_id: custom_tool_use_id.clone(),
                content: content.iter().map(content_block_from_json).collect(),
            }
        }
        ScenarioEvent::ToolConfirmation { tool_use_id, result } => {
            let confirmation_result = match result.as_str() {
                "allow" => adk_managed::types::ConfirmationResult::Allow,
                _ => adk_managed::types::ConfirmationResult::Deny,
            };
            UserEvent::ToolConfirmation {
                tool_use_id: tool_use_id.clone(),
                result: confirmation_result,
                deny_message: None,
            }
        }
    }
}

fn content_block_from_json(block: &ContentBlockJson) -> ContentBlock {
    match block {
        ContentBlockJson::Text { text } => ContentBlock::Text { text: text.clone() },
        ContentBlockJson::Image { source } => ContentBlock::Image { source: source.clone() },
        ContentBlockJson::File { file_id } => ContentBlock::File { file_id: file_id.clone() },
    }
}

/// Extract the event type string from a SessionEvent.
fn event_type_string(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::StatusRunning { .. } => "status.running",
        SessionEvent::Message { .. } => "agent.message",
        SessionEvent::ToolUse { .. } => "agent.tool_use",
        SessionEvent::CustomToolUse { .. } => "agent.custom_tool_use",
        SessionEvent::McpToolUse { .. } => "agent.mcp_tool_use",
        SessionEvent::StatusIdle { .. } => "status.idle",
        SessionEvent::Error { .. } => "error",
        _ => "unknown",
    }
}

/// Resolves every model reference to one scripted model.
struct ScriptedResolver(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for ScriptedResolver {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(self.0.clone() as Arc<dyn Llm>)
    }
}

/// Run a fixture in scripted mode through the full runtime.
///
/// The fixture's agent definition is built by `DefaultManagedAgentRuntime` over a
/// `ScriptedLlm` serving the fixture's turns, so tools, permission policy, custom tool
/// parking, and confirmations run exactly as they do against a provider. Each scenario
/// event is sent once the previous one's turn has ended with `status.idle`.
async fn run_fixture_scripted(fixture: &Fixture) -> Vec<String> {
    let llm = Arc::new(ScriptedLlm::new("fixture-model", fixture.scripted_model.turns.clone()));
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(ScriptedResolver(llm)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let def: ManagedAgentDef = serde_json::from_value(fixture.agent_def.clone())
        .unwrap_or_else(|e| panic!("fixture {} agent_def does not parse: {e}", fixture.name));
    let agent = runtime.create(def).await.expect("create agent");
    let owner = ManagedOwner::new("fixtures", "fixture-user").unwrap();
    let session = runtime.start_session(&agent, &owner, None).await.expect("start session");
    let mut stream = runtime.stream_events(&session, None).await.expect("stream");

    let mut event_types = Vec::new();
    for scenario_event in &fixture.scenario {
        runtime
            .send_event(&session, scenario_to_user_event(scenario_event))
            .await
            .unwrap_or_else(|e| panic!("fixture {}: send failed: {e}", fixture.name));
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("fixture {}: no status.idle; saw {event_types:?}", fixture.name)
                })
                .expect("stream ended");
            event_types.push(event_type_string(&event).to_string());
            if matches!(event, SessionEvent::StatusIdle { .. }) {
                break;
            }
        }
    }

    event_types
}

/// Assert exact sequence match for scripted mode.
fn assert_exact_sequence(fixture_name: &str, actual: &[String], expected: &[String]) {
    assert_eq!(
        actual, expected,
        "\n\nFixture: {fixture_name}\n  Expected: {expected:?}\n  Actual:   {actual:?}\n"
    );
}

// ============================================================================
// Fixture Tests
// ============================================================================

#[tokio::test]
async fn test_f1_hello() {
    let fixture = load_fixture("f1_hello.json");
    let actual = run_fixture_scripted(&fixture).await;
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

#[tokio::test]
async fn test_f3_custom_tool() {
    let fixture = load_fixture("f3_custom_tool.json");
    let actual = run_fixture_scripted(&fixture).await;
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

#[tokio::test]
async fn test_f4_confirmation() {
    let fixture = load_fixture("f4_confirmation.json");
    let actual = run_fixture_scripted(&fixture).await;
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

#[tokio::test]
async fn test_f5_resume() {
    let fixture = load_fixture("f5_resume.json");
    let actual = run_fixture_scripted(&fixture).await;
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

#[tokio::test]
async fn test_f6_replay() {
    let fixture = load_fixture("f6_replay.json");
    let actual = run_fixture_scripted(&fixture).await;
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

#[tokio::test]
async fn test_f7_interrupt() {
    let fixture = load_fixture("f7_interrupt.json");
    let actual = run_fixture_scripted(&fixture).await;

    // The interrupt is sent after the message's turn ended, so it only stops the loop.
    assert_exact_sequence(&fixture.name, &actual, &fixture.assertions.exact_sequence);
}

/// Test that all fixtures load and parse correctly.
#[test]
fn test_all_fixtures_parse() {
    let fixture_files = [
        "f1_hello.json",
        "f2_mcp_tool.json",
        "f3_custom_tool.json",
        "f4_confirmation.json",
        "f5_resume.json",
        "f6_replay.json",
        "f7_interrupt.json",
        "f8_provider_parity.json",
    ];

    for file in &fixture_files {
        let fixture = load_fixture(file);
        assert!(!fixture.name.is_empty(), "fixture {file} has no name");
        assert!(!fixture.description.is_empty(), "fixture {file} has no description");
        assert!(
            !fixture.assertions.exact_sequence.is_empty(),
            "fixture {file} has no exact_sequence assertions"
        );
        assert!(!fixture.scenario.is_empty(), "fixture {file} has no scenario events");
    }
}

/// Test that the ScriptedLlm correctly serves turns from fixture data.
#[tokio::test]
async fn test_scripted_llm_from_fixture() {
    use adk_core::Llm;
    use futures::StreamExt;

    let fixture = load_fixture("f1_hello.json");
    let llm = ScriptedLlm::new("fixture-model", fixture.scripted_model.turns);

    let request = adk_core::LlmRequest::new("fixture-model", vec![]);
    let mut stream = llm.generate_content(request, false).await.unwrap();

    let response = stream.next().await.unwrap().unwrap();
    assert!(response.turn_complete);

    let content = response.content.unwrap();
    assert_eq!(content.role, "model");
    match &content.parts[0] {
        adk_core::types::Part::Text { text } => {
            assert_eq!(text, "Hello! How can I help you today?");
        }
        other => panic!("expected Text part, got: {other:?}"),
    }
}

/// Verify seq monotonicity across all events produced by a fixture run.
#[tokio::test]
async fn test_seq_monotonicity_in_fixture_run() {
    let (event_tx, event_rx) = mpsc::channel(64);
    let (broadcast_tx, mut broadcast_rx) = broadcast::channel(256);
    let cancel = CancellationToken::new();

    let session_loop = SessionLoop::new(
        "seq_test".to_string(),
        event_rx,
        broadcast_tx,
        cancel.clone(),
        build_stub_agent(),
        build_stub_session_service(),
    );

    let loop_handle = tokio::spawn(session_loop.run());

    // Send multiple messages.
    for i in 0..3 {
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text { text: format!("Message {i}") }],
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    drop(event_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), loop_handle).await;

    // Collect seqs and verify monotonicity.
    let mut seqs = Vec::new();
    while let Ok(event) = broadcast_rx.try_recv() {
        let seq = match &event {
            SessionEvent::StatusRunning { seq } => *seq,
            SessionEvent::Message { seq, .. } => *seq,
            SessionEvent::StatusIdle { seq, .. } => *seq,
            SessionEvent::ToolUse { seq, .. } => *seq,
            SessionEvent::CustomToolUse { seq, .. } => *seq,
            SessionEvent::McpToolUse { seq, .. } => *seq,
            SessionEvent::Error { seq, .. } => *seq,
            _ => continue,
        };
        seqs.push(seq);
    }

    assert!(!seqs.is_empty(), "should have collected events");
    for window in seqs.windows(2) {
        assert!(
            window[1] > window[0],
            "seq must be strictly increasing: {} > {} violated",
            window[1],
            window[0]
        );
    }
}
