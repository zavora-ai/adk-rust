//! Output guardrails screen everything a model turn shows the client and the session — text
//! written beside a tool call included — a block is a structured error, and tool guardrails
//! screen a tool's result before the model sees it.
#![cfg(feature = "guardrails")]

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{
    ContentFilter, GUARDRAIL_WARNINGS_KEY, Guardrail, GuardrailResult, GuardrailSet,
    INPUT_BLOCKED_CODE, OUTPUT_BLOCKED_CODE, PiiRedactor, ResultTextGuardrail, SchemaValidator,
    Severity, ToolGuardrail, ToolGuardrailSet, ToolResultGuardrailResult,
};
use adk_core::{
    AdkError, Content, ErrorCategory, Event, FinishReason, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, SessionId, StreamingMode, Tool, ToolContext,
    UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const EMAIL: &str = "jane.doe@example.com";

/// Answers the n-th request with the n-th scripted turn and records every request.
struct ScriptedModel {
    turns: Vec<Vec<Part>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl ScriptedModel {
    fn new(turns: Vec<Vec<Part>>) -> Arc<Self> {
        Arc::new(Self { turns, requests: Mutex::new(Vec::new()) })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let turn = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            requests.len() - 1
        };
        let parts = self.turns.get(turn).cloned().unwrap_or_else(|| vec![text("done")]);
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts }),
            turn_complete: true,
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn text(text: &str) -> Part {
    Part::Text { text: text.to_string() }
}

fn call(name: &str) -> Part {
    Part::FunctionCall {
        name: name.to_string(),
        args: json!({ "order": 42 }),
        id: Some("call-1".to_string()),
        thought_signature: None,
    }
}

/// An OpenAI Responses `commentary` message, as the provider adapter keeps it for replay.
fn commentary(text: &str) -> Part {
    Part::ServerToolCall {
        server_tool_call: json!({
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        }),
    }
}

struct Lookup {
    result: Value,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks up an order."
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone())
    }
}

struct Turn {
    error: Option<AdkError>,
    received: Vec<Event>,
    saved: Vec<Event>,
    runs: usize,
}

impl Turn {
    fn received_json(&self) -> String {
        serde_json::to_string(&self.received).unwrap()
    }

    fn saved_agent_json(&self) -> String {
        let authored: Vec<&Event> = self.saved.iter().filter(|e| e.author != "user").collect();
        serde_json::to_string(&authored).unwrap()
    }
}

async fn run_turn(
    model: Arc<ScriptedModel>,
    tool_result: Value,
    configure: impl FnOnce(LlmAgentBuilder) -> LlmAgentBuilder,
    message: &str,
) -> Turn {
    let runs = Arc::new(AtomicUsize::new(0));
    let builder = LlmAgentBuilder::new("assistant")
        .model(model)
        .tool(Arc::new(Lookup { result: tool_result, runs: runs.clone() }));
    let agent = Arc::new(configure(builder).build().unwrap());
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "guarded".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("guarded")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut stream = runner
        .run_with_config(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text(message),
            Some(RunConfig::builder().streaming_mode(StreamingMode::SSE).build()),
        )
        .await
        .unwrap();
    let mut turn = Turn { error: None, received: Vec::new(), saved: Vec::new(), runs: 0 };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => turn.received.push(event),
            Err(error) => {
                turn.error = Some(error);
                break;
            }
        }
    }
    drop(stream);
    turn.saved = sessions
        .get(GetRequest {
            app_name: "guarded".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all();
    turn.runs = runs.load(Ordering::SeqCst);
    turn
}

fn pii_output() -> GuardrailSet {
    GuardrailSet::new().with(PiiRedactor::new())
}

#[tokio::test]
async fn text_beside_a_tool_call_is_redacted_before_the_client_or_the_session_sees_it() {
    let model = ScriptedModel::new(vec![
        vec![text(&format!("I'll email {EMAIL} the receipt.")), call("lookup")],
        vec![text("Order 42 has shipped.")],
    ]);
    let turn = run_turn(
        model.clone(),
        json!({ "status": "shipped" }),
        |b| b.output_guardrails(pii_output()),
        "where is order 42?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    assert_eq!(turn.runs, 1, "the call beside the text still runs");
    assert!(!turn.received_json().contains(EMAIL), "{}", turn.received_json());
    assert!(!turn.saved_agent_json().contains(EMAIL));
    assert!(turn.saved_agent_json().contains("[EMAIL REDACTED]"));
    let replayed = serde_json::to_string(&model.requests()[1].contents).unwrap();
    assert!(!replayed.contains(EMAIL), "the next request replays the redacted text: {replayed}");
}

#[tokio::test]
async fn a_blocked_preamble_fails_the_turn_with_a_structured_error_and_its_call_never_runs() {
    let model = ScriptedModel::new(vec![vec![text("I will wire the funds now."), call("lookup")]]);
    let filter = ContentFilter::blocked_keywords(vec!["wire the funds".into()]).unwrap();
    let turn = run_turn(
        model.clone(),
        json!({}),
        |b| b.output_guardrails(GuardrailSet::new().with(filter)),
        "pay it",
    )
    .await;

    let error = turn.error.as_ref().expect("the block ends the run");
    assert_eq!((error.category, error.code), (ErrorCategory::InvalidInput, OUTPUT_BLOCKED_CODE));
    assert_eq!(error.details.metadata["guardrail"], json!("blocked_keywords"));
    assert_eq!(error.details.metadata["severity"], json!("High"));
    assert_eq!(error.details.metadata["phase"], json!("output"));
    assert_eq!(turn.runs, 0);
    assert!(!turn.received_json().contains("wire the funds"));
    assert!(!turn.saved_agent_json().contains("wire the funds"));
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn a_schema_validator_does_not_hold_a_preamble_to_the_answer_schema() {
    let schema = json!({ "type": "object", "required": ["status"] });
    let model = ScriptedModel::new(vec![
        vec![text("Let me check."), call("lookup")],
        vec![text(r#"{"status": "shipped"}"#)],
    ]);
    let turn = run_turn(
        model,
        json!({ "status": "shipped" }),
        |b| b.output_guardrails(GuardrailSet::new().with(SchemaValidator::new(&schema).unwrap())),
        "status?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    assert_eq!(turn.runs, 1);
}

#[tokio::test]
async fn provider_native_commentary_beside_a_call_is_screened() {
    let model = ScriptedModel::new(vec![
        vec![commentary(&format!("Emailing {EMAIL} now.")), call("lookup")],
        vec![text("Done.")],
    ]);
    let turn = run_turn(
        model,
        json!({ "status": "shipped" }),
        |b| b.output_guardrails(pii_output()),
        "status?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    assert!(!turn.received_json().contains(EMAIL), "{}", turn.received_json());
    let saved = turn.saved_agent_json();
    assert!(!saved.contains(EMAIL), "{saved}");
    assert!(
        saved.contains("Emailing [EMAIL REDACTED] now."),
        "the message keeps its shape: {saved}"
    );
    assert!(saved.contains("commentary"), "{saved}");
}

#[tokio::test]
async fn the_event_debug_copy_carries_no_unscreened_text() {
    let model = ScriptedModel::new(vec![vec![text(&format!("Write to {EMAIL}."))]]);
    let turn = run_turn(model, json!({}), |b| b.output_guardrails(pii_output()), "contact?").await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    let metadata: Vec<&String> = turn
        .received
        .iter()
        .filter_map(|event| event.provider_metadata.get("gcp.vertex.agent.llm_response"))
        .collect();
    assert!(!metadata.is_empty(), "the debug copy is still recorded");
    assert!(metadata.iter().all(|debug| !debug.contains(EMAIL)), "{metadata:?}");
}

#[tokio::test]
async fn an_input_block_is_structured_and_distinct_from_an_internal_error() {
    let model = ScriptedModel::new(vec![]);
    let filter = ContentFilter::blocked_keywords(vec!["wire the funds".into()]).unwrap();
    let turn = run_turn(
        model.clone(),
        json!({}),
        |b| b.input_guardrails(GuardrailSet::new().with(filter)),
        "please wire the funds",
    )
    .await;

    let error = turn.error.as_ref().expect("the block ends the run");
    assert_eq!((error.category, error.code), (ErrorCategory::InvalidInput, INPUT_BLOCKED_CODE));
    assert_eq!(error.http_status_code(), 400);
    assert_eq!(error.details.metadata["guardrail"], json!("blocked_keywords"));
    assert_eq!(error.details.metadata["phase"], json!("input"));
    assert!(model.requests().is_empty());
    assert!(turn.saved.is_empty(), "a blocked message is not saved");
}

/// Flags, at low severity, any text that mentions a refund.
struct FlagRefunds;

#[async_trait]
impl Guardrail for FlagRefunds {
    fn name(&self) -> &str {
        "flag_refunds"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        if text.contains("refund") {
            GuardrailResult::fail("mentions a refund", Severity::Low)
        } else {
            GuardrailResult::pass()
        }
    }
}

#[tokio::test]
async fn a_low_severity_failure_passes_and_is_recorded_on_the_event() {
    let model = ScriptedModel::new(vec![vec![text("A refund was issued.")]]);
    let turn = run_turn(
        model,
        json!({}),
        |b| b.output_guardrails(GuardrailSet::new().with(FlagRefunds)),
        "news?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    let recorded = |events: &[Event]| {
        events.iter().filter_map(|e| e.provider_metadata.get(GUARDRAIL_WARNINGS_KEY)).any(|w| {
            serde_json::from_str::<Value>(w).unwrap()
                == json!([{ "guardrail": "flag_refunds", "reason": "mentions a refund", "severity": "Low" }])
        })
    };
    assert!(recorded(&turn.received), "the client sees the warning");
    assert!(recorded(&turn.saved), "the session keeps it");
    assert!(turn.received_json().contains("A refund was issued."));
}

#[tokio::test]
async fn tool_guardrails_screen_a_result_before_the_model_or_the_session_sees_it() {
    let model = ScriptedModel::new(vec![vec![call("lookup")], vec![text("Shipped.")]]);
    let guardrails = ToolGuardrailSet::new().with(ResultTextGuardrail::new(PiiRedactor::new()));
    let turn = run_turn(
        model.clone(),
        json!({ "status": "shipped", "contact": EMAIL }),
        |b| b.tool_guardrails(guardrails),
        "status?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    let sent = serde_json::to_string(&model.requests()[1].contents).unwrap();
    assert!(!sent.contains(EMAIL), "{sent}");
    assert!(sent.contains("[EMAIL REDACTED]"), "{sent}");
    assert!(!turn.saved_agent_json().contains(EMAIL));
    assert!(!turn.received_json().contains(EMAIL));
}

/// Withholds every result of `lookup`.
struct WithholdLookups;

#[async_trait]
impl ToolGuardrail for WithholdLookups {
    fn name(&self) -> &str {
        "withhold_lookups"
    }

    async fn validate_result(
        &self,
        _tool: &str,
        _args: &Value,
        _result: &Value,
    ) -> ToolResultGuardrailResult {
        ToolResultGuardrailResult::withhold("lookups are under review", Severity::High)
    }
}

#[tokio::test]
async fn a_withheld_result_is_answered_as_withheld_and_states_the_tool_ran() {
    let model = ScriptedModel::new(vec![vec![call("lookup")], vec![text("Sorry.")]]);
    let turn = run_turn(
        model.clone(),
        json!({ "secret": "s3cr3t" }),
        |b| b.tool_guardrails(ToolGuardrailSet::new().with(WithholdLookups)),
        "status?",
    )
    .await;

    assert!(turn.error.is_none(), "{:?}", turn.error);
    assert_eq!(turn.runs, 1);
    let response = model.requests()[1]
        .contents
        .iter()
        .flat_map(|content| &content.parts)
        .find_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        response,
        json!({
            "status": adk_core::RESULT_WITHHELD_STATUS,
            "error": "tool 'lookup' ran, but its result was withheld: tool guardrail \
                      'withhold_lookups' (High): lookups are under review; do not call it again \
                      for this request.",
        })
    );
    assert!(!turn.saved_agent_json().contains("s3cr3t"));
}
