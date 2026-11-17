//! Background sub-agents on a plain durable worker (`DurableSubagents`, no kernel): a
//! parent's `start_subagent` call runs another agent as a durable run, the parent is woken
//! once when it finishes, follow-ups, cancellation, the budget slice, the tool ceiling, a
//! held call, a fork, scoping, idempotent starts, depth and crash reconciliation.

#![cfg(feature = "background")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_agent::subagents::SubagentTools;
use adk_core::subagent::{SubagentParent, SubagentScope, SubagentStart, SubagentTarget};
use adk_core::{
    Content, DurableRunStore, Event, FollowUp, InMemoryRunStore, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, NewRun, Part, Result, RunBudget, RunConfig, RunFilter, RunRecord, RunState,
    SubagentHost, SubagentMode, SubagentRequest, SubagentStatus, Tool, ToolContext, ToolEffect,
};
use adk_runner::Runner;
use adk_server::background::{
    DurableSubagents, DurableWorker, DurableWorkerConfig, RunStoreBackend, SubagentConfig,
    SubagentHooks, WorkerHandle,
};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Notify;

const APP: &str = "app";
const USER: &str = "ana";

/// A model whose every answer is `respond(request)`.
struct FnModel {
    respond: Box<dyn Fn(&LlmRequest) -> Content + Send + Sync>,
    calls: AtomicUsize,
}

impl FnModel {
    fn new(respond: impl Fn(&LlmRequest) -> Content + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { respond: Box::new(respond), calls: AtomicUsize::new(0) })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Llm for FnModel {
    fn name(&self) -> &str {
        "fn-model"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let content = (self.respond)(&req);
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn texts(req: &LlmRequest) -> Vec<String> {
    req.contents.iter().flat_map(|c| &c.parts).filter_map(Part::text).map(str::to_string).collect()
}

fn responded(req: &LlmRequest, tool: &str) -> Option<Value> {
    req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
        Part::FunctionResponse { function_response, .. } if function_response.name == tool => {
            Some(function_response.response.clone())
        }
        _ => None,
    })
}

fn call(name: &str, args: Value, id: &str) -> Content {
    Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: name.to_string(),
            args,
            id: Some(id.to_string()),
            thought_signature: None,
        }],
    }
}

fn text(text: impl Into<String>) -> Content {
    Content::new("model").with_text(text.into())
}

/// The lead: answers a sub-agent notice with `ack`, otherwise starts the sub-agent its
/// user turn describes (`start <agent> <task>`) and then reports `started`.
fn lead_model() -> Arc<FnModel> {
    FnModel::new(|req| {
        let all = texts(req);
        if let Some(notice) = all.iter().rev().find(|t| t.starts_with("[sub-agent ")) {
            return text(format!("ack: {}", notice.lines().next().unwrap_or_default()));
        }
        if let Some(result) = responded(req, "start_subagent") {
            return text(format!("started {}", result["id"].as_str().unwrap_or("?")));
        }
        let ask = all.iter().find(|t| t.starts_with("start ")).cloned().unwrap_or_default();
        let mut words = ask.splitn(3, ' ').skip(1);
        let agent = words.next().unwrap_or("researcher").to_string();
        let task = words.next().unwrap_or("work").to_string();
        call(
            "start_subagent",
            json!({ "task": task, "agent": agent, "name": "alpha", "maxModelCalls": 3 }),
            "call-start-1",
        )
    })
}

/// Counts its calls, and waits on `gate` when one is set.
struct Probe {
    name: &'static str,
    runs: Arc<AtomicUsize>,
    gate: Option<Arc<Notify>>,
}

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a probe"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        Ok(json!({ "ok": true }))
    }
}

struct World {
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<InMemorySessionService>,
    host: DurableSubagents,
    lead_model: Arc<FnModel>,
    researcher_model: Arc<FnModel>,
    probe_runs: Arc<AtomicUsize>,
    secret_runs: Arc<AtomicUsize>,
    lead: Arc<Runner>,
    researcher: Arc<Runner>,
}

struct Options {
    researcher: Arc<FnModel>,
    gate: Option<Arc<Notify>>,
    confirm_probe: bool,
    lead_budget: Option<RunBudget>,
    max_depth: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            researcher: FnModel::new(|req| {
                let last = texts(req).into_iter().rev().find(|t| !t.is_empty()).unwrap_or_default();
                text(format!("report on {last}"))
            }),
            gate: None,
            confirm_probe: false,
            lead_budget: None,
            max_depth: 2,
        }
    }
}

impl World {
    fn new(options: Options) -> Self {
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let sessions = Arc::new(InMemorySessionService::new());
        let backend = RunStoreBackend::new(Arc::clone(&runs));
        let host = DurableSubagents::new(
            Arc::clone(&runs),
            Arc::clone(&sessions) as Arc<dyn SessionService>,
            Arc::new(backend.clone()),
            SubagentConfig::default()
                .with_target(SubagentTarget::new("researcher", "researches"))
                .with_max_depth(options.max_depth),
        );
        let probe_runs = Arc::new(AtomicUsize::new(0));
        let secret_runs = Arc::new(AtomicUsize::new(0));
        let lead_model = lead_model();
        let mut lead = LlmAgentBuilder::new("lead")
            .model(Arc::clone(&lead_model) as Arc<dyn Llm>)
            .tool(Arc::new(Probe { name: "probe", runs: Arc::clone(&probe_runs), gate: None }));
        for tool in SubagentTools::new(Arc::new(host.clone())).tools() {
            lead = lead.tool(tool);
        }
        let lead_config = match &options.lead_budget {
            Some(budget) => RunConfig::builder().budget(budget.clone()).build(),
            None => RunConfig::default(),
        };
        let lead = Arc::new(
            Runner::builder()
                .app_name(APP)
                .agent(Arc::new(lead.build().unwrap()))
                .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
                .run_config(lead_config)
                .build()
                .unwrap(),
        );
        let mut researcher = LlmAgentBuilder::new("researcher")
            .model(Arc::clone(&options.researcher) as Arc<dyn Llm>)
            .tool(Arc::new(Probe {
                name: "probe",
                runs: Arc::clone(&probe_runs),
                gate: options.gate.clone(),
            }))
            .tool(Arc::new(Probe { name: "secret", runs: Arc::clone(&secret_runs), gate: None }));
        if options.confirm_probe {
            researcher = researcher.require_tool_confirmation("probe");
        }
        let researcher = Arc::new(
            Runner::builder()
                .app_name(APP)
                .agent(Arc::new(researcher.build().unwrap()))
                .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
                .build()
                .unwrap(),
        );
        backend.add_runner(&lead);
        backend.add_runner(&researcher);
        Self {
            runs,
            sessions,
            host,
            lead_model,
            researcher_model: options.researcher,
            probe_runs,
            secret_runs,
            lead,
            researcher,
        }
    }

    fn worker(&self, hooks: bool) -> WorkerHandle {
        let mut worker = DurableWorker::new(Arc::clone(&self.runs))
            .with_agent("lead", Arc::clone(&self.lead))
            .with_agent("researcher", Arc::clone(&self.researcher))
            .with_config(
                DurableWorkerConfig::default()
                    .with_worker_id("w")
                    .with_lease_ttl(Duration::from_millis(500))
                    .with_poll_interval(Duration::from_millis(10)),
            );
        if hooks {
            worker = worker.with_hooks(Arc::new(SubagentHooks::new(self.host.clone())));
        }
        worker.spawn()
    }

    async fn parent_turn(&self, text: &str) -> RunRecord {
        let run = NewRun::new("default", APP, USER, "lead", json!({ "text": text }))
            .with_session_id("s-1");
        self.runs.enqueue(run).await.unwrap().record().unwrap().clone()
    }

    async fn all_runs(&self) -> Vec<RunRecord> {
        self.runs.list(RunFilter::default()).await.unwrap()
    }

    async fn notices(&self) -> Vec<RunRecord> {
        self.all_runs()
            .await
            .into_iter()
            .filter(|run| {
                run.target == "lead"
                    && run.idempotency_key.as_deref().is_some_and(|k| k.starts_with("subagent:sa-"))
            })
            .collect()
    }

    async fn events(&self, session: &str) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: session.into(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
    }

    fn scope(&self) -> SubagentScope {
        SubagentScope::new(APP, USER, "s-1")
    }
}

async fn eventually<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    let started = tokio::time::Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn only_subagent(world: &World) -> adk_core::SubagentRecord {
    let scope = world.scope();
    eventually("the parent started a sub-agent", async || {
        world.host.list(&scope).await.unwrap().into_iter().next()
    })
    .await
}

#[tokio::test]
async fn a_finished_sub_agent_wakes_its_parent_once() {
    let world = World::new(Options::default());
    let worker = world.worker(true);
    let parent = world.parent_turn("start researcher count the open bugs").await;
    let started = only_subagent(&world).await;
    assert_eq!((started.name.as_str(), started.target.as_str()), ("alpha", "researcher"));
    assert_eq!(started.parent.run_id.as_deref(), Some(parent.run_id.as_str()));
    assert_eq!(started.parent.target.as_deref(), Some("lead"));
    let scope = world.scope();
    let done = eventually("the sub-agent completed", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    assert_eq!(done.status, SubagentStatus::Completed);
    assert_eq!(done.result.as_deref(), Some("report on count the open bugs"));
    // The parent's turn had ended, so a new turn of its session carries the notice.
    let notice = eventually("the parent was woken", async || {
        let notices = world.notices().await;
        notices.into_iter().find(|run| run.state == RunState::Completed)
    })
    .await;
    assert_eq!(notice.session_id.as_deref(), Some("s-1"));
    let events = world.events("s-1").await;
    let header = format!("[sub-agent alpha ({}) completed · run {}]", done.id, done.run_id);
    let acks: Vec<String> = events
        .iter()
        .filter(|e| e.author == "lead")
        .filter_map(|e| e.llm_response.content.as_ref())
        .flat_map(|c| c.parts.iter().filter_map(Part::text).map(str::to_string))
        .filter(|t| t.starts_with("ack: "))
        .collect();
    assert_eq!(acks, vec![format!("ack: {header}")]);
    // Hooks seeing the finish again (a recovered hook, a reconcile) tell it nothing new.
    world.host.run_finished(&world.runs.get(&done.run_id).await.unwrap().unwrap()).await;
    world.host.reconcile().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(world.notices().await.len(), 1);
    assert_eq!(world.lead_model.calls(), 3, "start, report, ack");
    worker.shutdown().await;
}

#[tokio::test]
async fn the_slice_is_carved_from_the_parent_and_bounds_every_run_of_the_sub_agent() {
    // The researcher calls the probe forever; its slice of three model calls stops it.
    let researcher = FnModel::new(|req| {
        let n = req
            .contents
            .iter()
            .flat_map(|c| &c.parts)
            .filter(|p| matches!(p, Part::FunctionResponse { .. }))
            .count();
        call("probe", json!({}), &format!("call-probe-{n}"))
    });
    let world = World::new(Options {
        researcher,
        lead_budget: Some(RunBudget::new().max_model_calls(10)),
        ..Options::default()
    });
    let worker = world.worker(true);
    world.parent_turn("start researcher loop forever").await;
    let started = only_subagent(&world).await;
    assert_eq!(started.budget.max_model_calls, Some(3), "asked for 3, the parent had 9 left");
    assert_eq!(started.budget.max_tool_calls, Some(200), "the host's default for the rest");
    let scope = world.scope();
    eventually("the sub-agent stopped", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    assert_eq!(world.researcher_model.calls(), 3);
    // A follow-up continues the same session under what is left of the slice: nothing.
    let follow_up = world.host.send(&scope, &started.id, "keep going").await.unwrap();
    let FollowUp::Queued { run_id } = follow_up else { panic!("queued: {follow_up:?}") };
    eventually("the follow-up ended", async || {
        world.runs.get(&run_id).await.unwrap().filter(|r| r.state.is_terminal())
    })
    .await;
    assert_eq!(world.researcher_model.calls(), 3, "the follow-up made no model call");
    worker.shutdown().await;
}

#[tokio::test]
async fn a_sub_agent_cannot_call_a_tool_its_parent_does_not_have() {
    let researcher = FnModel::new(|req| match responded(req, "secret") {
        Some(response) => text(format!("secret said {response}")),
        None => call("secret", json!({}), "call-secret-1"),
    });
    let world = World::new(Options { researcher, ..Options::default() });
    let worker = world.worker(true);
    world.parent_turn("start researcher read the secret").await;
    let started = only_subagent(&world).await;
    let scope = world.scope();
    let done = eventually("the sub-agent finished", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    assert_eq!(world.secret_runs.load(Ordering::SeqCst), 0, "the ceiling denied it");
    assert!(
        done.result.unwrap_or_default().contains("not among the tools"),
        "the model was told why"
    );
    worker.shutdown().await;
}

#[tokio::test]
async fn a_held_call_reads_as_waiting_for_a_human() {
    let researcher = FnModel::new(|req| match responded(req, "probe") {
        Some(_) => text("probed"),
        None => call("probe", json!({}), "call-probe-1"),
    });
    let world = World::new(Options { researcher, confirm_probe: true, ..Options::default() });
    let worker = world.worker(true);
    world.parent_turn("start researcher probe once").await;
    let started = only_subagent(&world).await;
    let scope = world.scope();
    let waiting = eventually("the sub-agent waits for a person", async || {
        world
            .host
            .get(&scope, &started.id)
            .await
            .unwrap()
            .filter(|r| r.status == SubagentStatus::WaitingForHuman)
    })
    .await;
    assert_eq!(waiting.runs, 1);
    assert_eq!(world.probe_runs.load(Ordering::SeqCst), 0);
    worker.shutdown().await;
}

#[tokio::test]
async fn a_follow_up_to_a_finished_sub_agent_runs_in_its_session_and_wakes_the_parent_again() {
    let world = World::new(Options::default());
    let worker = world.worker(true);
    world.parent_turn("start researcher first question").await;
    let started = only_subagent(&world).await;
    let scope = world.scope();
    eventually("the first run completed", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    let follow_up = world.host.send(&scope, &started.id, "second question").await.unwrap();
    assert!(matches!(follow_up, FollowUp::Queued { .. }), "{follow_up:?}");
    let done = eventually("the follow-up completed", async || {
        world
            .host
            .get(&scope, &started.id)
            .await
            .unwrap()
            .filter(|r| r.runs == 2 && r.status.is_terminal())
    })
    .await;
    assert_eq!(done.result.as_deref(), Some("report on second question"));
    assert_eq!(done.session_id, format!("subagent-{}", started.id));
    eventually("both runs woke the parent", async || {
        let notices = world.notices().await;
        (notices.len() == 2 && notices.iter().all(|n| n.state == RunState::Completed)).then_some(())
    })
    .await;
    worker.shutdown().await;
}

#[tokio::test]
async fn a_sub_agent_its_parent_cancelled_ends_without_a_notice() {
    let gate = Arc::new(Notify::new());
    let researcher = FnModel::new(|req| match responded(req, "probe") {
        Some(_) => text("probed"),
        None => call("probe", json!({}), "call-probe-1"),
    });
    let world =
        World::new(Options { researcher, gate: Some(Arc::clone(&gate)), ..Options::default() });
    let worker = world.worker(true);
    world.parent_turn("start researcher probe slowly").await;
    let started = only_subagent(&world).await;
    eventually("the probe runs", async || {
        (world.probe_runs.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    let scope = world.scope();
    // Another session cannot reach it.
    let other = SubagentScope::new(APP, USER, "s-2");
    assert!(world.host.get(&other, &started.id).await.unwrap().is_none());
    let refused = world.host.cancel(&other, &started.id).await.unwrap_err();
    assert_eq!(refused.code, adk_core::subagent::SUBAGENT_NOT_FOUND_CODE);
    world.host.cancel(&scope, &started.id).await.unwrap();
    // The worker sees the request at its next heartbeat, a third of the lease.
    tokio::time::sleep(Duration::from_millis(500)).await;
    gate.notify_waiters();
    let done = eventually("the sub-agent was cancelled", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    assert_eq!(done.status, SubagentStatus::Cancelled);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(world.notices().await.is_empty());
    let refused = world.host.send(&scope, &started.id, "more").await.unwrap_err();
    assert_eq!(refused.code, adk_core::subagent::SUBAGENT_FINISHED_CODE);
    worker.shutdown().await;
}

#[tokio::test]
async fn a_restarted_host_tells_the_parent_it_missed_once() {
    let world = World::new(Options::default());
    // A worker without the hooks: the sub-agent finishes and no one tells the parent, as
    // when the host is killed between the run's end and the notice.
    let worker = world.worker(false);
    world.parent_turn("start researcher crash test").await;
    let started = only_subagent(&world).await;
    let scope = world.scope();
    eventually("the sub-agent completed", async || {
        world.host.get(&scope, &started.id).await.unwrap().filter(|r| r.status.is_terminal())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(world.notices().await.is_empty());
    worker.shutdown().await;
    let worker = world.worker(true);
    assert_eq!(world.host.reconcile().await.unwrap(), 1);
    eventually("the parent was told", async || {
        world.notices().await.into_iter().find(|n| n.state == RunState::Completed)
    })
    .await;
    world.host.reconcile().await.unwrap();
    assert_eq!(world.notices().await.len(), 1, "told once");
    worker.shutdown().await;
}

#[tokio::test]
async fn a_fork_copies_the_parents_conversation_without_its_pending_calls() {
    let world = World::new(Options::default());
    let sessions = &world.sessions;
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some("s-1".into()),
            state: [("topic".to_string(), json!("bugs")), ("app:shared".to_string(), json!(1))]
                .into_iter()
                .collect(),
        })
        .await
        .unwrap();
    let mut user = Event::new("inv-1");
    user.author = "user".into();
    user.llm_response.content = Some(Content::new("user").with_text("look at the bugs"));
    let mut answered = Event::new("inv-1");
    answered.author = "lead".into();
    answered.llm_response.content = Some(call("probe", json!({}), "call-done"));
    let mut response = Event::new("inv-1");
    response.author = "lead".into();
    response.llm_response.content = Some(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new("probe", json!({ "ok": true })),
            id: Some("call-done".into()),
            annotations: None,
        }],
    });
    let mut pending = Event::new("inv-1");
    pending.author = "lead".into();
    pending.llm_response.content = Some(call("start_subagent", json!({}), "call-pending"));
    for event in [user, answered, response, pending] {
        sessions.append_event("s-1", event).await.unwrap();
    }
    let parent = SubagentParent::new(APP, USER, "s-1", "lead").with_target("lead");
    let started = world
        .host
        .start(SubagentStart::new(
            parent,
            SubagentRequest::new("summarize")
                .with_target("researcher")
                .with_mode(SubagentMode::Fork),
        ))
        .await
        .unwrap();
    let copied = world.events(&started.session_id).await;
    assert_eq!(copied.len(), 3, "the pending call is left out");
    assert!(copied.iter().all(|e| e.invocation_id == "fork-inv-1"));
    let state = sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: started.session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .state()
        .all();
    assert_eq!(state.get("topic"), Some(&json!("bugs")));
}

#[tokio::test]
async fn a_replayed_start_returns_the_first_and_depth_and_targets_are_enforced() {
    let world = World::new(Options { max_depth: 1, ..Options::default() });
    let parent = SubagentParent::new(APP, USER, "s-1", "lead").with_target("lead");
    let start = |key: &str| {
        SubagentStart::new(parent.clone(), SubagentRequest::new("task").with_target("researcher"))
            .with_idempotency_key(key)
    };
    let first = world.host.start(start("call-1")).await.unwrap();
    let replay = world.host.start(start("call-1")).await.unwrap();
    assert_eq!(first.id, replay.id);
    assert_eq!(world.all_runs().await.len(), 1);
    let unknown = world
        .host
        .start(SubagentStart::new(parent.clone(), SubagentRequest::new("t").with_target("root")))
        .await
        .unwrap_err();
    assert_eq!(unknown.code, adk_core::subagent::SUBAGENT_REFUSED_CODE);
    // The sub-agent's own run is a parent at depth 1; this host allows no deeper.
    let child = SubagentParent::new(APP, USER, &first.session_id, "researcher")
        .with_run_id(first.run_id.clone());
    let deep = world
        .host
        .start(SubagentStart::new(child, SubagentRequest::new("t").with_target("researcher")))
        .await
        .unwrap_err();
    assert_eq!(deep.code, adk_core::subagent::SUBAGENT_REFUSED_CODE);
    assert!(deep.message.contains("depth 2"), "{}", deep.message);
}
