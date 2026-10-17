//! Output guardrails and tool-result screening across `LlmAgent` and the realtime runners.
//!
//! Every scenario drives the real stack and asserts on what reached the client, the session, and
//! the model:
//!
//! - `LlmAgentBuilder` with `adk-guardrail` output guardrails (PII redaction and a blocking
//!   content filter), `Runner`, `InMemorySessionService`, and `OpenAIClient` pointed at a scripted
//!   OpenAI-compatible server on 127.0.0.1 that writes text and calls a tool in the same turn.
//! - `RealtimeAgent` under `Runner`, and `RealtimeRunner` with a native handler, with
//!   `OpenAIRealtimeModel` pointed at a scripted WebSocket server speaking the OpenAI Realtime
//!   event protocol, which records the `function_call_output` each tool call produced.
//!
//! The `live_*` scenarios run against OpenAI when `OPENAI_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/guardrail_coverage/Cargo.toml
//! cargo run --manifest-path examples/guardrail_coverage/Cargo.toml -- --scenario preamble_blocked
//! ```

mod realtime_ws;
mod scripted;
mod setup;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{
    ContentFilter, Guardrail, GuardrailResult, GuardrailSet, PiiRedactor, Severity,
};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, Event, Llm, Part, RunConfig,
    SessionId, StreamingMode, Tool, ToolContext, ToolEffect, UserId,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_realtime::config::ToolDefinition;
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::{RealtimeAgent, RealtimeError, RealtimeRunner};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use realtime_ws::{RealtimeServer, ScriptedToolCall};
use scripted::{Reply, ScriptedServer, last_tool_message};
use serde_json::{Value, json};

const APP: &str = "guardrail-coverage";
const USER: &str = "customer";
const SESSION: &str = "session-1";
const EMAIL: &str = "jane.doe@example.com";
const SSN: &str = "123-45-6789";
const SECRET: &str = "hunter2";
const SECRET_HOST: &str = "db.internal";
const BLOCKED_PHRASE: &str = "wire the funds";
const LIVE_MODEL: &str = "gpt-5.6-terra";
const LIVE_REALTIME_MODEL: &str = "gpt-realtime-2.1";

const SCENARIOS: &[&str] = &[
    "preamble_pii_redacted",
    "preamble_blocked",
    "block_is_structured",
    "low_severity_recorded",
    "llm_tool_result_screened",
    "realtime_agent_result_screened",
    "realtime_agent_screen_fails_closed",
    "realtime_agent_error_redacted",
    "realtime_runner_result_screened",
    "realtime_runner_screen_fails_closed",
    "realtime_runner_error_redacted",
    "live_openai_preamble",
    "live_openai_realtime",
];

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// What a scripted tool does when it runs.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Returns an order status with no personal data.
    Status,
    /// Returns an order record carrying a customer's email address and SSN.
    Pii,
    /// Fails with an error whose message quotes a database credential.
    CredentialError,
}

struct OrderTool {
    name: &'static str,
    behaviour: Behaviour,
    runs: Arc<AtomicUsize>,
}

impl OrderTool {
    fn new(name: &'static str, behaviour: Behaviour) -> (Arc<Self>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        (Arc::new(Self { name, behaviour, runs: runs.clone() }), runs)
    }
}

fn credential_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "tool.orders_db.unavailable",
        format!("connect postgres://admin:{SECRET}@{SECRET_HOST}/prod failed"),
    )
}

fn pii_record() -> Value {
    json!({"order": 42, "status": "shipped", "contact": EMAIL, "ssn": SSN})
}

#[async_trait]
impl Tool for OrderTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Looks up an order by number."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"order": {"type": "integer"}},
            "required": ["order"],
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Status => Ok(json!({"order": 42, "status": "shipped"})),
            Behaviour::Pii => Ok(pii_record()),
            Behaviour::CredentialError => Err(credential_error()),
        }
    }
}

/// Flags, at low severity, an answer that mentions a refund.
struct FlagRefunds;

#[async_trait]
impl Guardrail for FlagRefunds {
    fn name(&self) -> &str {
        "flag_refunds"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        if text.contains("refund") {
            GuardrailResult::fail("the answer mentions a refund", Severity::Low)
        } else {
            GuardrailResult::Pass
        }
    }
}

// ---------------------------------------------------------------------------
// Running an agent
// ---------------------------------------------------------------------------

struct Turn {
    error: Option<AdkError>,
    /// Every event the client received.
    events: Vec<Event>,
    /// Every event the session holds afterwards.
    saved: Vec<Event>,
}

impl Turn {
    fn client_json(&self) -> String {
        serde_json::to_string(&self.events).unwrap_or_default()
    }

    fn saved_json(&self) -> String {
        serde_json::to_string(&self.saved).unwrap_or_default()
    }

    /// The events authored by an agent rather than the user, as JSON text.
    fn agent_json(events: &[Event]) -> String {
        let authored: Vec<&Event> = events.iter().filter(|event| event.author != "user").collect();
        serde_json::to_string(&authored).unwrap_or_default()
    }

    fn describe_error(&self) -> String {
        match &self.error {
            None => "no error".to_string(),
            Some(error) => format!(
                "category={:?} code={} metadata={}",
                error.category,
                error.code,
                serde_json::to_string(&error.details.metadata).unwrap_or_default()
            ),
        }
    }
}

async fn run_agent(
    agent: Arc<dyn Agent>,
    text: &str,
    mode: StreamingMode,
    stop_after: Option<Box<dyn Fn(&[Event]) -> bool + Send>>,
) -> anyhow::Result<Turn> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP).agent(agent).session_service(sessions.clone()).build()?;
    let config = RunConfig::builder().streaming_mode(mode).build();
    let mut stream = runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(SESSION)?,
            Content::new("user").with_text(text),
            Some(config),
        )
        .await?;
    let mut turn = Turn { error: None, events: Vec::new(), saved: Vec::new() };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                turn.events.push(event);
                if stop_after.as_ref().is_some_and(|stop| stop(&turn.events)) {
                    break;
                }
            }
            Err(error) => {
                turn.error = Some(error);
                break;
            }
        }
    }
    drop(stream);
    turn.saved = sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?
        .events()
        .all();
    Ok(turn)
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("test-key", &server.base_url, "scripted-model")?))
}

fn text_and_call(text: &str, name: &str) -> Reply {
    Reply::TextAndCall {
        text: text.to_string(),
        id: "call_1".to_string(),
        name: name.to_string(),
        arguments: r#"{"order":42}"#.to_string(),
    }
}

fn blocked_keywords() -> anyhow::Result<GuardrailSet> {
    Ok(GuardrailSet::new().with(ContentFilter::blocked_keywords(vec![BLOCKED_PHRASE.into()])?))
}

/// Whether `error` reports a guardrail block: an invalid-input category, the given stable code,
/// and the blocking guardrail's name and severity in its details.
fn structured_block(error: Option<&AdkError>, code: &str, guardrail: &str) -> Vec<(bool, String)> {
    let Some(error) = error else {
        return vec![(false, format!("the run ends with a `{code}` error (it ended without one)"))];
    };
    let metadata = &error.details.metadata;
    vec![
        (
            error.category == ErrorCategory::InvalidInput,
            format!("category is InvalidInput, not Internal (got {:?})", error.category),
        ),
        (error.code == code, format!("code is `{code}` (got `{}`)", error.code)),
        (
            metadata.get("guardrail").and_then(Value::as_str) == Some(guardrail),
            format!("details name guardrail `{guardrail}` (got {:?})", metadata.get("guardrail")),
        ),
        (
            metadata.get("severity").is_some(),
            format!("details carry the severity (got {:?})", metadata.get("severity")),
        ),
        (
            error.http_status_code() == 400,
            format!("maps to HTTP 400, not 500 (got {})", error.http_status_code()),
        ),
    ]
}

// ---------------------------------------------------------------------------
// LlmAgent scenarios
// ---------------------------------------------------------------------------

async fn preamble_pii_redacted() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn| match turn {
        0 => text_and_call(&format!("Let me check, and I'll email {EMAIL} the receipt."), "status"),
        _ => Reply::Text("Order 42 has shipped.".into()),
    }))
    .await?;
    let (tool, runs) = OrderTool::new("status", Behaviour::Status);
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .tool(tool)
        .output_guardrails(GuardrailSet::new().with(PiiRedactor::new()))
        .build()?;
    let turn = run_agent(Arc::new(agent), "Where is order 42?", StreamingMode::SSE, None).await?;
    let requests = server.requests();
    let replayed =
        requests.get(1).map(|request| request["messages"].to_string()).unwrap_or_default();
    let agent_saved = Turn::agent_json(&turn.saved);
    Ok(verdict(vec![
        (turn.error.is_none(), format!("the run completes ({})", turn.describe_error())),
        (runs.load(Ordering::SeqCst) == 1, "the tool beside the text still runs".into()),
        (
            !turn.client_json().contains(EMAIL),
            "no client event carries the email written beside the call".into(),
        ),
        (!agent_saved.contains(EMAIL), "no saved agent event carries the email".into()),
        (
            agent_saved.contains("[EMAIL REDACTED]"),
            "the saved preamble carries the redaction instead".into(),
        ),
        (!replayed.contains(EMAIL), "the next model request replays the redacted preamble".into()),
    ]))
}

async fn preamble_blocked() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn| match turn {
        0 => text_and_call(&format!("Understood, I will {BLOCKED_PHRASE} right away."), "transfer"),
        _ => Reply::Text("Done.".into()),
    }))
    .await?;
    let (tool, runs) = OrderTool::new("transfer", Behaviour::Status);
    let agent = LlmAgentBuilder::new("payments")
        .model(scripted_model(&server)?)
        .tool(tool)
        .output_guardrails(blocked_keywords()?)
        .build()?;
    let turn = run_agent(Arc::new(agent), "Pay the invoice.", StreamingMode::SSE, None).await?;
    let mut checks =
        structured_block(turn.error.as_ref(), "guardrail.output_blocked", "blocked_keywords");
    checks.extend([
        (
            runs.load(Ordering::SeqCst) == 0,
            format!(
                "the call in the blocked turn does not run (ran {} time(s))",
                runs.load(Ordering::SeqCst)
            ),
        ),
        (!turn.client_json().contains(BLOCKED_PHRASE), "no client event carries the text".into()),
        (!turn.saved_json().contains(BLOCKED_PHRASE), "the session does not hold the text".into()),
        (server.requests().len() == 1, "the model is not called again".into()),
    ]);
    Ok(verdict(checks))
}

async fn block_is_structured() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_| {
        Reply::Text(format!("Sure, I will {BLOCKED_PHRASE} today."))
    }))
    .await?;
    let output = LlmAgentBuilder::new("payments")
        .model(scripted_model(&server)?)
        .output_guardrails(blocked_keywords()?)
        .build()?;
    let output_turn = run_agent(Arc::new(output), "Pay it.", StreamingMode::None, None).await?;

    let input = LlmAgentBuilder::new("payments")
        .model(scripted_model(&server)?)
        .input_guardrails(blocked_keywords()?)
        .build()?;
    let input_turn = run_agent(
        Arc::new(input),
        &format!("Please {BLOCKED_PHRASE} to account X."),
        StreamingMode::None,
        None,
    )
    .await?;

    let mut checks = vec![(true, format!("output block: {}", output_turn.describe_error()))];
    checks.extend(structured_block(
        output_turn.error.as_ref(),
        "guardrail.output_blocked",
        "blocked_keywords",
    ));
    checks.push((true, format!("input block: {}", input_turn.describe_error())));
    checks.extend(structured_block(
        input_turn.error.as_ref(),
        "guardrail.input_blocked",
        "blocked_keywords",
    ));
    checks.push((
        !input_turn.saved_json().contains(BLOCKED_PHRASE),
        "the blocked input is not saved".into(),
    ));
    Ok(verdict(checks))
}

async fn low_severity_recorded() -> anyhow::Result<Verdict> {
    let server =
        ScriptedServer::start(Box::new(|_| Reply::Text("A refund was issued today.".into())))
            .await?;
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .output_guardrails(GuardrailSet::new().with(FlagRefunds))
        .build()?;
    let turn = run_agent(Arc::new(agent), "Any news?", StreamingMode::None, None).await?;
    let recorded = |events: &[Event]| {
        events
            .iter()
            .filter_map(|event| event.provider_metadata.get(setup::WARNINGS_KEY))
            .any(|warnings| warnings.contains("flag_refunds"))
    };
    Ok(verdict(vec![
        (
            turn.error.is_none(),
            format!("the low-severity finding passes ({})", turn.describe_error()),
        ),
        (turn.client_json().contains("refund was issued"), "the answer is delivered".into()),
        (
            recorded(&turn.events),
            format!("the delivered event records the finding under `{}`", setup::WARNINGS_KEY),
        ),
        (recorded(&turn.saved), "the saved event records the finding".into()),
    ]))
}

async fn llm_tool_result_screened() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn| match turn {
        0 => Reply::Call {
            id: "call_1".into(),
            name: "lookup_order".into(),
            arguments: r#"{"order":42}"#.into(),
        },
        _ => Reply::Text("Order 42 has shipped.".into()),
    }))
    .await?;
    let (tool, _) = OrderTool::new("lookup_order", Behaviour::Pii);
    let agent = setup::llm_redacts_results(
        LlmAgentBuilder::new("support").model(scripted_model(&server)?).tool(tool),
    )
    .build()?;
    let turn = run_agent(Arc::new(agent), "Where is order 42?", StreamingMode::None, None).await?;
    let sent = server.requests().get(1).and_then(last_tool_message).unwrap_or_default();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("the run completes ({})", turn.describe_error())),
        (true, format!("the model was sent: {sent}")),
        (!sent.contains(SSN) && !sent.contains(EMAIL), "the model never sees the PII".into()),
        (sent.contains("[SSN REDACTED]"), "the result reaches the model redacted".into()),
        (!turn.saved_json().contains(SSN), "the session holds no SSN".into()),
    ]))
}

// ---------------------------------------------------------------------------
// Realtime scenarios
// ---------------------------------------------------------------------------

fn scripted_realtime_model(server: &RealtimeServer) -> Arc<OpenAIRealtimeModel> {
    Arc::new(OpenAIRealtimeModel::new("test-key", "scripted-realtime").with_base_url(&server.url))
}

async fn realtime_server(name: &str) -> anyhow::Result<RealtimeServer> {
    RealtimeServer::start(ScriptedToolCall {
        call_id: "call_rt_1".into(),
        name: name.into(),
        arguments: r#"{"order":42}"#.into(),
    })
    .await
}

/// Runs a `RealtimeAgent` under `Runner` for one scripted tool call.
async fn run_realtime_agent(
    behaviour: Behaviour,
    configure: impl FnOnce(adk_realtime::RealtimeAgentBuilder) -> adk_realtime::RealtimeAgentBuilder,
) -> anyhow::Result<(RealtimeServer, Turn)> {
    let server = realtime_server("lookup_order").await?;
    let (tool, _) = OrderTool::new("lookup_order", behaviour);
    let builder = RealtimeAgent::builder("voice")
        .model(scripted_realtime_model(&server))
        .instruction("You are a support agent.")
        .tool(tool);
    let agent = configure(builder).build()?;
    let turn = run_agent(Arc::new(agent), "Where is order 42?", StreamingMode::None, None).await?;
    Ok((server, turn))
}

fn realtime_checks(server: &RealtimeServer, turn: &Turn) -> (String, Vec<(bool, String)>) {
    let sent = server.tool_outputs().join(" | ");
    let checks = vec![
        (turn.error.is_none(), format!("the session completes ({})", turn.describe_error())),
        (!server.tool_outputs().is_empty(), format!("the model was sent: {sent}")),
    ];
    (sent, checks)
}

async fn realtime_agent_result_screened() -> anyhow::Result<Verdict> {
    let (server, turn) =
        run_realtime_agent(Behaviour::Pii, |b| setup::realtime_agent_screen(b, false)).await?;
    let (sent, mut checks) = realtime_checks(&server, &turn);
    checks.extend([
        (!sent.contains(SSN) && !sent.contains(EMAIL), "the model never sees the PII".into()),
        (sent.contains("[SSN REDACTED]"), "the result reaches the model redacted".into()),
        (
            !turn.saved_json().contains(SSN) && !turn.saved_json().contains(EMAIL),
            "the session holds no PII".into(),
        ),
    ]);
    Ok(verdict(checks))
}

async fn realtime_agent_screen_fails_closed() -> anyhow::Result<Verdict> {
    let (server, turn) =
        run_realtime_agent(Behaviour::Pii, |b| setup::realtime_agent_screen(b, true)).await?;
    let (sent, mut checks) = realtime_checks(&server, &turn);
    checks.extend([
        (!sent.contains(SSN), "a failing screen withholds the result from the model".into()),
        (sent.contains("result_withheld"), "the model is told the result is withheld".into()),
        (!turn.saved_json().contains(SSN), "the session holds no SSN".into()),
    ]);
    Ok(verdict(checks))
}

async fn realtime_agent_error_redacted() -> anyhow::Result<Verdict> {
    let (server, turn) = run_realtime_agent(Behaviour::CredentialError, |b| b).await?;
    let (sent, mut checks) = realtime_checks(&server, &turn);
    checks.extend([
        (
            !sent.contains(SECRET) && !sent.contains(SECRET_HOST),
            "the credential in the error message never reaches the model".into(),
        ),
        (sent.contains("tool.unavailable"), "the model still learns the error category".into()),
        (!turn.saved_json().contains(SECRET), "the session holds no credential".into()),
    ]);
    let (exposed, _) =
        run_realtime_agent(Behaviour::CredentialError, |b| setup::realtime_agent_exposes_errors(b))
            .await?;
    checks.push((
        exposed.tool_outputs().join(" ").contains(SECRET_HOST),
        "expose_tool_error_messages(true) opts in to the full message".into(),
    ));
    Ok(verdict(checks))
}

/// Runs a `RealtimeRunner` whose native handler answers one scripted tool call.
async fn run_realtime_runner(
    behaviour: Behaviour,
    configure: impl FnOnce(
        adk_realtime::runner::RealtimeRunnerBuilder,
    ) -> adk_realtime::runner::RealtimeRunnerBuilder,
) -> anyhow::Result<(RealtimeServer, Option<RealtimeError>)> {
    let server = realtime_server("lookup_order").await?;
    let builder = RealtimeRunner::builder()
        .model(scripted_realtime_model(&server))
        .instruction("You are a support agent.")
        .tool_fn(ToolDefinition::new("lookup_order"), move |_call| match behaviour {
            Behaviour::Status => Ok(json!({"order": 42, "status": "shipped"})),
            Behaviour::Pii => Ok(pii_record()),
            Behaviour::CredentialError => Err(RealtimeError::ToolError(format!(
                "connect postgres://admin:{SECRET}@{SECRET_HOST}/prod failed"
            ))),
        });
    let runner = configure(builder).build()?;
    runner.connect().await?;
    runner.send_text("Where is order 42?").await?;
    runner.create_response().await?;
    let outcome = tokio::time::timeout(Duration::from_secs(20), runner.run()).await;
    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(RealtimeError::Timeout("runner did not finish".into())),
    };
    Ok((server, error))
}

async fn realtime_runner_result_screened() -> anyhow::Result<Verdict> {
    let (server, error) =
        run_realtime_runner(Behaviour::Pii, |b| setup::realtime_runner_screen(b, false)).await?;
    let sent = server.tool_outputs().join(" | ");
    Ok(verdict(vec![
        (error.is_none(), format!("the session completes ({error:?})")),
        (true, format!("the model was sent: {sent}")),
        (!sent.contains(SSN) && !sent.contains(EMAIL), "the model never sees the PII".into()),
        (sent.contains("[SSN REDACTED]"), "the result reaches the model redacted".into()),
    ]))
}

async fn realtime_runner_screen_fails_closed() -> anyhow::Result<Verdict> {
    let (server, error) =
        run_realtime_runner(Behaviour::Pii, |b| setup::realtime_runner_screen(b, true)).await?;
    let sent = server.tool_outputs().join(" | ");
    Ok(verdict(vec![
        (error.is_none(), format!("the session completes ({error:?})")),
        (true, format!("the model was sent: {sent}")),
        (!sent.contains(SSN), "a failing screen withholds the result from the model".into()),
        (sent.contains("result_withheld"), "the model is told the result is withheld".into()),
    ]))
}

async fn realtime_runner_error_redacted() -> anyhow::Result<Verdict> {
    let (server, error) = run_realtime_runner(Behaviour::CredentialError, |b| b).await?;
    let sent = server.tool_outputs().join(" | ");
    Ok(verdict(vec![
        (error.is_none(), format!("the session completes ({error:?})")),
        (true, format!("the model was sent: {sent}")),
        (
            !sent.contains(SECRET) && !sent.contains(SECRET_HOST),
            "the credential in the handler's error never reaches the model".into(),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Live scenarios
// ---------------------------------------------------------------------------

fn openai_key() -> Option<String> {
    std::env::var("OPENAI_API_KEY").ok().filter(|key| !key.trim().is_empty())
}

async fn live_openai_preamble() -> anyhow::Result<Verdict> {
    let Some(key) = openai_key() else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, LIVE_MODEL))?);
    let (tool, runs) = OrderTool::new("lookup_order", Behaviour::Status);
    let agent = LlmAgentBuilder::new("support")
        .model(model)
        .instruction(
            "Whenever you call a tool, write one short sentence to the user in the same reply, \
             before the tool call, that repeats the user's contact email address exactly. Then \
             call the tool. After the tool answers, reply in one sentence.",
        )
        .tool(tool)
        .output_guardrails(GuardrailSet::new().with(PiiRedactor::new()))
        .build()?;
    let turn = run_agent(
        Arc::new(agent),
        &format!("My contact email is {EMAIL}. Where is order 42?"),
        StreamingMode::SSE,
        None,
    )
    .await?;
    // Text beside the call arrives as a text part, or, from the Responses API, as a native
    // `commentary` message part.
    let text_beside_call = turn.saved.iter().any(|event| {
        event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
                && content.parts.iter().any(|part| match part {
                    Part::Text { text } => !text.trim().is_empty(),
                    Part::ServerToolCall { server_tool_call } => {
                        server_tool_call["type"] == "message"
                            && server_tool_call["phase"] == "commentary"
                    }
                    _ => false,
                })
        })
    });
    let leaks: Vec<String> = turn
        .saved
        .iter()
        .filter(|event| event.author != "user")
        .filter_map(|event| {
            let json = serde_json::to_value(event).ok()?;
            let fields: Vec<&String> = json
                .as_object()?
                .iter()
                .filter(|(_, value)| value.to_string().contains(EMAIL))
                .map(|(key, _)| key)
                .collect();
            (!fields.is_empty()).then(|| format!("fields {fields:?} content {}", json["content"]))
        })
        .collect();
    Ok(verdict(vec![
        (turn.error.is_none(), format!("the run completes ({})", turn.describe_error())),
        (runs.load(Ordering::SeqCst) >= 1, "the model called the tool".into()),
        (true, format!("the model wrote text beside the call: {text_beside_call}")),
        (true, format!("saved agent events carrying the email: {leaks:?}")),
        (
            !Turn::agent_json(&turn.events).contains(EMAIL),
            "no agent event the client received carries the email".into(),
        ),
        (
            !Turn::agent_json(&turn.saved).contains(EMAIL),
            "no saved agent event carries the email".into(),
        ),
    ]))
}

async fn live_openai_realtime() -> anyhow::Result<Verdict> {
    let Some(key) = openai_key() else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let (tool, runs) = OrderTool::new("lookup_order", Behaviour::Pii);
    let builder = RealtimeAgent::builder("voice")
        .model(Arc::new(OpenAIRealtimeModel::new(key, LIVE_REALTIME_MODEL)))
        .instruction(
            "You are a support agent. Always call lookup_order to answer questions about an \
             order, then read back every field of the record verbatim, including the SSN and \
             the contact email.",
        )
        .modalities(vec!["text".to_string()])
        .tool(tool);
    let agent = setup::realtime_agent_screen(builder, false).build()?;
    // The live session stays open, so the run stops at the first answer after the tool result.
    let stop: Box<dyn Fn(&[Event]) -> bool + Send> = Box::new(|events: &[Event]| {
        let answered_tool = events.iter().any(|event| {
            event.llm_response.content.as_ref().is_some_and(|content| {
                content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
            })
        });
        answered_tool
            && events.last().is_some_and(|event| {
                !event.llm_response.partial
                    && event.llm_response.content.as_ref().is_some_and(|content| {
                        content.role == "model" && content.parts.iter().any(|p| p.text().is_some())
                    })
            })
    });
    let turn = tokio::time::timeout(
        Duration::from_secs(90),
        run_agent(
            Arc::new(agent),
            "What is on file for order 42?",
            StreamingMode::None,
            Some(stop),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("the live realtime session did not answer within 90s"))??;
    let answer: String = turn
        .events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .filter(|content| content.role == "model")
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join(" ");
    Ok(verdict(vec![
        (turn.error.is_none(), format!("the session runs ({})", turn.describe_error())),
        (runs.load(Ordering::SeqCst) >= 1, "the model called the tool".into()),
        (true, format!("the model answered: {answer}")),
        (
            !answer.contains(SSN) && !answer.contains(EMAIL),
            "the model never had the PII to read back".into(),
        ),
        (!turn.saved_json().contains(SSN), "the session holds no SSN".into()),
    ]))
}

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "preamble_pii_redacted" => preamble_pii_redacted().await,
        "preamble_blocked" => preamble_blocked().await,
        "block_is_structured" => block_is_structured().await,
        "low_severity_recorded" => low_severity_recorded().await,
        "llm_tool_result_screened" => llm_tool_result_screened().await,
        "realtime_agent_result_screened" => realtime_agent_result_screened().await,
        "realtime_agent_screen_fails_closed" => realtime_agent_screen_fails_closed().await,
        "realtime_agent_error_redacted" => realtime_agent_error_redacted().await,
        "realtime_runner_result_screened" => realtime_runner_result_screened().await,
        "realtime_runner_screen_fails_closed" => realtime_runner_screen_fails_closed().await,
        "realtime_runner_error_redacted" => realtime_runner_error_redacted().await,
        "live_openai_preamble" => live_openai_preamble().await,
        "live_openai_realtime" => live_openai_realtime().await,
        other => anyhow::bail!("unknown scenario `{other}`; known: {}", SCENARIOS.join(", ")),
    }
}

/// Loads API keys from `ADK_ENV_FILE`, else the repository's `.env`, overriding the shell, whose
/// exported keys may be stale.
fn load_env() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = match std::env::var("ADK_ENV_FILE") {
        Ok(path) => vec![std::path::PathBuf::from(path)],
        Err(_) => vec![manifest.join(".env"), manifest.join("../../.env")],
    };
    if let Some(path) = candidates.into_iter().find_map(|path| path.canonicalize().ok()) {
        let _ = dotenvy::from_path_override(path);
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .try_init();
    adk_core::ensure_crypto_provider();
    load_env();

    let mut selected: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    println!("guardrail coverage — {} scenario(s)\n", selected.len());
    let mut failures = 0;
    for name in &selected {
        let result = tokio::time::timeout(Duration::from_secs(180), run_scenario(name)).await;
        let (status, evidence) = match result {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failures += 1;
                ("FAIL", evidence)
            }
            Ok(Err(error)) => {
                failures += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failures += 1;
                ("FAIL", "timed out after 180s".into())
            }
        };
        println!("{status}  {name}\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
