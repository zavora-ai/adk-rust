//! What an agent behind an `AgentTool` inherits from its caller, what it hands back, and how
//! its timeouts and held calls reach the caller.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder`, `AgentTool`, `Runner`,
//! `InMemorySessionService`, and `OpenAIClient` — and asserts on what reached each model, what
//! the session recorded, and how often each tool ran. Deterministic scenarios point the real
//! client at a scripted OpenAI-compatible server on 127.0.0.1, one per agent. The live scenarios
//! run when `OPENAI_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/agent_tool_isolation/Cargo.toml
//! cargo run --manifest-path examples/agent_tool_isolation/Cargo.toml -- --scenario nested_confirmation
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, ApprovalScope, ApprovalStore, BeforeModelResult,
    CallbackContext, Content, ErrorCategory, Event, InMemoryActionLedger, InMemoryApprovalStore,
    Llm, LlmRequest, Part, RunConfig, Tool, ToolApproval, ToolConfirmationDecision,
    SessionId, ToolConfirmationRequest, ToolContext, ToolEffect, UserId,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::{AgentTool, BasicToolset, FunctionTool};
use async_trait::async_trait;
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, call, tool_message};
use serde_json::{Value, json};

const APP: &str = "agent-tool-isolation";
const USER: &str = "user";
const SESSION: &str = "session-1";
const COORDINATOR_CACHE: &str = "cachedContents/coordinator-cache";
const LIVE_MODEL: &str = "gpt-5.6-terra";

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

struct RunOutcome {
    error: Option<String>,
    events: Vec<Event>,
}

impl RunOutcome {
    fn confirmation_requests(&self) -> Vec<ToolConfirmationRequest> {
        self.events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
    }

    fn final_text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
}

impl Harness {
    async fn new(
        root: LlmAgent,
        run_config: RunConfig,
        store: Option<Arc<InMemoryApprovalStore>>,
    ) -> anyhow::Result<Self> {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await?;
        let mut builder = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(root))
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .run_config(run_config);
        if let Some(store) = store {
            builder = builder.approval_store(store as Arc<dyn ApprovalStore>);
        }
        Ok(Self { runner: builder.build()?, sessions })
    }

    /// Runs one turn; `None` text sends the empty turn that resumes held calls.
    async fn run(&self, text: Option<&str>, config: Option<RunConfig>) -> anyhow::Result<RunOutcome> {
        self.run_paced(text, config, |_| Duration::ZERO).await
    }

    /// Runs one turn, pausing after the `n`th event for `pause(n)`, like a slow client.
    async fn run_paced(
        &self,
        text: Option<&str>,
        config: Option<RunConfig>,
        pause: impl Fn(usize) -> Duration,
    ) -> anyhow::Result<RunOutcome> {
        let content = match text {
            Some(text) => Content::new("user").with_text(text),
            None => Content::new("user"),
        };
        let mut stream = self
            .runner
            .run_with_config(UserId::new(USER)?, SessionId::new(SESSION)?, content, config)
            .await?;
        let mut events = Vec::new();
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(err) => {
                    error = Some(err.to_string());
                    break;
                }
            }
            let delay = pause(events.len());
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        Ok(RunOutcome { error, events })
    }

    async fn session_events(&self) -> anyhow::Result<Vec<Event>> {
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
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

fn live_model() -> anyhow::Result<Option<Arc<dyn Llm>>> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else { return Ok(None) };
    if key.trim().is_empty() {
        return Ok(None);
    }
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| LIVE_MODEL.to_string());
    Ok(Some(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?)))
}

type RequestSink = Arc<Mutex<Vec<LlmRequest>>>;

/// Records every request the agent sends its model, after the agent assembled it.
fn capture_requests(builder: LlmAgentBuilder, sink: RequestSink) -> LlmAgentBuilder {
    builder.before_model_callback(Box::new(move |_ctx, request| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().unwrap().push(request.clone());
            Ok(BeforeModelResult::Continue(request))
        })
    }))
}

/// A specialist that delegates `request` to the helper once, then reports the helper's result.
fn ask_helper_then_finish(call_id: &'static str, request: &'static str) -> scripted::Script {
    Box::new(move |body, _| match tool_message(body, call_id) {
        Some(result) if !result.contains("confirmation_pending") => {
            Reply::Text(format!("specialist saw: {result}"))
        }
        _ => call(call_id, "helper", &json!({ "request": request }).to_string()),
    })
}

fn counting_tool(name: &str, description: &str, runs: Arc<AtomicUsize>) -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(name, description, move |_ctx: Arc<dyn ToolContext>, args: Value| {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "done": true, "args": args }))
            }
        })
        .with_effect(ToolEffect::NonIdempotent),
    )
}

// ---------------------------------------------------------------------------
// Scenario: the delegate does not inherit the caller's topology, cache or toolsets
// ---------------------------------------------------------------------------

async fn child_topology() -> anyhow::Result<Verdict> {
    let coordinator_server = ScriptedServer::start(Box::new(|_, _| {
        call("call_transfer", "transfer_to_agent", r#"{"agent_name":"specialist"}"#)
    }))
    .await?;
    let specialist_server =
        ScriptedServer::start(ask_helper_then_finish("call_helper", "summarise the report"))
            .await?;
    let helper_server =
        ScriptedServer::start(Box::new(|_, _| Reply::Text("the report is short".into()))).await?;

    let helper_requests = RequestSink::default();
    let helper = capture_requests(
        LlmAgentBuilder::new("helper")
            .description("Summarises documents")
            .instruction("Summarise what you are given in one sentence.")
            .model(scripted_model(&helper_server)?),
        helper_requests.clone(),
    )
    .build()?;
    let specialist_requests = RequestSink::default();
    let specialist = capture_requests(
        LlmAgentBuilder::new("specialist")
            .description("Handles report work")
            .instruction("Use the helper tool for summaries.")
            .model(scripted_model(&specialist_server)?)
            .tool(Arc::new(AgentTool::new(Arc::new(helper)))),
        specialist_requests.clone(),
    )
    .build()?;
    let peer = LlmAgentBuilder::new("peer")
        .description("Handles everything else")
        .model(scripted_model(&helper_server)?)
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Routes requests")
        .instruction("Route every request to the right agent.")
        .model(scripted_model(&coordinator_server)?)
        .sub_agent(Arc::new(specialist))
        .sub_agent(Arc::new(peer))
        .build()?;

    let client_files = counting_tool("client_files", "Lists the client's files", Arc::default());
    let run_config = RunConfig::builder()
        .cached_content(COORDINATOR_CACHE)
        .runtime_toolset(Arc::new(BasicToolset::new("client", vec![client_files])))
        .build();
    let harness = Harness::new(coordinator, run_config, None).await?;
    let outcome = harness.run(Some("Summarise the report"), None).await?;

    let helper_seen = helper_requests.lock().unwrap().clone();
    let specialist_seen = specialist_requests.lock().unwrap().clone();
    let tool_names = |requests: &[LlmRequest]| {
        let mut names: Vec<String> =
            requests.iter().flat_map(|request| request.tools.keys().cloned()).collect();
        names.sort();
        names.dedup();
        names
    };
    let cache_handles = |requests: &[LlmRequest]| {
        requests
            .iter()
            .filter_map(|request| request.config.as_ref()?.cached_content.clone())
            .collect::<Vec<_>>()
    };
    let helper_tools = tool_names(&helper_seen);
    let specialist_tools = tool_names(&specialist_seen);
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (!helper_seen.is_empty(), format!("helper model called {} time(s)", helper_seen.len())),
        (
            !helper_tools.iter().any(|name| name == "transfer_to_agent"),
            format!("helper request tools: {helper_tools:?}"),
        ),
        (
            !helper_tools.iter().any(|name| name == "client_files"),
            "helper request has no invocation toolset of its caller".into(),
        ),
        (
            cache_handles(&helper_seen).is_empty(),
            format!("helper request cache handles: {:?}", cache_handles(&helper_seen)),
        ),
        (
            specialist_tools.iter().any(|name| name == "transfer_to_agent")
                && specialist_tools.iter().any(|name| name == "client_files"),
            format!("specialist request tools (its own): {specialist_tools:?}"),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Scenario: a delegate that ends on a tool result hands that result back
// ---------------------------------------------------------------------------

async fn skip_summarization_payload() -> anyhow::Result<Verdict> {
    let specialist_server =
        ScriptedServer::start(ask_helper_then_finish("call_helper", "compute the answer")).await?;
    let helper_server =
        ScriptedServer::start(Box::new(|_, _| call("call_compute", "compute", "{}"))).await?;
    let compute = Arc::new(FunctionTool::new(
        "compute",
        "Computes the answer; its result is the final answer",
        |ctx: Arc<dyn ToolContext>, _args: Value| async move {
            let mut actions = ctx.actions();
            actions.skip_summarization = true;
            ctx.set_actions(actions);
            Ok(json!({ "answer": 42 }))
        },
    ));
    let helper = LlmAgentBuilder::new("helper")
        .description("Computes answers")
        .model(scripted_model(&helper_server)?)
        .tool(compute)
        .build()?;
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Answers questions")
        .model(scripted_model(&specialist_server)?)
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .build()?;
    let harness = Harness::new(specialist, RunConfig::default(), None).await?;
    let outcome = harness.run(Some("What is the answer?"), None).await?;

    let delegated = specialist_server
        .requests()
        .iter()
        .find_map(|request| tool_message(request, "call_helper"))
        .unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            delegated.contains("\"answer\":42") && !delegated.contains("No response from agent"),
            format!("specialist received from helper: {delegated}"),
        ),
    ]))
}

async fn nested_skip_summarization() -> anyhow::Result<Verdict> {
    let specialist_server =
        ScriptedServer::start(ask_helper_then_finish("call_helper", "find the figure")).await?;
    let helper_server = ScriptedServer::start(Box::new(|_, _| {
        call("call_inner", "inner", r#"{"request":"look the figure up"}"#)
    }))
    .await?;
    let inner_server =
        ScriptedServer::start(Box::new(|_, _| Reply::Text("the figure is 17".into()))).await?;
    let inner = LlmAgentBuilder::new("inner")
        .description("Looks figures up")
        .model(scripted_model(&inner_server)?)
        .build()?;
    let helper = LlmAgentBuilder::new("helper")
        .description("Finds figures")
        .model(scripted_model(&helper_server)?)
        .tool(Arc::new(AgentTool::new(Arc::new(inner)).skip_summarization(true)))
        .build()?;
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Answers questions")
        .model(scripted_model(&specialist_server)?)
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .build()?;
    let harness = Harness::new(specialist, RunConfig::default(), None).await?;
    let outcome = harness.run(Some("What is the figure?"), None).await?;

    let delegated = specialist_server
        .requests()
        .iter()
        .find_map(|request| tool_message(request, "call_helper"))
        .unwrap_or_default();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            delegated.contains("the figure is 17"),
            format!("specialist received from helper: {delegated}"),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Scenario: a delegation that runs out of time is a failed call
// ---------------------------------------------------------------------------

/// Records every outcome the agent writes to the ledger.
#[derive(Debug, Default)]
struct RecordingLedger {
    inner: InMemoryActionLedger,
    completed: Mutex<Vec<(String, ActionOutcome)>>,
}

#[async_trait]
impl ActionLedger for RecordingLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        self.completed.lock().unwrap().push((key.to_string(), outcome.clone()));
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

#[derive(Debug, Clone)]
struct ToolErrorSeen {
    tool: String,
    message: String,
    category: Option<ErrorCategory>,
}

async fn helper_timeout(propagate: bool) -> anyhow::Result<Verdict> {
    let specialist_server =
        ScriptedServer::start(ask_helper_then_finish("call_helper", "look the order up")).await?;
    let helper_server =
        ScriptedServer::start(Box::new(|_, _| call("call_slow", "slow_lookup", "{}"))).await?;
    let slow_lookup = Arc::new(FunctionTool::new(
        "slow_lookup",
        "Looks an order up slowly",
        |_ctx: Arc<dyn ToolContext>, _args: Value| async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(json!({ "order": "found" }))
        },
    ));
    let helper = LlmAgentBuilder::new("helper")
        .description("Looks orders up")
        .model(scripted_model(&helper_server)?)
        .tool(slow_lookup)
        .build()?;
    let tool_errors = Arc::new(Mutex::new(Vec::<ToolErrorSeen>::new()));
    let seen = tool_errors.clone();
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Answers order questions")
        .model(scripted_model(&specialist_server)?)
        .tool(Arc::new(
            AgentTool::new(Arc::new(helper))
                .timeout(Duration::from_millis(50))
                .propagate_failures(propagate),
        ))
        .on_tool_error(Box::new(
            move |ctx: Arc<dyn CallbackContext>, tool: Arc<dyn Tool>, _args: Value, message: String| {
            let seen = seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(ToolErrorSeen {
                    tool: tool.name().to_string(),
                    message,
                    category: ctx
                        .tool_outcome()
                        .and_then(|outcome| outcome.error.map(|error| error.category)),
                });
                Ok(None)
            })
        },
        ))
        .build()?;
    let ledger = Arc::new(RecordingLedger::default());
    let run_config =
        RunConfig::builder().action_ledger(ledger.clone() as Arc<dyn ActionLedger>).build();
    let harness = Harness::new(specialist, run_config, None).await?;
    let outcome = harness.run(Some("Where is order 7?"), None).await?;

    let errors = tool_errors.lock().unwrap().clone();
    let completed = ledger.completed.lock().unwrap().clone();
    let helper_outcomes: Vec<_> = completed
        .iter()
        .filter(|(key, _)| key.ends_with("/call_helper"))
        .map(|(_, outcome)| outcome.clone())
        .collect();
    let delegated = specialist_server
        .requests()
        .iter()
        .find_map(|request| tool_message(request, "call_helper"))
        .unwrap_or_default();
    let mut checks = vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            errors.len() == 1 && errors[0].tool == "helper",
            format!("on_tool_error calls: {}", errors.len()),
        ),
        (
            errors.first().is_some_and(|error| error.message.contains("50ms")),
            format!(
                "error message: {:?}",
                errors.first().map(|error| error.message.clone()).unwrap_or_default()
            ),
        ),
        (
            !helper_outcomes
                .iter()
                .any(|outcome| matches!(outcome, ActionOutcome::Succeeded { .. })),
            format!("ledger outcomes for the helper call: {helper_outcomes:?}"),
        ),
        (
            delegated.contains("error") || delegated.contains("outcome_unknown"),
            format!("specialist received from helper: {delegated}"),
        ),
    ];
    if propagate {
        checks.push((
            errors.first().and_then(|error| error.category) == Some(ErrorCategory::Timeout),
            format!(
                "propagated error category: {:?}",
                errors.first().and_then(|error| error.category)
            ),
        ));
    }
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// Scenario: a call the delegate holds for confirmation is held by its caller
// ---------------------------------------------------------------------------

struct ConfirmationTree {
    harness: Harness,
    store: Arc<InMemoryApprovalStore>,
    deletes: Arc<AtomicUsize>,
    helper_server: ScriptedServer,
    _servers: Vec<ScriptedServer>,
}

async fn confirmation_tree() -> anyhow::Result<ConfirmationTree> {
    let coordinator_server = ScriptedServer::start(Box::new(|_, _| {
        call("call_transfer", "transfer_to_agent", r#"{"agent_name":"specialist"}"#)
    }))
    .await?;
    let specialist_server =
        ScriptedServer::start(ask_helper_then_finish("call_helper", "delete report.txt")).await?;
    let helper_server = ScriptedServer::start(Box::new(|body, _| {
        match tool_message(body, "call_delete") {
            Some(result) if !result.contains("confirmation_pending") => {
                Reply::Text("report.txt is deleted".into())
            }
            _ => call("call_delete", "delete_file", r#"{"path":"report.txt"}"#),
        }
    }))
    .await?;
    let deletes = Arc::new(AtomicUsize::new(0));
    let helper = LlmAgentBuilder::new("helper")
        .description("Manages files")
        .model(scripted_model(&helper_server)?)
        .tool(counting_tool("delete_file", "Deletes a file", deletes.clone()))
        .require_tool_confirmation("delete_file")
        .build()?;
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Handles file clean-up")
        .model(scripted_model(&specialist_server)?)
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Routes requests")
        .model(scripted_model(&coordinator_server)?)
        .sub_agent(Arc::new(specialist))
        .build()?;
    let store = Arc::new(InMemoryApprovalStore::new());
    let harness = Harness::new(coordinator, RunConfig::default(), Some(store.clone())).await?;
    Ok(ConfirmationTree {
        harness,
        store,
        deletes,
        helper_server,
        _servers: vec![coordinator_server, specialist_server],
    })
}

/// How the approver answers the request the caller surfaced.
enum Approval {
    Store,
    CallId,
}

async fn nested_confirmation(approval: Approval) -> anyhow::Result<Verdict> {
    let tree = confirmation_tree().await?;
    let scope = ApprovalScope::new(APP, USER, SESSION);

    let first = tree.harness.run(Some("Clean up report.txt"), None).await?;
    let requests = first.confirmation_requests();
    let pending = tree.store.pending(&scope).await?;
    let mut checks = vec![
        (first.error.is_none(), format!("run 1 error: {:?}", first.error)),
        (
            requests.len() == 1
                && requests[0].tool_name == "delete_file"
                && requests[0].args == json!({ "path": "report.txt" }),
            format!(
                "run 1 confirmation requests: {:?}",
                requests
                    .iter()
                    .map(|request| (&request.tool_name, &request.function_call_id, &request.args))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            pending.len() == 1,
            format!("pending approvals in the caller's session scope: {}", pending.len()),
        ),
        (
            tree.deletes.load(Ordering::SeqCst) == 0,
            format!("delete_file runs before approval: {}", tree.deletes.load(Ordering::SeqCst)),
        ),
    ];
    let Some(request) = requests.first().cloned() else {
        checks.push((false, format!("run 1 ended with: {:?}", first.final_text())));
        return Ok(verdict(checks));
    };

    let resume_config = match approval {
        Approval::Store => {
            tree.store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await?;
            None
        }
        Approval::CallId => {
            let call_id = request.function_call_id.clone().unwrap_or_default();
            Some(
                RunConfig::builder()
                    .tool_confirmation_decisions(HashMap::from([(
                        call_id.clone(),
                        ToolConfirmationDecision::Approve,
                    )]))
                    .tool_confirmation_fingerprints(HashMap::from([(
                        call_id,
                        request.fingerprint(),
                    )]))
                    .build(),
            )
        }
    };
    let helper_requests_before = tree.helper_server.requests().len();
    let second = tree.harness.run(None, resume_config).await?;
    let helper_requests = tree.helper_server.requests();
    let resumed_with_result = helper_requests[helper_requests_before..].first().and_then(|request| {
        tool_message(request, "call_delete").filter(|result| !result.contains("confirmation_pending"))
    });
    let session = tree.harness.session_events().await?;
    let leaked_holds = session
        .iter()
        .filter(|event| event.author == "helper" && event.actions.tool_confirmation.is_some())
        .count();
    checks.extend([
        (second.error.is_none(), format!("run 2 error: {:?}", second.error)),
        (
            tree.deletes.load(Ordering::SeqCst) == 1,
            format!("delete_file runs after approval: {}", tree.deletes.load(Ordering::SeqCst)),
        ),
        (
            resumed_with_result.is_some(),
            format!(
                "helper's first model request after approval already carries the delete_file result: {}",
                resumed_with_result.is_some()
            ),
        ),
        (second.confirmation_requests().is_empty(), "run 2 surfaces no new request".into()),
        (
            second.final_text().contains("report.txt is deleted"),
            format!("run 2 final text: {:?}", second.final_text()),
        ),
        (leaked_holds == 0, format!("holds recorded under the helper's name: {leaked_holds}")),
    ]);
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// Scenario: events a tool forwards survive a slow client
// ---------------------------------------------------------------------------

const FORWARDED: usize = 400;

async fn forwarded_events_slow_consumer() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|body, _| match tool_message(body, "call_steps") {
        Some(_) => Reply::Text("all steps reported".into()),
        None => call("call_steps", "run_steps", "{}"),
    }))
    .await?;
    let run_steps = Arc::new(FunctionTool::new(
        "run_steps",
        "Runs the steps and reports each one",
        |ctx: Arc<dyn ToolContext>, _args: Value| async move {
            // The steps run on a worker task, which reports each one as it completes.
            let reporter = ctx.clone();
            let worker = tokio::spawn(async move {
                for step in 0..FORWARDED {
                    let mut event = Event::new(reporter.invocation_id());
                    event.author = "stepper".to_string();
                    event.llm_response.content =
                        Some(Content::new("model").with_text(format!("step {step} complete")));
                    reporter.emit_event(event).await;
                }
            });
            let _ = worker.await;
            Ok(json!({ "steps": FORWARDED }))
        },
    ));
    let agent = LlmAgentBuilder::new("worker")
        .description("Runs steps")
        .model(scripted_model(&server)?)
        .tool(run_steps)
        .build()?;
    let harness = Harness::new(agent, RunConfig::default(), None).await?;
    // The client stalls once, early in the run, for longer than a forwarded event may wait.
    let outcome =
        harness.run_paced(Some("Run the steps"), None, |n| match n {
            3 => Duration::from_millis(400),
            _ => Duration::ZERO,
        })
        .await?;
    let session = harness.session_events().await?;
    let persisted = session.iter().filter(|event| event.author == "stepper").count();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (
            persisted == FORWARDED,
            format!("forwarded events in the session: {persisted} of {FORWARDED}"),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Live scenarios
// ---------------------------------------------------------------------------

async fn live_child_topology() -> anyhow::Result<Verdict> {
    let Some(model) = live_model()? else {
        return Ok(Verdict::Skip("OPENAI_API_KEY not set".into()));
    };
    let helper_requests = RequestSink::default();
    let helper = capture_requests(
        LlmAgentBuilder::new("helper")
            .description("Writes one-sentence summaries")
            .instruction("Summarise the request in one short sentence. Do not call any tool.")
            .model(model.clone()),
        helper_requests.clone(),
    )
    .build()?;
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Handles summary requests")
        .instruction(
            "Always call the helper tool exactly once with the user's request, then reply with its result.",
        )
        .model(model.clone())
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .max_iterations(4)
        .build()?;
    let peer = LlmAgentBuilder::new("peer")
        .description("Handles anything that is not a summary")
        .model(model.clone())
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Routes requests")
        .instruction("Transfer every request to the specialist agent. Do not answer yourself.")
        .model(model)
        .sub_agent(Arc::new(specialist))
        .sub_agent(Arc::new(peer))
        .max_iterations(3)
        .build()?;
    let harness = Harness::new(coordinator, RunConfig::default(), None).await?;
    let outcome = harness
        .run(Some("Summarise: the quarterly report shows revenue up 12 percent."), None)
        .await?;
    let seen = helper_requests.lock().unwrap().clone();
    let tools: Vec<String> = seen.iter().flat_map(|request| request.tools.keys().cloned()).collect();
    Ok(verdict(vec![
        (outcome.error.is_none(), format!("run error: {:?}", outcome.error)),
        (!seen.is_empty(), format!("helper model called {} time(s)", seen.len())),
        (
            !tools.iter().any(|name| name == "transfer_to_agent"),
            format!("helper request tools: {tools:?}"),
        ),
        (true, format!("final text: {:?}", outcome.final_text())),
    ]))
}

async fn live_nested_confirmation() -> anyhow::Result<Verdict> {
    let Some(model) = live_model()? else {
        return Ok(Verdict::Skip("OPENAI_API_KEY not set".into()));
    };
    let deletes = Arc::new(AtomicUsize::new(0));
    let helper = LlmAgentBuilder::new("helper")
        .description("Deletes files")
        .instruction(
            "Delete the file named in the request with the delete_file tool, then say which file you deleted.",
        )
        .model(model.clone())
        .tool(counting_tool("delete_file", "Deletes a file. Argument: path (string).", deletes.clone()))
        .require_tool_confirmation("delete_file")
        .max_iterations(4)
        .build()?;
    let specialist = LlmAgentBuilder::new("specialist")
        .description("Handles file clean-up")
        .instruction(
            "Always call the helper tool exactly once with the user's request, then reply with its result.",
        )
        .model(model.clone())
        .tool(Arc::new(AgentTool::new(Arc::new(helper))))
        .max_iterations(4)
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Routes requests")
        .instruction("Transfer every request to the specialist agent. Do not answer yourself.")
        .model(model)
        .sub_agent(Arc::new(specialist))
        .max_iterations(3)
        .build()?;
    let store = Arc::new(InMemoryApprovalStore::new());
    let harness = Harness::new(coordinator, RunConfig::default(), Some(store.clone())).await?;
    let scope = ApprovalScope::new(APP, USER, SESSION);

    let first = harness.run(Some("Please delete the file notes.txt"), None).await?;
    let requests = first.confirmation_requests();
    let mut checks = vec![
        (first.error.is_none(), format!("run 1 error: {:?}", first.error)),
        (
            requests.len() == 1 && requests[0].tool_name == "delete_file",
            format!(
                "run 1 confirmation requests: {:?}",
                requests.iter().map(|request| (&request.tool_name, &request.args)).collect::<Vec<_>>()
            ),
        ),
        (deletes.load(Ordering::SeqCst) == 0, "delete_file did not run before approval".into()),
    ];
    let Some(request) = requests.first().cloned() else {
        checks.push((false, format!("run 1 ended with: {:?}", first.final_text())));
        return Ok(verdict(checks));
    };
    store.decide(&scope, &request.fingerprint(), ToolApproval::approve()).await?;
    let second = harness.run(None, None).await?;
    checks.extend([
        (second.error.is_none(), format!("run 2 error: {:?}", second.error)),
        (
            deletes.load(Ordering::SeqCst) == 1,
            format!("delete_file runs after approval: {}", deletes.load(Ordering::SeqCst)),
        ),
        (true, format!("run 2 final text: {:?}", second.final_text())),
    ]);
    Ok(verdict(checks))
}

const SCENARIOS: &[&str] = &[
    "child_topology",
    "skip_summarization_payload",
    "nested_skip_summarization",
    "helper_timeout_is_failure",
    "propagated_timeout_category",
    "nested_confirmation",
    "nested_confirmation_call_id",
    "forwarded_events_slow_consumer",
    "live_child_topology",
    "live_nested_confirmation",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "child_topology" => child_topology().await,
        "skip_summarization_payload" => skip_summarization_payload().await,
        "nested_skip_summarization" => nested_skip_summarization().await,
        "helper_timeout_is_failure" => helper_timeout(false).await,
        "propagated_timeout_category" => helper_timeout(true).await,
        "nested_confirmation" => nested_confirmation(Approval::Store).await,
        "nested_confirmation_call_id" => nested_confirmation(Approval::CallId).await,
        "forwarded_events_slow_consumer" => forwarded_events_slow_consumer().await,
        "live_child_topology" => live_child_topology().await,
        "live_nested_confirmation" => live_nested_confirmation().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
    let _ = dotenvy::dotenv();
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
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

    println!("agent tool isolation — {} scenario(s)\n", selected.len());
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
