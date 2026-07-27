//! Provider errors delivered inside an `Ok` response must not read as success.
//!
//! `LlmResponse` carries `interrupted`, `error_code`, and `error_message`. A
//! provider adapter can report a terminal failure that way — `adk-anthropic`'s
//! `from_stream_error`, the OpenAI Responses error event, and the OpenAI
//! websocket transport all do. `LlmAgent` previously copied content, finish
//! reason, usage, and provider metadata onto its events but not those three
//! fields, and never inspected them, so a failed turn arrived as an ordinary
//! event with no content and the run completed successfully.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ── Mocks ─────────────────────────────────────────────────────────────

struct MockModel {
    response: LlmResponse,
}

impl MockModel {
    /// An ordinary successful text turn.
    fn new_text(text: &str) -> Self {
        Self { response: base(Some(text), Some(FinishReason::Stop), false, None, None) }
    }

    /// A provider reporting a terminal error inside a successful stream item,
    /// with no content — the shape `from_stream_error` produces.
    fn new_provider_error(code: &str, message: &str) -> Self {
        Self { response: base(None, None, false, Some(code), Some(message)) }
    }

    /// A provider reporting an interruption mid-turn.
    fn new_interrupted() -> Self {
        Self { response: base(Some("partial answer"), None, true, None, None) }
    }
}

fn base(
    text: Option<&str>,
    finish_reason: Option<FinishReason>,
    interrupted: bool,
    error_code: Option<&str>,
    error_message: Option<&str>,
) -> LlmResponse {
    LlmResponse {
        content: text.map(|t| Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: t.to_string() }],
        }),
        usage_metadata: None,
        finish_reason,
        citation_metadata: None,
        partial: false,
        turn_complete: true,
        interrupted,
        error_code: error_code.map(str::to_string),
        error_message: error_message.map(str::to_string),
        provider_metadata: None,
        interaction_id: None,
    }
}

#[async_trait]
impl Llm for MockModel {
    fn name(&self) -> &str {
        "mock-model"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.response.clone();
        let s = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(s))
    }
}

struct MockState;

impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockSession;

impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "start".to_string() }],
            },
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised by these tests")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

/// Drain an agent run into (events, errors).
async fn run(model: MockModel) -> (Vec<adk_core::Event>, Vec<adk_core::AdkError>) {
    let agent = LlmAgentBuilder::new("test-agent").model(Arc::new(model)).build().unwrap();
    let mut stream = agent.run(Arc::new(MockContext::new())).await.unwrap();
    let (mut events, mut errors) = (Vec::new(), Vec::new());
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => events.push(event),
            Err(e) => errors.push(e),
        }
    }
    (events, errors)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_provider_error_ends_the_turn_with_an_error() {
    let (_events, errors) =
        run(MockModel::new_provider_error("overloaded_error", "the model is overloaded")).await;

    assert_eq!(errors.len(), 1, "the run must fail, not complete successfully");
    let rendered = errors[0].to_string();
    assert!(
        rendered.contains("overloaded_error"),
        "the provider's code must survive into the error: {rendered}"
    );
    assert!(
        rendered.contains("the model is overloaded"),
        "the provider's message must survive into the error: {rendered}"
    );
}

#[tokio::test]
async fn the_emitted_event_records_the_provider_error() {
    // The event is emitted before the failure, so the turn stays observable and
    // persistable rather than vanishing into an error.
    let (events, _errors) =
        run(MockModel::new_provider_error("rate_limit_error", "slow down")).await;

    let carrying = events
        .iter()
        .find(|e| e.llm_response.error_code.is_some())
        .expect("an event must carry the provider error fields");
    assert_eq!(carrying.llm_response.error_code.as_deref(), Some("rate_limit_error"));
    assert_eq!(carrying.llm_response.error_message.as_deref(), Some("slow down"));
}

#[tokio::test]
async fn an_interruption_is_recorded_on_the_event() {
    // `interrupted` is not a failure, so the run still succeeds — but the flag
    // must reach the caller instead of being dropped.
    let (events, errors) = run(MockModel::new_interrupted()).await;

    assert!(errors.is_empty(), "an interruption alone is not a terminal error");
    assert!(
        events.iter().any(|e| e.llm_response.interrupted),
        "the interrupted flag must be recorded on an event"
    );
}

#[tokio::test]
async fn a_healthy_turn_still_succeeds() {
    // Guards against the error path firing on ordinary responses.
    let (events, errors) = run(MockModel::new_text("a perfectly ordinary answer")).await;

    assert!(errors.is_empty(), "a healthy turn must not fail");
    assert!(!events.is_empty());
    assert!(
        events.iter().all(|e| e.llm_response.error_code.is_none()),
        "a healthy turn must not carry an error code"
    );
}
