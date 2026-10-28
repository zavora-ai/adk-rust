//! The workflow observer (spec 3.3 §12 developer track, §7.1, §7.2, §11 S-I1, S-I8): card
//! hints and the money route for a governed graph node, the run view, step details, and the
//! fork command's refusals, audit record and Stop all, over an in-memory runner; and its
//! routes, with a caller and without.

#![cfg(all(feature = "web", feature = "workflow"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecorder, AuditTrail, InMemoryAuditRecorder,
};
use adk_core::{
    ApprovalPolicy, ApproverSelector, GovernanceControl, InMemoryActionLedger,
    InMemoryApprovalStore, RequestContext, RunConfig, StoreApprovalService,
};
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::governed::GovernedNode;
use adk_graph::node::{Node, NodeContext, NodeOutput};
use adk_graph::state::Checkpoint;
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::workflow::{
    Change, ForkRequest, NodeState, RunStatus, StateChange, StepState, WorkflowRun, Workflows,
};
use adk_surface::{
    Answer, Caller, CardDetail, Command, LocalSource, Origin, Reversibility, SurfaceEngine,
    SurfaceKind,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const APP: &str = "adk-rust";
const OWNER: &str = "ana";
const WITHIN: Duration = Duration::from_secs(20);

/// Charges the invoice and counts every charge it sends.
struct Pay(Arc<AtomicUsize>);

impl Pay {
    fn charge(&self, args: &Value) -> NodeOutput {
        let hit = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        NodeOutput::new().with_update(
            "charge",
            json!({ "id": format!("ch_{hit}"), "amountCents": args["amountCents"] }),
        )
    }
}

#[async_trait]
impl Node for Pay {
    fn name(&self) -> &str {
        "pay"
    }

    fn call_args(&self, ctx: &NodeContext) -> Option<Value> {
        let invoice = ctx.get("invoice")?;
        Some(json!({ "customer": invoice["customer"], "amountCents": invoice["amountCents"] }))
    }

    async fn execute(&self, ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
        Ok(self.charge(&self.call_args(ctx).unwrap_or_default()))
    }

    async fn run_call(&self, _ctx: &NodeContext, args: Value) -> adk_graph::Result<NodeOutput> {
        Ok(self.charge(&args))
    }
}

struct Rig {
    checkpointer: Arc<MemoryCheckpointer>,
    sessions: Arc<InMemorySessionService>,
    governance: GovernanceControl,
    hits: Arc<AtomicUsize>,
    audit: InMemoryAuditRecorder,
    engine: SurfaceEngine,
    workflows: Workflows,
}

fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer)
        .with_groups(["owner"])
        .with_presence_confirmed()
}

impl Rig {
    fn new() -> Self {
        Self::hiding(&[])
    }

    fn hiding(keys: &[&str]) -> Self {
        Self::with(keys, invoice_run, None, true)
    }

    /// A rig whose graph `graph` builds, whose audit fails for the action `failing`, and that
    /// starts forks only `with_runner`.
    fn with(
        keys: &[&str],
        graph: fn(Arc<AtomicUsize>, Arc<MemoryCheckpointer>) -> GraphAgent,
        failing: Option<&'static str>,
        with_runner: bool,
    ) -> Self {
        let checkpointer = Arc::new(MemoryCheckpointer::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let graph = Arc::new(graph(Arc::clone(&hits), Arc::clone(&checkpointer)));
        let sessions = Arc::new(InMemorySessionService::new());
        let workflows = Workflows::new(APP)
            .graph(Arc::clone(&graph))
            .sessions(sessions.clone())
            .hide_keys(keys.iter().copied())
            .utc_offset(chrono::FixedOffset::east_opt(3 * 3600).unwrap());
        let approvals = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
                workflows.approval_router(
                    ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
                ),
            ),
        );
        let governance = GovernanceControl::new();
        let runner = Arc::new(
            Runner::builder()
                .app_name(APP)
                .agent(graph)
                .session_service(sessions.clone())
                .governance(governance.clone())
                .run_config(
                    RunConfig::builder()
                        .approval_service(approvals.clone())
                        .action_ledger(Arc::new(InMemoryActionLedger::new()))
                        .build(),
                )
                .build()
                .unwrap(),
        );
        let workflows = if with_runner { workflows.runner(Arc::clone(&runner)) } else { workflows };
        let source = LocalSource::builder(APP)
            .sessions(sessions.clone())
            .approvals(approvals)
            .runner(runner)
            .governance(governance.clone())
            .build()
            .unwrap();
        let audit = InMemoryAuditRecorder::new();
        let recorder: Arc<dyn AuditRecorder> = match failing {
            Some(action) => Arc::new(Failing { inner: audit.clone(), action }),
            None => Arc::new(audit.clone()),
        };
        let engine = SurfaceEngine::builder(Arc::new(source))
            .card_hints(Arc::new(workflows.clone()))
            .audit(AuditTrail::new(recorder))
            .poll_interval(Duration::from_millis(20))
            .build();
        Self { checkpointer, sessions, governance, hits, audit, engine, workflows }
    }
}

/// An audit recorder that fails for one action and records every other.
struct Failing {
    inner: InMemoryAuditRecorder,
    action: &'static str,
}

#[async_trait]
impl AuditRecorder for Failing {
    fn digest(&self, value: &Value) -> String {
        self.inner.digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        if draft.action == self.action {
            return Err(adk_core::AdkError::session("the audit store is unavailable"));
        }
        self.inner.record(draft).await
    }
}

/// draft (writes the invoice) → pay (governed, money) → receipt.
fn invoice_run(hits: Arc<AtomicUsize>, checkpointer: Arc<MemoryCheckpointer>) -> GraphAgent {
    GraphAgent::builder("invoice-run")
        .channels(&["invoice", "note", "charge", "receipt", "token"])
        .node_fn("draft", |_ctx| async move {
            Ok(NodeOutput::new()
                .with_update("invoice", json!({ "customer": "Acme", "amountCents": 4200 })))
        })
        .node(Pay(hits))
        .node_fn("receipt", |ctx| async move {
            let id = ctx.get("charge").and_then(|charge| charge["id"].as_str()).unwrap_or("-");
            Ok(NodeOutput::new().with_update("receipt", json!(format!("R-{id}"))))
        })
        .edge(START, "draft")
        .edge("draft", "pay")
        .edge("pay", "receipt")
        .edge("receipt", END)
        .governed_node("pay", GovernedNode::new().with_approval().spending_money())
        .checkpointer_arc(checkpointer.clone())
        .input_mapper(|ctx| {
            let text: String =
                ctx.user_content().parts.iter().filter_map(|part| part.text()).collect();
            if text.is_empty() {
                adk_graph::state::State::new()
            } else {
                [
                    ("note".to_string(), json!(text)),
                    ("token".to_string(), json!("tok_live_0123456789abcdef")),
                ]
                .into()
            }
        })
        .build()
        .unwrap()
}

impl Rig {
    fn thread(session: &str) -> String {
        session_thread_id(APP, OWNER, session)
    }

    async fn start(&self, session: &str) {
        self.engine
            .execute(
                &web(),
                Command::NewThread {
                    agent: "invoice-run".into(),
                    session_id: session.into(),
                    text: "Bill Acme for October".into(),
                },
            )
            .await
            .unwrap();
    }

    async fn run(&self, session: &str) -> WorkflowRun {
        self.workflows.run(&self.engine, &web(), session, None).await.unwrap()
    }

    async fn until(&self, session: &str, status: RunStatus) -> WorkflowRun {
        let started = tokio::time::Instant::now();
        loop {
            let run = self.run(session).await;
            if run.status == status {
                return run;
            }
            assert!(started.elapsed() < WITHIN, "{session} never reached {status:?}: {run:#?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn approve(&self, session: &str) {
        let card = self.engine.session_decisions(&web(), session).await.unwrap().remove(0);
        let outcome = self
            .engine
            .execute(
                &web(),
                Command::Decide {
                    decision_id: card.id,
                    version: card.version,
                    answer: Answer::AllowOnce,
                    edited_args: None,
                    reason: None,
                    expanded: true,
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, adk_surface::CommandOutcome::Answered { .. }), "{outcome:?}");
    }

    /// Runs `session` to Done: started, approved once.
    async fn done(&self, session: &str) -> WorkflowRun {
        self.start(session).await;
        self.until(session, RunStatus::Waiting).await;
        self.approve(session).await;
        self.until(session, RunStatus::Done).await
    }

    async fn checkpoints(&self, session: &str) -> Vec<Checkpoint> {
        self.checkpointer.list(&Self::thread(session)).await.unwrap()
    }

    /// The completed checkpoint after draft.
    async fn after_draft(&self, session: &str) -> String {
        self.run(session)
            .await
            .steps
            .iter()
            .find(|step| step.step == 1)
            .unwrap()
            .checkpoint_id
            .clone()
            .unwrap()
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Spec 3.3 §7.1, §7.2, §7.4 rule 4: a governed node that spends money is a money card with no
/// Always allow, high risk from the router, can't be undone, and says where it came from.
#[tokio::test]
async fn a_graph_nodes_card_is_money_without_always_allow_and_links_to_runs() {
    let rig = Rig::new();
    rig.start("s1").await;
    let run = rig.until("s1", RunStatus::Waiting).await;
    let card = rig.engine.session_decisions(&web(), "s1").await.unwrap().remove(0);
    let answers: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(
        (answers, card.risk_text.as_str(), card.money, card.reversibility, card.detail.clone()),
        (
            vec![Answer::AllowOnce, Answer::Edit, Answer::Deny],
            "High risk",
            true,
            Reversibility::CantBeUndone,
            Some(
                CardDetail::new("From the invoice-run workflow")
                    .with_link("Open in Runs", "/ui/developer/runs/s1")
            ),
        )
    );
    assert_eq!(card.allowed.options[0].label, "Confirm it's you and approve");
    let pay = run.nodes.iter().find(|node| node.node == "pay").unwrap();
    assert_eq!(
        (pay.state, pay.decision_id.as_deref(), rig.hits()),
        (NodeState::Waiting, Some(card.id.as_str()), 0)
    );
}

/// Spec 3.3 §12: the run view's nodes, steps and taken edges while pay waits, then done.
#[tokio::test]
async fn the_run_view_shows_each_node_and_step() {
    let rig = Rig::new();
    rig.start("s1").await;
    let waiting = rig.until("s1", RunStatus::Waiting).await;
    let states = |run: &WorkflowRun| -> Vec<(String, NodeState)> {
        run.nodes.iter().map(|node| (node.node.clone(), node.state)).collect()
    };
    let steps = |run: &WorkflowRun| -> Vec<(usize, StepState, Vec<String>, bool)> {
        run.steps
            .iter()
            .map(|step| (step.step, step.state, step.wrote.clone(), step.forkable))
            .collect()
    };
    assert_eq!(
        (states(&waiting), steps(&waiting), waiting.status_text.as_str()),
        (
            vec![
                ("draft".to_string(), NodeState::Done),
                ("pay".to_string(), NodeState::Waiting),
                ("receipt".to_string(), NodeState::Idle),
            ],
            vec![
                (0, StepState::Input, vec!["note".to_string(), "token".to_string()], false),
                (1, StepState::Completed, vec!["invoice".to_string()], true),
                (2, StepState::Paused, vec![], false),
            ],
            "Waiting for you",
        )
    );
    rig.approve("s1").await;
    let done = rig.until("s1", RunStatus::Done).await;
    assert_eq!(
        (states(&done), steps(&done).len(), done.taken.len(), rig.hits()),
        (
            vec![
                ("draft".to_string(), NodeState::Done),
                ("pay".to_string(), NodeState::Done),
                ("receipt".to_string(), NodeState::Done),
            ],
            4,
            3,
            1,
        )
    );
}

/// Spec 3.3 §12, S-I9: a step's "What changed" passes secret redaction and the host's hidden
/// keys; the step detail is read from one checkpoint and changes nothing.
#[tokio::test]
async fn a_step_detail_redacts_secrets_and_hides_keys_and_writes_nothing() {
    let rig = Rig::hiding(&["customer"]);
    rig.done("s1").await;
    let before = rig.checkpoints("s1").await;
    let c1 = rig.after_draft("s1").await;
    let detail = rig.workflows.step(&rig.engine, &web(), "s1", &c1).await.unwrap();
    assert_eq!(
        detail.changes,
        vec![StateChange {
            key: "invoice".into(),
            change: Change::Added,
            change_text: "Added".into(),
            before: None,
            after: Some(json!({ "customer": "[hidden]", "amountCents": 4200 })),
            note: Some("Hidden: this home hides this key".into()),
        },]
    );
    let token = detail.state_values.iter().find(|value| value.key == "token").unwrap();
    assert_eq!(
        (&token.value, token.note.as_deref()),
        (&json!("[redacted]"), Some("Hidden: looks like a secret"))
    );
    assert_eq!(
        (detail.fork.allowed, detail.fork.runs_again.clone(), detail.fork.already_happened.len()),
        (true, vec!["pay".to_string(), "receipt".to_string()], 1)
    );
    assert_eq!(rig.checkpoints("s1").await, before, "reading a step writes nothing");
}

/// Spec 3.3 §11 S-I8: another person's session answers not found; a paired device and any
/// surface but the web home cannot fork.
#[tokio::test]
async fn a_fork_is_the_owners_on_the_web_home_only() {
    let rig = Rig::new();
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    let request = ForkRequest { from: c1, set: Default::default(), start: false };
    let bob = Caller::human("bob", SurfaceKind::Web, Origin::Pointer);
    let device = web().with_device("phone-1");
    let terminal = Caller::human(OWNER, SurfaceKind::Terminal, Origin::Keyboard);
    let mut codes = Vec::new();
    for caller in [bob, device, terminal] {
        let error =
            rig.workflows.fork(&rig.engine, &caller, "s1", request.clone()).await.unwrap_err();
        codes.push(error.code());
    }
    assert_eq!(codes, vec!["surface.not_found", "surface.not_here", "surface.not_here"]);
    assert_eq!(rig.sessions.list(list()).await.unwrap().len(), 1, "no fork was made");
}

/// A refusal's code and the words its problem document carries.
fn said(error: adk_surface::SurfaceError) -> (String, String) {
    let error = adk_core::AdkError::from(error);
    (error.code.to_string(), error.message)
}

fn list() -> adk_session::ListRequest {
    adk_session::ListRequest {
        app_name: APP.into(),
        user_id: OWNER.into(),
        limit: None,
        offset: None,
    }
}

/// Spec 3.3 §12, owner question 9: a fork waits until the session's decision is answered.
#[tokio::test]
async fn a_fork_is_refused_while_a_decision_waits() {
    let rig = Rig::new();
    rig.start("s1").await;
    rig.until("s1", RunStatus::Waiting).await;
    let c1 = rig.after_draft("s1").await;
    let error = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: c1, set: Default::default(), start: true },
        )
        .await
        .unwrap_err();
    assert_eq!(
        said(error),
        ("workflow.decision_waiting".to_string(), "Answer the waiting decision first".to_string())
    );
}

/// Spec 3.3 §12, §11: under Stop all a fork is made and not started; its record names the
/// session, the checkpoint, the fork and the edits, with the surface; the original is
/// unchanged; Resume all, then start, runs it from the record without a second charge.
#[tokio::test]
async fn a_fork_under_stop_all_is_made_not_started_and_recorded() {
    let rig = Rig::new();
    rig.done("s1").await;
    let before = rig.checkpoints("s1").await;
    let c1 = rig.after_draft("s1").await;
    rig.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
    let outcome = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest {
                from: c1.clone(),
                set: [("note".to_string(), json!("re-run for audit"))].into(),
                start: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (outcome.started, outcome.text.as_str(), outcome.edited.clone()),
        (
            false,
            "Forked. Everything is stopped — start it after Resume all.",
            vec!["note".to_string()]
        )
    );
    let fork = rig.run(&outcome.session_id).await;
    assert_eq!(fork.status, RunStatus::NotStarted);
    let records: Vec<AuditRecordDraft> = rig
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::workflow::SURFACE_AUDIT_WORKFLOW_FORK)
        .collect();
    let expected = AuditRecordDraft::new(
        AuditRecordKind::Other("surface".into()),
        adk_surface::workflow::SURFACE_AUDIT_WORKFLOW_FORK,
    )
    .actor(AuditActor::subject(web().subject))
    .ext("surface", json!("web"))
    .ext("origin", json!({ "kind": "pointer" }))
    .ext("presence", json!("confirmed"))
    .ext("sessionId", json!("s1"))
    .ext("from", json!(c1))
    .ext("forkSessionId", json!(outcome.session_id))
    .ext("edited", json!(["note"]));
    assert_eq!(records, vec![expected]);

    let refused = rig.workflows.start(&rig.engine, &web(), &outcome.session_id).await.unwrap_err();
    assert_eq!(refused.code(), "workflow.stopped");
    rig.engine.execute(&web(), Command::ResumeAll).await.unwrap();
    rig.workflows.start(&rig.engine, &web(), &outcome.session_id).await.unwrap();
    let done = rig.until(&outcome.session_id, RunStatus::Done).await;
    let pay = done.nodes.iter().find(|node| node.node == "pay").unwrap();
    assert_eq!((pay.state, rig.hits()), (NodeState::Recorded, 1));
    assert_eq!(rig.checkpoints("s1").await, before, "the original thread is unchanged");
    assert_eq!(rig.run("s1").await.forks.len(), 1);
    assert!(!rig.governance.is_frozen());
}

/// Spec 3.3 §12: a fork from a pause or from the last step is refused in the dialog's words.
#[tokio::test]
async fn a_fork_from_the_last_step_is_refused() {
    let rig = Rig::new();
    let done = rig.done("s1").await;
    let last = done.steps.last().unwrap().checkpoint_id.clone().unwrap();
    let error = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: last, set: Default::default(), start: false },
        )
        .await
        .unwrap_err();
    assert_eq!(
        said(error),
        ("fork.nothing_to_run".to_string(), "Nothing runs after the last step".to_string())
    );
    let c1 = rig.after_draft("s1").await;
    let error = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: c1, set: [("amount".to_string(), json!(1))].into(), start: false },
        )
        .await
        .unwrap_err();
    assert_eq!(
        said(error),
        ("fork.undeclared_channel".to_string(), "Not a channel of this graph: amount".to_string())
    );
}

// ── Routes ─────────────────────────────────────────────────────────

fn routes(rig: &Rig, caller: bool) -> axum::Router {
    let routes = adk_surface::web::workflow::router(rig.workflows.clone(), rig.engine.clone());
    let router = axum::Router::new().nest("/api", routes);
    if caller {
        router.layer(axum::Extension(Some(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: Default::default(),
        })))
    } else {
        router
    }
}

async fn get(router: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let response =
        router.clone().oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Spec 3.3 §12: every route needs a caller; another person's session is not found (S-I8).
#[tokio::test]
async fn the_routes_need_a_caller_and_answer_only_their_own_sessions() {
    let rig = Rig::new();
    rig.start("s1").await;
    rig.until("s1", RunStatus::Waiting).await;
    let anonymous = routes(&rig, false);
    let owner = routes(&rig, true);
    let mut statuses = Vec::new();
    for (router, uri) in [
        (&anonymous, "/api/surface/workflow/graphs"),
        (&anonymous, "/api/surface/workflow/sessions/s1/run"),
        (&owner, "/api/surface/workflow/graphs"),
        (&owner, "/api/surface/workflow/sessions/s1/run"),
        (&owner, "/api/surface/workflow/sessions/s1/runs"),
        (&owner, "/api/surface/workflow/sessions/nobody/run"),
        (&owner, "/api/surface/workflow/sessions/s1/checkpoints/not-a-checkpoint"),
    ] {
        statuses.push(get(router, uri).await.0);
    }
    assert_eq!(
        statuses,
        vec![
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED,
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::NOT_FOUND,
            StatusCode::NOT_FOUND,
        ]
    );
    let (_, run) = get(&owner, "/api/surface/workflow/sessions/s1/run").await;
    assert_eq!(
        (run["status"].clone(), run["graph"].clone()),
        (json!("waiting"), json!("invoice-run"))
    );
}

/// The first `changed` frame of the feed from `last`, if one comes within a second.
async fn first_frame(router: &axum::Router, last: Option<u64>) -> Option<String> {
    let mut request = Request::get("/api/surface/workflow/sessions/s1/feed");
    if let Some(last) = last {
        request = request.header("last-event-id", last.to_string());
    }
    let response = router.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(1), body.frame()).await.ok()??.ok()?;
    frame.into_data().ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Spec 3.3 §12: the feed sends `changed` with the newest version, and resumes after
/// `Last-Event-ID`: nothing until the thread writes a newer checkpoint.
#[tokio::test]
async fn the_feed_resumes_after_the_last_event_id() {
    let rig = Rig::new();
    rig.start("s1").await;
    rig.until("s1", RunStatus::Waiting).await;
    let version = rig.workflows.version(&web(), "s1").await.unwrap().unwrap();
    let router = routes(&rig, true);
    assert_eq!(
        first_frame(&router, None).await,
        Some(format!("id: {version}\nevent: changed\ndata: {{\"version\":{version}}}\n\n"))
    );
    assert_eq!(first_frame(&router, Some(version)).await, None, "nothing new after the last id");
    assert_eq!(
        first_frame(&router, Some(version - 1)).await,
        Some(format!("id: {version}\nevent: changed\ndata: {{\"version\":{version}}}\n\n"))
    );
}

// ── Wave 4 review fixes ────────────────────────────────────────────

/// draft → billing, a subgraph whose governed `pay` spends money.
fn billing_run(hits: Arc<AtomicUsize>, checkpointer: Arc<MemoryCheckpointer>) -> GraphAgent {
    let billing = adk_graph::graph::StateGraph::with_channels(&["invoice", "charge"])
        .add_node(Pay(hits))
        .add_edge(START, "pay")
        .add_edge("pay", END)
        .compile()
        .unwrap()
        .with_checkpointer_arc(checkpointer.clone())
        .with_governed_node("pay", GovernedNode::new().with_approval().spending_money());
    GraphAgent::builder("invoice-run")
        .channels(&["invoice", "note", "charge"])
        .node_fn("draft", |_ctx| async move {
            Ok(NodeOutput::new()
                .with_update("invoice", json!({ "customer": "Acme", "amountCents": 4200 })))
        })
        .node(adk_graph::subgraph::SubgraphNode::new("billing", Arc::new(billing)))
        .edge(START, "draft")
        .edge("draft", "billing")
        .edge("billing", END)
        .checkpointer_arc(checkpointer)
        .build()
        .unwrap()
}

/// Spec 3.3 §7.1, §7.4 rule 4, §12: a governed node inside a subgraph is a money card with
/// no Always allow and high risk, and the run view shows its subgraph waiting for it.
#[tokio::test]
async fn a_governed_node_inside_a_subgraph_is_carded_routed_and_shown_waiting() {
    let rig = Rig::with(&[], billing_run, None, true);
    rig.start("s1").await;
    let run = rig.until("s1", RunStatus::Waiting).await;
    let card = rig.engine.session_decisions(&web(), "s1").await.unwrap().remove(0);
    let answers: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(
        (answers, card.risk_text.as_str(), card.money),
        (vec![Answer::AllowOnce, Answer::Edit, Answer::Deny], "High risk", true)
    );
    let billing = run.nodes.iter().find(|node| node.node == "billing").unwrap();
    assert_eq!(
        (billing.state, billing.decision_id.as_deref()),
        (NodeState::Waiting, Some(card.id.as_str()))
    );
}

/// Spec 3.3 §12: a fork's run that is under way is not started again, and shows as working
/// rather than not started.
#[tokio::test]
async fn a_fork_under_way_is_not_started_twice() {
    let rig = Rig::new();
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    let outcome = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: c1, set: Default::default(), start: true },
        )
        .await
        .unwrap();
    assert!(outcome.started);
    assert_ne!(rig.run(&outcome.session_id).await.status, RunStatus::NotStarted);
    let again = rig.workflows.start(&rig.engine, &web(), &outcome.session_id).await;
    assert_eq!(again.unwrap_err().code(), "workflow.started");

    let done = rig.until(&outcome.session_id, RunStatus::Done).await;
    let pay = done.nodes.iter().find(|node| node.node == "pay").unwrap();
    assert_eq!((pay.state, rig.hits()), (NodeState::Recorded, 1));
    let starts = rig
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::workflow::SURFACE_AUDIT_WORKFLOW_START)
        .count();
    assert_eq!(starts, 1, "the start is recorded once");
}

/// Spec 3.3 §11, §12: a fork the audit could not record is removed, so it cannot be started
/// unrecorded.
#[tokio::test]
async fn a_fork_the_audit_could_not_record_is_removed() {
    let rig =
        Rig::with(&[], invoice_run, Some(adk_surface::workflow::SURFACE_AUDIT_WORKFLOW_FORK), true);
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    let failed = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: c1, set: Default::default(), start: true },
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(rig.sessions.list(list()).await.unwrap().len(), 1, "no fork session is left");
    assert_eq!(rig.run("s1").await.forks.len(), 0);
    assert_eq!(rig.hits(), 1);
}

/// Spec 3.3 §12: a fork asked to start where no run can start is refused before anything is
/// made, so a retry makes no second fork.
#[tokio::test]
async fn a_fork_that_cannot_start_is_refused_before_it_is_made() {
    let rig = Rig::with(&[], invoice_run, None, false);
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    let refused = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest { from: c1, set: Default::default(), start: true },
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "surface.unsupported");
    assert_eq!(rig.sessions.list(list()).await.unwrap().len(), 1, "no fork was made");
}

/// Spec 3.3 §12: what a fork from a step would run again is the rest of that step's run, not
/// a later run of the session.
#[tokio::test]
async fn a_fork_preview_names_only_its_own_runs_nodes() {
    let rig = Rig::new();
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    rig.engine
        .execute(&web(), Command::Send { session_id: "s1".into(), text: "And for November".into() })
        .await
        .unwrap();
    rig.until("s1", RunStatus::Waiting).await;
    let detail = rig.workflows.step(&rig.engine, &web(), "s1", &c1).await.unwrap();
    assert_eq!(detail.fork.runs_again, vec!["pay".to_string(), "receipt".to_string()]);
    assert_eq!(detail.fork.changed_calls.as_deref(), Some("A changed call waits for you."));
}

/// Spec 3.3 §12, S-I9: an edit made from a value shown with a hidden part keeps the value the
/// part stands for.
#[tokio::test]
async fn a_fork_edit_keeps_the_value_a_hidden_part_stands_for() {
    let rig = Rig::hiding(&["customer"]);
    rig.done("s1").await;
    let c1 = rig.after_draft("s1").await;
    let outcome = rig
        .workflows
        .fork(
            &rig.engine,
            &web(),
            "s1",
            ForkRequest {
                from: c1,
                set: [(
                    "invoice".to_string(),
                    json!({ "customer": "[hidden]", "amountCents": 5000 }),
                )]
                .into(),
                start: false,
            },
        )
        .await
        .unwrap();
    let fork = rig.checkpoints(&outcome.session_id).await.remove(0);
    assert_eq!(fork.state["invoice"], json!({ "customer": "Acme", "amountCents": 5000 }));
}

/// Spec 3.3 §10 K2, S-I7: a paired device's Runs feed ends when the device is revoked.
#[tokio::test]
async fn a_revoked_devices_feed_ends() {
    use adk_kernel::pairing::{
        DEVICE_ID_METADATA, DEVICE_KIND_METADATA, InMemoryDeviceStore, IssueCode, Pairing,
    };
    let rig = Rig::new();
    rig.start("s1").await;
    rig.until("s1", RunStatus::Waiting).await;
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    let issued =
        pairing.issue(IssueCode::new(OWNER, ["owner"]).with_name("Your phone")).await.unwrap();
    let device = pairing.claim(&issued.code, None).await.unwrap().device.id;
    let context = RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into()],
        metadata: [
            (DEVICE_ID_METADATA.to_string(), device.clone()),
            (DEVICE_KIND_METADATA.to_string(), "phone".to_string()),
        ]
        .into(),
    };
    let app = axum::Router::new()
        .nest(
            "/api",
            adk_surface::web::workflow::router_with_devices(
                rig.workflows.clone(),
                rig.engine.clone(),
                pairing.clone(),
            ),
        )
        .layer(axum::Extension(Some(context)));
    let response = app
        .oneshot(
            Request::get("/api/surface/workflow/sessions/s1/feed").body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    // The newest version arrives, then the feed stays open while the phone is paired.
    let _ = tokio::time::timeout(Duration::from_millis(500), body.frame()).await;
    let open = tokio::time::timeout(Duration::from_millis(300), body.frame()).await;
    assert!(open.is_err(), "the feed stays open while the phone is paired");
    pairing.revoke(&device).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while body.frame().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "the feed ended with the phone");
}
