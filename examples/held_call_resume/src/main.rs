//! Held tool calls across compaction, workflows, providers without call IDs, and budgets.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder` agents in `SequentialAgent`,
//! `LoopAgent`, `ParallelAgent` and `LlmConditionalAgent`, an `AgentTool`, `Runner` with
//! session compaction (`EventsCompactionConfig`, overlap 0 and 1), SQLite and in-memory session
//! services, and the durable worker — and counts every tool execution and model call. A
//! scripted upstream on 127.0.0.1 plays the model: the real `OpenAIClient` talks to its
//! Chat Completions route, and the real `OllamaModel` to its `/api/chat` route, which sends
//! tool calls without IDs as Ollama does. The `live_*` scenario runs against OpenAI.
//!
//! ```bash
//! cargo run --manifest-path examples/held_call_resume/Cargo.toml
//! cargo run --manifest-path examples/held_call_resume/Cargo.toml -- --scenario sequential_stops_at_hold
//! ```

mod scripted;

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::{
    LlmAgentBuilder, LlmConditionalAgent, LlmEventSummarizer, LoopAgent, ParallelAgent,
    SequentialAgent,
};
use adk_core::{
    AdkError, Agent, ApprovalScope, ApprovalStore, BeforeModelResult, BudgetTracker, Content,
    DurableRunStore, Event, EventsCompactionConfig, InMemoryApprovalStore, InMemoryRunStore, Llm,
    NewRun, RunBudget, RunConfig, RunState, SessionId, Tool, ToolApproval, ToolConfirmationRequest,
    ToolContext, UserId,
};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{
    CreateRequest, GetRequest, InMemorySessionService, SessionService, SqliteSessionService,
};
use adk_tool::AgentTool;
use async_trait::async_trait;
use futures::StreamExt;
use scripted::{Recorded, Reply, Script, ScriptedServer, Turn};
use serde_json::{Value, json};

const APP: &str = "held-calls";
const USER: &str = "customer";
const SESSION: &str = "session-1";
const LEASE: Duration = Duration::from_millis(300);
const OPENAI_MODEL: &str = "gpt-5.6-terra";

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

    /// The calls every agent still holds, as the agent finds them when it next runs.
    async fn held(&self) -> anyhow::Result<Vec<adk_core::HeldToolCall>> {
        let events = self.events().await?;
        let mut holders = Vec::<(String, String)>::new();
        for event in &events {
            if event.actions.tool_confirmation.is_some() {
                let holder = (event.author.clone(), event.branch.clone());
                if !holders.contains(&holder) {
                    holders.push(holder);
                }
            }
        }
        Ok(holders
            .iter()
            .flat_map(|(agent, branch)| adk_core::held_tool_calls(&events, agent, branch))
            .collect())
    }

    /// Every confirmation request the session recorded, oldest first, compacted or not.
    async fn requests(&self) -> anyhow::Result<Vec<ToolConfirmationRequest>> {
        Ok(self
            .events()
            .await?
            .into_iter()
            .filter_map(|event| event.actions.tool_confirmation)
            .collect())
    }

    async fn runner(
        &self,
        agent: Arc<dyn Agent>,
        compaction: Option<(u32, &ScriptedServer)>,
    ) -> anyhow::Result<Runner> {
        let builder =
            Runner::builder().app_name(APP).agent(agent).session_service(self.sessions.clone());
        Ok(match compaction {
            Some((overlap, server)) => {
                builder.compaction_config(compaction_config(overlap, server)?)
            }
            None => builder,
        }
        .build()?)
    }
}

/// Compacts after every run, keeping `overlap` runs uncompacted.
fn compaction_config(
    overlap: u32,
    server: &ScriptedServer,
) -> anyhow::Result<EventsCompactionConfig> {
    Ok(EventsCompactionConfig {
        compaction_interval: 1,
        overlap_size: overlap,
        summarizer: Arc::new(LlmEventSummarizer::new(openai_model(server)?)),
    })
}

struct Run {
    error: Option<AdkError>,
    events: Vec<Event>,
}

impl Run {
    fn final_text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|event| event.author != "user" && !event.llm_response.partial)
            .filter(|event| event.actions.tool_confirmation.is_none())
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| part.text())
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

async fn run(runner: &Runner, text: &str, config: Option<RunConfig>) -> anyhow::Result<Run> {
    let mut stream = runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(SESSION)?,
            Content::new("user").with_text(text),
            config,
        )
        .await?;
    let mut run = Run { error: None, events: Vec::new() };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => run.events.push(event),
            Err(error) => {
                run.error = Some(error);
                break;
            }
        }
    }
    Ok(run)
}

fn describe(error: &Option<AdkError>) -> String {
    match error {
        Some(error) => format!("{} ({error})", error.code),
        None => "none".to_string(),
    }
}

/// A run config approving the calls `requests` describe.
fn approving<'a>(requests: impl IntoIterator<Item = &'a ToolConfirmationRequest>) -> RunConfig {
    requests
        .into_iter()
        .fold(RunConfig::builder(), |builder, request| {
            builder.tool_approval(request.fingerprint(), ToolApproval::approve())
        })
        .build()
}

/// The `transfer` amounts held calls carry, as the agent would run them.
fn held_amounts(held: &[adk_core::HeldToolCall]) -> Vec<Value> {
    held.iter().map(|call| call.args["amount"].clone()).collect()
}

/// Tool call IDs answered more than once, or answered without a call, in one request.
fn malformed_tool_results(request: &Recorded) -> Vec<String> {
    let messages = request.body["messages"].as_array().cloned().unwrap_or_default();
    let mut called = HashSet::new();
    let mut answered = HashMap::<String, usize>::new();
    for message in &messages {
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            if let Some(id) = call["id"].as_str() {
                called.insert(id.to_string());
            }
        }
        if let Some(id) = message["tool_call_id"].as_str() {
            *answered.entry(id.to_string()).or_default() += 1;
        }
    }
    answered
        .into_iter()
        .filter(|(id, count)| *count > 1 || !called.contains(id))
        .map(|(id, count)| format!("{id} x{count}"))
        .collect()
}

fn all_well_formed(server: &ScriptedServer) -> (bool, String) {
    let bad: Vec<String> = server
        .requests()
        .iter()
        .filter(|request| request.body.get("messages").is_some() && request.agent != "summarizer")
        .flat_map(|request| {
            malformed_tool_results(request)
                .into_iter()
                .map(|id| format!("{}: {id}", request.agent))
                .collect::<Vec<_>>()
        })
        .collect();
    (bad.is_empty(), format!("every model request answers each tool call once (bad: {bad:?})"))
}

/// The model's side of every scripted agent.
fn script() -> Script {
    Box::new(|turn: &Turn| {
        let send = turn.last_user_text.contains("send");
        let after_tool = turn.last_role == "tool";
        match turn.agent.as_str() {
            "summarizer" => Reply::Text("Summary: the customer asked for a transfer.".into()),
            "classifier" => Reply::Text(if send { "payments" } else { "general" }.into()),
            "ops" | "helper" if after_tool => Reply::Text("transfer handled".into()),
            "ops" | "helper" if send => {
                Reply::Calls(vec![("transfer".into(), json!({"amount": 10}))])
            }
            "ops" | "helper" => Reply::Text("waiting for the transfer approval".into()),
            "pair" if after_tool => Reply::Text("transfers handled".into()),
            "pair" if send => Reply::Calls(vec![
                ("transfer".into(), json!({"amount": 10})),
                ("transfer".into(), json!({"amount": 20})),
            ]),
            "pair" => Reply::Text("waiting".into()),
            "desk" if after_tool => Reply::Text("the helper finished".into()),
            "desk" => Reply::Calls(vec![("helper".into(), json!({"request": "send 10"}))]),
            "looper" if after_tool => Reply::Text("transfer handled".into()),
            "looper" if turn.fresh == 2 => {
                Reply::Calls(vec![("transfer".into(), json!({"amount": 10}))])
            }
            "looper" => Reply::Text(format!("round {}", turn.fresh)),
            other => Reply::Text(format!("{other} done")),
        }
    })
}

fn openai_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.openai_base, "scripted-model")?))
}

fn ollama_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OllamaModel::new(OllamaConfig::with_host(&server.ollama_host, "scripted-model"))?))
}

/// Moves money; records the arguments of every execution.
struct Transfer {
    runs: Arc<Mutex<Vec<Value>>>,
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

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.runs.lock().unwrap().push(args);
        Ok(json!({"status": "sent"}))
    }
}

type Runs = Arc<Mutex<Vec<Value>>>;

fn runs() -> Runs {
    Arc::new(Mutex::new(Vec::new()))
}

fn ran(runs: &Runs) -> Vec<Value> {
    runs.lock().unwrap().clone()
}

/// An agent whose `transfer` calls wait for approval.
fn holder(name: &str, model: Arc<dyn Llm>, runs: &Runs) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new(name)
            .instruction(format!("You are {name}."))
            .model(model)
            .tool(Arc::new(Transfer { runs: runs.clone() }))
            .require_tool_confirmation("transfer")
            .build()?,
    ))
}

/// An agent that answers with text.
fn talker(name: &str, model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new(name).instruction(format!("You are {name}.")).model(model).build()?,
    ))
}

/// (a) Compaction after every run, overlap 0: the held call stays held and resumes once.
async fn compaction_overlap_0(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let runner = store
        .runner(holder("ops", openai_model(&server)?, &transfers)?, Some((0, &server)))
        .await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let held_after_hold = store.held().await?.len();
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let held_after_resume = store.held().await?.len();
    let resumed_answered = server
        .requests()
        .iter()
        .any(|request| request.agent == "ops" && request.last_role == "tool");
    let (well_formed, shape) = all_well_formed(&server);
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (held_after_hold == 1, format!("{held_after_hold} call(s) held after the compacted run")),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (resumed_answered, "the model is called with the resumed result".into()),
        (held_after_resume == 0, format!("{held_after_resume} call(s) held after the resume")),
        (well_formed, shape),
    ]))
}

/// (a) Overlap 1: a second message while the call waits compacts the first run; the call stays
/// held, and the approval given in a third run resumes it once.
async fn compaction_overlap_1(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let runner = store
        .runner(holder("ops", openai_model(&server)?, &transfers)?, Some((1, &server)))
        .await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let second = run(&runner, "what is the status?", None).await?;
    let held_after_second = store.held().await?.len();
    let compactions =
        store.events().await?.iter().filter(|event| event.actions.compaction.is_some()).count();
    let requests = store.requests().await?;
    let third = run(&runner, "", Some(approving(&requests))).await?;
    let held_after_resume = store.held().await?.len();
    let (well_formed, shape) = all_well_formed(&server);
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none() && third.error.is_none(),
            format!(
                "run errors: {} / {} / {}",
                describe(&first.error),
                describe(&second.error),
                describe(&third.error)
            ),
        ),
        (compactions >= 1, format!("{compactions} compaction(s) before the approval")),
        (
            held_after_second == 1,
            format!("{held_after_second} call(s) held after the second message"),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (held_after_resume == 0, format!("{held_after_resume} call(s) held after the resume")),
        (well_formed, shape),
    ]))
}

/// (a) A durable run that holds a call parks as `Waiting` although compaction ran after it,
/// and the approval wakes it to run the call once.
async fn worker_parks_after_compaction(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let runs_store: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(holder("ops", openai_model(&server)?, &transfers)?)
            .session_service(store.sessions.clone())
            .durable_run_store(Arc::clone(&runs_store))
            .approval_store(approvals.clone())
            .compaction_config(compaction_config(0, &server)?)
            .build()?,
    );
    let new_run =
        NewRun::new("default", APP, USER, "ops", json!("send 10 to bob")).with_session_id(SESSION);
    let record = runs_store.enqueue(new_run).await?.record().cloned().expect("a new run");
    let worker = DurableWorker::new(Arc::clone(&runs_store)).with_agent("ops", runner).with_config(
        DurableWorkerConfig::default()
            .with_worker_id("replica-a")
            .with_lease_ttl(LEASE)
            .with_poll_interval(Duration::from_millis(20)),
    );

    let first = worker.run_next().await?.expect("the queued run");
    let scope = ApprovalScope::new(APP, USER, SESSION);
    let pending = approvals.pending(&scope).await?;
    let mut checks = vec![
        (first.state == RunState::Waiting, format!("the attempt ends {:?}", first.state)),
        (pending.len() == 1, format!("{} approval(s) pending", pending.len())),
    ];
    if let Some(entry) = pending.first() {
        approvals.decide(&scope, &entry.fingerprint, ToolApproval::approve()).await?;
        runs_store.wake(&record.run_id).await?;
        let second = worker.run_next().await?;
        let state = second.as_ref().map(|record| record.state);
        checks.push((state == Some(RunState::Completed), format!("the woken run ends {state:?}")));
    }
    checks.push((
        ran(&transfers).len() == 1,
        format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
    ));
    Ok(verdict(checks))
}

/// Model calls per agent, from the scripted upstream.
fn calls(server: &ScriptedServer, agents: &[&str]) -> String {
    agents
        .iter()
        .map(|agent| format!("{agent}={}", server.calls(agent)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// (b) `SequentialAgent[intake, ops, report]`: the workflow stops at ops's hold, and the resume
/// starts at ops — intake never runs again, report runs once.
async fn sequential_stops_at_hold(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let model = openai_model(&server)?;
    let pipeline = SequentialAgent::new(
        "pipeline",
        vec![
            talker("intake", model.clone())?,
            holder("ops", model.clone(), &transfers)?,
            talker("report", model)?,
        ],
    );
    let runner = store.runner(Arc::new(pipeline), None).await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let after_hold = calls(&server, &["intake", "ops", "report"]);
    let report_in_hold = server.calls("report");
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let (well_formed, shape) = all_well_formed(&server);
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (report_in_hold == 0, format!("after the hold: {after_hold}")),
        (
            server.calls("intake") == 1 && server.calls("ops") == 2 && server.calls("report") == 1,
            format!("after the resume: {}", calls(&server, &["intake", "ops", "report"])),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (second.final_text() == "report done", format!("final reply: {:?}", second.final_text())),
        (well_formed, shape),
    ]))
}

/// (b) `LoopAgent[counter, looper]` × 3, where looper holds in round 2: the loop stops there,
/// resumes in round 2, and runs round 3 — three rounds in all.
async fn loop_keeps_its_iteration(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let model = openai_model(&server)?;
    let rounds = LoopAgent::new(
        "rounds",
        vec![talker("counter", model.clone())?, holder("looper", model, &transfers)?],
    )
    .with_max_iterations(3);
    let runner = store.runner(Arc::new(rounds), None).await?;

    let first = run(&runner, "go", None).await?;
    let after_hold = calls(&server, &["counter", "looper"]);
    let counter_in_hold = server.calls("counter");
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let (well_formed, shape) = all_well_formed(&server);
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (counter_in_hold == 2, format!("after the hold in round 2: {after_hold}")),
        (
            server.calls("counter") == 3 && server.calls("looper") == 4,
            format!(
                "after the resume (3 rounds, one resumed call): {}",
                calls(&server, &["counter", "looper"])
            ),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (well_formed, shape),
    ]))
}

/// (b) `SequentialAgent[ParallelAgent[fast, ops], report]`: fast finishes and is not run again,
/// ops resumes, and report runs once after both.
async fn parallel_resumes_held_branch(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let model = openai_model(&server)?;
    let both = ParallelAgent::new(
        "both",
        vec![talker("fast", model.clone())?, holder("ops", model.clone(), &transfers)?],
    );
    let fanout = SequentialAgent::new("fanout", vec![Arc::new(both), talker("report", model)?]);
    let runner = store.runner(Arc::new(fanout), None).await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let after_hold = calls(&server, &["fast", "ops", "report"]);
    let report_in_hold = server.calls("report");
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let (well_formed, shape) = all_well_formed(&server);
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (report_in_hold == 0, format!("after the hold: {after_hold}")),
        (
            server.calls("fast") == 1 && server.calls("ops") == 2 && server.calls("report") == 1,
            format!("after the resume: {}", calls(&server, &["fast", "ops", "report"])),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (well_formed, shape),
    ]))
}

/// (b) `LlmConditionalAgent` routes "send 10" to ops, which holds: the resume, an empty
/// message, goes back to ops without classifying again.
async fn router_keeps_its_route(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let model = openai_model(&server)?;
    let router = LlmConditionalAgent::builder("router", model.clone())
        .instruction("Classify the request as 'payments' or 'general'.")
        .route("payments", holder("ops", model.clone(), &transfers)?)
        .route("general", talker("intake", model)?)
        .build()?;
    let runner = store.runner(Arc::new(router), None).await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (
            server.calls("classifier") == 1 && server.calls("intake") == 0,
            format!("model calls: {}", calls(&server, &["classifier", "ops", "intake"])),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
    ]))
}

/// (c) Ollama sends two `transfer` calls ({10}, {20}) without IDs: each held call keeps its
/// own arguments, and approving the one for 10 runs exactly that one.
async fn idless_calls_keep_arguments(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let runner = store.runner(holder("pair", ollama_model(&server)?, &transfers)?, None).await?;

    let first = run(&runner, "send 10 and 20", None).await?;
    let held = store.held().await?;
    let requests = store.requests().await?;
    let ten: Vec<_> = requests.iter().filter(|request| request.args["amount"] == 10).collect();
    let second = run(&runner, "", Some(approving(ten.iter().copied()))).await?;
    let still_held = store.held().await?;
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (
            held_amounts(&held) == vec![json!(10), json!(20)],
            format!("held calls run with amounts {:?}", held_amounts(&held)),
        ),
        (
            ran(&transfers) == vec![json!({"amount": 10})],
            format!("executed: {:?}", ran(&transfers)),
        ),
        (
            held_amounts(&still_held) == vec![json!(20)],
            format!("still held after the approval: {:?}", held_amounts(&still_held)),
        ),
    ]))
}

/// (c) An Ollama agent delegates through an `AgentTool` call without an ID, and the delegate
/// holds a call: the approval resumes the delegation and runs the delegate's call once.
async fn idless_agent_tool_resumes(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let helper = holder("helper", openai_model(&server)?, &transfers)?;
    let desk = LlmAgentBuilder::new("desk")
        .instruction("You are desk.")
        .model(ollama_model(&server)?)
        .tool(Arc::new(AgentTool::new(helper)))
        .build()?;
    let runner = store.runner(Arc::new(desk), None).await?;

    let first = run(&runner, "send 10 to bob", None).await?;
    let held = store.held().await?;
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let desk_answer = second.final_text();
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (
            held.len() == 1 && held[0].name == "helper",
            format!(
                "held: {:?}",
                held.iter()
                    .map(|call| format!("{} for {}", call.name, call.request.tool_name))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            ran(&transfers).len() == 1,
            format!("the delegate's transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (desk_answer == "the helper finished", format!("desk's reply: {desk_answer:?}")),
    ]))
}

/// (d) One budget tracker across the hold and the resume, at most one tool call: the
/// held-then-resumed call counts once, so it fits.
async fn budget_counts_held_call_once(backend: Backend) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(script()).await?;
    let store = Store::open(backend).await?;
    let transfers = runs();
    let runner = store.runner(holder("ops", openai_model(&server)?, &transfers)?, None).await?;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_tool_calls(1)));
    let config = RunConfig { budget_tracker: Some(tracker.clone()), ..Default::default() };

    let first = run(&runner, "send 10 to bob", Some(config)).await?;
    let used_by_hold = tracker.usage().tool_calls;
    let requests = store.requests().await?;
    let mut resume = approving(&requests);
    resume.budget_tracker = Some(tracker.clone());
    let second = run(&runner, "", Some(resume)).await?;
    let used = tracker.usage().tool_calls;
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!("run errors: {} / {}", describe(&first.error), describe(&second.error)),
        ),
        (used_by_hold == 0, format!("{used_by_hold} tool call(s) counted for the held call")),
        (
            ran(&transfers).len() == 1,
            format!("the approved transfer ran {} time(s)", ran(&transfers).len()),
        ),
        (used == 1, format!("{used} tool call(s) counted in all")),
    ]))
}

/// Loads keys from the dotenv file named by `ADK_ENV_FILE`, else this example's `.env`, else
/// the repository's `.env`, over the process environment, whose exported keys may be stale.
fn load_env() {
    let candidates = match std::env::var("ADK_ENV_FILE") {
        Ok(path) => vec![path],
        Err(_) => vec![
            concat!(env!("CARGO_MANIFEST_DIR"), "/.env").to_string(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string(),
        ],
    };
    if let Some(path) = candidates.iter().find(|path| std::path::Path::new(path).exists()) {
        let _ = dotenvy::from_path_override(path);
    }
}

/// Counts model requests per agent.
fn counting(
    name: &str,
    builder: LlmAgentBuilder,
    counts: &Arc<Mutex<HashMap<String, usize>>>,
) -> LlmAgentBuilder {
    let counts = counts.clone();
    let name = name.to_string();
    builder.before_model_callback(Box::new(move |_ctx, request| {
        let counts = counts.clone();
        let name = name.clone();
        Box::pin(async move {
            *counts.lock().unwrap().entry(name).or_default() += 1;
            Ok(BeforeModelResult::Continue(request))
        })
    }))
}

/// A live model holds a transfer in `SequentialAgent[ops, report]` with compaction on; the
/// approval resumes it once and report runs once.
async fn live_workflow_resume() -> anyhow::Result<Verdict> {
    load_env();
    let Some(key) = std::env::var("OPENAI_API_KEY").ok().filter(|key| !key.trim().is_empty())
    else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| OPENAI_MODEL.to_string());
    let model: Arc<dyn Llm> =
        Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model_name.clone()))?);
    let store = Store::open(Backend::Sqlite).await?;
    let transfers = runs();
    let counts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let ops = counting(
        "ops",
        LlmAgentBuilder::new("ops")
            .instruction(
                "You move money. When asked to send money, call the transfer tool exactly once \
                 with the amount, then confirm in one short sentence.",
            )
            .model(model.clone())
            .tool(Arc::new(Transfer { runs: transfers.clone() }))
            .require_tool_confirmation("transfer"),
        &counts,
    )
    .build()?;
    let report = counting(
        "report",
        LlmAgentBuilder::new("report")
            .instruction("Summarize what happened in the conversation in one short sentence.")
            .model(model.clone()),
        &counts,
    )
    .build()?;
    let desk = SequentialAgent::new("desk", vec![Arc::new(ops), Arc::new(report)]);
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(desk))
        .session_service(store.sessions.clone())
        .compaction_config(EventsCompactionConfig {
            compaction_interval: 1,
            overlap_size: 0,
            summarizer: Arc::new(LlmEventSummarizer::new(model)),
        })
        .build()?;

    let first = run(&runner, "Please send 10 dollars to Bob.", None).await?;
    let held = store.held().await?;
    let report_in_hold = counts.lock().unwrap().get("report").copied().unwrap_or_default();
    let requests = store.requests().await?;
    let second = run(&runner, "", Some(approving(&requests))).await?;
    let counted = counts.lock().unwrap().clone();
    let reply = second.final_text();
    Ok(verdict(vec![
        (
            first.error.is_none() && second.error.is_none(),
            format!(
                "{model_name}: run errors {} / {}",
                describe(&first.error),
                describe(&second.error)
            ),
        ),
        (
            held.len() == 1 && report_in_hold == 0,
            format!(
                "after the hold: {} held call(s), report ran {report_in_hold} time(s)",
                held.len()
            ),
        ),
        (
            ran(&transfers).len() == 1,
            format!(
                "the approved transfer ran {} time(s): {:?}",
                ran(&transfers).len(),
                ran(&transfers)
            ),
        ),
        (counted.get("report").copied() == Some(1), format!("model calls per agent: {counted:?}")),
        (!reply.is_empty(), format!("final reply: {reply:?}")),
    ]))
}

const SCENARIOS: &[&str] = &[
    "compaction_overlap_0",
    "compaction_overlap_1",
    "worker_parks_after_compaction",
    "sequential_stops_at_hold",
    "loop_keeps_its_iteration",
    "parallel_resumes_held_branch",
    "router_keeps_its_route",
    "idless_calls_keep_arguments",
    "idless_agent_tool_resumes",
    "budget_counts_held_call_once",
    "live_workflow_resume",
];

async fn run_scenario(name: &str, backend: Backend) -> anyhow::Result<Verdict> {
    match name {
        "compaction_overlap_0" => compaction_overlap_0(backend).await,
        "compaction_overlap_1" => compaction_overlap_1(backend).await,
        "worker_parks_after_compaction" => worker_parks_after_compaction(backend).await,
        "sequential_stops_at_hold" => sequential_stops_at_hold(backend).await,
        "loop_keeps_its_iteration" => loop_keeps_its_iteration(backend).await,
        "parallel_resumes_held_branch" => parallel_resumes_held_branch(backend).await,
        "router_keeps_its_route" => router_keeps_its_route(backend).await,
        "idless_calls_keep_arguments" => idless_calls_keep_arguments(backend).await,
        "idless_agent_tool_resumes" => idless_agent_tool_resumes(backend).await,
        "budget_counts_held_call_once" => budget_counts_held_call_once(backend).await,
        "live_workflow_resume" => live_workflow_resume().await,
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

    println!("held call resume — {} case(s)\n", cases.len());
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
