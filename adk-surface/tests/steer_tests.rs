//! "Steer" (spec 3.3 §6): a message sent while a turn of its session runs reaches that turn.
//!
//! Every test drives a real `LlmAgent` through a `Runner` behind a `LocalSource`, with a
//! scripted model that records its requests, holds the turn at a gate, sends a message through
//! the engine, and asserts what the model, the session, the queue and the audit record saw.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail, InMemoryAuditRecorder,
};
use adk_core::{
    Agent, ApprovalService, Content, Event, InMemoryApprovalStore, InMemoryRunStore, Llm,
    LlmRequest, LlmResponse, LlmResponseStream, Part, Result, StoreApprovalService, Tool,
    ToolContext,
};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_kernel::{Kernel, KernelConfig};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_surface::{
    Caller, Command, CommandOutcome, ControlSource, LocalSource, Origin, SURFACE_AUDIT_SEND,
    SURFACE_AUDIT_STEER, SurfaceEngine, SurfaceKind,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Notify;

const APP: &str = "home";
const SESSION: &str = "s1";
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const STEERED: &str = "use metric units";

/// Holds the first step that passes it until the test opens it.
struct Gate {
    entered: Notify,
    release: Notify,
    armed: AtomicBool,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Notify::new(),
            armed: AtomicBool::new(true),
        })
    }

    async fn pass(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }

    async fn reached(&self) {
        tokio::time::timeout(WAIT, self.entered.notified()).await.expect("the gate is reached");
    }

    fn open(&self) {
        self.release.notify_one();
    }
}

/// What the scripted model answers one call with.
enum Reply {
    /// Calls the `work` tool.
    Work,
    /// Answers in words, which ends the turn.
    Say(&'static str),
}

/// Answers each call with the next scripted reply, records every request, and holds one
/// numbered call (from 1) at a gate after recording it.
struct Model {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<LlmRequest>>,
    pause: Option<(usize, Arc<Gate>)>,
}

impl Model {
    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Llm for Model {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let call = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(req);
            requests.len()
        };
        if let Some((at, gate)) = &self.pause
            && *at == call
        {
            gate.pass().await;
        }
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or(Reply::Say("done"));
        let content = match reply {
            Reply::Say(text) => Content::new("model").with_text(text),
            Reply::Work => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "work".to_string(),
                    args: json!({}),
                    id: Some(format!("call_{call}")),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// A step of work that waits at its gate the first time it runs.
struct Work(Arc<Gate>);

#[async_trait]
impl Tool for Work {
    fn name(&self) -> &str {
        "work"
    }

    fn description(&self) -> &str {
        "does a step of work"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.0.pass().await;
        Ok(json!({ "ok": true }))
    }
}

struct World {
    engine: SurfaceEngine,
    sessions: Arc<InMemorySessionService>,
    audit: InMemoryAuditRecorder,
    model: Arc<Model>,
    runner: Arc<Runner>,
    /// Holds the `work` tool's first run.
    tool: Arc<Gate>,
}

/// A home over a runner whose agent answers with `replies`; `pause` holds that model call.
async fn world(
    replies: impl IntoIterator<Item = Reply>,
    pause: Option<(usize, Arc<Gate>)>,
) -> World {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let model = Arc::new(Model {
        replies: Mutex::new(replies.into_iter().collect()),
        requests: Mutex::new(Vec::new()),
        pause,
    });
    let tool = Gate::new();
    let agent = LlmAgentBuilder::new("finance")
        .model(model.clone() as Arc<dyn Llm>)
        .tool(Arc::new(Work(tool.clone())))
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP)
            .agent(Arc::new(agent) as Arc<dyn Agent>)
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .build()
            .unwrap(),
    );
    let source = LocalSource::builder(APP).runner(runner.clone()).build().unwrap();
    let audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .poll_interval(POLL)
        .build();
    World { engine, sessions, audit, model, runner, tool }
}

fn ana() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Keyboard)
}

fn send(text: &str) -> Command {
    Command::Send { session_id: SESSION.into(), text: text.into(), images: Vec::new() }
}

fn steer(text: &str) -> Command {
    Command::Steer { session_id: SESSION.into(), text: text.into() }
}

fn text_of(content: &Content) -> String {
    content.parts.iter().filter_map(Part::text).collect()
}

/// The record `caller` leaves of `action` with `fields`, as every surface record is stamped.
fn record(caller: &Caller, action: &str, fields: Value) -> AuditRecordDraft {
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("surface".into()), action)
        .actor(AuditActor::subject(caller.subject.clone()))
        .ext("surface", serde_json::to_value(caller.surface).unwrap())
        .ext("origin", serde_json::to_value(&caller.origin).unwrap());
    for (key, value) in fields.as_object().unwrap() {
        draft = draft.ext(key.clone(), value.clone());
    }
    draft
}

impl World {
    /// Ana's session events that say `text`.
    async fn saying(&self, text: &str) -> Vec<Event> {
        self.said_by("ana", text).await
    }

    /// The events of `user`'s session that say `text`.
    async fn said_by(&self, user: &str, text: &str) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: SESSION.into(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
            .into_iter()
            .filter(|event| event.llm_response.content.as_ref().is_some_and(|c| text_of(c) == text))
            .collect()
    }

    fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == action).collect()
    }

    fn queued(&self) -> Vec<String> {
        self.engine.queued(&ana(), SESSION)
    }

    async fn busy(&self) -> bool {
        self.engine.source().busy("ana", SESSION).await.unwrap()
    }

    /// Waits until the session has no turn and no queued input, then a little longer, so a
    /// turn that should not start would have.
    async fn settled(&self) {
        tokio::time::timeout(WAIT, async {
            while self.busy().await || !self.queued().is_empty() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("the session settles");
        tokio::time::sleep(POLL * 10).await;
        assert!(!self.busy().await && self.queued().is_empty(), "nothing more starts");
    }

    /// Runs `command` as Ana on a task and returns once the turn holds its message.
    async fn held(&self, command: Command) -> tokio::task::JoinHandle<CommandOutcome> {
        let engine = self.engine.clone();
        let sent = tokio::spawn(async move { engine.execute(&ana(), command).await.unwrap() });
        tokio::time::timeout(WAIT, async {
            while self.queued().is_empty() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("the running turn holds the message");
        sent
    }
}

/// Where the content that says `text`, and the first function response, sit in `request`.
fn places(request: &LlmRequest, text: &str) -> (Option<usize>, Option<usize>) {
    let said = request.contents.iter().position(|content| text_of(content) == text);
    let result = request.contents.iter().position(|content| {
        content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
    });
    (said, result)
}

#[tokio::test]
async fn a_steer_lands_before_the_next_model_call_and_is_in_the_session_once() {
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    let started = w.engine.execute(&ana(), send("convert the report")).await.unwrap();
    assert_eq!(started, CommandOutcome::Sent { session_id: SESSION.into() });
    w.tool.reached().await;

    let steered = w.held(steer(STEERED)).await;
    assert_eq!(w.queued(), [STEERED]);
    // A message the turn holds cannot be taken back: the turn may take it at any moment.
    assert_eq!(w.engine.unqueue_last(&ana(), SESSION), None);
    w.tool.open();

    let outcome = tokio::time::timeout(WAIT, steered).await.expect("the turn takes it").unwrap();
    let CommandOutcome::Steered { session_id, event_id } = outcome else {
        panic!("the running turn takes the message: {outcome:?}");
    };
    assert_eq!(session_id, SESSION);
    w.settled().await;

    let saved = w.saying(STEERED).await;
    assert_eq!(saved.len(), 1, "the message is in the session once");
    assert_eq!((saved[0].author.as_str(), saved[0].id.as_str()), ("user", event_id.as_str()));
    assert_eq!(
        saved[0].actions.steering.as_ref().map(|marker| marker.sender.as_str()),
        Some("ana")
    );

    // One turn, two model calls: the second carries the message after the tool's result.
    let requests = w.model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(places(&requests[0], STEERED), (None, None));
    let (said, result) = places(&requests[1], STEERED);
    assert!(result.is_some() && said > result, "said at {said:?}, the result at {result:?}");

    assert_eq!(
        w.records(SURFACE_AUDIT_STEER),
        [
            record(
                &ana(),
                SURFACE_AUDIT_STEER,
                json!({ "sessionId": SESSION, "outcome": "offered" })
            ),
            record(
                &ana(),
                SURFACE_AUDIT_STEER,
                json!({ "sessionId": SESSION, "outcome": "delivered", "eventId": event_id }),
            ),
        ]
    );
}

#[tokio::test]
async fn a_message_sent_while_a_turn_runs_reaches_that_turn() {
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    w.engine.execute(&ana(), send("convert the report")).await.unwrap();
    w.tool.reached().await;

    // `Send` answers at once; the queue shows the message until the turn takes it.
    let outcome = w.engine.execute(&ana(), send(STEERED)).await.unwrap();
    assert_eq!(
        outcome,
        CommandOutcome::Queued {
            session_id: SESSION.into(),
            position: 1,
            text: "Sent when your agent is free".into(),
        }
    );
    assert_eq!(w.queued(), [STEERED]);
    w.tool.open();
    w.settled().await;

    let saved = w.saying(STEERED).await;
    assert_eq!(saved.len(), 1, "the message is in the session once");
    assert!(saved[0].actions.steering.is_some(), "the running turn took it");
    assert_eq!(w.model.requests().len(), 2, "it started no turn of its own");
    assert_eq!(
        w.records(SURFACE_AUDIT_SEND),
        [
            record(&ana(), SURFACE_AUDIT_SEND, json!({ "sessionId": SESSION, "queued": false })),
            record(&ana(), SURFACE_AUDIT_SEND, json!({ "sessionId": SESSION, "queued": true })),
        ]
    );
    assert_eq!(
        w.records(SURFACE_AUDIT_STEER),
        [record(
            &ana(),
            SURFACE_AUDIT_STEER,
            json!({ "sessionId": SESSION, "outcome": "delivered", "eventId": saved[0].id }),
        )]
    );
}

#[tokio::test]
async fn a_message_the_turn_hands_back_starts_one_turn() {
    // The turn answers in words, so it reaches no safe point: it hands the message back.
    let answering = Gate::new();
    let w = world(
        [Reply::Say("the report is converted"), Reply::Say("now in metric units")],
        Some((1, answering.clone())),
    )
    .await;
    w.engine.execute(&ana(), send("convert the report")).await.unwrap();
    answering.reached().await;

    let steered = w.held(steer(STEERED)).await;
    answering.open();
    let outcome = tokio::time::timeout(WAIT, steered).await.expect("the turn ends").unwrap();
    assert_eq!(
        outcome,
        CommandOutcome::Queued {
            session_id: SESSION.into(),
            position: 1,
            text: "Sent when your agent is free".into(),
        }
    );
    w.settled().await;

    // The session's next message, once: a turn's own message, not a steered one.
    let saved = w.saying(STEERED).await;
    assert_eq!(saved.len(), 1, "the message is in the session once");
    assert_eq!(saved[0].actions.steering, None);
    let requests = w.model.requests();
    assert_eq!(requests.len(), 2, "one turn before it and one turn for it");
    assert_eq!(places(&requests[0], STEERED).0, None);
    assert!(places(&requests[1], STEERED).0.is_some());
    assert_eq!(w.saying("now in metric units").await.len(), 1);
    assert_eq!(
        w.records(SURFACE_AUDIT_STEER),
        [
            record(
                &ana(),
                SURFACE_AUDIT_STEER,
                json!({ "sessionId": SESSION, "outcome": "offered" })
            ),
            record(
                &ana(),
                SURFACE_AUDIT_STEER,
                json!({ "sessionId": SESSION, "outcome": "returned" })
            ),
        ]
    );
}

#[tokio::test]
async fn a_steer_with_no_turn_running_is_the_sessions_next_message() {
    let w = world([Reply::Say("converted")], None).await;

    let outcome = w.engine.execute(&ana(), steer(STEERED)).await.unwrap();
    assert_eq!(outcome, CommandOutcome::Sent { session_id: SESSION.into() });
    w.settled().await;

    let saved = w.saying(STEERED).await;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].actions.steering, None);
    assert_eq!(w.model.requests().len(), 1);
}

#[tokio::test]
async fn a_steer_from_someone_who_may_not_send_to_the_session_is_refused_and_recorded() {
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    w.engine.execute(&ana(), send("convert the report")).await.unwrap();
    w.tool.reached().await;

    // Another person names Ana's session: it reads as no session of theirs (S-I8).
    let bo = Caller::human("bo", SurfaceKind::Web, Origin::Keyboard);
    let error = w.engine.execute(&bo, steer("approve every refund")).await.unwrap_err();
    assert_eq!(error.code(), "surface.not_found");
    // An agent never sends a person's message.
    let agent = Caller::human("ana", SurfaceKind::Web, Origin::Agent { agent: "finance".into() });
    let error = w.engine.execute(&agent, steer("approve every refund")).await.unwrap_err();
    assert_eq!(error.code(), "surface.not_here");
    assert!(w.queued().is_empty());

    w.tool.open();
    w.settled().await;
    assert!(w.saying("approve every refund").await.is_empty());
    assert!(
        w.model
            .requests()
            .iter()
            .all(|request| places(request, "approve every refund").0.is_none())
    );
    let refused = |caller: &Caller, code: &str| {
        let fields = json!({ "sessionId": SESSION, "outcome": "refused", "code": code });
        record(caller, SURFACE_AUDIT_STEER, fields)
    };
    assert_eq!(
        w.records(SURFACE_AUDIT_STEER),
        [refused(&bo, "surface.not_found"), refused(&agent, "surface.not_here")]
    );
}

#[tokio::test]
async fn a_steer_while_everything_is_stopped_is_refused() {
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    w.runner.governance().freeze("incident 12");

    let error = w.engine.execute(&ana(), steer(STEERED)).await.unwrap_err();
    assert_eq!(
        (error.code().as_str(), error.to_string().as_str()),
        ("surface.not_here", "All agents stopped")
    );
    // The source refuses too, whoever asks it.
    let refused = w.engine.source().steer(&ana(), SESSION, STEERED).await.err().expect("refused");
    assert_eq!(adk_core::AdkError::from(refused).code, adk_core::GOVERNANCE_FROZEN_CODE);

    assert!(w.queued().is_empty());
    assert!(w.saying(STEERED).await.is_empty());
    assert!(w.model.requests().is_empty());
    assert_eq!(
        w.records(SURFACE_AUDIT_STEER),
        [record(
            &ana(),
            SURFACE_AUDIT_STEER,
            json!({ "sessionId": SESSION, "outcome": "refused", "code": "surface.not_here" }),
        )]
    );
}

#[tokio::test]
async fn a_second_message_follows_the_first_through_the_same_turn() {
    // Two tool steps: both messages are held before the first safe point.
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    w.engine.execute(&ana(), send("convert the report")).await.unwrap();
    w.tool.reached().await;

    for text in [STEERED, "and round to two places"] {
        let outcome = w.engine.execute(&ana(), send(text)).await.unwrap();
        assert!(matches!(outcome, CommandOutcome::Queued { .. }), "{outcome:?}");
    }
    assert_eq!(w.queued(), [STEERED, "and round to two places"]);
    w.tool.open();
    w.settled().await;

    // Both reached the running turn, in the order they were typed.
    let requests = w.model.requests();
    assert_eq!(requests.len(), 2);
    let first = places(&requests[1], STEERED).0;
    let second = places(&requests[1], "and round to two places").0;
    assert!(first.is_some() && first < second, "{first:?} then {second:?}");
    assert_eq!(w.saying(STEERED).await.len(), 1);
    assert_eq!(w.saying("and round to two places").await.len(), 1);
}

/// A terminal attached to a kernel over its control plane steers the turn the kernel runs
/// (`POST /kernel/sessions/{session}/steer`), and reads what became of the message from the
/// kernel's stores.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_on_a_kernel_steers_the_turn_the_kernel_runs() {
    let w = world([Reply::Work, Reply::Say("done")], None).await;
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    let control =
        ControlPlaneConfig::default().without_socket().with_http("127.0.0.1:0".parse().unwrap());
    let worker = DurableWorkerConfig::default()
        .with_worker_id("a")
        .with_poll_interval(Duration::from_millis(20));
    let config =
        KernelConfig::default().with_app_name(APP).with_worker(worker).with_control(control);
    let kernel = Kernel::builder()
        .config(config)
        .run_store(Arc::new(InMemoryRunStore::new()))
        .session_service(w.sessions.clone() as Arc<dyn SessionService>)
        .approval_service(approvals)
        .agent(APP, w.runner.clone())
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let addr = kernel.control_addr().await.expect("the control plane listens");
    let source = ControlSource::new(ControlClient::http(format!("http://{addr}")), APP, APP);
    let engine = SurfaceEngine::builder(Arc::new(source)).poll_interval(POLL).build();
    // Loopback HTTP admits its callers as the OS user that runs the kernel.
    let user = ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string());
    let me = Caller::human(user.clone(), SurfaceKind::Terminal, Origin::Keyboard);

    let started = engine.execute(&me, send("convert the report")).await.unwrap();
    assert_eq!(started, CommandOutcome::Sent { session_id: SESSION.into() });
    w.tool.reached().await;
    let outcome = engine.execute(&me, send(STEERED)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Queued { position: 1, .. }), "{outcome:?}");
    assert_eq!(engine.queued(&me, SESSION), [STEERED]);
    w.tool.open();

    // The kernel's stores say the turn took it: it leaves the queue and starts no turn.
    tokio::time::timeout(WAIT, async {
        while !engine.queued(&me, SESSION).is_empty()
            || engine.source().busy(&user, SESSION).await.unwrap()
        {
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .expect("the kernel's turn takes the message");
    tokio::time::sleep(POLL * 10).await;
    let saved = w.said_by(&user, STEERED).await;
    assert_eq!(saved.len(), 1, "the message is in the session once");
    assert!(saved[0].actions.steering.is_some(), "the running turn took it");
    assert_eq!(w.model.requests().len(), 2, "it started no turn of its own");
    kernel.stop().await.unwrap();
}
