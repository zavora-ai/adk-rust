//! Graph dispatch, streaming, retries, pauses and cancellation on the real stack.
//!
//! Every graph scenario runs a `GraphAgent` under a `Runner` with `LlmAgent` nodes, and the
//! functional scenarios run an `#[entrypoint]` workflow whose tasks call an `LlmAgent` through a
//! `Runner`. Scripted scenarios point the real `OpenAIClient` at a scripted OpenAI-compatible
//! server on 127.0.0.1; live scenarios run the same graphs on Gemini, or on the real OpenAI
//! endpoint with a key it rejects. Checkpoints go to the checkpointer `--checkpointer` names.
//!
//! ```bash
//! cargo run --manifest-path examples/graph_runtime/Cargo.toml
//! cargo run --manifest-path examples/graph_runtime/Cargo.toml -- --checkpointer redis
//! cargo run --manifest-path examples/graph_runtime/Cargo.toml -- --scenario node_events_stream
//! ```

mod graph;
mod scripted;
mod workflows;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Agent, BeforeAgentCallback, BeforeModelCallback, BeforeModelResult, Content, Event,
    Llm, LlmRequest, LlmResponseStream, Part, RunConfig, SessionId, ToolConfirmationRequest,
    UserId,
};
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer, SqliteCheckpointer};
use adk_graph::checkpointers::redis::RedisCheckpointer;
use adk_graph::node::AgentNode;
use adk_graph::state::State;
use adk_model::RetryConfig;
use adk_model::openai::OpenAIClient;
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use scripted::ScriptedServer;
use serde_json::{Value, json};

const APP: &str = "graph-runtime";
const USER: &str = "user";
const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
const OPENAI_MODEL: &str = "gpt-5.6-terra";

/// Every scenario, and whether it calls a live provider.
const SCENARIOS: &[(&str, &str)] = &[
    ("session_state_templates", "scripted"),
    ("live_session_state_templates", "live"),
    ("node_events_stream", "scripted"),
    ("live_node_events_stream", "live"),
    ("failed_step_stops_dispatch", "scripted"),
    ("live_failed_step_stops_dispatch", "live"),
    ("suspension_skips_retry", "scripted"),
    ("live_suspension_skips_retry", "live"),
    ("entry_join_runs_once", "scripted"),
    ("live_entry_join_runs_once", "live"),
    ("retry_delay_within_max", "scripted"),
    ("live_retry_delay_within_max", "live"),
    ("parallel_pauses_surface", "scripted"),
    ("live_parallel_pauses_surface", "live"),
    ("resume_answer_is_scoped", "scripted"),
    ("live_resume_answer_is_scoped", "live"),
    ("agent_error_category", "scripted"),
    ("live_agent_error_category", "live"),
    ("cancellation_stops_workflow", "scripted"),
    ("live_cancellation_stops_workflow", "live"),
    ("child_single_flight", "scripted"),
    ("live_child_single_flight", "live"),
];

pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
pub fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

/// Shared by every scenario: the checkpointer and the identifiers of this run.
pub struct Context {
    pub checkpointer: Arc<dyn Checkpointer>,
    pub backend: String,
    /// Appended to session and thread ids, so a Redis server keeps runs apart.
    run_id: String,
    _dir: tempfile::TempDir,
}

impl Context {
    async fn new(backend: &str) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let checkpointer: Arc<dyn Checkpointer> = match backend {
            "memory" => Arc::new(MemoryCheckpointer::new()),
            "sqlite" => {
                let url =
                    format!("sqlite://{}?mode=rwc", dir.path().join("checkpoints.db").display());
                Arc::new(SqliteCheckpointer::new(&url).await?)
            }
            "redis" => {
                let url = std::env::var("REDIS_URL")
                    .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
                Arc::new(RedisCheckpointer::new(&url).await?)
            }
            other => anyhow::bail!("unknown checkpointer '{other}'; use memory, sqlite or redis"),
        };
        let run_id = format!(
            "{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
        );
        Ok(Self { checkpointer, backend: backend.to_string(), run_id, _dir: dir })
    }

    /// A session (and thread) id unique to this run and to the scenario variant.
    pub fn id(&self, name: &str, live: bool) -> String {
        let variant = if live { "live-" } else { "" };
        format!("{variant}{name}-{}", self.run_id)
    }
}

/// What one turn produced: every event with the instant it arrived, or the error that ended it.
pub struct Turn {
    pub events: Vec<(Instant, Event)>,
    pub error: Option<AdkError>,
    pub started: Instant,
    pub ended: Instant,
}

impl Turn {
    /// The text of the last complete event that has any.
    pub fn text(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|(_, event)| !event.llm_response.partial)
            .map(|(_, event)| event_text(event))
            .find(|text| !text.is_empty())
            .unwrap_or_default()
    }

    /// The first complete event with text that `author` wrote, and when it arrived.
    pub fn first_from(&self, author: &str) -> Option<(Instant, &Event)> {
        self.events
            .iter()
            .find(|(_, event)| {
                event.author == author
                    && !event.llm_response.partial
                    && !event_text(event).is_empty()
            })
            .map(|(at, event)| (*at, event))
    }

    pub fn confirmations(&self) -> Vec<ToolConfirmationRequest> {
        self.events
            .iter()
            .filter_map(|(_, event)| event.actions.tool_confirmation.clone())
            .collect()
    }

    pub fn paused(&self) -> bool {
        self.events.iter().any(|(_, event)| {
            adk_graph::interrupt::GraphInterruptPayload::from_event(event).is_some()
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.ended - self.started
    }

    pub fn error_text(&self) -> String {
        self.error.as_ref().map(ToString::to_string).unwrap_or_default()
    }
}

pub fn event_text(event: &Event) -> String {
    event
        .llm_response
        .content
        .as_ref()
        .map(|content| content.parts.iter().filter_map(Part::text).collect())
        .unwrap_or_default()
}

/// A `Runner` over one in-memory session, driving one agent turn by turn.
pub struct Harness {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
    session_id: String,
}

impl Harness {
    pub async fn new(
        agent: Arc<dyn Agent>,
        session_id: &str,
        state: HashMap<String, Value>,
    ) -> anyhow::Result<Self> {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some(session_id.to_string()),
                state,
            })
            .await?;
        let runner = Runner::builder()
            .app_name(APP)
            .agent(agent)
            .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
            .build()?;
        Ok(Self { runner, sessions, session_id: session_id.to_string() })
    }

    pub async fn turn(&self, input: &str, run_config: Option<RunConfig>) -> anyhow::Result<Turn> {
        let started = Instant::now();
        let mut stream = self
            .runner
            .run_with_config(
                UserId::new(USER)?,
                SessionId::new(&self.session_id)?,
                Content::new("user").with_text(input),
                run_config,
            )
            .await?;
        let mut events = Vec::new();
        let mut error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push((Instant::now(), event)),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        Ok(Turn { events, error, started, ended: Instant::now() })
    }

    /// The events the session service holds for this session.
    pub async fn persisted(&self) -> anyhow::Result<Vec<Event>> {
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
}

/// The real `OpenAIClient` against a scripted server, with its own retries off, so each
/// request the server sees is one node attempt.
pub fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(
        OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?
            .with_retry_config(RetryConfig::disabled()),
    ))
}

/// Gemini, when a key is set.
pub fn gemini() -> anyhow::Result<Option<Arc<dyn Llm>>> {
    let Some(key) =
        std::env::var("GOOGLE_API_KEY").ok().or_else(|| std::env::var("GEMINI_API_KEY").ok())
    else {
        return Ok(None);
    };
    let model = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| GEMINI_MODEL.to_string());
    Ok(Some(Arc::new(adk_model::GeminiModel::new(&key, &model)?)))
}

/// The real OpenAI endpoint with a key it rejects: every request is a live `401`.
pub fn openai_rejected_key() -> anyhow::Result<Arc<dyn Llm>> {
    let config = adk_model::openai::OpenAIConfig::new("sk-graph-runtime-invalid", OPENAI_MODEL);
    Ok(Arc::new(OpenAIClient::new(config)?.with_retry_config(RetryConfig::disabled())))
}

/// An `LlmAgent` that records when each of its model calls starts.
pub fn llm_agent(
    name: &str,
    model: Arc<dyn Llm>,
    instruction: &str,
    calls: &Calls,
) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new(name)
            .model(model)
            .instruction(instruction)
            .before_model_callback(calls.callback())
            .build()?,
    ))
}

/// When each model call of one agent started.
#[derive(Clone, Default)]
pub struct Calls(Arc<Mutex<Vec<Instant>>>);

impl Calls {
    pub fn callback(&self) -> BeforeModelCallback {
        let calls = Arc::clone(&self.0);
        Box::new(move |_ctx, request: LlmRequest| {
            calls.lock().unwrap().push(Instant::now());
            Box::pin(async move { Ok(BeforeModelResult::Continue(request)) })
        })
    }

    pub fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn first(&self) -> Option<Instant> {
        self.0.lock().unwrap().first().copied()
    }
}

/// Records the session id each run of an agent sees.
pub fn record_session(seen: Arc<Mutex<Vec<String>>>) -> BeforeAgentCallback {
    Box::new(move |ctx| {
        seen.lock().unwrap().push(ctx.session_id().to_string());
        Box::pin(async { Ok(None) })
    })
}

/// An `AgentNode` whose prompt is built from state and whose answer lands in `channel`.
pub fn agent_node(
    agent: Arc<dyn Agent>,
    prompt: impl Fn(&State) -> String + Send + Sync + 'static,
    channel: &'static str,
) -> AgentNode {
    AgentNode::new(agent)
        .with_input_mapper(move |state| Content::new("user").with_text(prompt(state)))
        .with_output_mapper(move |events| {
            let text = events
                .iter()
                .rev()
                .filter(|event| !event.llm_response.partial)
                .map(event_text)
                .find(|text| !text.is_empty())
                .unwrap_or_default();
            HashMap::from([(channel.to_string(), json!(text))])
        })
}

pub fn text(state: &State, key: &str) -> String {
    state.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// When one model call started, and when its stream ended.
type Span = (Instant, Option<Instant>);

/// A model that records when each of its calls starts and when its stream ends.
pub struct TimedLlm {
    inner: Arc<dyn Llm>,
    spans: Arc<Mutex<Vec<Span>>>,
}

impl TimedLlm {
    pub fn new(inner: Arc<dyn Llm>) -> Self {
        Self { inner, spans: Arc::default() }
    }

    /// The pause between the end of each call and the start of the next.
    pub fn gaps(&self) -> Vec<Duration> {
        let spans = self.spans.lock().unwrap();
        spans
            .windows(2)
            .filter_map(|pair| pair[0].1.map(|ended| pair[1].0.saturating_duration_since(ended)))
            .collect()
    }
}

#[async_trait::async_trait]
impl Llm for TimedLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let index = {
            let mut spans = self.spans.lock().unwrap();
            spans.push((Instant::now(), None));
            spans.len() - 1
        };
        let spans = Arc::clone(&self.spans);
        let mark_end = move || {
            if let Some(span) = spans.lock().unwrap().get_mut(index) {
                span.1.get_or_insert_with(Instant::now);
            }
        };
        match self.inner.generate_content(req, stream).await {
            Ok(inner) => Ok(Box::pin(futures::stream::unfold(inner, move |mut inner| {
                let mark_end = mark_end.clone();
                async move {
                    let item = inner.next().await;
                    if item.as_ref().is_none_or(Result::is_err) {
                        mark_end();
                    }
                    item.map(|item| (item, inner))
                }
            }))),
            Err(error) => {
                mark_end();
                Err(error)
            }
        }
    }
}

async fn run_scenario(name: &str, cx: &Context) -> anyhow::Result<Verdict> {
    match name {
        "session_state_templates" => graph::session_state_templates(cx, false).await,
        "live_session_state_templates" => graph::session_state_templates(cx, true).await,
        "node_events_stream" => graph::node_events_stream(cx, false).await,
        "live_node_events_stream" => graph::node_events_stream(cx, true).await,
        "failed_step_stops_dispatch" => graph::failed_step_stops_dispatch(cx, false).await,
        "live_failed_step_stops_dispatch" => graph::failed_step_stops_dispatch(cx, true).await,
        "suspension_skips_retry" => workflows::suspension_skips_retry(cx, false).await,
        "live_suspension_skips_retry" => workflows::suspension_skips_retry(cx, true).await,
        "entry_join_runs_once" => graph::entry_join_runs_once(cx, false).await,
        "live_entry_join_runs_once" => graph::entry_join_runs_once(cx, true).await,
        "retry_delay_within_max" => graph::retry_delay_within_max(cx, false).await,
        "live_retry_delay_within_max" => graph::retry_delay_within_max(cx, true).await,
        "parallel_pauses_surface" => graph::parallel_pauses_surface(cx, false).await,
        "live_parallel_pauses_surface" => graph::parallel_pauses_surface(cx, true).await,
        "resume_answer_is_scoped" => graph::resume_answer_is_scoped(cx, false).await,
        "live_resume_answer_is_scoped" => graph::resume_answer_is_scoped(cx, true).await,
        "agent_error_category" => graph::agent_error_category(cx, false).await,
        "live_agent_error_category" => graph::agent_error_category(cx, true).await,
        "cancellation_stops_workflow" => workflows::cancellation_stops_workflow(cx, false).await,
        "live_cancellation_stops_workflow" => {
            workflows::cancellation_stops_workflow(cx, true).await
        }
        "child_single_flight" => graph::child_single_flight(cx, false).await,
        "live_child_single_flight" => graph::child_single_flight(cx, true).await,
        other => {
            let known: Vec<&str> = SCENARIOS.iter().map(|(name, _)| *name).collect();
            anyhow::bail!("unknown scenario '{other}'; known: {known:?}")
        }
    }
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
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

    let mut selected: Vec<String> = SCENARIOS.iter().map(|(name, _)| name.to_string()).collect();
    let mut backend = "sqlite".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--scenario", Some(value)) => {
                selected = value.split(',').map(str::to_string).collect();
            }
            ("--checkpointer", Some(value)) => backend = value,
            (other, _) => {
                println!("unknown argument '{other}'; use --scenario and --checkpointer");
                return ExitCode::FAILURE;
            }
        }
    }

    let context = match Context::new(&backend).await {
        Ok(context) => context,
        Err(error) => {
            println!("could not open the {backend} checkpointer: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "graph runtime — {} scenario(s) on the {} checkpointer\n",
        selected.len(),
        context.backend
    );
    let mut failures = 0;
    for name in &selected {
        let kind = SCENARIOS.iter().find(|(known, _)| known == name).map_or("?", |(_, kind)| *kind);
        let result =
            tokio::time::timeout(Duration::from_secs(180), run_scenario(name, &context)).await;
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
        println!("{status}  {name} ({kind})\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
