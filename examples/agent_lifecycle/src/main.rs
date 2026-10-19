//! Agent lifecycle and callbacks: transfers, instruction templates, after-agent and after-run
//! stages, model-error plugins, retry-reflect counters, `output_key`, parallel escalation,
//! `on_event` coverage, and `Runner::close`.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder` agents, `ParallelAgent`,
//! `LoopAgent`, a `PluginManager`, the `RetryReflectPlugin`, and `Runner` over the in-memory
//! session service. A scripted OpenAI-compatible upstream on 127.0.0.1 plays the model through
//! the real `OpenAIClient` and records every request, so each scenario asserts on what a
//! provider receives and what the session keeps, never on model prose. The `live_*` scenarios
//! run the same checks against Gemini (`gemini-3.5-flash-lite`) when `GOOGLE_API_KEY` is set.
//!
//! ```bash
//! cargo run --manifest-path examples/agent_lifecycle/Cargo.toml
//! cargo run --manifest-path examples/agent_lifecycle/Cargo.toml -- --scenario ar5_tool_transfer_restrictions
//! ```

#[cfg(not(feature = "main-baseline"))]
mod close;
mod scripted;

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::{LlmAgentBuilder, LoopAgent, ParallelAgent};
use adk_core::{
    AdkError, AfterAgentCallback, Agent, BeforeModelCallback, BeforeModelResult, Content, Event,
    Llm, LlmResponse, Part, RunBudget, RunConfig, SessionId, Tool, ToolContext, UserId,
};
use adk_model::{GeminiModel, OpenAIClient, RetryConfig};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_retry_reflect::RetryReflectPluginBuilder;
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::{AgentTool, FunctionTool};
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, Turn};
use serde_json::{Value, json};

const APP: &str = "agent-lifecycle";
const USER: &str = "customer";
const SESSION: &str = "session-1";
const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

pub(crate) enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
pub(crate) fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

type Counter = Arc<AtomicUsize>;

fn counter() -> Counter {
    Arc::new(AtomicUsize::new(0))
}

fn read(counter: &Counter) -> usize {
    counter.load(Ordering::SeqCst)
}

/// Waits up to `limit` for `counter` to reach `expected`, and returns its value.
async fn wait_for(counter: &Counter, expected: usize, limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    while read(counter) < expected && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    read(counter)
}

/// An in-memory session service.
pub(crate) struct Store {
    sessions: Arc<InMemorySessionService>,
}

impl Store {
    pub(crate) async fn with_session(
        user: &str,
        session: &str,
        state: HashMap<String, Value>,
    ) -> anyhow::Result<Self> {
        let store = Self { sessions: Arc::new(InMemorySessionService::new()) };
        store.create(user, session, state).await?;
        Ok(store)
    }

    async fn create(
        &self,
        user: &str,
        session: &str,
        state: HashMap<String, Value>,
    ) -> anyhow::Result<()> {
        self.sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: user.to_string(),
                session_id: Some(session.to_string()),
                state,
            })
            .await?;
        Ok(())
    }

    async fn session(
        &self,
        user: &str,
        session: &str,
    ) -> anyhow::Result<Box<dyn adk_session::Session>> {
        Ok(self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?)
    }

    async fn events(&self, user: &str, session: &str) -> anyhow::Result<Vec<Event>> {
        Ok(self.session(user, session).await?.events().all())
    }

    async fn state(&self, user: &str, session: &str) -> anyhow::Result<HashMap<String, Value>> {
        Ok(self.session(user, session).await?.state().all())
    }

    pub(crate) fn runner(
        &self,
        agent: Arc<dyn Agent>,
        plugins: Option<Arc<PluginManager>>,
    ) -> anyhow::Result<Runner> {
        let builder =
            Runner::builder().app_name(APP).agent(agent).session_service(self.sessions.clone());
        let builder = match plugins {
            Some(plugins) => builder.plugin_manager(plugins),
            None => builder,
        };
        Ok(builder.build()?)
    }
}

pub(crate) struct Run {
    pub(crate) error: Option<AdkError>,
    pub(crate) events: Vec<Event>,
}

impl Run {
    pub(crate) fn final_text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|event| event.author != "user" && !event.llm_response.partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .find(|text| !text.trim().is_empty())
            .unwrap_or_default()
            .to_string()
    }
}

pub(crate) async fn run(
    runner: &Runner,
    user: &str,
    session: &str,
    text: &str,
    config: Option<RunConfig>,
) -> anyhow::Result<Run> {
    let mut stream = runner
        .run_with_config(
            UserId::new(user)?,
            SessionId::new(session)?,
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

/// What the consumer does once the agent has issued a tool call.
#[derive(Clone, Copy)]
enum Stop {
    /// Calls `Runner::interrupt` and reads the stream to its end.
    Interrupt,
    /// Drops the stream, as a disconnected client does.
    Drop,
}

/// Runs until the first complete event that carries a function call, then stops as `stop` says.
async fn run_and_stop_at_call(
    runner: &Runner,
    user: &str,
    session: &str,
    text: &str,
    stop: Stop,
    config: Option<RunConfig>,
) -> anyhow::Result<Run> {
    let mut stream = runner
        .run_with_config(
            UserId::new(user)?,
            SessionId::new(session)?,
            Content::new("user").with_text(text),
            config,
        )
        .await?;
    let mut run = Run { error: None, events: Vec::new() };
    let mut stopped = false;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                let calls = !event.llm_response.partial
                    && event.llm_response.content.as_ref().is_some_and(|content| {
                        content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
                    });
                run.events.push(event);
                if calls && !stopped {
                    stopped = true;
                    // Lets the tool start before the run is stopped.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    match stop {
                        Stop::Interrupt => {
                            runner.interrupt(session);
                        }
                        Stop::Drop => {
                            drop(stream);
                            return Ok(run);
                        }
                    }
                }
            }
            Err(error) => {
                run.error = Some(error);
                break;
            }
        }
    }
    Ok(run)
}

pub(crate) fn describe(error: &Option<AdkError>) -> String {
    match error {
        Some(error) => format!("{} ({})", error.code, brief(&error.message)),
        None => "none".to_string(),
    }
}

pub(crate) fn brief(text: &str) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() > 160 {
        format!("{}…", text.chars().take(160).collect::<String>())
    } else {
        text
    }
}

pub(crate) fn no_error(run: &Run, what: &str) -> (bool, String) {
    (run.error.is_none(), format!("{what} ends without an error (error: {})", describe(&run.error)))
}

pub(crate) fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(
        OpenAIClient::compatible("scripted-key", &server.base, "scripted-model")?
            .with_retry_config(RetryConfig::disabled()),
    ))
}

pub(crate) fn gemini(key: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(GeminiModel::new(key, GEMINI_MODEL)?))
}

fn agent(name: &str, model: Arc<dyn Llm>) -> LlmAgentBuilder {
    LlmAgentBuilder::new(name).model(model).instruction(format!("You are {name}. Be brief."))
}

fn count_after_agent(counter: Counter) -> AfterAgentCallback {
    Box::new(move |_ctx| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    })
}

fn count_model_calls(counter: Counter) -> BeforeModelCallback {
    Box::new(move |_ctx, request| {
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(BeforeModelResult::Continue(request))
        })
    })
}

/// A tool that hands control to `target` through its actions.
fn transfer_tool(name: &str, target: &'static str, calls: Counter) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        name,
        format!("Hands the conversation to {target}."),
        move |ctx: Arc<dyn ToolContext>, _args| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let mut actions = ctx.actions();
                actions.transfer_to_agent = Some(target.to_string());
                ctx.set_actions(actions);
                Ok(json!({"status": format!("handing over to {target}")}))
            }
        },
    ))
}

/// A tool whose result ends the agent's turn.
fn terminal_tool(name: &str) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        name,
        "Files the ticket and ends the turn.",
        |ctx: Arc<dyn ToolContext>, _args| async move {
            let mut actions = ctx.actions();
            actions.skip_summarization = true;
            ctx.set_actions(actions);
            Ok(json!({"ticket": "T-100", "status": "filed"}))
        },
    ))
}

/// A tool that escalates out of the enclosing workflow.
fn escalate_tool(name: &str) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        name,
        "Stops the whole workflow.",
        |ctx: Arc<dyn ToolContext>, _args| async move {
            let mut actions = ctx.actions();
            actions.escalate = true;
            ctx.set_actions(actions);
            Ok(json!({"status": "stopping"}))
        },
    ))
}

/// A tool that takes `duration` and counts the calls that finish.
fn slow_tool(name: &str, duration: Duration, finished: Counter) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(name, "Looks something up slowly.", move |_ctx, _args| {
        let finished = finished.clone();
        async move {
            tokio::time::sleep(duration).await;
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"status": "found"}))
        }
    }))
}

fn lookup_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("lookup", "Looks up an order's status.", |_ctx, _args| async move {
        Ok(json!({"status": "ships Monday"}))
    }))
}

/// The function responses the session holds for `tool`, in order.
fn responses_for(events: &[Event], tool: &str) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } if function_response.name == tool => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

/// The ID of the first function call in `events`.
fn first_call_id(events: &[Event]) -> Option<String> {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .find_map(|part| match part {
            Part::FunctionCall { id, .. } => id.clone(),
            _ => None,
        })
}

/// Whether the session answers the call `id`, and whether the answer is outcome-unknown.
fn answer_for(events: &[Event], id: &str) -> Option<bool> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .find_map(|part| match part {
            Part::FunctionResponse { function_response, id: Some(answered), .. }
                if answered == id =>
            {
                Some(adk_core::is_outcome_unknown(&function_response.response))
            }
            _ => None,
        })
}

// ===== AR5: a tool's transfer obeys the agent's transfer restrictions =====

async fn ar5_tool_transfer_restrictions() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();
    for (tool_name, target) in [("hand_back", "coordinator"), ("hand_to_peer", "auditor")] {
        let server =
            ScriptedServer::start(Box::new(move |turn: &Turn| match turn.agent.as_str() {
                "coordinator" if turn.count == 1 => {
                    Reply::call("transfer_to_agent", json!({"agent_name": "worker"}))
                }
                "coordinator" => Reply::text("coordinator ran again"),
                "worker" if turn.last_role == "tool" => Reply::text("worker done"),
                "worker" => Reply::call(tool_name, json!({})),
                _ => Reply::text("auditor ran"),
            }))
            .await?;
        let model = scripted_model(&server)?;
        let worker = agent("worker", model.clone())
            .tool(transfer_tool(tool_name, target, counter()))
            .disallow_transfer_to_parent(true)
            .disallow_transfer_to_peers(true)
            .build()?;
        let auditor = agent("auditor", model.clone()).build()?;
        let coordinator = agent("coordinator", model)
            .sub_agent(Arc::new(worker))
            .sub_agent(Arc::new(auditor))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(coordinator), None)?;
        let outcome = run(&runner, USER, SESSION, "please handle my refund", None).await?;
        let worker_calls = server.requests_from("worker").len();
        let target_calls = server.requests_from(target).len();
        let expected = usize::from(target == "coordinator");
        checks.push(no_error(&outcome, &format!("the {tool_name} run")));
        checks.push((
            worker_calls == 2,
            format!("worker calls {tool_name} and answers ({worker_calls} model calls)"),
        ));
        checks.push((
            target_calls == expected,
            format!(
                "{tool_name} cannot hand control to {target}, which worker may not transfer to \
                 ({target} model calls: {target_calls}, expected {expected})"
            ),
        ));
    }
    Ok(verdict(checks))
}

async fn live_ar5_tool_transfer_restrictions() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let model = gemini(&key)?;
    let coordinator_calls = counter();
    let hand_backs = counter();
    let worker = LlmAgentBuilder::new("worker")
        .model(model.clone())
        .description("Handles refund requests.")
        .instruction(
            "You are worker. First call the hand_back tool exactly once. After it returns, \
             reply with the single word DONE.",
        )
        .tool(transfer_tool("hand_back", "coordinator", hand_backs.clone()))
        .disallow_transfer_to_parent(true)
        .disallow_transfer_to_peers(true)
        .build()?;
    let coordinator = LlmAgentBuilder::new("coordinator")
        .model(model)
        .instruction(
            "You are coordinator. Hand every request to the worker agent by calling \
             transfer_to_agent with agent_name \"worker\". Never answer yourself.",
        )
        .before_model_callback(count_model_calls(coordinator_calls.clone()))
        .sub_agent(Arc::new(worker))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(coordinator), None)?;
    let config = RunConfig { max_transfer_depth: Some(2), ..RunConfig::default() };
    let outcome = run(&runner, USER, SESSION, "Please handle my refund.", Some(config)).await?;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (read(&hand_backs) >= 1, format!("worker called hand_back ({} calls)", read(&hand_backs))),
        (
            read(&coordinator_calls) == 1,
            format!(
                "the coordinator, which worker may not transfer to, gets no second turn \
                 ({} coordinator model calls)",
                read(&coordinator_calls)
            ),
        ),
    ]))
}

// ===== AR8: `${x}`, `{{x}}` and `\{x}` in instructions stay literal =====

const BRACE_INSTRUCTION: &str = "You are desk. Answer with the topic in one word.\n\
Shell example: echo ${HOME}\n\
Template example: Hello {{customer_name}}!\n\
Escaped: \\{not_a_var}\n\
Topic: {topic}. Nickname: [{nickname?}]";

const BRACE_EXPECTED: &str = "You are desk. Answer with the topic in one word.\n\
Shell example: echo ${HOME}\n\
Template example: Hello {{customer_name}}!\n\
Escaped: \\{not_a_var}\n\
Topic: billing. Nickname: []";

fn topic_state() -> HashMap<String, Value> {
    HashMap::from([("topic".to_string(), json!("billing"))])
}

async fn ar8_literal_braces() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_turn: &Turn| Reply::text("billing"))).await?;
    let desk = LlmAgentBuilder::new("desk")
        .model(scripted_model(&server)?)
        .instruction(BRACE_INSTRUCTION)
        .build()?;
    let store = Store::with_session(USER, SESSION, topic_state()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let outcome = run(&runner, USER, SESSION, "What is the topic?", None).await?;
    let system = server
        .requests_from("desk")
        .first()
        .map(|request| request.texts("system"))
        .unwrap_or_default();
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            system.contains(BRACE_EXPECTED),
            format!(
                "the model receives the literal text with {{topic}} filled in: {}",
                brief(&system)
            ),
        ),
    ]))
}

async fn live_ar8_literal_braces() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let desk =
        LlmAgentBuilder::new("desk").model(gemini(&key)?).instruction(BRACE_INSTRUCTION).build()?;
    let store = Store::with_session(USER, SESSION, topic_state()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let outcome = run(&runner, USER, SESSION, "What is the topic?", None).await?;
    let answer = outcome.final_text();
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            answer.to_lowercase().contains("billing"),
            format!("Gemini answers from the filled-in topic ({})", brief(&answer)),
        ),
    ]))
}

// ===== AR9: after-agent callbacks run on every exit except a held call =====

async fn ar9_after_agent_exit_paths() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();

    // Transfer: the agent that hands off still finishes its own lifecycle.
    {
        let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.agent.as_str() {
            "coordinator" => Reply::call("transfer_to_agent", json!({"agent_name": "worker"})),
            _ => Reply::text("worker done"),
        }))
        .await?;
        let model = scripted_model(&server)?;
        let (coordinator_after, worker_after) = (counter(), counter());
        let worker = agent("worker", model.clone())
            .after_callback(count_after_agent(worker_after.clone()))
            .build()?;
        let coordinator = agent("coordinator", model)
            .after_callback(count_after_agent(coordinator_after.clone()))
            .sub_agent(Arc::new(worker))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(coordinator), None)?;
        let outcome = run(&runner, USER, SESSION, "refund please", None).await?;
        checks.push(no_error(&outcome, "the transfer run"));
        checks.push((
            read(&coordinator_after) == 1 && read(&worker_after) == 1,
            format!(
                "transfer: after-agent runs for the coordinator ({}) and the worker ({})",
                read(&coordinator_after),
                read(&worker_after)
            ),
        ));
    }

    // A tool result that ends the turn.
    {
        let server =
            ScriptedServer::start(Box::new(|_turn: &Turn| Reply::call("file_ticket", json!({}))))
                .await?;
        let after = counter();
        let desk = agent("desk", scripted_model(&server)?)
            .tool(terminal_tool("file_ticket"))
            .after_callback(count_after_agent(after.clone()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let outcome = run(&runner, USER, SESSION, "file a ticket", None).await?;
        checks.push(no_error(&outcome, "the terminal-tool run"));
        checks.push((
            read(&after) == 1,
            format!("turn-ending tool result: after-agent runs ({})", read(&after)),
        ));
    }

    // Cancellation while a tool is running.
    {
        let server =
            ScriptedServer::start(Box::new(|_turn: &Turn| Reply::call("slow_lookup", json!({}))))
                .await?;
        let (after, finished) = (counter(), counter());
        let desk = agent("desk", scripted_model(&server)?)
            .tool(slow_tool("slow_lookup", Duration::from_secs(3), finished))
            .after_callback(count_after_agent(after.clone()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        run_and_stop_at_call(&runner, USER, SESSION, "look it up", Stop::Interrupt, None).await?;
        let seen = wait_for(&after, 1, Duration::from_secs(2)).await;
        checks.push((seen == 1, format!("cancellation: after-agent runs ({seen})")));
    }

    // A held call pauses the agent: its after-agent stage waits for the resumed run.
    {
        let server = ScriptedServer::start(Box::new(|_turn: &Turn| {
            Reply::call("refund", json!({"amount": 5}))
        }))
        .await?;
        let after = counter();
        let refund: Arc<dyn Tool> =
            Arc::new(FunctionTool::new("refund", "Refunds an order.", |_ctx, _args| async move {
                Ok(json!({"status": "refunded"}))
            }));
        let desk = agent("desk", scripted_model(&server)?)
            .tool(refund)
            .require_tool_confirmation("refund")
            .after_callback(count_after_agent(after.clone()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let outcome = run(&runner, USER, SESSION, "refund me", None).await?;
        let held = outcome.events.iter().any(|event| event.actions.tool_confirmation.is_some());
        checks.push((held, "held call: the run asks for a confirmation".to_string()));
        checks.push((
            read(&after) == 0,
            format!("held call: after-agent does not run yet ({})", read(&after)),
        ));
    }

    // A normal answer.
    {
        let server = ScriptedServer::start(Box::new(|_turn: &Turn| Reply::text("hello"))).await?;
        let after = counter();
        let desk = agent("desk", scripted_model(&server)?)
            .after_callback(count_after_agent(after.clone()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let outcome = run(&runner, USER, SESSION, "hi", None).await?;
        checks.push(no_error(&outcome, "the answer run"));
        checks.push((read(&after) == 1, format!("answer: after-agent runs ({})", read(&after))));
    }

    Ok(verdict(checks))
}

async fn live_ar9_after_agent_exit_paths() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let model = gemini(&key)?;
    let mut checks = Vec::new();
    {
        let (coordinator_after, worker_after) = (counter(), counter());
        let worker = LlmAgentBuilder::new("worker")
            .model(model.clone())
            .description("Handles refund requests.")
            .instruction("You are worker. Reply with the single word REFUNDED.")
            .after_callback(count_after_agent(worker_after.clone()))
            .build()?;
        let coordinator = LlmAgentBuilder::new("coordinator")
            .model(model.clone())
            .instruction(
                "You are coordinator. Hand every request to the worker agent by calling \
                 transfer_to_agent with agent_name \"worker\". Never answer yourself.",
            )
            .after_callback(count_after_agent(coordinator_after.clone()))
            .sub_agent(Arc::new(worker))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(coordinator), None)?;
        let outcome = run(&runner, USER, SESSION, "Please refund order A1.", None).await?;
        checks.push(no_error(&outcome, "the transfer run"));
        checks.push((
            read(&coordinator_after) == 1 && read(&worker_after) == 1,
            format!(
                "transfer: after-agent runs for the coordinator ({}) and the worker ({})",
                read(&coordinator_after),
                read(&worker_after)
            ),
        ));
    }
    {
        let after = counter();
        let desk = LlmAgentBuilder::new("desk")
            .model(model)
            .instruction("You are desk. Always call the file_ticket tool for the user's request.")
            .tool(terminal_tool("file_ticket"))
            .after_callback(count_after_agent(after.clone()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let outcome =
            run(&runner, USER, SESSION, "My printer is broken, file a ticket.", None).await?;
        let filed = !responses_for(&outcome.events, "file_ticket").is_empty();
        checks.push(no_error(&outcome, "the terminal-tool run"));
        checks.push((filed, "Gemini called file_ticket".to_string()));
        checks.push((
            read(&after) == 1,
            format!("turn-ending tool result: after-agent runs ({})", read(&after)),
        ));
    }
    Ok(verdict(checks))
}

// ===== AR10: after_run runs when the consumer drops the stream =====

fn after_run_plugin(after_run: Counter) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(vec![Plugin::new(PluginConfig {
        name: "lifecycle".to_string(),
        after_run: Some(Box::new(move |_ctx| {
            let after_run = after_run.clone();
            Box::pin(async move {
                after_run.fetch_add(1, Ordering::SeqCst);
            })
        })),
        ..Default::default()
    })]))
}

async fn dropped_stream_checks(
    model: Arc<dyn Llm>,
    instruction: &str,
    tool_time: Duration,
) -> anyhow::Result<Vec<(bool, String)>> {
    let (after_run, finished) = (counter(), counter());
    let desk = LlmAgentBuilder::new("desk")
        .model(model)
        .instruction(instruction)
        .tool(slow_tool("slow_lookup", tool_time, finished.clone()))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), Some(after_run_plugin(after_run.clone())))?;
    let outcome =
        run_and_stop_at_call(&runner, USER, SESSION, "Look up order A1.", Stop::Drop, None).await?;
    let call_id = first_call_id(&outcome.events);
    let seen = wait_for(&after_run, 1, Duration::from_secs(3)).await;
    // The answer is persisted by the task that runs after_run.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let events = store.events(USER, SESSION).await?;
    let answer = call_id.as_deref().and_then(|id| answer_for(&events, id));
    Ok(vec![
        (call_id.is_some(), format!("the model called slow_lookup ({call_id:?})")),
        (seen == 1, format!("after_run runs once after the client disconnects ({seen})")),
        (
            answer == Some(true),
            format!("the session answers the interrupted call as outcome-unknown ({answer:?})"),
        ),
        (
            read(&finished) == 0,
            format!("the dropped tool call did not finish ({})", read(&finished)),
        ),
        (runner.active_runs().is_empty(), "the run is no longer active".to_string()),
    ])
}

async fn ar10_after_run_on_dropped_stream() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
        0 => Reply::call("slow_lookup", json!({"order": "A1"})),
        _ => Reply::text("found it"),
    }))
    .await?;
    Ok(verdict(
        dropped_stream_checks(scripted_model(&server)?, "You are desk.", Duration::from_secs(3))
            .await?,
    ))
}

async fn live_ar10_after_run_on_dropped_stream() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    Ok(verdict(
        dropped_stream_checks(
            gemini(&key)?,
            "You are desk. Always call slow_lookup for the user's request before answering.",
            Duration::from_secs(5),
        )
        .await?,
    ))
}

// ===== AR13: plugin on_model_error runs, and its response answers the turn =====

/// A plugin whose `on_model_error` answers the turn: from `fallback` when set, else with a
/// fixed text.
fn fallback_plugin(
    errors: Arc<Mutex<Vec<String>>>,
    fallback: Option<Arc<dyn Llm>>,
) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(vec![Plugin::new(PluginConfig {
        name: "model-fallback".to_string(),
        on_model_error: Some(Box::new(move |_ctx, mut request, error| {
            let errors = errors.clone();
            let fallback = fallback.clone();
            Box::pin(async move {
                errors.lock().unwrap().push(error);
                let Some(model) = fallback else {
                    return Ok(Some(LlmResponse {
                        content: Some(Content::new("model").with_text("fallback answer")),
                        turn_complete: true,
                        ..Default::default()
                    }));
                };
                request.model = model.name().to_string();
                let mut responses = model.generate_content(request, false).await?;
                let mut answer = None;
                while let Some(response) = responses.next().await {
                    answer = Some(response?);
                }
                Ok(answer)
            })
        })),
        ..Default::default()
    })]))
}

async fn ar13_on_model_error_fallback() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();
    for (case, reply) in
        [("HTTP 400", Reply::http_error(400)), ("stream error frame", Reply::broken_stream("par"))]
    {
        let server = ScriptedServer::start(Box::new(move |_turn: &Turn| reply.clone())).await?;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let desk = agent("desk", scripted_model(&server)?).build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), Some(fallback_plugin(errors.clone(), None)))?;
        let outcome = run(&runner, USER, SESSION, "hello", None).await?;
        let seen = errors.lock().unwrap().len();
        let persisted = store
            .events(USER, SESSION)
            .await?
            .iter()
            .filter(|event| event.author == "desk")
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(Part::text)
            .collect::<String>();
        checks.push(no_error(&outcome, &format!("the {case} run")));
        checks.push((seen == 1, format!("{case}: on_model_error runs once ({seen})")));
        checks.push((
            outcome.final_text() == "fallback answer" && persisted.contains("fallback answer"),
            format!(
                "{case}: the plugin's response answers the turn and is persisted (answer: {}, persisted: {})",
                brief(&outcome.final_text()),
                brief(&persisted)
            ),
        ));
    }
    Ok(verdict(checks))
}

async fn live_ar13_on_model_error_fallback() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    // A model ID the API does not serve: every call fails with a 404 from Google.
    let primary: Arc<dyn Llm> =
        Arc::new(GeminiModel::new(key.clone(), "gemini-0.0-retired-model")?);
    let errors = Arc::new(Mutex::new(Vec::new()));
    let desk = LlmAgentBuilder::new("desk")
        .model(primary)
        .instruction("You are desk. Reply with the single word PONG.")
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner =
        store.runner(Arc::new(desk), Some(fallback_plugin(errors.clone(), Some(gemini(&key)?))))?;
    let outcome = run(&runner, USER, SESSION, "ping", None).await?;
    let seen = errors.lock().unwrap().clone();
    let answer = outcome.final_text();
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            seen.len() == 1,
            format!(
                "on_model_error runs once for the failed call ({:?})",
                seen.iter().map(|error| brief(error)).collect::<Vec<_>>()
            ),
        ),
        (
            answer.to_uppercase().contains("PONG"),
            format!("{GEMINI_MODEL} answers through the fallback plugin ({})", brief(&answer)),
        ),
    ]))
}

// ===== AR14: retry-reflect counters are per invocation and reset on success =====

fn failing_tool(name: &str, calls: Counter, succeed_on: Option<usize>) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(name, "Checks the inventory service.", move |_ctx, _args| {
        let calls = calls.clone();
        async move {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if succeed_on == Some(call) {
                Ok(json!({"in_stock": 4}))
            } else {
                Ok(json!({"error": "inventory service returned 503"}))
            }
        }
    }))
}

fn reflected(response: Option<&Value>) -> bool {
    response.is_some_and(|response| response.get("reflection").is_some())
}

async fn ar14_retry_reflect_counters() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();

    // Two users: the second user's first failure still gets a reflection.
    {
        let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
            0 | 1 => Reply::call("check_inventory", json!({})),
            _ => Reply::text("the inventory service is down"),
        }))
        .await?;
        let plugin = Arc::new(RetryReflectPluginBuilder::new().max_retries(1).build()?);
        let desk = agent("desk", scripted_model(&server)?)
            .tool(failing_tool("check_inventory", counter(), None))
            .enhanced_plugin(plugin)
            .build()?;
        let store = Store::with_session("user-a", SESSION, HashMap::new()).await?;
        store.create("user-b", SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let first = run(&runner, "user-a", SESSION, "is A1 in stock?", None).await?;
        let second = run(&runner, "user-b", SESSION, "is A1 in stock?", None).await?;
        let a = responses_for(&store.events("user-a", SESSION).await?, "check_inventory");
        let b = responses_for(&store.events("user-b", SESSION).await?, "check_inventory");
        checks.push(no_error(&first, "user-a's run"));
        checks.push(no_error(&second, "user-b's run"));
        checks.push((
            reflected(a.first()) && !reflected(a.get(1)),
            format!(
                "user-a: a reflection, then the budget of 1 is used up ({})",
                brief(&json!(a).to_string())
            ),
        ));
        checks.push((
            reflected(b.first()),
            format!(
                "user-b's first failure gets its own reflection ({})",
                brief(&json!(b).to_string())
            ),
        ));
    }

    // A success resets the tool's counter within one invocation.
    {
        let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
            0..=2 => Reply::call("check_inventory", json!({})),
            _ => Reply::text("done"),
        }))
        .await?;
        let plugin = Arc::new(RetryReflectPluginBuilder::new().max_retries(1).build()?);
        let desk = agent("desk", scripted_model(&server)?)
            .tool(failing_tool("check_inventory", counter(), Some(2)))
            .enhanced_plugin(plugin)
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), None)?;
        let outcome = run(&runner, USER, SESSION, "is A1 in stock?", None).await?;
        let responses = responses_for(&store.events(USER, SESSION).await?, "check_inventory");
        checks.push(no_error(&outcome, "the fail-succeed-fail run"));
        checks.push((
            reflected(responses.first())
                && responses.get(1).is_some_and(|response| response.get("in_stock").is_some())
                && reflected(responses.get(2)),
            format!(
                "fail, succeed, fail: the failure after a success is reflected again ({})",
                brief(&json!(responses).to_string())
            ),
        ));
    }
    Ok(verdict(checks))
}

async fn live_ar14_retry_reflect_counters() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let plugin = Arc::new(RetryReflectPluginBuilder::new().max_retries(1).build()?);
    let desk = LlmAgentBuilder::new("desk")
        .model(gemini(&key)?)
        .instruction(
            "You are desk. Call check_inventory once. If it fails, do not call it again; tell \
             the user in one sentence that the inventory service is down.",
        )
        .tool(failing_tool("check_inventory", counter(), None))
        .enhanced_plugin(plugin)
        .build()?;
    let store = Store::with_session("user-a", SESSION, HashMap::new()).await?;
    store.create("user-b", SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let first = run(&runner, "user-a", SESSION, "Is item A1 in stock?", None).await?;
    let second = run(&runner, "user-b", SESSION, "Is item A1 in stock?", None).await?;
    let a = responses_for(&store.events("user-a", SESSION).await?, "check_inventory");
    let b = responses_for(&store.events("user-b", SESSION).await?, "check_inventory");
    Ok(verdict(vec![
        no_error(&first, "user-a's run"),
        no_error(&second, "user-b's run"),
        (
            reflected(a.first()),
            format!("user-a's first failure is reflected ({} responses)", a.len()),
        ),
        (
            reflected(b.first()),
            format!("user-b's first failure is reflected too ({})", brief(&json!(b).to_string())),
        ),
    ]))
}

// ===== AR17: a before-agent short-circuit sets output_key =====

fn cached_answer(only_when: Option<&'static str>) -> adk_core::BeforeAgentCallback {
    Box::new(move |ctx| {
        Box::pin(async move {
            let text: String = ctx.user_content().parts.iter().filter_map(Part::text).collect();
            Ok(only_when
                .is_none_or(|marker| text.contains(marker))
                .then(|| Content::new("model").with_text("cached: 42")))
        })
    })
}

async fn ar17_short_circuit_output_key() -> anyhow::Result<Verdict> {
    let server =
        ScriptedServer::start(Box::new(|_turn: &Turn| Reply::text("model answer"))).await?;
    let desk = agent("desk", scripted_model(&server)?)
        .output_key("answer")
        .before_callback(cached_answer(None))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let outcome = run(&runner, USER, SESSION, "what is the answer?", None).await?;
    let state = store.state(USER, SESSION).await?;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            server.requests_from("desk").is_empty(),
            "the callback answers without a model call".to_string(),
        ),
        (
            state.get("answer") == Some(&json!("cached: 42")),
            format!("output_key holds the callback's answer ({:?})", state.get("answer")),
        ),
    ]))
}

async fn live_ar17_short_circuit_output_key() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let desk = LlmAgentBuilder::new("desk")
        .model(gemini(&key)?)
        .instruction("You are desk. Reply with the single word HELLO.")
        .output_key("answer")
        .before_callback(cached_answer(Some("cached")))
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let cached = run(&runner, USER, SESSION, "use the cached answer", None).await?;
    let after_cached = store.state(USER, SESSION).await?.get("answer").cloned();
    let live = run(&runner, USER, SESSION, "Say hello.", None).await?;
    let after_live = store.state(USER, SESSION).await?.get("answer").cloned();
    let answer = live.final_text();
    Ok(verdict(vec![
        no_error(&cached, "the short-circuited run"),
        no_error(&live, "the live run"),
        (
            after_cached == Some(json!("cached: 42")),
            format!("turn 1: output_key holds the callback's answer ({after_cached:?})"),
        ),
        (
            !answer.is_empty() && after_live == Some(json!(answer)),
            format!("turn 2: output_key holds Gemini's answer ({after_live:?})"),
        ),
    ]))
}

// ===== AR18: output_key holds the final turn's text =====

async fn ar18_output_key_last_turn() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
        0 => Reply::text_and_call("Checking the order.", "lookup", json!({"order": "A1"})),
        _ => Reply::text("It ships Monday."),
    }))
    .await?;
    let desk =
        agent("desk", scripted_model(&server)?).tool(lookup_tool()).output_key("answer").build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let outcome = run(&runner, USER, SESSION, "where is A1?", None).await?;
    let state = store.state(USER, SESSION).await?;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            state.get("answer") == Some(&json!("It ships Monday.")),
            format!(
                "output_key holds the final answer, not the text beside the tool call ({:?})",
                state.get("answer")
            ),
        ),
    ]))
}

async fn live_ar18_output_key_last_turn() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let desk = LlmAgentBuilder::new("desk")
        .model(gemini(&key)?)
        .instruction(
            "You are desk. Call lookup for the order, then state its status in one sentence.",
        )
        .tool(lookup_tool())
        .output_key("answer")
        .build()?;
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(desk), None)?;
    let outcome = run(&runner, USER, SESSION, "Where is order A1?", None).await?;
    let state = store.state(USER, SESSION).await?;
    let answer = outcome.final_text();
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (!responses_for(&outcome.events, "lookup").is_empty(), "Gemini called lookup".to_string()),
        (
            !answer.is_empty() && state.get("answer") == Some(&json!(answer)),
            format!("output_key holds the final turn's text ({:?})", state.get("answer")),
        ),
    ]))
}

// ===== AR19: ParallelAgent stops its other branches on escalate =====

async fn escalate_checks(
    model: Arc<dyn Llm>,
    scan_time: Duration,
    limit: Duration,
    live: bool,
) -> anyhow::Result<Vec<(bool, String)>> {
    let (escalator_calls, scans) = (counter(), counter());
    let (escalator_instruction, slowpoke_instruction) = if live {
        (
            "You are escalator. Always call stop_now immediately.",
            "You are slowpoke. Always call slow_scan first, then summarise its result in one word.",
        )
    } else {
        ("You are escalator.", "You are slowpoke.")
    };
    let escalator = LlmAgentBuilder::new("escalator")
        .model(model.clone())
        .instruction(escalator_instruction)
        .tool(escalate_tool("stop_now"))
        .before_model_callback(count_model_calls(escalator_calls.clone()))
        .build()?;
    let slowpoke = LlmAgentBuilder::new("slowpoke")
        .model(model)
        .instruction(slowpoke_instruction)
        .tool(slow_tool("slow_scan", scan_time, scans.clone()))
        .build()?;
    let fanout = ParallelAgent::new("fanout", vec![Arc::new(escalator), Arc::new(slowpoke)]);
    let rounds = LoopAgent::new("rounds", vec![Arc::new(fanout)]).with_max_iterations(3);
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(rounds), None)?;
    let started = Instant::now();
    let outcome = run(&runner, USER, SESSION, "Start the scan.", None).await?;
    let elapsed = started.elapsed();
    let escalated = outcome.events.iter().any(|event| event.actions.escalate);
    Ok(vec![
        no_error(&outcome, "the run"),
        (escalated, "escalator escalates".to_string()),
        (
            elapsed < limit,
            format!(
                "the workflow stops without waiting for slowpoke ({elapsed:.1?}, limit {limit:?})"
            ),
        ),
        (read(&scans) == 0, format!("slowpoke's scan is abandoned ({} finished)", read(&scans))),
        (
            read(&escalator_calls) == 1,
            format!("the loop runs one round ({} escalator model calls)", read(&escalator_calls)),
        ),
    ])
}

async fn ar19_parallel_escalate() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| {
        match (turn.agent.as_str(), turn.tool_results) {
            ("escalator", _) => Reply::call("stop_now", json!({})),
            ("slowpoke", 0) => Reply::call("slow_scan", json!({})),
            _ => Reply::text("scan finished"),
        }
    }))
    .await?;
    Ok(verdict(
        escalate_checks(
            scripted_model(&server)?,
            Duration::from_secs(4),
            Duration::from_secs(3),
            false,
        )
        .await?,
    ))
}

async fn live_ar19_parallel_escalate() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    Ok(verdict(
        escalate_checks(gemini(&key)?, Duration::from_secs(10), Duration::from_secs(8), true)
            .await?,
    ))
}

// ===== AR20: on_event sees every event the run yields =====

fn recording_plugin(seen: Arc<Mutex<HashSet<String>>>, early_exit: bool) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(vec![Plugin::new(PluginConfig {
        name: "recorder".to_string(),
        on_event: Some(Box::new(move |_ctx, event| {
            let seen = seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().insert(event.id.clone());
                Ok(None)
            })
        })),
        before_run: early_exit.then(|| -> adk_plugin::BeforeRunCallback {
            Box::new(|_ctx| {
                Box::pin(
                    async move { Ok(Some(Content::new("model").with_text("maintenance window"))) },
                )
            })
        }),
        ..Default::default()
    })]))
}

fn unseen(run: &Run, seen: &Arc<Mutex<HashSet<String>>>) -> Vec<String> {
    let seen = seen.lock().unwrap();
    run.events
        .iter()
        .filter(|event| !event.llm_response.partial && !seen.contains(&event.id))
        .map(|event| {
            let kind = if event.llm_response.error_code.is_some() {
                "budget stop"
            } else if event
                .llm_response
                .content
                .as_ref()
                .is_some_and(|content| content.role == "function")
            {
                "tool answer"
            } else {
                "message"
            };
            format!("{kind} by {}", event.author)
        })
        .collect()
}

async fn ar20_on_event_coverage() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();

    // before_run ends the run early.
    {
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let server = ScriptedServer::start(Box::new(|_turn: &Turn| Reply::text("hi"))).await?;
        let desk = agent("desk", scripted_model(&server)?).build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), Some(recording_plugin(seen.clone(), true)))?;
        let outcome = run(&runner, USER, SESSION, "hello", None).await?;
        let missed = unseen(&outcome, &seen);
        checks.push((
            !outcome.events.is_empty() && missed.is_empty(),
            format!("before_run early exit: on_event sees the exit event (missed: {missed:?})"),
        ));
    }

    // Cancellation answers the call in flight.
    {
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let server =
            ScriptedServer::start(Box::new(|_turn: &Turn| Reply::call("slow_lookup", json!({}))))
                .await?;
        let desk = agent("desk", scripted_model(&server)?)
            .tool(slow_tool("slow_lookup", Duration::from_secs(3), counter()))
            .build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), Some(recording_plugin(seen.clone(), false)))?;
        let outcome =
            run_and_stop_at_call(&runner, USER, SESSION, "look it up", Stop::Interrupt, None)
                .await?;
        let answered = !responses_for(&outcome.events, "slow_lookup").is_empty();
        let missed = unseen(&outcome, &seen);
        checks.push((answered, "cancellation: the run answers the interrupted call".to_string()));
        checks.push((
            missed.is_empty(),
            format!("cancellation: on_event sees the answer (missed: {missed:?})"),
        ));
    }

    // The run budget stops the run.
    {
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.tool_results {
            0 => Reply::call("lookup", json!({})),
            _ => Reply::text("it ships Monday"),
        }))
        .await?;
        let desk = agent("desk", scripted_model(&server)?).tool(lookup_tool()).build()?;
        let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
        let runner = store.runner(Arc::new(desk), Some(recording_plugin(seen.clone(), false)))?;
        let config =
            RunConfig { budget: Some(RunBudget::new().max_model_calls(1)), ..RunConfig::default() };
        let outcome = run(&runner, USER, SESSION, "where is A1?", Some(config)).await?;
        let stopped = outcome.events.iter().any(|event| {
            event.llm_response.error_code.as_deref().is_some_and(|code| code.starts_with("budget."))
        });
        let missed = unseen(&outcome, &seen);
        checks.push((
            stopped,
            format!(
                "budget: the run ends with a budget stop event (error: {})",
                describe(&outcome.error)
            ),
        ));
        checks.push((
            missed.is_empty(),
            format!("budget: on_event sees the stop event (missed: {missed:?})"),
        ));
    }
    Ok(verdict(checks))
}

// ===== Nested agent tools: three levels on a worker thread's stack =====

/// Runs one turn and returns its events and error; an empty `text` resumes held calls.
async fn turn(runner: &Runner, text: &str, config: Option<RunConfig>) -> anyhow::Result<Run> {
    let content =
        if text.is_empty() { Content::new("user") } else { Content::new("user").with_text(text) };
    let mut stream = runner
        .run_with_config(UserId::new(USER)?, SessionId::new(SESSION)?, content, config)
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

/// `ops` delegates through three `AgentTool`s; the innermost agent's `delete_file` needs a
/// confirmation, which run 1 surfaces and run 2 approves.
async fn agent_tool_nesting_checks(model: Arc<dyn Llm>) -> anyhow::Result<Vec<(bool, String)>> {
    let deletions = counter();
    let deleted = deletions.clone();
    let delete_file: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "delete_file",
        "Deletes the file at `path`.",
        move |_ctx, _args| {
            let deleted = deleted.clone();
            async move {
                deleted.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"deleted": true}))
            }
        },
    ));
    let mut agent = LlmAgentBuilder::new("level3")
        .description("Deletes files.")
        .model(model.clone())
        .instruction("You are level3. Call delete_file with path c.txt, then reply DELETED.")
        .tool(delete_file)
        .require_tool_confirmation("delete_file")
        .build()?;
    for (name, delegate) in [("level2", "level3"), ("level1", "level2"), ("ops", "level1")] {
        agent = LlmAgentBuilder::new(name)
            .description("Delegates file work.")
            .model(model.clone())
            .instruction(format!(
                "You are {name}. Always call the {delegate} tool with the user's request, then                  reply with its result in one sentence."
            ))
            .tool(Arc::new(AgentTool::new(Arc::new(agent))))
            .build()?;
    }
    let store = Store::with_session(USER, SESSION, HashMap::new()).await?;
    let runner = store.runner(Arc::new(agent), None)?;
    let first = turn(&runner, "Please delete c.txt.", None).await?;
    let request = first.events.iter().find_map(|event| event.actions.tool_confirmation.clone());
    let mut checks = vec![
        no_error(&first, "run 1"),
        (
            request.as_ref().is_some_and(|request| request.tool_name == "delete_file"),
            // A delegate's failure, such as a rate limit, reaches the caller as level1's result.
            format!(
                "run 1 surfaces level3's delete_file for confirmation ({:?}; level1 answered {})",
                request.as_ref().map(|request| request.tool_name.clone()),
                brief(&json!(responses_for(&first.events, "level1")).to_string())
            ),
        ),
    ];
    let Some(call_id) = request.and_then(|request| request.function_call_id) else {
        return Ok(checks);
    };
    let approve = RunConfig {
        tool_confirmation_decisions: HashMap::from([(
            call_id,
            adk_core::ToolConfirmationDecision::Approve,
        )]),
        ..RunConfig::default()
    };
    let second = turn(&runner, "", Some(approve)).await?;
    checks.push(no_error(&second, "run 2"));
    checks.push((
        read(&deletions) == 1,
        format!("the approved call runs once ({})", read(&deletions)),
    ));
    Ok(checks)
}

async fn live_agent_tool_nesting() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let model = gemini(&key)?;
    // A spawned task runs on a runtime worker thread with its 2 MiB stack, as a server's
    // request handlers do, rather than on the main thread's larger stack.
    let checks = tokio::spawn(agent_tool_nesting_checks(model)).await??;
    Ok(verdict(checks))
}

// ===== AR21: Runner::close closes plugins and toolsets =====

#[cfg(feature = "main-baseline")]
async fn ar21_runner_close() -> anyhow::Result<Verdict> {
    Ok(Verdict::Fail("Runner::close and Toolset::close do not exist in this build".into()))
}

#[cfg(feature = "main-baseline")]
async fn live_ar21_runner_close() -> anyhow::Result<Verdict> {
    Ok(Verdict::Fail("Runner::close and Toolset::close do not exist in this build".into()))
}

#[cfg(not(feature = "main-baseline"))]
use close::{ar21_runner_close, live_ar21_runner_close};

// ===== Harness =====

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

pub(crate) fn google_key() -> Option<String> {
    env_key("GOOGLE_API_KEY").or_else(|| env_key("GEMINI_API_KEY"))
}

type ScenarioFn =
    fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Verdict>>>>;

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        vec![$((stringify!($name), (|| Box::pin($name()) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Verdict>>>>) as ScenarioFn)),*]
    };
}

#[tokio::main]
async fn main() -> ExitCode {
    match std::env::var("ADK_ENV_FILE") {
        // The named file wins over variables already in the environment.
        Ok(path) => {
            if let Err(error) = dotenvy::from_path_override(&path) {
                eprintln!("could not load ADK_ENV_FILE: {error}");
                return ExitCode::from(2);
            }
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .init();

    let all: Vec<(&str, ScenarioFn)> = scenarios![
        ar5_tool_transfer_restrictions,
        live_ar5_tool_transfer_restrictions,
        ar8_literal_braces,
        live_ar8_literal_braces,
        ar9_after_agent_exit_paths,
        live_ar9_after_agent_exit_paths,
        ar10_after_run_on_dropped_stream,
        live_ar10_after_run_on_dropped_stream,
        ar13_on_model_error_fallback,
        live_ar13_on_model_error_fallback,
        ar14_retry_reflect_counters,
        live_ar14_retry_reflect_counters,
        ar17_short_circuit_output_key,
        live_ar17_short_circuit_output_key,
        ar18_output_key_last_turn,
        live_ar18_output_key_last_turn,
        ar19_parallel_escalate,
        live_ar19_parallel_escalate,
        ar20_on_event_coverage,
        live_agent_tool_nesting,
        ar21_runner_close,
        live_ar21_runner_close,
    ];
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selected: Option<Vec<String>> = args
        .iter()
        .position(|arg| arg == "--scenario")
        .and_then(|index| args.get(index + 1))
        .map(|list| list.split(',').map(str::to_string).collect());
    let skip_live = args.iter().any(|arg| arg == "--no-live");

    let mut failed = 0;
    let mut rows = Vec::new();
    for (name, scenario) in all {
        if selected.as_ref().is_some_and(|names| !names.iter().any(|wanted| wanted == name)) {
            continue;
        }
        if skip_live && name.starts_with("live_") {
            continue;
        }
        let mut outcome = tokio::time::timeout(Duration::from_secs(120), scenario()).await;
        // A free-tier key runs out of requests per minute; the scenario runs again once.
        if name.starts_with("live_")
            && matches!(&outcome, Ok(Ok(Verdict::Fail(evidence))) if evidence.contains("rate_limited"))
        {
            println!("RETRY {name} after a rate limit; waiting {RATE_LIMIT_BACKOFF:?}");
            tokio::time::sleep(RATE_LIMIT_BACKOFF).await;
            outcome = tokio::time::timeout(Duration::from_secs(120), scenario()).await;
        }
        let (status, evidence) = match outcome {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failed += 1;
                ("FAIL", evidence)
            }
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Err(error)) => {
                failed += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failed += 1;
                ("FAIL", "scenario timed out after 120 s".to_string())
            }
        };
        println!("{status} {name}\n      {evidence}");
        rows.push((name, status));
    }
    println!("\n| Scenario | Result |\n|---|---|");
    for (name, status) in rows {
        println!("| `{name}` | {status} |");
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
