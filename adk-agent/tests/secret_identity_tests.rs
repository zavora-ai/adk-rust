//! A secret request must carry the identity of the tool the framework dispatched.
//!
//! `ToolContext::get_secret` forwarded a bare name to the invocation context, so the
//! service saw no tool identity and could not tell which tool was asking. This drives
//! a real tool through `LlmAgent` and inspects what the secret service received.

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, SecretRequest, Session, State, Tool, ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

// ── Mocks ─────────────────────────────────────────────────────────────

struct SequencedModel {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl SequencedModel {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self { responses: Arc::new(Mutex::new(responses.into_iter().collect())) }
    }

    fn call(name: &str, id: &str) -> LlmResponse {
        Self::wrap(Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args: json!({}),
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        }))
    }

    fn text(text: &str) -> LlmResponse {
        Self::wrap(Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: text.to_string() }],
        }))
    }

    fn wrap(content: Option<Content>) -> LlmResponse {
        LlmResponse {
            content,
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
        }
    }
}

#[async_trait]
impl Llm for SequencedModel {
    fn name(&self) -> &str {
        "sequenced-model"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| SequencedModel::text("ok"));
        let s = async_stream::stream! { yield Ok(next); };
        Ok(Box::pin(s))
    }
}

/// A tool that asks for a secret, optionally stating a purpose.
struct SecretReadingTool {
    purpose: Option<&'static str>,
}

#[async_trait]
impl Tool for SecretReadingTool {
    fn name(&self) -> &str {
        "weather_lookup"
    }
    fn description(&self) -> &str {
        "reads its API key"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let secret = match self.purpose {
            Some(purpose) => ctx.get_secret_for_purpose("weather-api-key", purpose).await?,
            None => ctx.get_secret("weather-api-key").await?,
        };
        Ok(json!({ "got_secret": secret.is_some() }))
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

/// Captures the secret requests the agent produced.
struct RecordingContext {
    session: MockSession,
    user_content: Content,
    requests: Arc<Mutex<Vec<SecretRequest>>>,
}

#[async_trait]
impl adk_core::ReadonlyContext for RecordingContext {
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
impl adk_core::CallbackContext for RecordingContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for RecordingContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
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

    async fn get_secret_for(&self, request: &SecretRequest) -> Result<Option<String>> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Some("secret-value".to_string()))
    }
}

/// Runs one tool-calling turn and returns the secret requests observed.
async fn run_with(tool: SecretReadingTool) -> Vec<SecretRequest> {
    let model = Arc::new(SequencedModel::new(vec![
        SequencedModel::call("weather_lookup", "call-1"),
        SequencedModel::text("done"),
    ]));
    let agent =
        LlmAgentBuilder::new("test-agent").model(model).tool(Arc::new(tool)).build().unwrap();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let ctx = Arc::new(RecordingContext {
        session: MockSession,
        user_content: Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "go".to_string() }],
        },
        requests: requests.clone(),
    });

    let mut stream = agent.run(ctx).await.unwrap();
    while let Some(result) = stream.next().await {
        result.expect("the run must not fail");
    }
    requests.lock().unwrap().clone()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_secret_request_from_a_tool_carries_that_tools_name() {
    let requests = run_with(SecretReadingTool { purpose: None }).await;

    let request = requests.first().expect("the tool's secret access must reach the service");
    assert_eq!(
        request.tool_name.as_deref(),
        Some("weather_lookup"),
        "the service saw no tool identity, so it cannot authorize per tool"
    );
    assert_eq!(request.name, "weather-api-key");
}

#[tokio::test]
async fn a_secret_request_carries_the_run_identity() {
    let requests = run_with(SecretReadingTool { purpose: None }).await;
    let request = requests.first().expect("a request must be recorded");

    assert_eq!(request.app_name.as_deref(), Some("test-app"));
    assert_eq!(request.user_id.as_deref(), Some("user-1"));
    assert_eq!(request.session_id.as_deref(), Some("session-1"));
    assert_eq!(request.invocation_id.as_deref(), Some("inv-1"));
}

#[tokio::test]
async fn a_stated_purpose_reaches_the_service() {
    let requests =
        run_with(SecretReadingTool { purpose: Some("call the forecast endpoint") }).await;
    let request = requests.first().expect("a request must be recorded");

    assert_eq!(request.purpose.as_deref(), Some("call the forecast endpoint"));
    // The identity still comes from the framework, not from the tool.
    assert_eq!(request.tool_name.as_deref(), Some("weather_lookup"));
}
