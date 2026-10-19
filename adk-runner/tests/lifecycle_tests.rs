//! The runner's lifecycle: every transfer obeys the source agent's restrictions, every event the
//! run yields reaches the plugins' `on_event`, `after_run` runs once however the run ends,
//! including a consumer that drops the stream, and `Runner::close` closes the plugins and the
//! agent tree.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, EventStream, InvocationContext, Part,
    Result, SessionId, UserId,
};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

const APP: &str = "app";
const USER: &str = "user";
const SESSION: &str = "session";

/// What a scripted agent does when it runs.
#[derive(Clone)]
enum Script {
    /// Requests a transfer to the named agent, the first time it runs.
    TransferOnce(&'static str),
    /// Calls a tool and never finishes.
    CallAndHang,
    /// Fails with a run-budget refusal.
    BudgetRefusal,
    /// Answers.
    Answer,
}

struct ScriptedAgent {
    name: &'static str,
    script: Script,
    sub_agents: Vec<Arc<dyn Agent>>,
    allows_parent: bool,
    allows_peers: bool,
    runs: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}

impl ScriptedAgent {
    fn new(name: &'static str, script: Script) -> Self {
        Self {
            name,
            script,
            sub_agents: Vec::new(),
            allows_parent: true,
            allows_peers: true,
            runs: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl Agent for ScriptedAgent {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "scripted"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    fn allows_transfer_to_parent(&self) -> bool {
        self.allows_parent
    }

    fn allows_transfer_to_peers(&self) -> bool {
        self.allows_peers
    }

    async fn close(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
        for agent in &self.sub_agents {
            agent.close().await;
        }
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst);
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name.to_string();
        match self.script {
            Script::TransferOnce(target) if run == 0 => {
                event.actions.transfer_to_agent = Some(target.to_string());
                Ok(Box::pin(futures::stream::iter([Ok(event)])))
            }
            Script::TransferOnce(_) | Script::Answer => {
                event.llm_response.content = Some(Content::new("model").with_text("done"));
                Ok(Box::pin(futures::stream::iter([Ok(event)])))
            }
            Script::CallAndHang => {
                event.llm_response.content = Some(Content {
                    role: "model".to_string(),
                    parts: vec![Part::FunctionCall {
                        name: "slow_tool".to_string(),
                        args: serde_json::json!({}),
                        id: Some("call-1".to_string()),
                        thought_signature: None,
                    }],
                });
                Ok(Box::pin(async_stream::stream! {
                    yield Ok(event);
                    futures::future::pending::<()>().await;
                }))
            }
            Script::BudgetRefusal => Ok(Box::pin(futures::stream::iter([Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::ResourceExhausted,
                "budget.model_calls",
                "budget exhausted: 1 of 1 model calls used",
            ))]))),
        }
    }
}

async fn sessions() -> Arc<InMemorySessionService> {
    let service = Arc::new(InMemorySessionService::new());
    service
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    service
}

async fn stored_events(service: &InMemorySessionService) -> Vec<Event> {
    service
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
}

fn build_runner(
    agent: Arc<dyn Agent>,
    service: Arc<InMemorySessionService>,
    plugins: Option<Arc<PluginManager>>,
) -> Runner {
    let builder = Runner::builder().app_name(APP).agent(agent).session_service(service);
    match plugins {
        Some(plugins) => builder.plugin_manager(plugins).build().unwrap(),
        None => builder.build().unwrap(),
    }
}

async fn start(runner: &Runner) -> EventStream {
    runner
        .run(
            UserId::new(USER).unwrap(),
            SessionId::new(SESSION).unwrap(),
            Content::new("user").with_text("hello"),
        )
        .await
        .unwrap()
}

/// Every event the run yields, and its error.
async fn drain(mut stream: EventStream) -> (Vec<Event>, Option<AdkError>) {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => return (events, Some(error)),
        }
    }
    (events, None)
}

/// A plugin that counts `after_run` and `close`, records the IDs `on_event` sees, and with
/// `early_exit` ends every run from `before_run`.
struct Recorder {
    after_run: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    seen: Arc<Mutex<HashSet<String>>>,
}

impl Recorder {
    fn new() -> Self {
        Self {
            after_run: Arc::new(AtomicUsize::new(0)),
            closed: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn plugins(&self, early_exit: bool) -> Arc<PluginManager> {
        let after_run = self.after_run.clone();
        let closed = self.closed.clone();
        let seen = self.seen.clone();
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
                    Box::pin(async move { Ok(Some(Content::new("model").with_text("closed"))) })
                })
            }),
            after_run: Some(Box::new(move |_ctx| {
                let after_run = after_run.clone();
                Box::pin(async move {
                    after_run.fetch_add(1, Ordering::SeqCst);
                })
            })),
            close_fn: Some(Box::new(move || {
                let closed = closed.clone();
                Box::pin(async move {
                    closed.fetch_add(1, Ordering::SeqCst);
                })
            })),
            ..Default::default()
        })]))
    }

    fn unseen(&self, events: &[Event]) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        events
            .iter()
            .filter(|event| !seen.contains(&event.id))
            .map(|event| event.id.clone())
            .collect()
    }
}

async fn wait_for(counter: &AtomicUsize, expected: usize) -> usize {
    for _ in 0..100 {
        if counter.load(Ordering::SeqCst) >= expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    counter.load(Ordering::SeqCst)
}

/// A coordinator that hands control to `worker`, whose own transfer goes to `target`.
fn transfer_tree(
    target: &'static str,
    allows: bool,
) -> (Arc<dyn Agent>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let mut worker = ScriptedAgent::new("worker", Script::TransferOnce(target));
    worker.allows_parent = allows;
    worker.allows_peers = allows;
    let auditor = ScriptedAgent::new("auditor", Script::Answer);
    let auditor_runs = auditor.runs.clone();
    let mut coordinator = ScriptedAgent::new("coordinator", Script::TransferOnce("worker"));
    let coordinator_runs = coordinator.runs.clone();
    coordinator.sub_agents = vec![Arc::new(worker), Arc::new(auditor)];
    (Arc::new(coordinator), coordinator_runs, auditor_runs)
}

#[tokio::test]
async fn a_transfer_the_source_agent_disallows_is_refused() {
    for target in ["coordinator", "auditor"] {
        let (root, coordinator_runs, auditor_runs) = transfer_tree(target, false);
        let runner = build_runner(root, sessions().await, None);
        let (_, error) = drain(start(&runner).await).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(
            (coordinator_runs.load(Ordering::SeqCst), auditor_runs.load(Ordering::SeqCst)),
            (1, 0),
            "worker may not transfer to {target}"
        );
    }
}

#[tokio::test]
async fn a_transfer_the_source_agent_allows_still_runs() {
    let (root, coordinator_runs, _) = transfer_tree("coordinator", true);
    let runner = build_runner(root, sessions().await, None);
    drain(start(&runner).await).await;
    assert_eq!(coordinator_runs.load(Ordering::SeqCst), 2);

    let (root, _, auditor_runs) = transfer_tree("auditor", true);
    let runner = build_runner(root, sessions().await, None);
    drain(start(&runner).await).await;
    assert_eq!(auditor_runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn allowed_transfer_targets_follow_the_tree_and_the_restrictions() {
    let (root, _, _) = transfer_tree("coordinator", false);
    assert_eq!(
        Runner::allowed_transfer_targets(&root, "coordinator"),
        Some(vec!["worker".to_string(), "auditor".to_string()])
    );
    assert_eq!(Runner::allowed_transfer_targets(&root, "worker"), Some(Vec::new()));
    assert_eq!(
        Runner::allowed_transfer_targets(&root, "auditor"),
        Some(vec!["coordinator".to_string(), "worker".to_string()])
    );
    assert_eq!(Runner::allowed_transfer_targets(&root, "nobody"), None);
}

#[tokio::test]
async fn a_dropped_stream_answers_its_call_and_runs_after_run() {
    let service = sessions().await;
    let recorder = Recorder::new();
    let runner = build_runner(
        Arc::new(ScriptedAgent::new("desk", Script::CallAndHang)),
        service.clone(),
        Some(recorder.plugins(false)),
    );
    let mut stream = start(&runner).await;
    let call = stream.next().await.unwrap().unwrap();
    assert!(call.llm_response.content.is_some());
    drop(stream);

    assert_eq!(wait_for(&recorder.after_run, 1).await, 1, "after_run runs once");
    let events = stored_events(&service).await;
    let answers: Vec<_> = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, id, .. } => {
                Some((id.clone(), adk_core::is_outcome_unknown(&function_response.response)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(answers, [(Some("call-1".to_string()), true)]);
    assert!(runner.active_runs().is_empty());
}

#[tokio::test]
async fn a_dropped_durable_attempt_writes_nothing() {
    let service = sessions().await;
    let recorder = Recorder::new();
    let runner = build_runner(
        Arc::new(ScriptedAgent::new("desk", Script::CallAndHang)),
        service.clone(),
        Some(recorder.plugins(false)),
    );
    let config = adk_core::RunConfig {
        durable_run_id: Some("run-1".to_string()),
        ..adk_core::RunConfig::default()
    };
    let mut stream = runner
        .run_with_config(
            UserId::new(USER).unwrap(),
            SessionId::new(SESSION).unwrap(),
            Content::new("user").with_text("hello"),
            Some(config),
        )
        .await
        .unwrap();
    stream.next().await.unwrap().unwrap();
    // A worker drops the attempt when its lease is lost; the recovering worker answers the
    // call from the action ledger, so the dropped attempt must not answer it too.
    drop(stream);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let answers = stored_events(&service)
        .await
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter(|part| matches!(part, Part::FunctionResponse { .. }))
        .count();
    assert_eq!((answers, recorder.after_run.load(Ordering::SeqCst)), (0, 0));
}

#[tokio::test]
async fn after_run_runs_once_however_the_run_ends() {
    for script in [Script::Answer, Script::BudgetRefusal] {
        let recorder = Recorder::new();
        let runner = build_runner(
            Arc::new(ScriptedAgent::new("desk", script)),
            sessions().await,
            Some(recorder.plugins(false)),
        );
        drain(start(&runner).await).await;
        assert_eq!(recorder.after_run.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn on_event_sees_a_before_run_exit() {
    let recorder = Recorder::new();
    let runner = build_runner(
        Arc::new(ScriptedAgent::new("desk", Script::Answer)),
        sessions().await,
        Some(recorder.plugins(true)),
    );
    let (events, error) = drain(start(&runner).await).await;
    assert!(error.is_none());
    assert_eq!(events.len(), 1);
    assert_eq!(recorder.unseen(&events), Vec::<String>::new());
    assert_eq!(recorder.after_run.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn on_event_sees_a_budget_stop_event() {
    let service = sessions().await;
    let recorder = Recorder::new();
    let runner = build_runner(
        Arc::new(ScriptedAgent::new("desk", Script::BudgetRefusal)),
        service.clone(),
        Some(recorder.plugins(false)),
    );
    let (events, error) = drain(start(&runner).await).await;
    assert_eq!(error.map(|error| error.code), Some("budget.model_calls"));
    let stop: Vec<_> =
        events.iter().filter(|event| event.llm_response.error_code.is_some()).collect();
    assert_eq!(stop.len(), 1);
    assert_eq!(recorder.unseen(&events), Vec::<String>::new());
    assert!(stored_events(&service).await.iter().any(|event| event.id == stop[0].id));
}

#[tokio::test]
async fn on_event_sees_the_answers_of_an_interrupted_run() {
    let recorder = Recorder::new();
    let runner = build_runner(
        Arc::new(ScriptedAgent::new("desk", Script::CallAndHang)),
        sessions().await,
        Some(recorder.plugins(false)),
    );
    let mut stream = start(&runner).await;
    let mut events = vec![stream.next().await.unwrap().unwrap()];
    assert!(runner.interrupt(SESSION));
    let (rest, error) = drain(stream).await;
    assert!(error.is_none());
    events.extend(rest);
    assert_eq!(events.len(), 2, "the call and its outcome-unknown answer");
    assert_eq!(recorder.unseen(&events), Vec::<String>::new());
    assert_eq!(recorder.after_run.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn close_closes_the_agent_tree_and_the_plugins() {
    let recorder = Recorder::new();
    let worker = ScriptedAgent::new("worker", Script::Answer);
    let worker_closed = worker.closed.clone();
    let mut root = ScriptedAgent::new("root", Script::Answer);
    let root_closed = root.closed.clone();
    root.sub_agents = vec![Arc::new(worker)];
    let runner = build_runner(Arc::new(root), sessions().await, Some(recorder.plugins(false)));
    runner.close().await;
    assert_eq!(
        (
            root_closed.load(Ordering::SeqCst),
            worker_closed.load(Ordering::SeqCst),
            recorder.closed.load(Ordering::SeqCst)
        ),
        (1, 1, 1)
    );
}
