//! History correctness: what each agent's model receives, and what the session keeps.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder` agents, `SequentialAgent`,
//! `LoopAgent`, an `AgentTool`, `Runner` with intra-invocation compaction, session compaction
//! (`EventsCompactionConfig` with `LlmEventSummarizer`) and context compaction
//! (`TruncationCompaction`, `SummarisationCompaction`), over a SQLite session service. A
//! scripted OpenAI-compatible upstream on 127.0.0.1 plays the model through the real
//! `OpenAIClient` and records every request, so each scenario asserts on exactly what a
//! provider would receive. `ScriptedLlm` plays a provider whose stream ends on a partial chunk.
//! The `live_*` scenarios run against Gemini, Anthropic and OpenAI when their keys are set.
//!
//! ```bash
//! cargo run --manifest-path examples/history_correctness/Cargo.toml
//! cargo run --manifest-path examples/history_correctness/Cargo.toml -- --scenario ar1_sequential_foreign_turns
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use adk_agent::{LlmAgentBuilder, LlmEventSummarizer, LoopAgent, SequentialAgent};
use adk_core::{
    AdkError, Agent, Content, Event, EventsCompactionConfig, IntraCompactionConfig, Llm,
    LlmResponse, RunConfig, SessionId, Tool, ToolContext, UserId,
};
use adk_model::anthropic::AnthropicConfig;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{AnthropicClient, GeminiModel, OpenAIClient, ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_runner::compaction::{CompactionConfig, SummarisationCompaction, TruncationCompaction};
use adk_session::{CreateRequest, GetRequest, SessionService, SqliteSessionService};
use adk_tool::{AgentTool, FunctionTool};
use futures::StreamExt;
use scripted::{Recorded, Reply, Script, ScriptedServer, Turn};
use serde_json::{Value, json};

const APP: &str = "history-correctness";
const USER: &str = "customer";
const SESSION: &str = "session-1";
const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
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

/// A SQLite session service holding one empty session.
struct Store {
    sessions: Arc<dyn SessionService>,
    _dir: tempfile::TempDir,
}

impl Store {
    async fn open() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("sessions.db").display());
        let service = SqliteSessionService::new(&url).await?;
        service.migrate().await?;
        service
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(SESSION.to_string()),
                state: HashMap::new(),
            })
            .await?;
        Ok(Self { sessions: Arc::new(service), _dir: dir })
    }

    async fn session(&self) -> anyhow::Result<Box<dyn adk_session::Session>> {
        Ok(self
            .sessions
            .get(GetRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: SESSION.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?)
    }

    async fn events(&self) -> anyhow::Result<Vec<Event>> {
        Ok(self.session().await?.events().all())
    }

    async fn state(&self) -> anyhow::Result<HashMap<String, Value>> {
        Ok(self.session().await?.state().all())
    }

    fn runner(&self, agent: Arc<dyn Agent>) -> anyhow::Result<Runner> {
        Ok(Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(self.sessions.clone())
            .build()?)
    }
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
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| part.text())
            .find(|text| !text.trim().is_empty())
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
        Some(error) => format!("{} ({})", error.code, brief(&error.message)),
        None => "none".to_string(),
    }
}

fn brief(text: &str) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() > 160 {
        format!("{}…", text.chars().take(160).collect::<String>())
    } else {
        text
    }
}

fn no_error(run: &Run, what: &str) -> (bool, String) {
    (run.error.is_none(), format!("{what} ends without an error (error: {})", describe(&run.error)))
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base, "scripted-model")?))
}

fn agent(name: &str, model: Arc<dyn Llm>, tools: Vec<Arc<dyn Tool>>) -> adk_agent::LlmAgentBuilder {
    tools.into_iter().fold(
        LlmAgentBuilder::new(name).model(model).instruction(format!("You are {name}. Be brief.")),
        |builder, tool| builder.tool(tool),
    )
}

/// Looks up an order; the payload has `padding` extra characters.
fn lookup_tool(padding: usize) -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new("lookup", "Looks up an order.", move |_ctx, _args| async move {
        Ok(json!({
            "status": "ships Monday",
            "warehouse": "Nairobi-7",
            "notes": "n".repeat(padding),
        }))
    }))
}

/// The request `agent` sent on its `index`-th model call, from 1.
fn nth(server: &ScriptedServer, agent: &str, index: usize) -> Option<Recorded> {
    server.requests_from(agent).into_iter().nth(index - 1)
}

fn well_formed(request: &Option<Recorded>, who: &str) -> (bool, String) {
    let bad = request.as_ref().map(Recorded::malformed_tool_results).unwrap_or_default();
    (
        request.is_some() && bad.is_empty(),
        format!("{who}: every tool result answers a call in the same request (bad: {bad:?})"),
    )
}

fn ends_on_user(request: &Option<Recorded>, who: &str) -> (bool, String) {
    let last = request.as_ref().map(Recorded::last_role).unwrap_or_default();
    (last == "user" || last == "tool", format!("{who} ends on a user-side message (last: {last})"))
}

fn roles(request: &Option<Recorded>) -> Vec<String> {
    request.as_ref().map(Recorded::roles).unwrap_or_default()
}

fn texts(request: &Option<Recorded>, role: &str) -> String {
    request.as_ref().map(|request| request.texts(role).join(" | ")).unwrap_or_default()
}

// ===== AR1: workflow sub-agents see earlier agents' turns as context =====

async fn ar1_sequential_foreign_turns() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.agent.as_str() {
        "a" if turn.last_role == "tool" => Reply::text("a done: order A1 ships Monday"),
        "a" => Reply::call("lookup", json!({"order": "A1"})),
        _ => Reply::text("b done"),
    }))
    .await?;
    let model = scripted_model(&server)?;
    let a = agent("a", model.clone(), vec![lookup_tool(0)]).build()?;
    let b = agent("b", model, Vec::new()).build()?;
    let store = Store::open().await?;
    let runner =
        store.runner(Arc::new(SequentialAgent::new("pipeline", vec![Arc::new(a), Arc::new(b)])))?;
    let outcome = run(&runner, "where is order A1?", None).await?;
    let b_request = nth(&server, "b", 1);
    let b_roles = roles(&b_request);
    Ok(verdict(vec![
        no_error(&outcome, "Sequential[a, b]"),
        (
            !b_roles.iter().any(|role| role == "assistant" || role == "tool"),
            format!("b's request carries no assistant or tool turn of a's (roles: {b_roles:?})"),
        ),
        ends_on_user(&b_request, "b's request"),
        (
            texts(&b_request, "user").contains("a done"),
            "b sees a's answer as user-role context".to_string(),
        ),
        (
            texts(&b_request, "user").contains("ships Monday"),
            "b sees a's tool result as user-role context".to_string(),
        ),
    ]))
}

async fn ar1_loop_foreign_turns() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.agent.as_str() {
        "a" if turn.last_role == "tool" => Reply::text("a done round 1"),
        "a" if turn.fresh == 1 => Reply::call("lookup", json!({"order": "A1"})),
        "a" => Reply::text(format!("a done round {}", turn.fresh)),
        _ => Reply::text(format!("b done round {}", turn.fresh)),
    }))
    .await?;
    let model = scripted_model(&server)?;
    let a = agent("a", model.clone(), vec![lookup_tool(0)]).build()?;
    let b = agent("b", model, Vec::new()).build()?;
    let store = Store::open().await?;
    let runner = store.runner(Arc::new(
        LoopAgent::new("loop", vec![Arc::new(a), Arc::new(b)]).with_max_iterations(2),
    ))?;
    let outcome = run(&runner, "where is order A1?", None).await?;
    // a: call, after tool, round 2. b: round 1, round 2.
    let a_round_2 = nth(&server, "a", 3);
    let b_round_2 = nth(&server, "b", 2);
    Ok(verdict(vec![
        no_error(&outcome, "LoopAgent[a, b] x2"),
        (
            texts(&a_round_2, "assistant").contains("a done round 1")
                && !texts(&a_round_2, "assistant").contains("b done"),
            format!(
                "a's round-2 request keeps its own answer as its turn and none of b's (assistant: {})",
                brief(&texts(&a_round_2, "assistant"))
            ),
        ),
        (
            texts(&a_round_2, "user").contains("b done round 1"),
            "a's round-2 request carries b's answer as user-role context".to_string(),
        ),
        ends_on_user(&a_round_2, "a's round-2 request"),
        (
            texts(&b_round_2, "assistant").contains("b done round 1")
                && !texts(&b_round_2, "assistant").contains("a done"),
            format!(
                "b's round-2 request keeps only its own answers as its turns (assistant: {})",
                brief(&texts(&b_round_2, "assistant"))
            ),
        ),
        ends_on_user(&b_round_2, "b's round-2 request"),
        well_formed(&a_round_2, "a round 2"),
    ]))
}

async fn live_ar1_gemini_then_claude() -> anyhow::Result<Verdict> {
    let (Some(google), Some(anthropic)) = (google_key(), env_key("ANTHROPIC_API_KEY")) else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY and ANTHROPIC_API_KEY are both needed".into()));
    };
    let gemini: Arc<dyn Llm> = Arc::new(GeminiModel::new(google, GEMINI_MODEL)?);
    let claude: Arc<dyn Llm> =
        Arc::new(AnthropicClient::new(AnthropicConfig::new(anthropic, ANTHROPIC_MODEL))?);
    let a = LlmAgentBuilder::new("researcher")
        .model(gemini)
        .instruction(
            "Call the lookup tool once for the order, then state its status in one sentence.",
        )
        .tool(lookup_tool(0))
        .build()?;
    let b = LlmAgentBuilder::new("writer")
        .model(claude)
        .instruction("Write a one-sentence reply to the customer from the research above.")
        .build()?;
    let store = Store::open().await?;
    let runner =
        store.runner(Arc::new(SequentialAgent::new("pipeline", vec![Arc::new(a), Arc::new(b)])))?;
    let outcome = run(&runner, "Where is order A1?", None).await?;
    let writer_text = outcome
        .events
        .iter()
        .filter(|event| event.author == "writer" && !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text())
        .collect::<String>();
    Ok(verdict(vec![
        no_error(&outcome, &format!("Sequential[{GEMINI_MODEL}, {ANTHROPIC_MODEL}]")),
        (!writer_text.trim().is_empty(), format!("the writer answered: {}", brief(&writer_text))),
    ]))
}

// ===== AR2 + SS1: history windows and compaction cut only at balanced points =====

/// `desk` looks the order up in its first run and answers plainly afterwards.
fn desk_script(first_answer: &'static str) -> Script {
    Box::new(move |turn: &Turn| match turn.agent.as_str() {
        "desk" if turn.last_role == "tool" => Reply::text(first_answer),
        "desk" if turn.fresh == 1 => Reply::call("lookup", json!({"order": "A1"})),
        "desk" => Reply::text(format!("answer {}", turn.fresh)),
        "summarizer" => Reply::text("Summary: the customer asked about order A1."),
        _ => Reply::text("ok"),
    })
}

async fn ar2_history_window_orphan() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(desk_script("order A1 ships Monday")).await?;
    let desk = agent("desk", scripted_model(&server)?, vec![lookup_tool(0)]).build()?;
    let store = Store::open().await?;
    let runner = store.runner(Arc::new(desk))?;
    let first = run(&runner, "where is order A1?", None).await?;
    // The two most recent events are the tool result and the answer, so the window starts on
    // a result whose call it cut.
    let window = RunConfig::builder().history_max_events(Some(2)).build();
    let second = run(&runner, "thanks, anything else?", Some(window)).await?;
    let request = nth(&server, "desk", 3);
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2 with history_max_events = 2"),
        well_formed(&request, "run 2's request"),
        (!roles(&request).is_empty(), format!("run 2's request roles: {:?}", roles(&request))),
    ]))
}

/// Builds a session whose first run made a call, then answers run 2 with `model` over a
/// two-event window, which on its own would start on the call's result.
async fn live_window(model: Arc<dyn Llm>, label: &str) -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(desk_script("order A1 ships Monday")).await?;
    let store = Store::open().await?;
    let scripted = agent("desk", scripted_model(&server)?, vec![lookup_tool(0)]).build()?;
    let first = run(&store.runner(Arc::new(scripted))?, "where is order A1?", None).await?;
    let live = LlmAgentBuilder::new("desk")
        .model(model)
        .instruction("You answer order questions in one short sentence.")
        .tool(lookup_tool(0))
        .build()?;
    let window = RunConfig::builder().history_max_events(Some(2)).build();
    let second =
        run(&store.runner(Arc::new(live))?, "Thanks. Reply with one word: ok.", Some(window))
            .await?;
    Ok(verdict(vec![
        no_error(&first, "the scripted first run"),
        no_error(&second, &format!("{label} over a window that cuts the call")),
        (
            !second.final_text().is_empty(),
            format!("{label} answered: {}", brief(&second.final_text())),
        ),
    ]))
}

async fn live_ar2_anthropic() -> anyhow::Result<Verdict> {
    let Some(key) = env_key("ANTHROPIC_API_KEY") else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    live_window(
        Arc::new(AnthropicClient::new(AnthropicConfig::new(key, ANTHROPIC_MODEL))?),
        ANTHROPIC_MODEL,
    )
    .await
}

async fn live_ar2_openai() -> anyhow::Result<Verdict> {
    let Some(key) = env_key("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    live_window(
        Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(key, OPENAI_MODEL))?),
        OPENAI_MODEL,
    )
    .await
}

async fn live_ar2_gemini() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    live_window(Arc::new(GeminiModel::new(key, GEMINI_MODEL)?), GEMINI_MODEL).await
}

async fn ss1_intra_compaction_cut() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(desk_script("order A1 ships Monday")).await?;
    let model = scripted_model(&server)?;
    let desk = agent("desk", model.clone(), vec![lookup_tool(0)]).build()?;
    let store = Store::open().await?;
    // Run 2 starts with [q1, call, result, answer, q2]; keeping three events cuts between the
    // call and its result.
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(desk))
        .session_service(store.sessions.clone())
        .intra_compaction_config(IntraCompactionConfig {
            token_threshold: 50,
            overlap_event_count: 3,
            chars_per_token: 4,
        })
        .intra_compaction_summarizer(Arc::new(LlmEventSummarizer::new(model)))
        .build()?;
    let long_question = format!("where is order A1? {}", "Some background. ".repeat(60));
    let first = run(&runner, &long_question, None).await?;
    let second = run(&runner, "and the warehouse?", None).await?;
    let request = nth(&server, "desk", 3);
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2"),
        (
            !server.requests_from("summarizer").is_empty(),
            "intra-invocation compaction ran before run 2".to_string(),
        ),
        well_formed(&request, "run 2's request"),
    ]))
}

async fn ss1_truncation_compaction_cut() -> anyhow::Result<Verdict> {
    let long_answer: &'static str =
        Box::leak(format!("A long answer. {}", "word ".repeat(400)).into_boxed_str());
    let server = ScriptedServer::start(Box::new(move |turn: &Turn| match turn.agent.as_str() {
        "desk" if turn.last_role == "tool" => Reply::text("order A1 ships Monday"),
        "desk" if turn.fresh == 1 => Reply::text(long_answer),
        "desk" if turn.fresh == 2 => Reply::call("lookup", json!({"order": "A1"})),
        _ => Reply::text(format!("answer {}", turn.fresh)),
    }))
    .await?;
    let desk = agent("desk", scripted_model(&server)?, vec![lookup_tool(0)]).build()?;
    let store = Store::open().await?;
    // Run 3 starts with [q1, long answer, q2, call, result, answer, q3]; keeping the first
    // event and the last three cuts between the call and its result.
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(desk))
        .session_service(store.sessions.clone())
        .context_compaction(CompactionConfig::new(
            Box::new(TruncationCompaction { preserve_recent: 3 }),
            300,
        ))
        .build()?;
    let runs = [
        run(&runner, "hello", None).await?,
        run(&runner, "where is order A1?", None).await?,
        run(&runner, "thanks", None).await?,
    ];
    let request = nth(&server, "desk", 4);
    let raw = request.as_ref().map(Recorded::raw).unwrap_or_default();
    Ok(verdict(vec![
        no_error(&runs[0], "run 1"),
        no_error(&runs[1], "run 2"),
        no_error(&runs[2], "run 3"),
        (!raw.contains("A long answer"), "run 3's request was truncated".to_string()),
        well_formed(&request, "run 3's request"),
    ]))
}

async fn ss1_summarisation_compaction_cut() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(desk_script("order A1 ships Monday")).await?;
    let model = scripted_model(&server)?;
    let desk = agent("desk", model.clone(), vec![lookup_tool(0)]).build()?;
    let store = Store::open().await?;
    // Run 2 starts with [q1, call, result, answer, q2]; summarising two events cuts between
    // the call and its result.
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(desk))
        .session_service(store.sessions.clone())
        .context_compaction(CompactionConfig::new(
            Box::new(SummarisationCompaction { model, turns_to_summarise: 2 }),
            300,
        ))
        .build()?;
    let long_question = format!("where is order A1? {}", "Some background. ".repeat(120));
    let first = run(&runner, &long_question, None).await?;
    let second = run(&runner, "and the warehouse?", None).await?;
    let request = nth(&server, "desk", 3);
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2"),
        (
            !server.requests_from("summarizer").is_empty(),
            "context compaction summarised before run 2".to_string(),
        ),
        well_formed(&request, "run 2's request"),
    ]))
}

// ===== SS16: compaction counts and summarises tool payloads =====

async fn ss16_tool_payload_compaction() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(desk_script("order A1 ships Monday")).await?;
    let model = scripted_model(&server)?;
    let desk = agent("desk", model.clone(), vec![lookup_tool(8_000)]).build()?;
    let store = Store::open().await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(desk))
        .session_service(store.sessions.clone())
        .context_compaction(CompactionConfig::new(
            Box::new(SummarisationCompaction { model, turns_to_summarise: 4 }),
            500,
        ))
        .build()?;
    let first = run(&runner, "where is order A1?", None).await?;
    let second = run(&runner, "and the warehouse?", None).await?;
    let summary_prompt =
        server.requests_from("summarizer").first().map(Recorded::raw).unwrap_or_default();
    let request = nth(&server, "desk", 3);
    let raw = request.as_ref().map(Recorded::raw).unwrap_or_default();
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2"),
        (
            !summary_prompt.is_empty(),
            "an 8,000-character tool result puts the history over its 500-token budget".to_string(),
        ),
        (
            summary_prompt.contains("Nairobi-7"),
            "the summary prompt carries the tool result's facts".to_string(),
        ),
        (!raw.contains("nnnnnnnnnnnnnnnn"), "run 2's request no longer carries the payload".into()),
        well_formed(&request, "run 2's request"),
    ]))
}

// ===== SS19: a summary replaces history only when it is smaller and complete =====

async fn ss19_summary_must_shrink() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();
    for (case, summary) in [
        ("verbose", Reply::text(format!("Summary: {}", "detail ".repeat(600)))),
        ("truncated", Reply::text("Summary: the customer").finishing("length")),
    ] {
        let server =
            ScriptedServer::start(Box::new(move |turn: &Turn| match turn.agent.as_str() {
                "summarizer" => summary.clone(),
                _ => Reply::text(format!("answer {}", turn.fresh)),
            }))
            .await?;
        let model = scripted_model(&server)?;
        let desk = agent("desk", model.clone(), Vec::new()).build()?;
        let store = Store::open().await?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(Arc::new(desk))
            .session_service(store.sessions.clone())
            .compaction_config(EventsCompactionConfig {
                compaction_interval: 1,
                overlap_size: 1,
                summarizer: Arc::new(LlmEventSummarizer::new(model)),
            })
            .build()?;
        run(&runner, "hi", None).await?;
        run(&runner, "and again", None).await?;
        let compactions =
            store.events().await?.iter().filter(|event| event.actions.compaction.is_some()).count();
        checks.push((
            !server.requests_from("summarizer").is_empty(),
            format!("{case}: the summarizer was asked"),
        ));
        checks.push((
            compactions == 0,
            format!("{case}: the summary does not replace the history (compaction events: {compactions})"),
        ));
    }
    Ok(verdict(checks))
}

// ===== AR7: temp: state is visible for the rest of its invocation =====

async fn ar7_temp_state_in_invocation() -> anyhow::Result<Verdict> {
    let seen = Arc::new(Mutex::new(None::<Value>));
    let login: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "login",
        "Signs in.",
        |ctx: Arc<dyn ToolContext>, _args| async move {
            let mut actions = ctx.actions();
            actions.state_delta.insert("temp:token".into(), json!("tok-123"));
            actions.state_delta.insert("signed_in".into(), json!(true));
            ctx.set_actions(actions);
            Ok(json!({"ok": true}))
        },
    ));
    let observed = seen.clone();
    let use_token: Arc<dyn Tool> = Arc::new(FunctionTool::new(
        "use_token",
        "Uses the session token.",
        move |ctx: Arc<dyn ToolContext>, _args| {
            let observed = observed.clone();
            async move {
                let token = ctx.session().and_then(|session| session.state().get("temp:token"));
                *observed.lock().unwrap() = Some(token.clone().unwrap_or(Value::Null));
                Ok(json!({"token": token}))
            }
        },
    ));
    let server =
        ScriptedServer::start(Box::new(|turn: &Turn| match (turn.agent.as_str(), turn.count) {
            ("front", 1) => Reply::call("login", json!({})),
            ("front", 2) => Reply::call("use_token", json!({})),
            ("front", _) => Reply::call("transfer_to_agent", json!({"agent_name": "back"})),
            _ => Reply::text("back done"),
        }))
        .await?;
    let model = scripted_model(&server)?;
    let back = LlmAgentBuilder::new("back")
        .model(model.clone())
        .instruction("You are back. Your session token is {temp:token}.")
        .build()?;
    let front = agent("front", model, vec![login, use_token]).sub_agent(Arc::new(back)).build()?;
    let store = Store::open().await?;
    let outcome = run(&store.runner(Arc::new(front))?, "sign me in", None).await?;
    let token_seen = seen.lock().unwrap().clone();
    let back_instruction = texts(&nth(&server, "back", 1), "system");
    let state = store.state().await?;
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (
            token_seen == Some(json!("tok-123")),
            format!("a later tool in the invocation reads temp:token (saw {token_seen:?})"),
        ),
        (
            back_instruction.contains("tok-123"),
            "the transferred agent's {temp:token} instruction resolves".to_string(),
        ),
        (
            !state.contains_key("temp:token") && state.get("signed_in") == Some(&json!(true)),
            format!(
                "the session stores signed_in but not temp:token (keys: {:?})",
                state.keys().collect::<Vec<_>>()
            ),
        ),
    ]))
}

// ===== AR11: a stream that ends on a partial chunk is persisted =====

async fn ar11_partial_tail_persisted() -> anyhow::Result<Verdict> {
    // Deltas as a provider streams them: partial, with no finish reason, never completing.
    let chunk = |text: &str| {
        let mut chunk = LlmResponse::new(Content::new("model").with_text(text));
        chunk.partial = true;
        chunk.turn_complete = false;
        chunk.finish_reason = None;
        chunk
    };
    let llm = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::stream([chunk("Hello "), chunk("world")]),
            ScriptedResponse::text("again"),
        ],
    ));
    let desk = LlmAgentBuilder::new("desk").model(llm.clone()).build()?;
    let store = Store::open().await?;
    let runner = store.runner(Arc::new(desk))?;
    let first = run(&runner, "greet me", None).await?;
    let second = run(&runner, "again", None).await?;
    let persisted = store
        .events()
        .await?
        .iter()
        .filter(|event| event.author == "desk")
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text().map(str::to_string))
        .collect::<Vec<_>>();
    let second_request = llm.requests().get(1).cloned();
    let replayed = second_request
        .map(|request| {
            request
                .contents
                .iter()
                .filter(|content| content.role == "model")
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| part.text().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2"),
        (
            persisted.iter().any(|text| text == "Hello world"),
            format!("the session keeps the streamed answer (desk texts: {persisted:?})"),
        ),
        (
            replayed.iter().any(|text| text == "Hello world"),
            format!("run 2's request carries the answer as the model's turn ({replayed:?})"),
        ),
    ]))
}

// ===== AR12: an empty model turn is an error =====

async fn ar12_empty_stop_turns() -> anyhow::Result<Verdict> {
    let mut checks = Vec::new();
    for (case, reply) in [
        ("empty", Reply::default()),
        ("whitespace", Reply::text("   ")),
        ("thinking only", Reply::thinking_only("Let me think about that.")),
    ] {
        let server = ScriptedServer::start(Box::new(move |_turn: &Turn| reply.clone())).await?;
        let desk =
            agent("desk", scripted_model(&server)?, Vec::new()).output_key("answer").build()?;
        let store = Store::open().await?;
        let outcome = run(&store.runner(Arc::new(desk))?, "hello?", None).await?;
        let state = store.state().await?;
        let code = outcome.error.as_ref().map(|error| error.code).unwrap_or("none");
        checks.push((
            code == "model.empty_response",
            format!(
                "{case}: the run fails with model.empty_response (error: {})",
                describe(&outcome.error)
            ),
        ));
        checks.push((
            !state.contains_key("answer"),
            format!("{case}: output_key is not written ({:?})", state.get("answer")),
        ));
    }
    Ok(verdict(checks))
}

// ===== AR6: an after_model_callback replacement keeps the chunk's stream flags and usage =====

async fn ar6_after_model_replacement() -> anyhow::Result<Verdict> {
    let server =
        ScriptedServer::start(Box::new(|_turn: &Turn| Reply::deltas(&["Hel", "lo ", "there"])))
            .await?;
    let desk = agent("desk", scripted_model(&server)?, Vec::new())
        .after_model_callback(Box::new(|_ctx, response: LlmResponse| {
            Box::pin(async move {
                let Some(content) = response.content else {
                    return Ok(None);
                };
                let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
                // A redaction callback that rebuilds the response from its content alone.
                Ok(Some(LlmResponse {
                    content: Some(Content::new("model").with_text(text.to_uppercase())),
                    ..Default::default()
                }))
            })
        }))
        .build()?;
    let store = Store::open().await?;
    let runner = store.runner(Arc::new(desk))?;
    let first = run(&runner, "hi", None).await?;
    let second = run(&runner, "again", None).await?;
    let events = store.events().await?;
    let answers: Vec<&Event> = events
        .iter()
        .filter(|event| {
            event.author == "desk"
                && event.invocation_id
                    == first.events.first().map(|e| e.invocation_id.clone()).unwrap_or_default()
        })
        .filter(|event| event.llm_response.content.is_some())
        .collect();
    let texts_seen: Vec<String> = answers
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .map(|content| content.parts.iter().filter_map(|part| part.text()).collect())
        .collect();
    let with_usage =
        answers.iter().filter(|event| event.llm_response.usage_metadata.is_some()).count();
    let replayed = texts(&nth(&server, "desk", 2), "assistant");
    Ok(verdict(vec![
        no_error(&first, "run 1 on SQLite"),
        no_error(&second, "run 2 on SQLite"),
        (
            texts_seen == ["HELLO THERE"],
            format!("run 1 persists one model event with the whole answer ({texts_seen:?})"),
        ),
        (
            with_usage == 1,
            format!("the persisted answer keeps its usage ({with_usage} with usage)"),
        ),
        (
            replayed == "HELLO THERE",
            format!("run 2's request replays the answer once ({replayed:?})"),
        ),
    ]))
}

// ===== AR15: fenced JSON passes output_schema validation =====

async fn ar15_fenced_json_schema() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|_turn: &Turn| {
        Reply::text("```json\n{\"answer\": 1}\n```")
    }))
    .await?;
    let desk = agent("desk", scripted_model(&server)?, Vec::new())
        .output_schema(json!({
            "type": "object",
            "properties": {"answer": {"type": "integer"}},
            "required": ["answer"],
        }))
        .output_key("result")
        .build()?;
    let store = Store::open().await?;
    let outcome = run(&store.runner(Arc::new(desk))?, "what is one?", None).await?;
    let calls = server.requests_from("desk").len();
    let stored = store.state().await?.get("result").cloned();
    let parsed = stored
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    Ok(verdict(vec![
        no_error(&outcome, "the run"),
        (calls == 1, format!("one model call, no correction retries ({calls} calls)")),
        (
            parsed == Some(json!({"answer": 1})),
            format!("output_key holds the JSON without its fence ({stored:?})"),
        ),
    ]))
}

// ===== AR16: forwarded AgentTool events stay out of the caller's history =====

async fn ar16_forwarded_agent_tool_events() -> anyhow::Result<Verdict> {
    let server = ScriptedServer::start(Box::new(|turn: &Turn| match turn.agent.as_str() {
        "helper" => Reply::text("helper says hi"),
        "desk" if turn.last_role == "tool" => Reply::text("desk done"),
        "desk" if turn.fresh == 1 => Reply::call("helper", json!({"request": "say hi"})),
        _ => Reply::text("desk again"),
    }))
    .await?;
    let model = scripted_model(&server)?;
    let helper = agent("helper", model.clone(), Vec::new()).build()?;
    let tool: Arc<dyn Tool> = Arc::new(AgentTool::new(Arc::new(helper)).forward_events(true));
    let desk = agent("desk", model, vec![tool]).build()?;
    let store = Store::open().await?;
    let runner = store.runner(Arc::new(desk))?;
    let first = run(&runner, "ask the helper", None).await?;
    let second = run(&runner, "thanks", None).await?;
    let request = nth(&server, "desk", 3);
    let forwarded = first.events.iter().any(|event| event.author == "helper");
    Ok(verdict(vec![
        no_error(&first, "run 1"),
        no_error(&second, "run 2"),
        (forwarded, "run 1 forwards the helper's events to the caller".to_string()),
        (
            !texts(&request, "assistant").contains("helper says hi")
                && !texts(&request, "user").contains("helper says hi"),
            format!(
                "run 2's request does not replay the helper's text as a turn (assistant: {})",
                brief(&texts(&request, "assistant"))
            ),
        ),
        (
            texts(&request, "tool").contains("helper says hi"),
            "the helper's answer still reaches the caller as the tool result".to_string(),
        ),
        well_formed(&request, "run 2's request"),
    ]))
}

// ===== Harness =====

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn google_key() -> Option<String> {
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
        ar1_sequential_foreign_turns,
        ar1_loop_foreign_turns,
        live_ar1_gemini_then_claude,
        ar2_history_window_orphan,
        live_ar2_anthropic,
        live_ar2_openai,
        live_ar2_gemini,
        ss1_intra_compaction_cut,
        ss1_truncation_compaction_cut,
        ss1_summarisation_compaction_cut,
        ss16_tool_payload_compaction,
        ss19_summary_must_shrink,
        ar7_temp_state_in_invocation,
        ar11_partial_tail_persisted,
        ar12_empty_stop_turns,
        ar6_after_model_replacement,
        ar15_fenced_json_schema,
        ar16_forwarded_agent_tool_events,
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
        let (status, evidence) = match scenario().await {
            Ok(Verdict::Pass(evidence)) => ("PASS", evidence),
            Ok(Verdict::Fail(evidence)) => {
                failed += 1;
                ("FAIL", evidence)
            }
            Ok(Verdict::Skip(reason)) => ("SKIP", reason),
            Err(error) => {
                failed += 1;
                ("FAIL", format!("scenario error: {error:#}"))
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
