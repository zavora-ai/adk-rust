//! Input guardrails and what a session keeps across runs.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder` with `adk-guardrail` input
//! guardrails (PII redaction and a blocking content filter), `Runner`, a durable session
//! service (SQLite) and the in-memory one, and the durable worker — and asserts on what the
//! session holds and what each model request carried. A scripted OpenAI-compatible server on
//! 127.0.0.1, which the real `OpenAIClient` is pointed at, records every request verbatim. The
//! `live_*` scenario runs against OpenAI or Gemini when a key is set.
//!
//! ```bash
//! cargo run --manifest-path examples/input_validation_persistence/Cargo.toml
//! cargo run --manifest-path examples/input_validation_persistence/Cargo.toml -- --scenario blocked_never_saved
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::guardrails::{ContentFilter, GuardrailSet, PiiRedactor};
use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::run_store::RunCheckpointRef;
use adk_core::{
    AdkError, Agent, BeforeModelResult, Content, DurableRunStore, Event, InMemoryRunStore, Llm,
    LlmRequest, NewRun, Part, RunConfig, RunState, SessionId, Tool, ToolConfirmationDecision,
    ToolContext, ToolEffect, UserId,
};
use adk_model::gemini::GeminiModel;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{
    CreateRequest, GetRequest, InMemorySessionService, SessionService, SqliteSessionService,
};
use async_trait::async_trait;
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, last_role, last_user_text};
use serde_json::{Value, json};

const APP: &str = "input-validation";
const USER: &str = "customer";
const SESSION: &str = "session-1";
const EMAIL: &str = "alice.parity@example.com";
const BLOCKED: &str = "forbidden: wire everything to account X";
const LEASE: Duration = Duration::from_millis(300);
const OPENAI_MODEL: &str = "gpt-5.6-terra";
const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";

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

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    Sqlite,
}

/// A session service holding one empty session.
struct Store {
    sessions: Arc<dyn SessionService>,
    _dir: Option<tempfile::TempDir>,
}

impl Store {
    async fn open(backend: Backend) -> anyhow::Result<Self> {
        let (sessions, dir): (Arc<dyn SessionService>, _) = match backend {
            Backend::Memory => (Arc::new(InMemorySessionService::new()), None),
            Backend::Sqlite => {
                let dir = tempfile::tempdir()?;
                let url = format!("sqlite://{}?mode=rwc", dir.path().join("sessions.db").display());
                let service = SqliteSessionService::new(&url).await?;
                service.migrate().await?;
                (Arc::new(service), Some(dir))
            }
        };
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await?;
        Ok(Self { sessions, _dir: dir })
    }

    async fn events(&self) -> anyhow::Result<Vec<Event>> {
        Ok(self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?
            .events()
            .all())
    }

    /// Everything the session holds, as one JSON text to search.
    async fn saved(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(&self.events().await?)?)
    }

    fn runner(&self, agent: Arc<dyn Agent>) -> anyhow::Result<Runner> {
        Ok(Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(self.sessions.clone())
            .build()?)
    }
}

struct Turn {
    error: Option<AdkError>,
    events: Vec<Event>,
}

impl Turn {
    fn final_text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|event| event.author != "user" && !event.llm_response.partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

async fn run_turn(runner: &Runner, text: &str, config: Option<RunConfig>) -> anyhow::Result<Turn> {
    let mut stream = runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(SESSION)?,
            Content::new("user").with_text(text),
            config,
        )
        .await?;
    let mut turn = Turn { error: None, events: Vec::new() };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => turn.events.push(event),
            Err(error) => {
                turn.error = Some(error);
                break;
            }
        }
    }
    Ok(turn)
}

fn describe(error: &Option<AdkError>) -> String {
    match error {
        Some(error) => format!("{} ({error})", error.code),
        None => "none".to_string(),
    }
}

/// PII redaction plus a content filter that blocks the word "forbidden".
fn guardrails() -> anyhow::Result<GuardrailSet> {
    Ok(GuardrailSet::new()
        .with(PiiRedactor::new())
        .with(ContentFilter::blocked_keywords(vec!["forbidden".to_string()])?))
}

/// Calls `transfer` once for a "send" request, answers a tool result or anything else with text.
async fn transfer_script() -> anyhow::Result<ScriptedServer> {
    ScriptedServer::start(Box::new(|request: &Value| {
        if last_role(request).as_deref() == Some("tool") {
            Reply::Text("the transfer was sent".to_string())
        } else if last_user_text(request).contains("send 10") {
            Reply::Call("call-1".into(), "transfer".into(), r#"{"amount":10}"#.into())
        } else {
            Reply::Text("noted".to_string())
        }
    }))
    .await
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

/// Moves money and counts its runs; when `ends_turn`, its result is the final response.
struct Transfer {
    ends_turn: bool,
    runs: Arc<Mutex<usize>>,
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "Sends money to a payee."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {"amount": {"type": "integer"}},
            "required": ["amount"],
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        *self.runs.lock().unwrap() += 1;
        if self.ends_turn {
            let mut actions = ctx.actions();
            actions.skip_summarization = true;
            ctx.set_actions(actions);
        }
        Ok(json!({"status": "sent"}))
    }
}

fn approve(call_id: &str) -> RunConfig {
    RunConfig::builder()
        .tool_confirmation_decisions(HashMap::from([(
            call_id.to_string(),
            ToolConfirmationDecision::Approve,
        )]))
        .build()
}

/// (a) Run 1's redacted message: run 2's model request never carries the raw text.
async fn redaction_persists(backend: Backend) -> anyhow::Result<Verdict> {
    let server = transfer_script().await?;
    let store = Store::open(backend).await?;
    let agent = LlmAgentBuilder::new("assistant")
        .model(scripted_model(&server)?)
        .input_guardrails(guardrails()?)
        .build()?;
    let runner = store.runner(Arc::new(agent))?;

    let first = run_turn(&runner, &format!("my email is {EMAIL}"), None).await?;
    let after_first = server.requests().len();
    let second = run_turn(&runner, "what was my email?", None).await?;
    let requests = server.requests();
    let run_two = requests[after_first..].iter().map(Value::to_string).collect::<String>();
    let saved = store.saved().await?;
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (requests.len() == 2, format!("{} model request(s)", requests.len())),
        (!run_two.contains(EMAIL), "run 2's model request does not carry the raw email".into()),
        (
            run_two.contains("[EMAIL REDACTED]"),
            "run 2's model request carries the redacted form".into(),
        ),
        (!saved.contains(EMAIL), "the session does not hold the raw email".into()),
    ]))
}

/// (b) A blocked message: absent from the session and from every later model request,
/// including an empty resume after an approval.
async fn blocked_never_saved(backend: Backend) -> anyhow::Result<Verdict> {
    let server = transfer_script().await?;
    let store = Store::open(backend).await?;
    let runs = Arc::new(Mutex::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(scripted_model(&server)?)
        .tool(Arc::new(Transfer { ends_turn: false, runs: runs.clone() }))
        .require_tool_confirmation("transfer")
        .input_guardrails(guardrails()?)
        .build()?;
    let runner = store.runner(Arc::new(agent))?;

    let held = run_turn(&runner, "send 10 to bob", None).await?;
    let blocked = run_turn(&runner, BLOCKED, None).await?;
    let saved_after_block = store.saved().await?;
    let resumed = run_turn(&runner, "", Some(approve("call-1"))).await?;
    let sent = server.sent();
    let saved = store.saved().await?;
    let ran = *runs.lock().unwrap();
    Ok(verdict(vec![
        (
            held.error.is_none(),
            format!("run 1 holds the transfer (error: {})", describe(&held.error)),
        ),
        (blocked.error.is_some(), format!("run 2 is refused: {}", describe(&blocked.error))),
        (!saved_after_block.contains("forbidden"), "the blocked message is not saved".into()),
        (resumed.error.is_none(), format!("run 3 resumes (error: {})", describe(&resumed.error))),
        (ran == 1, format!("the approved transfer ran {ran} time(s)")),
        (
            !sent.contains("forbidden"),
            format!(
                "no model request carries the blocked message ({} request(s))",
                server.requests().len()
            ),
        ),
        (!saved.contains("forbidden"), "the session never holds the blocked message".into()),
    ]))
}

/// (c) A durable worker recovers a run whose turn was saved before it was screened (by an
/// earlier release); the turn is screened before the model sees it.
async fn worker_recovery_rechecks(backend: Backend) -> anyhow::Result<Verdict> {
    let blocked = recover_unscreened(backend, BLOCKED).await?;
    let redacted = recover_unscreened(backend, &format!("my email is {EMAIL}")).await?;
    Ok(verdict(vec![
        (blocked.0 == RunState::Failed, format!("blocked turn: run {:?}", blocked.0)),
        (blocked.1.is_empty(), format!("blocked turn: {} model request(s)", blocked.1.len())),
        (redacted.0 == RunState::Completed, format!("redacted turn: run {:?}", redacted.0)),
        (
            redacted.1.len() == 1 && !redacted.1[0].contains(EMAIL),
            "redacted turn: the model request does not carry the raw email".into(),
        ),
        (
            redacted.1.first().is_some_and(|sent| sent.contains("[EMAIL REDACTED]")),
            "redacted turn: the model request carries the redacted form".into(),
        ),
    ]))
}

/// Recovers a stopped run whose saved, unscreened turn is `text`; returns how the run ended
/// and every model request.
async fn recover_unscreened(
    backend: Backend,
    text: &str,
) -> anyhow::Result<(RunState, Vec<String>)> {
    let server = transfer_script().await?;
    let store = Store::open(backend).await?;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let agent = LlmAgentBuilder::new("support")
        .model(scripted_model(&server)?)
        .input_guardrails(guardrails()?)
        .build()?;
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent))
            .session_service(store.sessions.clone())
            .durable_run_store(Arc::clone(&runs))
            .build()?,
    );
    let run = NewRun::new("default", APP, USER, "support", json!(text)).with_session_id(SESSION);
    let run = runs.enqueue(run).await?.record().cloned().expect("a new run");

    // The stopped worker saved the turn unscreened and never finished.
    let claimed = runs.claim("default", "replica-a", LEASE).await?.expect("the queued run");
    let mut turn = Event::new("inv-stopped");
    turn.author = "user".to_string();
    turn.llm_response.content = Some(Content::new("user").with_text(text));
    store.sessions.append_event(SESSION, turn).await?;
    runs.set_checkpoint(
        &run.run_id,
        claimed.epoch,
        RunCheckpointRef::Session {
            session_id: SESSION.to_string(),
            invocation_id: Some("inv-stopped".to_string()),
        },
    )
    .await?;
    tokio::time::sleep(LEASE + Duration::from_millis(50)).await;

    let worker = DurableWorker::new(Arc::clone(&runs)).with_agent("support", runner).with_config(
        DurableWorkerConfig::default()
            .with_worker_id("replica-b")
            .with_lease_ttl(LEASE)
            .with_poll_interval(Duration::from_millis(20)),
    );
    let finished = worker.run_next().await?.expect("the expired run is recovered");
    Ok((finished.state, server.requests().iter().map(Value::to_string).collect()))
}

/// (d) A nested agent's input guardrails apply before the message is saved, so the first
/// agent, which has none, never sees what the nested agent would redact or block.
async fn nested_agent_guardrails(backend: Backend) -> anyhow::Result<Verdict> {
    let server = transfer_script().await?;
    let store = Store::open(backend).await?;
    let intake_requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let sink = intake_requests.clone();
    let intake = LlmAgentBuilder::new("intake")
        .model(scripted_model(&server)?)
        .before_model_callback(Box::new(move |_ctx, request| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(request.clone());
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;
    let billing = LlmAgentBuilder::new("billing")
        .model(scripted_model(&server)?)
        .input_guardrails(guardrails()?)
        .build()?;
    let desk = SequentialAgent::new("desk", vec![Arc::new(intake), Arc::new(billing)]);
    let runner = store.runner(Arc::new(desk))?;

    let first = run_turn(&runner, &format!("refund {EMAIL}"), None).await?;
    let intake_seen = serde_json::to_string(&*intake_requests.lock().unwrap())?;
    let before_block = intake_requests.lock().unwrap().len();
    let blocked = run_turn(&runner, BLOCKED, None).await?;
    let after_block = intake_requests.lock().unwrap().len();
    let saved = store.saved().await?;
    Ok(verdict(vec![
        (first.error.is_none(), format!("run 1 error: {}", describe(&first.error))),
        (
            !intake_seen.is_empty() && !intake_seen.contains(EMAIL),
            format!("intake's {before_block} model request(s) do not carry the raw email"),
        ),
        (!server.sent().contains(EMAIL), "no model request carries the raw email".into()),
        (
            blocked.error.is_some(),
            format!("a message billing blocks is refused: {}", describe(&blocked.error)),
        ),
        (after_block == before_block, "intake makes no model call for the blocked message".into()),
        (
            !saved.contains(EMAIL) && !saved.contains("forbidden"),
            "the session holds neither".into(),
        ),
    ]))
}

/// (e) A resumed call that ends the turn, in a run that brought new text: the run answers the
/// text or returns an error, never leaves it unanswered without a word.
async fn resume_with_new_text(backend: Backend) -> anyhow::Result<Verdict> {
    let server = transfer_script().await?;
    let store = Store::open(backend).await?;
    let runs = Arc::new(Mutex::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(scripted_model(&server)?)
        .tool(Arc::new(Transfer { ends_turn: true, runs: runs.clone() }))
        .require_tool_confirmation("transfer")
        .build()?;
    let runner = store.runner(Arc::new(agent))?;

    let held = run_turn(&runner, "send 10 to bob", None).await?;
    let before = server.requests().len();
    let resumed = run_turn(&runner, "and tell me the fee", Some(approve("call-1"))).await?;
    let answered = server.requests()[before..]
        .iter()
        .any(|request| last_user_text(request).contains("tell me the fee"));
    let ran = *runs.lock().unwrap();
    Ok(verdict(vec![
        (
            held.error.is_none(),
            format!("run 1 holds the transfer (error: {})", describe(&held.error)),
        ),
        (ran == 1, format!("the approved transfer ran {ran} time(s)")),
        (
            answered || resumed.error.is_some(),
            format!(
                "the new text is answered ({answered}) or the run errors: {}",
                describe(&resumed.error)
            ),
        ),
    ]))
}

/// An API key from a dotenv file — `ADK_ENV_FILE`, else `.env` in the working directory or in
/// this example's directory — ahead of the process environment, whose exported keys may be
/// stale.
fn api_key(name: &str) -> Option<String> {
    let files = match std::env::var("ADK_ENV_FILE") {
        Ok(path) => vec![path],
        Err(_) => {
            vec![".env".to_string(), concat!(env!("CARGO_MANIFEST_DIR"), "/.env").to_string()]
        }
    };
    let from_file = files.iter().find_map(|path| {
        dotenvy::from_path_iter(path)
            .ok()
            .and_then(|items| items.flatten().find(|(key, _)| key == name).map(|(_, value)| value))
    });
    from_file.or_else(|| std::env::var(name).ok()).filter(|value| !value.trim().is_empty())
}

fn live_model() -> anyhow::Result<Option<(Arc<dyn Llm>, String)>> {
    if let Some(key) = api_key("OPENAI_API_KEY") {
        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| OPENAI_MODEL.to_string());
        let client = OpenAIClient::new(OpenAIConfig::new(key, model.clone()))?;
        return Ok(Some((Arc::new(client), model)));
    }
    if let Some(key) = api_key("GEMINI_API_KEY").or_else(|| api_key("GOOGLE_API_KEY")) {
        let model = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| GEMINI_MODEL.to_string());
        return Ok(Some((Arc::new(GeminiModel::new(key, model.clone())?), model)));
    }
    Ok(None)
}

/// A live model told a redacted email in run 1 cannot repeat it in run 2, because no request
/// carried it; the happy path still answers.
async fn live_redaction() -> anyhow::Result<Verdict> {
    let Some((model, name)) = live_model()? else {
        return Ok(Verdict::Skip("neither OPENAI_API_KEY nor GEMINI_API_KEY is set".into()));
    };
    let store = Store::open(Backend::Sqlite).await?;
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let sink = requests.clone();
    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a concise assistant. Answer in one short sentence.")
        .model(model)
        .input_guardrails(guardrails()?)
        .before_model_callback(Box::new(move |_ctx, request| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(request.clone());
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;
    let runner = store.runner(Arc::new(agent))?;

    let first =
        run_turn(&runner, &format!("My email address is {EMAIL}. Please remember it."), None)
            .await?;
    let after_first = requests.lock().unwrap().len();
    let second = run_turn(
        &runner,
        "What is my email address? Reply with only the address exactly as I gave it.",
        None,
    )
    .await?;
    let run_two = serde_json::to_string(&requests.lock().unwrap()[after_first..])?;
    let reply = second.final_text();
    let saved = store.saved().await?;
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("{name}: run errors {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (!first.final_text().is_empty(), format!("run 1 reply: {:?}", first.final_text())),
        (!run_two.contains(EMAIL), "run 2's model request does not carry the raw email".into()),
        (!reply.contains(EMAIL), format!("run 2 reply: {reply:?}")),
        (!saved.contains(EMAIL), "the SQLite session does not hold the raw email".into()),
    ]))
}

const SCENARIOS: &[&str] = &[
    "redaction_persists",
    "blocked_never_saved",
    "worker_recovery_rechecks",
    "nested_agent_guardrails",
    "resume_with_new_text",
    "live_redaction",
];

async fn run_scenario(name: &str, backend: Backend) -> anyhow::Result<Verdict> {
    match name {
        "redaction_persists" => redaction_persists(backend).await,
        "blocked_never_saved" => blocked_never_saved(backend).await,
        "worker_recovery_rechecks" => worker_recovery_rechecks(backend).await,
        "nested_agent_guardrails" => nested_agent_guardrails(backend).await,
        "resume_with_new_text" => resume_with_new_text(backend).await,
        "live_redaction" => live_redaction().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
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

    let mut selected: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    let mut cases = Vec::new();
    for name in &selected {
        if name.starts_with("live_") {
            cases.push((name.clone(), Backend::Sqlite, name.clone()));
        } else {
            for backend in [Backend::Memory, Backend::Sqlite] {
                let label = format!("{name} [{}]", format!("{backend:?}").to_lowercase());
                cases.push((name.clone(), backend, label));
            }
        }
    }

    println!("input validation persistence — {} case(s)\n", cases.len());
    let mut failures = 0;
    for (name, backend, label) in &cases {
        let result =
            tokio::time::timeout(Duration::from_secs(180), run_scenario(name, *backend)).await;
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
        println!("{status}  {label}\n      {evidence}");
    }
    println!("\n{} case(s), {failures} failed", cases.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
