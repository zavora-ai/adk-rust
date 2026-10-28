//! Tool approvals bound to the agent that held the call and the tool it was held for.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder`, `SequentialAgent`, `Runner`, the
//! SQLite session service and approval store, `OpenAIClient`, and `McpToolset` over in-process
//! MCP servers — and asserts which tool executions an approval authorizes. A scripted
//! OpenAI-compatible server on 127.0.0.1 plays the model where two agents must issue the same
//! call with the same arguments. The live scenario runs against OpenAI when `OPENAI_API_KEY` is
//! set.
//!
//! ```bash
//! cargo run --manifest-path examples/approval_scoping/Cargo.toml
//! cargo run --manifest-path examples/approval_scoping/Cargo.toml -- --scenario mcp_resume_stays_on_its_server
//! ```

mod acting;
mod bank;
mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::identity::{SessionId, UserId};
use adk_core::{
    Agent, ApprovalScope, ApprovalStore, Content, Event, Llm, Part, RunConfig, Tool, ToolApproval,
    ToolConfirmationDecision, ToolConfirmationRequest, ToolContext, Toolset,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{
    CreateRequest, GetRequest, SessionService, SqliteApprovalStore, SqliteSessionService,
};
use async_trait::async_trait;
use bank::{BankServer, SwitchToolset};
use futures::StreamExt;
use scripted::{Reply, Script, ScriptedServer, call};
use serde_json::{Value, json};

const APP: &str = "approval-scoping";
const USER: &str = "user-1";
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

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// A `transfer` tool owned by one agent, counting its executions.
struct Transfer {
    owner: &'static str,
    description: String,
    executions: AtomicUsize,
}

impl Transfer {
    fn new(owner: &'static str, account: &str) -> Arc<Self> {
        Arc::new(Self {
            owner,
            description: format!("Moves money out of the {account} account."),
            executions: AtomicUsize::new(0),
        })
    }

    fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for Transfer {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "amount": { "type": "integer" } },
            "required": ["amount"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "transferred": args["amount"], "by": self.owner }))
    }
}

/// A SQLite session service and approval store in a temporary directory, as a deployment
/// that survives restarts would use.
struct Durable {
    _dir: tempfile::TempDir,
    sessions: Arc<dyn SessionService>,
    store: Arc<SqliteApprovalStore>,
    session_id: String,
}

impl Durable {
    async fn open(session_id: &str) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let url = |file: &str| format!("sqlite://{}?mode=rwc", dir.path().join(file).display());
        let sessions = SqliteSessionService::new(&url("sessions.db")).await?;
        sessions.migrate().await?;
        let store = SqliteApprovalStore::new(&url("approvals.db")).await?;
        store.migrate().await?;
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await?;
        Ok(Self {
            _dir: dir,
            sessions: Arc::new(sessions),
            store: Arc::new(store),
            session_id: session_id.to_string(),
        })
    }

    fn scope(&self) -> ApprovalScope {
        ApprovalScope::new(APP, USER, &self.session_id)
    }

    fn base_config(&self) -> RunConfig {
        RunConfig::builder().approval_store(self.store.clone()).build()
    }

    async fn events(&self) -> anyhow::Result<Vec<Event>> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: self.session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        Ok(session.events().all())
    }

    /// The agent ids of the calls waiting for a decision.
    async fn pending_agents(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .store
            .pending(&self.scope())
            .await?
            .iter()
            .map(|entry| agent_of(&entry.request))
            .collect())
    }

    /// Approves the one pending call made by `agent`, the way an approver's inbox does.
    async fn approve_pending_of(&self, agent: &str) -> anyhow::Result<()> {
        let pending = self.store.pending(&self.scope()).await?;
        let entry = pending
            .iter()
            .find(|entry| agent_of(&entry.request).ends_with(agent))
            .ok_or_else(|| anyhow::anyhow!("no pending call from {agent}"))?;
        self.store.decide(&self.scope(), &entry.fingerprint, ToolApproval::approve()).await?;
        Ok(())
    }
}

fn agent_of(request: &ToolConfirmationRequest) -> String {
    request.acting.as_ref().map(|acting| acting.agent.id.to_string()).unwrap_or_default()
}

/// What one run produced.
struct Turn {
    events: Vec<Event>,
    error: Option<String>,
}

impl Turn {
    fn held(&self) -> Vec<ToolConfirmationRequest> {
        self.events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
    }

    fn called_by(&self, author: &str, tool: &str) -> bool {
        self.events.iter().any(|event| {
            event.author == author
                && event.llm_response.content.as_ref().is_some_and(|content| {
                    content
                        .parts
                        .iter()
                        .any(|part| matches!(part, Part::FunctionCall { name, .. } if name == tool))
                })
        })
    }
}

async fn run_turn(
    runner: &Runner,
    session_id: &str,
    text: &str,
    config: Option<RunConfig>,
) -> anyhow::Result<Turn> {
    let mut events = Vec::new();
    let mut error = None;
    let content = Content::new("user").with_text(text);
    let user = UserId::try_from(USER)?;
    let session = SessionId::try_from(session_id)?;
    match runner.run_with_config(user, session, content, config).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) => events.push(event),
                    Err(err) => {
                        error = Some(err.to_string());
                        break;
                    }
                }
            }
        }
        Err(err) => error = Some(err.to_string()),
    }
    Ok(Turn { events, error })
}

/// The response that answered `call_id` last in `events`, if any.
fn answer_to(events: &[Event], call_id: &str) -> Option<Value> {
    events
        .iter()
        .flat_map(|event| event.llm_response.content.iter().flat_map(|content| &content.parts))
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. }
                if id.as_deref() == Some(call_id) =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .last()
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

/// The `[agent:X]` marker in the request's system message.
fn agent_marker(request: &Value) -> String {
    let system = request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .map(Value::to_string)
        .collect::<String>();
    ["A", "B", "X"]
        .iter()
        .find(|marker| system.contains(&format!("[agent:{marker}]")))
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn last_role(request: &Value) -> Option<String> {
    request.get("messages")?.as_array()?.last()?.get("role")?.as_str().map(str::to_string)
}

// ---------------------------------------------------------------------------
// Two agents, one tool name
// ---------------------------------------------------------------------------

/// How the approval for agent A's held call is granted.
#[derive(Clone, Copy)]
enum Grant {
    /// The approver decides A's pending entry in the durable approval store.
    Store,
    /// The next runs carry A's fingerprint in `RunConfig::tool_approvals`.
    RunWide,
    /// The next runs carry a decision for A's call ID, bound to A's fingerprint, and the
    /// provider reuses call IDs across responses.
    CallId,
}

/// What `Sequential[agent_b, agent_a]` did after A's held call was approved.
struct PairRun {
    held_in_run1: Vec<String>,
    a_after_run2: usize,
    b_after_run2: usize,
    a_after_run3: usize,
    b_after_run3: usize,
    b_called_in_run2: bool,
    pending_after_run2: Vec<String>,
    a_answer: Option<Value>,
    errors: Vec<String>,
}

/// Phase 1: A transfers 40, B answers. Phase 2: B transfers 40, A answers. Phase 3: both
/// answer. A tool result is always answered with text.
fn pair_script(phase: Arc<AtomicUsize>, reuse_ids: bool) -> Script {
    Box::new(move |request, _| {
        if last_role(request).as_deref() == Some("tool") {
            return Reply::Text("done".into());
        }
        let id = |own: &str| if reuse_ids { "call_0".to_string() } else { own.to_string() };
        match (agent_marker(request).as_str(), phase.load(Ordering::SeqCst)) {
            ("A", 1) => call(&id("call_a_1"), "transfer", r#"{"amount":40}"#),
            ("B", 2) => call(&id("call_b_2"), "transfer", r#"{"amount":40}"#),
            _ => Reply::Text("nothing to do".into()),
        }
    })
}

async fn pair_run(grant: Grant) -> anyhow::Result<PairRun> {
    let phase = Arc::new(AtomicUsize::new(1));
    let server =
        ScriptedServer::start(pair_script(phase.clone(), matches!(grant, Grant::CallId))).await?;
    let model = scripted_model(&server)?;
    pair_run_with(grant, model, &phase, ["pay", "continue", "anything else?"]).await
}

async fn pair_run_with(
    grant: Grant,
    model: Arc<dyn Llm>,
    phase: &AtomicUsize,
    prompts: [&str; 3],
) -> anyhow::Result<PairRun> {
    let a_tool = Transfer::new("agent_a", "treasury");
    let b_tool = Transfer::new("agent_b", "petty-cash");
    let agent_a = LlmAgentBuilder::new("agent_a")
        .instruction(
            "[agent:A] You are the treasurer. If the latest user message is exactly PAY, call \
             the transfer tool once with amount 40. In every other case reply with one short \
             sentence and call no tools.",
        )
        .model(model.clone())
        .tool(a_tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()?;
    let agent_b = LlmAgentBuilder::new("agent_b")
        .instruction(
            "[agent:B] You are the office assistant. If the latest user message is exactly \
             CONTINUE, call the transfer tool once with amount 40. In every other case reply \
             with one short sentence and call no tools.",
        )
        .model(model)
        .tool(b_tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()?;
    let workflow = SequentialAgent::new(
        "pipeline",
        vec![Arc::new(agent_b) as Arc<dyn Agent>, Arc::new(agent_a)],
    );

    let env = Durable::open("pair").await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(workflow) as Arc<dyn Agent>)
        .session_service(env.sessions.clone())
        .run_config(env.base_config())
        .build()?;
    let mut errors = Vec::new();

    // Run 1: A's transfer is held for approval.
    let first = run_turn(&runner, &env.session_id, prompts[0], None).await?;
    errors.extend(first.error.clone());
    let held_in_run1: Vec<String> = first.held().iter().map(agent_of).collect();
    let Some(held) =
        first.held().into_iter().find(|request| agent_of(request).ends_with("agent_a"))
    else {
        return Ok(PairRun {
            held_in_run1,
            a_after_run2: 0,
            b_after_run2: 0,
            a_after_run3: 0,
            b_after_run3: 0,
            b_called_in_run2: false,
            pending_after_run2: Vec::new(),
            a_answer: None,
            errors,
        });
    };
    let call_id = held.function_call_id.clone().unwrap_or_default();

    let config = match grant {
        Grant::Store => {
            env.approve_pending_of("agent_a").await?;
            None
        }
        Grant::RunWide => Some(
            RunConfig::builder()
                .approval_store(env.store.clone())
                .tool_approval(held.fingerprint(), ToolApproval::approve())
                .build(),
        ),
        Grant::CallId => Some(
            RunConfig::builder()
                .approval_store(env.store.clone())
                .tool_confirmation_decisions(HashMap::from([(
                    call_id.clone(),
                    ToolConfirmationDecision::Approve,
                )]))
                .tool_confirmation_fingerprints(HashMap::from([(
                    call_id.clone(),
                    held.fingerprint(),
                )]))
                .build(),
        ),
    };

    // Run 2: B issues the same call with the same arguments before A resumes.
    phase.store(2, Ordering::SeqCst);
    let second = run_turn(&runner, &env.session_id, prompts[1], config.clone()).await?;
    errors.extend(second.error.clone());
    let (a_after_run2, b_after_run2) = (a_tool.executions(), b_tool.executions());
    let b_called_in_run2 = second.called_by("agent_b", "transfer");
    let pending_after_run2 = env.pending_agents().await?;

    // Run 3: nothing new is asked for; A's call must not run again.
    phase.store(3, Ordering::SeqCst);
    let third = run_turn(&runner, &env.session_id, prompts[2], config).await?;
    errors.extend(third.error.clone());

    let events = env.events().await?;
    let a_answer = events.iter().filter(|event| event.author == "agent_a").find_map(|event| {
        answer_to(std::slice::from_ref(event), &call_id)
            .filter(|response| !adk_core::is_confirmation_pending(response))
    });
    Ok(PairRun {
        held_in_run1,
        a_after_run2,
        b_after_run2,
        a_after_run3: a_tool.executions(),
        b_after_run3: b_tool.executions(),
        b_called_in_run2,
        pending_after_run2,
        a_answer,
        errors,
    })
}

fn no_errors(errors: &[String]) -> (bool, String) {
    (errors.is_empty(), format!("run errors: {errors:?}"))
}

async fn store_approval_stays_with_its_agent() -> anyhow::Result<Verdict> {
    let run = pair_run(Grant::Store).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.held_in_run1.len() == 1 && run.held_in_run1[0].ends_with("agent_a"),
            format!("run 1 held one call, from agent_a: {:?}", run.held_in_run1),
        ),
        (
            run.b_after_run2 == 0,
            format!(
                "agent_b's transfer executions after A's approval: {} (expected 0)",
                run.b_after_run2
            ),
        ),
        (
            run.pending_after_run2.iter().any(|agent| agent.ends_with("agent_b")),
            format!(
                "agent_b's call waits for its own approval; pending after run 2: {:?}",
                run.pending_after_run2
            ),
        ),
    ]))
}

async fn approved_call_runs_once() -> anyhow::Result<Verdict> {
    let run = pair_run(Grant::Store).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.a_after_run2 == 1 && run.a_after_run3 == 1,
            format!(
                "agent_a's transfer executions: {} after run 2, {} after run 3 (expected 1, 1)",
                run.a_after_run2, run.a_after_run3
            ),
        ),
        (
            run.a_answer.as_ref().is_some_and(|answer| answer["transferred"] == 40),
            format!("agent_a's held call is answered by its result: {:?}", run.a_answer),
        ),
    ]))
}

async fn run_wide_approval_stays_with_its_agent() -> anyhow::Result<Verdict> {
    let run = pair_run(Grant::RunWide).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.b_after_run2 == 0 && run.b_after_run3 == 0,
            format!(
                "agent_b's transfer executions under A's run-wide fingerprint approval: {} after \
                 run 2, {} after run 3 (expected 0)",
                run.b_after_run2, run.b_after_run3
            ),
        ),
        (
            run.a_after_run2 == 1 && run.a_after_run3 == 1,
            format!(
                "agent_a's held call ran {} time(s) after run 2, {} after run 3 (expected 1, 1)",
                run.a_after_run2, run.a_after_run3
            ),
        ),
    ]))
}

async fn call_id_decision_stays_with_its_agent() -> anyhow::Result<Verdict> {
    let run = pair_run(Grant::CallId).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.b_called_in_run2,
            "agent_b issued transfer under the reused call ID call_0".to_string(),
        ),
        (
            run.b_after_run2 == 0,
            format!(
                "agent_b's transfer executions under A's fingerprint-bound call-ID decision: {} \
                 (expected 0)",
                run.b_after_run2
            ),
        ),
        (
            run.a_after_run2 == 1 && run.a_after_run3 == 1,
            format!(
                "agent_a's held call ran {} time(s) after run 2, {} after run 3 (expected 1, 1)",
                run.a_after_run2, run.a_after_run3
            ),
        ),
    ]))
}

/// One agent; A's own fingerprint, taken from the request it surfaced, approves its calls in
/// every later run.
async fn preapproved_fingerprint_portable_across_runs() -> anyhow::Result<Verdict> {
    let phase = Arc::new(AtomicUsize::new(1));
    let script_phase = phase.clone();
    let server = ScriptedServer::start(Box::new(move |request, _| {
        if last_role(request).as_deref() == Some("tool") {
            return Reply::Text("done".into());
        }
        match script_phase.load(Ordering::SeqCst) {
            1 => call("call_a_1", "transfer", r#"{"amount":40}"#),
            3 => call("call_a_3", "transfer", r#"{"amount":40}"#),
            _ => Reply::Text("nothing to do".into()),
        }
    }))
    .await?;
    let tool = Transfer::new("agent_a", "treasury");
    let agent = LlmAgentBuilder::new("agent_a")
        .instruction("[agent:A] You are the treasurer.")
        .model(scripted_model(&server)?)
        .tool(tool.clone() as Arc<dyn Tool>)
        .require_tool_confirmation("transfer")
        .build()?;
    let env = Durable::open("portable").await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(env.sessions.clone())
        .run_config(env.base_config())
        .build()?;

    let first = run_turn(&runner, &env.session_id, "pay", None).await?;
    let Some(held) = first.held().into_iter().next() else {
        return Ok(Verdict::Fail("run 1 held no call".into()));
    };
    let approved = RunConfig::builder()
        .approval_store(env.store.clone())
        .tool_approval(held.fingerprint(), ToolApproval::approve())
        .build();
    phase.store(2, Ordering::SeqCst);
    let second = run_turn(&runner, &env.session_id, "continue", Some(approved.clone())).await?;
    let after_run2 = tool.executions();
    phase.store(3, Ordering::SeqCst);
    let third = run_turn(&runner, &env.session_id, "pay again", Some(approved)).await?;
    let errors: Vec<String> =
        [first.error, second.error, third.error.clone()].into_iter().flatten().collect();
    Ok(verdict(vec![
        no_errors(&errors),
        (after_run2 == 1, format!("run 2 resumed the held call: {after_run2} execution(s)")),
        (
            tool.executions() == 2 && third.held().is_empty(),
            format!(
                "run 3's new call (call_a_3) ran under the same fingerprint: {} execution(s) in \
                 total, {} held",
                tool.executions(),
                third.held().len()
            ),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// MCP servers behind one tool name
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum ServerChange {
    /// The tool name resolves to another server's `wire` with the same declaration.
    OtherServer,
    /// The same server redefines `wire` with a new description and schema.
    Redefined,
    /// Nothing changes.
    Unchanged,
}

struct McpRun {
    primary_calls: usize,
    secondary_calls: usize,
    answer: Option<Value>,
    errors: Vec<String>,
}

async fn mcp_run(change: ServerChange) -> anyhow::Result<McpRun> {
    let phase = Arc::new(AtomicUsize::new(1));
    let script_phase = phase.clone();
    let server = ScriptedServer::start(Box::new(move |request, _| {
        if last_role(request).as_deref() == Some("tool") {
            return Reply::Text("done".into());
        }
        match script_phase.load(Ordering::SeqCst) {
            1 => call("call_x_1", "wire", r#"{"amount":40,"to":"acct-9"}"#),
            _ => Reply::Text("nothing to do".into()),
        }
    }))
    .await?;
    let primary = BankServer::new("bank_a");
    let secondary = BankServer::new("bank_b");
    let primary_toolset = primary.connect("bank_a").await?;
    let switch =
        Arc::new(SwitchToolset::new(primary_toolset.clone(), secondary.connect("bank_b").await?));
    let agent = LlmAgentBuilder::new("agent_x")
        .instruction("[agent:X] You move money with the wire tool.")
        .model(scripted_model(&server)?)
        .toolset(switch.clone() as Arc<dyn Toolset>)
        .require_tool_confirmation("wire")
        .build()?;
    let env = Durable::open("mcp").await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(env.sessions.clone())
        .run_config(env.base_config())
        .build()?;

    let mut errors = Vec::new();
    let first = run_turn(&runner, &env.session_id, "wire 40 to acct-9", None).await?;
    errors.extend(first.error.clone());
    if first.held().is_empty() {
        errors.push("run 1 held no call".into());
    }
    env.approve_pending_of("agent_x").await?;
    match change {
        ServerChange::OtherServer => switch.switch_to_secondary(),
        ServerChange::Redefined => {
            primary.redefine();
            primary_toolset.invalidate_tool_list_cache().await;
        }
        ServerChange::Unchanged => {}
    }
    phase.store(2, Ordering::SeqCst);
    let second = run_turn(&runner, &env.session_id, "continue", None).await?;
    errors.extend(second.error);
    let third = run_turn(&runner, &env.session_id, "anything else?", None).await?;
    errors.extend(third.error);
    let events = env.events().await?;
    Ok(McpRun {
        primary_calls: primary.calls(),
        secondary_calls: secondary.calls(),
        answer: answer_to(&events, "call_x_1"),
        errors,
    })
}

fn not_run(answer: &Option<Value>) -> bool {
    answer.as_ref().is_some_and(|answer| {
        answer.get("error").and_then(Value::as_str).is_some_and(|text| text.contains("not run"))
    })
}

async fn mcp_resume_stays_on_its_server() -> anyhow::Result<Verdict> {
    let run = mcp_run(ServerChange::OtherServer).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.secondary_calls == 0,
            format!(
                "bank_b received {} wire call(s) for the call approved on bank_a (expected 0)",
                run.secondary_calls
            ),
        ),
        (run.primary_calls == 0, format!("bank_a received {} wire call(s)", run.primary_calls)),
        (
            not_run(&run.answer),
            format!("the held call is answered as not run, with the reason: {:?}", run.answer),
        ),
    ]))
}

async fn mcp_redefined_tool_needs_new_approval() -> anyhow::Result<Verdict> {
    let run = mcp_run(ServerChange::Redefined).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.primary_calls == 0,
            format!(
                "the redefined wire tool received {} call(s) under the old approval (expected 0)",
                run.primary_calls
            ),
        ),
        (
            not_run(&run.answer),
            format!("the held call is answered as not run, with the reason: {:?}", run.answer),
        ),
    ]))
}

async fn mcp_resume_on_same_server_runs_once() -> anyhow::Result<Verdict> {
    let run = mcp_run(ServerChange::Unchanged).await?;
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.primary_calls == 1 && run.secondary_calls == 0,
            format!(
                "bank_a received {} wire call(s), bank_b {} (expected 1, 0)",
                run.primary_calls, run.secondary_calls
            ),
        ),
        (
            run.answer
                .as_ref()
                .is_some_and(|answer| answer.to_string().contains("wired by bank_a")),
            format!("the held call is answered by bank_a's result: {:?}", run.answer),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Live
// ---------------------------------------------------------------------------

async fn live_store_approval_stays_with_its_agent() -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| LIVE_MODEL.to_string());
    let llm: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model.as_str()))?);
    let phase = AtomicUsize::new(1);
    let run = pair_run_with(Grant::Store, llm, &phase, ["PAY", "CONTINUE", "Thanks."]).await?;
    if run.held_in_run1.is_empty() {
        return Ok(Verdict::Skip(format!("{model} did not call transfer in run 1")));
    }
    if !run.b_called_in_run2 {
        return Ok(Verdict::Skip(format!("{model} did not have agent_b call transfer in run 2")));
    }
    Ok(verdict(vec![
        no_errors(&run.errors),
        (
            run.b_after_run2 == 0,
            format!(
                "{model}: agent_b's transfer executions after A's approval: {} (expected 0)",
                run.b_after_run2
            ),
        ),
        (
            run.a_after_run3 == 1,
            format!(
                "{model}: agent_a's approved call ran {} time(s) (expected 1)",
                run.a_after_run3
            ),
        ),
    ]))
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

const SCENARIOS: &[&str] = &[
    "store_approval_stays_with_its_agent",
    "approved_call_runs_once",
    "run_wide_approval_stays_with_its_agent",
    "call_id_decision_stays_with_its_agent",
    "preapproved_fingerprint_portable_across_runs",
    "mcp_resume_stays_on_its_server",
    "mcp_redefined_tool_needs_new_approval",
    "mcp_resume_on_same_server_runs_once",
    "live_store_approval_stays_with_its_agent",
    "live_claude_approval_stays_with_its_agent",
    "live_claude_approval_naming_no_agent_is_refused",
    "realtime_handler_approval_follows_its_definition",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "store_approval_stays_with_its_agent" => store_approval_stays_with_its_agent().await,
        "approved_call_runs_once" => approved_call_runs_once().await,
        "run_wide_approval_stays_with_its_agent" => run_wide_approval_stays_with_its_agent().await,
        "call_id_decision_stays_with_its_agent" => call_id_decision_stays_with_its_agent().await,
        "preapproved_fingerprint_portable_across_runs" => {
            preapproved_fingerprint_portable_across_runs().await
        }
        "mcp_resume_stays_on_its_server" => mcp_resume_stays_on_its_server().await,
        "mcp_redefined_tool_needs_new_approval" => mcp_redefined_tool_needs_new_approval().await,
        "mcp_resume_on_same_server_runs_once" => mcp_resume_on_same_server_runs_once().await,
        "live_store_approval_stays_with_its_agent" => {
            live_store_approval_stays_with_its_agent().await
        }
        "live_claude_approval_stays_with_its_agent" => {
            acting::live_claude_approval_stays_with_its_agent().await
        }
        "live_claude_approval_naming_no_agent_is_refused" => {
            acting::live_claude_approval_naming_no_agent_is_refused().await
        }
        "realtime_handler_approval_follows_its_definition" => {
            acting::realtime_handler_approval_follows_its_definition().await
        }
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    // The first dotenv file found wins over stale shell exports.
    let candidates = [
        std::env::var("ADK_ENV_FILE").ok(),
        Some(".env".to_string()),
        Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string()),
    ];
    for path in candidates.into_iter().flatten() {
        if dotenvy::from_path_override(&path).is_ok() {
            break;
        }
    }
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

    println!("approval scoping — {} scenario(s)\n", selected.len());
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
