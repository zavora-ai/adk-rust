//! Spec 3.3 §12 developer track, "Approve, rewind, fork": a graph with a model step, a
//! payment-like HTTP call and a decision, approved from the observer; a fork at an earlier step
//! leaves the original intact and the HTTP call is not repeated.
//!
//! `invoice-run` is draft (a model step on `ScriptedLlm`) → pay (a governed HTTP POST of the
//! invoice to a loopback provider that counts its charges) → receipt, run by a `Runner` under
//! the engine, with the observer's card hints, router and routes. The scenario runs on memory
//! and on SQLite, with a restart; the PostgreSQL leg runs by hand:
//!
//! ```text
//! cargo nextest run -p adk-surface --features web,workflow --test workflow_scenario_tests
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-surface --features web,workflow --test workflow_scenario_tests --run-ignored only
//! ```

#![cfg(all(feature = "web", feature = "workflow"))]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_action::{
    ActionNodeConfig, Callbacks, ErrorHandling, ErrorMode, ExecutionControl, HttpAuth, HttpBody,
    HttpMethod, HttpNodeConfig, HttpResponse, InputOutputMapping, LogLevel, StandardProperties,
    Tracing,
};
use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActionLedger, ActionOutcome, ApprovalPolicy, ApprovalRequest, ApprovalStore, ApproverSelector,
    Content, Event, GovernanceControl, InMemoryActionLedger, InMemoryApprovalStore, RequestContext,
    RunConfig, StoreApprovalService,
};
use adk_graph::action::ActionNodeExecutor;
use adk_graph::action::http::HttpActionPolicy;
use adk_graph::agent::{GraphAgent, session_thread_id};
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer, SqliteCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::governed::{GovernedNode, effect_log};
use adk_graph::node::{AgentNode, NodeOutput};
use adk_graph::record::Lineage;
use adk_graph::state::{Checkpoint, State};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{
    InMemorySessionService, SessionService, SqliteActionLedger, SqliteApprovalStore,
    SqliteSessionService,
};
use adk_surface::web::PRESENCE_METADATA_KEY;
use adk_surface::workflow::{NodeState, RunStatus, WorkflowRun, Workflows};
use adk_surface::{DecisionCard, LocalSource, Place, SurfaceEngine};
use axum::body::Body;
use axum::extract::State as AxumState;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const OWNER: &str = "ana";
const WITHIN: Duration = Duration::from_secs(30);

// ── The provider ───────────────────────────────────────────────────

/// A loopback payment provider that counts every charge; stopped when dropped.
struct Provider {
    base: String,
    hits: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Provider {
    async fn start() -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/charges",
                post(|AxumState(hits): AxumState<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                    let hit = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    Json(json!({ "id": format!("ch_{hit}"), "amountCents": body["amountCents"] }))
                }),
            )
            .route(
                "/hits",
                get(|AxumState(hits): AxumState<Arc<AtomicUsize>>| async move {
                    Json(json!({ "hits": hits.load(Ordering::SeqCst) }))
                }),
            )
            .with_state(Arc::clone(&hits));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { base, hits, task }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

// ── The stores ─────────────────────────────────────────────────────

enum Backend {
    Memory,
    Sqlite(PathBuf),
    Postgres(String),
}

#[derive(Clone)]
struct Stores {
    sessions: Arc<dyn SessionService>,
    checkpointer: Arc<dyn Checkpointer>,
    ledger: Arc<dyn ActionLedger>,
    inbox: Arc<dyn ApprovalStore>,
}

/// The backend's stores; opening SQLite or PostgreSQL again is a restart.
async fn open(backend: &Backend) -> Stores {
    match backend {
        Backend::Memory => Stores {
            sessions: Arc::new(InMemorySessionService::new()),
            checkpointer: Arc::new(MemoryCheckpointer::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
            inbox: Arc::new(InMemoryApprovalStore::new()),
        },
        Backend::Sqlite(dir) => {
            let url = |name: &str| format!("sqlite://{}?mode=rwc", dir.join(name).display());
            let sessions = SqliteSessionService::new(&url("sessions.db")).await.unwrap();
            sessions.migrate().await.unwrap();
            let ledger = SqliteActionLedger::new(&url("ledger.db")).await.unwrap();
            ledger.migrate().await.unwrap();
            let inbox = SqliteApprovalStore::new(&url("approvals.db")).await.unwrap();
            inbox.migrate().await.unwrap();
            Stores {
                sessions: Arc::new(sessions),
                checkpointer: Arc::new(
                    SqliteCheckpointer::new(&url("checkpoints.db")).await.unwrap(),
                ),
                ledger: Arc::new(ledger),
                inbox: Arc::new(inbox),
            }
        }
        Backend::Postgres(url) => {
            let sessions = adk_session::PostgresSessionService::new(url).await.unwrap();
            sessions.migrate().await.unwrap();
            let ledger = adk_session::PostgresActionLedger::new(url).await.unwrap();
            ledger.migrate().await.unwrap();
            let inbox = adk_session::PostgresApprovalStore::new(url).await.unwrap();
            inbox.migrate().await.unwrap();
            let checkpointer = adk_graph::PostgresCheckpointer::new(url).await.unwrap();
            checkpointer.migrate().await.unwrap();
            Stores {
                sessions: Arc::new(sessions),
                checkpointer: Arc::new(checkpointer),
                ledger: Arc::new(ledger),
                inbox: Arc::new(inbox),
            }
        }
    }
}

// ── The home ───────────────────────────────────────────────────────

struct Home {
    api: Router,
}

fn invoice_run(base: &str, checkpointer: Arc<dyn Checkpointer>) -> GraphAgent {
    let invoice = r#"{"customer":"Acme","amountCents":4200}"#;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        (0..16).map(|_| ScriptedResponse::text(invoice)).collect::<Vec<_>>(),
    ));
    let drafter = LlmAgentBuilder::new("draft")
        .model(model)
        .instruction("Draft the invoice.")
        .build()
        .unwrap();
    let draft = AgentNode::new(Arc::new(drafter))
        .with_input_mapper(|state: &State| {
            Content::new("user")
                .with_text(state.get("note").and_then(Value::as_str).unwrap_or_default())
        })
        .with_output_mapper(|events: &[Event]| {
            let text: String = events
                .iter()
                .filter_map(|event| event.content())
                .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
                .collect();
            serde_json::from_str::<Value>(&text)
                .map(|invoice| [("invoice".to_string(), invoice)].into())
                .unwrap_or_default()
        });
    let pay = ActionNodeExecutor::new(ActionNodeConfig::Http(HttpNodeConfig {
        standard: StandardProperties {
            id: "pay".to_string(),
            name: "pay".to_string(),
            description: None,
            position: None,
            error_handling: ErrorHandling {
                mode: ErrorMode::Stop,
                retry_count: None,
                retry_delay: None,
                fallback_value: None,
            },
            tracing: Tracing { enabled: false, log_level: LogLevel::None },
            callbacks: Callbacks { on_start: None, on_complete: None, on_error: None },
            execution: ExecutionControl { timeout: 10_000, condition: None },
            mapping: InputOutputMapping { input_mapping: None, output_key: "charge".to_string() },
        },
        method: HttpMethod::Post,
        url: format!("{base}/charges"),
        auth: HttpAuth::None,
        headers: Default::default(),
        body: HttpBody::Json {
            data: json!({ "customer": "{{invoice.customer}}", "amountCents": "{{invoice.amountCents}}" }),
        },
        response: HttpResponse { response_type: "json".to_string(), status_validation: None },
        rate_limit: None,
    }))
    .with_http_policy(
        HttpActionPolicy::new()
            .allow_schemes(["http"])
            .allow_hosts(["127.0.0.1"])
            .allow_private_networks()
            .max_redirects(0),
    );
    GraphAgent::builder("invoice-run")
        .channels(&["invoice", "note", "charge", "receipt"])
        .node(draft)
        .node(pay)
        .node_fn("receipt", |ctx| async move {
            let id =
                ctx.get("charge").and_then(|charge| charge["data"]["id"].as_str()).unwrap_or("-");
            Ok(NodeOutput::new().with_update("receipt", json!(format!("R-{id}"))))
        })
        .edge(START, "draft")
        .conditional_edge(
            "draft",
            |state: &State| {
                if state.get("invoice").is_none_or(Value::is_null) {
                    END.to_string()
                } else {
                    "pay".to_string()
                }
            },
            [("pay", "pay"), (END, END)],
        )
        .edge("pay", "receipt")
        .edge("receipt", END)
        .governed_node("pay", GovernedNode::new().with_approval().spending_money())
        .checkpointer_arc(checkpointer)
        .input_mapper(|ctx| {
            let text: String =
                ctx.user_content().parts.iter().filter_map(|part| part.text()).collect();
            if text.is_empty() { State::new() } else { [("note".to_string(), json!(text))].into() }
        })
        .build()
        .unwrap()
}

fn heading(request: &ApprovalRequest) -> String {
    let body = &request.args_redacted["body"];
    let cents: i64 = body["amountCents"].as_str().and_then(|cents| cents.parse().ok()).unwrap_or(0);
    format!(
        "Charge {} {}.{:02} USD",
        body["customer"].as_str().unwrap_or("?"),
        cents / 100,
        cents % 100
    )
}

/// Builds the runner, the source, the engine and the routes of application `app` over
/// `stores`.
fn home(app: &str, stores: &Stores, provider: &Provider, audit: &InMemoryAuditRecorder) -> Home {
    let graph = Arc::new(invoice_run(&provider.base, Arc::clone(&stores.checkpointer)));
    let workflows = Workflows::new(app)
        .graph(Arc::clone(&graph))
        .sessions(Arc::clone(&stores.sessions))
        .utc_offset(chrono::FixedOffset::east_opt(3 * 3600).unwrap());
    let approvals = Arc::new(StoreApprovalService::new(Arc::clone(&stores.inbox)).with_router(
        workflows.approval_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ),
    ));
    let governance = GovernanceControl::new();
    let runner = Arc::new(
        Runner::builder()
            .app_name(app)
            .agent(graph)
            .session_service(Arc::clone(&stores.sessions))
            .governance(governance.clone())
            .run_config(
                RunConfig::builder()
                    .approval_service(approvals.clone())
                    .action_ledger(Arc::clone(&stores.ledger))
                    .build(),
            )
            .build()
            .unwrap(),
    );
    let workflows = workflows.runner(Arc::clone(&runner));
    let source = LocalSource::builder(app)
        .sessions(Arc::clone(&stores.sessions))
        .approvals(approvals)
        .runner(runner)
        .governance(governance)
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web()])
        .heading(heading)
        .card_hints(Arc::new(workflows.clone()))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .poll_interval(Duration::from_millis(20))
        .build();
    let routes = adk_surface::web::router(engine.clone())
        .merge(adk_surface::web::workflow::router(workflows.clone(), engine.clone()));
    let api = Router::new().nest("/api", routes).layer(axum::Extension(Some(RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: [(PRESENCE_METADATA_KEY.to_string(), "confirmed".to_string())].into(),
    })));
    Home { api }
}

impl Home {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        // The web home's own page: a browser's same-origin request (spec 3.3 S-I10).
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("sec-fetch-site", "same-origin")
            .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
            .unwrap();
        let response = self.api.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method, uri, body).await;
        assert_eq!(status, StatusCode::OK, "{method} {uri}: {value}");
        value
    }

    async fn run(&self, session: &str) -> WorkflowRun {
        serde_json::from_value(
            self.ok("GET", &format!("/api/surface/workflow/sessions/{session}/run"), None).await,
        )
        .unwrap()
    }

    async fn until(&self, session: &str, status: RunStatus) -> WorkflowRun {
        let started = tokio::time::Instant::now();
        loop {
            let run = self.run(session).await;
            if run.status == status {
                return run;
            }
            assert!(started.elapsed() < WITHIN, "{session} never reached {status:?}: {run:#?}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn cards(&self, session: &str) -> Vec<DecisionCard> {
        serde_json::from_value(
            self.ok("GET", &format!("/api/surface/decisions?session={session}"), None).await["decisions"].clone(),
        )
        .unwrap()
    }

    async fn command(&self, command: Value) -> Value {
        self.ok(
            "POST",
            "/api/surface/commands",
            Some(json!({ "surface": "web", "origin": { "kind": "pointer" }, "command": command })),
        )
        .await
    }

    async fn decide(&self, card: &DecisionCard, answer: &str) {
        let outcome = self
            .command(json!({ "type": "decide", "decisionId": card.id, "version": card.version, "answer": answer, "expanded": true }))
            .await;
        assert_eq!(outcome["type"], "answered", "{outcome}");
    }

    async fn fork(&self, session: &str, from: &str, set: Value, start: bool) -> Value {
        self.ok(
            "POST",
            &format!("/api/surface/workflow/sessions/{session}/fork"),
            Some(json!({ "from": from, "set": set, "start": start })),
        )
        .await
    }
}

fn thread(app: &str, session: &str) -> String {
    session_thread_id(app, OWNER, session)
}

fn node(run: &WorkflowRun, name: &str) -> NodeState {
    run.nodes.iter().find(|node| node.node == name).map(|node| node.state).unwrap()
}

async fn newest(app: &str, stores: &Stores, session: &str) -> Checkpoint {
    stores.checkpointer.load(&thread(app, session)).await.unwrap().unwrap()
}

fn fork_records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|record| record.action == action).collect()
}

/// The scenario as application `app`, so a shared database keeps each run apart.
async fn approve_rewind_fork(app: &str, backend: Backend) {
    let provider = Provider::start().await;
    let audit = InMemoryAuditRecorder::new();
    let mut stores = open(&backend).await;
    let mut home = home(app, &stores, &provider, &audit);
    let t1 = thread(app, "s1");
    let agent_id = format!("urn:adk:agent:{app}/invoice-run");
    let newest = |stores: Stores, session: String| {
        let app = app.to_string();
        async move { newest(&app, &stores, &session).await }
    };

    // 1. Start: one card for pay, nothing charged, the run waits at pay.
    home.command(json!({ "type": "newThread", "agent": "invoice-run", "sessionId": "s1", "text": "Bill Acme for October" }))
        .await;
    let waiting = home.until("s1", RunStatus::Waiting).await;
    let cards = home.cards("s1").await;
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    let answers: Vec<String> =
        card.allowed.options.iter().map(|option| option.label.clone()).collect();
    assert_eq!(
        (
            card.heading.as_str(),
            card.call.tool.as_str(),
            card.call.args.clone(),
            card.money,
            answers,
            card.detail.as_ref().map(|detail| (detail.text.clone(), detail.link_text.clone())),
        ),
        (
            "Charge Acme 42.00 USD",
            "pay",
            json!({
                "method": "POST",
                "url": format!("{}/charges", provider.base),
                "headers": { "content-type": "application/json" },
                "body": { "customer": "Acme", "amountCents": "4200" },
            }),
            true,
            vec![
                "Confirm it's you and approve".to_string(),
                "Edit".to_string(),
                "Deny".to_string()
            ],
            Some(("From the invoice-run workflow".to_string(), Some("Open in Runs".to_string()))),
        )
    );
    let pay = waiting.nodes.iter().find(|node| node.node == "pay").unwrap();
    assert_eq!(
        (
            node(&waiting, "draft"),
            pay.state,
            pay.decision_id.as_deref(),
            node(&waiting, "receipt"),
            provider.hits()
        ),
        (NodeState::Done, NodeState::Waiting, Some(card.id.as_str()), NodeState::Idle, 0)
    );

    // 2. Approve from the Runs pane: the run finishes, one charge, one ledger record, one
    // effect-log entry, the answer on the record with its surface.
    home.decide(card, "allowOnce").await;
    let done = home.until("s1", RunStatus::Done).await;
    assert_eq!(provider.hits(), 1);
    let decides: Vec<Value> = fork_records(&audit, "surface.decide")
        .into_iter()
        .map(|record| record.ext["surface"].clone())
        .collect();
    assert_eq!(decides, vec![json!("web")]);
    let key = format!("graph/{agent_id}/{t1}/pay/1/{}", adk_core::json_digest(&card.call.args));
    let record = stores.ledger.get(&key).await.unwrap().expect("one ledger record");
    assert!(matches!(record.outcome, Some(ActionOutcome::Succeeded { .. })), "{record:?}");
    let effects = effect_log(stores.checkpointer.as_ref(), &t1).await.unwrap();
    let charge = newest(stores.clone(), "s1".into()).await.state["charge"].clone();
    assert_eq!(
        effects
            .iter()
            .map(|effect| (effect.key.clone(), effect.writes.clone()))
            .collect::<Vec<_>>(),
        vec![(key, [("charge".to_string(), charge.clone())].into())]
    );
    let snapshot = stores.checkpointer.list(&t1).await.unwrap();

    // 3. Restart (SQLite and PostgreSQL): the same run, read again.
    if !matches!(backend, Backend::Memory) {
        drop(home);
        stores = open(&backend).await;
        home = self::home(app, &stores, &provider, &audit);
        assert_eq!(home.run("s1").await, done);
    }

    // 4. Rewind is read-only: the step after draft holds no charge, and nothing changed.
    let c1 = done
        .steps
        .iter()
        .find(|step| step.step == 1)
        .and_then(|step| step.checkpoint_id.clone())
        .unwrap();
    let detail =
        home.ok("GET", &format!("/api/surface/workflow/sessions/s1/checkpoints/{c1}"), None).await;
    let charge_then: Vec<Value> = detail["stateValues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|value| value["key"] == "charge")
        .map(|value| value["value"].clone())
        .collect();
    assert_eq!(charge_then, vec![Value::Null]);
    assert_eq!(
        (stores.checkpointer.list(&t1).await.unwrap(), provider.hits()),
        (snapshot.clone(), 1)
    );

    // 5. Fork with the call unchanged: answered from the record, never sent again.
    let fork = home.fork("s1", &c1, json!({ "note": "re-run for audit" }), true).await;
    let s2 = fork["sessionId"].as_str().unwrap().to_string();
    let forked = home.until(&s2, RunStatus::Done).await;
    assert_eq!(
        (node(&forked, "pay"), home.cards(&s2).await.len(), provider.hits()),
        (NodeState::Recorded, 0, 1)
    );
    assert_eq!(newest(stores.clone(), s2.clone()).await.state["charge"], charge);
    assert_eq!(
        newest(stores.clone(), s2.clone()).await.lineage().unwrap(),
        Some(Lineage {
            root: t1.clone(),
            parent: t1.clone(),
            parent_checkpoint: c1.clone(),
            parent_step: 1,
            edited: vec!["note".to_string()],
            checked: true,
        })
    );
    let forks: Vec<Vec<Value>> =
        fork_records(&audit, adk_surface::workflow::SURFACE_AUDIT_WORKFLOW_FORK)
            .into_iter()
            .map(|record| {
                ["sessionId", "from", "forkSessionId", "edited", "surface"]
                    .iter()
                    .map(|key| record.ext[*key].clone())
                    .collect()
            })
            .collect();
    assert_eq!(forks, vec![vec![json!("s1"), json!(c1), json!(s2), json!(["note"]), json!("web")]]);
    assert_eq!(stores.checkpointer.list(&t1).await.unwrap(), snapshot);

    // 6. Fork with the call changed: a new decision for 5000; denied, nothing is sent.
    let changed = json!({ "invoice": { "customer": "Acme", "amountCents": 5000 } });
    let s3 = home.fork("s1", &c1, changed.clone(), true).await["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    home.until(&s3, RunStatus::Waiting).await;
    let card = home.cards(&s3).await.remove(0);
    assert_eq!(card.call.args["body"], json!({ "customer": "Acme", "amountCents": "5000" }));
    home.decide(&card, "deny").await;
    let denied = home.until(&s3, RunStatus::Failed).await;
    assert_eq!((node(&denied, "pay"), provider.hits()), (NodeState::NotRun, 1));
    // A sibling asks again and, approved, charges 5000 once; the next sibling is answered
    // from the effect log.
    let s4 = home.fork("s1", &c1, changed.clone(), true).await["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    home.until(&s4, RunStatus::Waiting).await;
    let card = home.cards(&s4).await.remove(0);
    home.decide(&card, "allowOnce").await;
    home.until(&s4, RunStatus::Done).await;
    assert_eq!(provider.hits(), 2);
    let s5 = home.fork("s1", &c1, changed.clone(), true).await["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let sibling = home.until(&s5, RunStatus::Done).await;
    assert_eq!(
        (
            node(&sibling, "pay"),
            provider.hits(),
            newest(stores.clone(), s5.clone()).await.state["charge"].clone()
        ),
        (NodeState::Recorded, 2, newest(stores.clone(), s4.clone()).await.state["charge"].clone())
    );
    assert_eq!(stores.checkpointer.list(&t1).await.unwrap(), snapshot);

    // 7. Stop all during a fork: made, not started; started after Resume all, answered from
    // the record. A freeze before a changed call's charge refuses it before the ledger.
    home.command(json!({ "type": "stopAll" })).await;
    let stopped = home.fork("s1", &c1, json!({ "note": "while stopped" }), true).await;
    assert_eq!(
        (stopped["started"].clone(), stopped["text"].clone()),
        (json!(false), json!("Forked. Everything is stopped — start it after Resume all."))
    );
    let s6 = stopped["sessionId"].as_str().unwrap().to_string();
    assert_eq!(home.run(&s6).await.status, RunStatus::NotStarted);
    let (status, refused) =
        home.call("POST", &format!("/api/surface/workflow/sessions/{s6}/start"), None).await;
    assert_eq!(
        (status, refused["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("workflow.stopped"))
    );
    home.command(json!({ "type": "resumeAll" })).await;
    home.ok("POST", &format!("/api/surface/workflow/sessions/{s6}/start"), None).await;
    let resumed = home.until(&s6, RunStatus::Done).await;
    assert_eq!((node(&resumed, "pay"), provider.hits()), (NodeState::Recorded, 2));

    let s7 = home
        .fork("s1", &c1, json!({ "invoice": { "customer": "Acme", "amountCents": 7000 } }), true)
        .await["sessionId"]
        .as_str()
        .unwrap()
        .to_string();
    home.until(&s7, RunStatus::Waiting).await;
    let card = home.cards(&s7).await.remove(0);
    home.command(json!({ "type": "stopAll" })).await;
    home.decide(&card, "allowOnce").await;
    let frozen = home.until(&s7, RunStatus::Stopped).await;
    let key = format!("graph/{agent_id}/{t1}/pay/1/{}", adk_core::json_digest(&card.call.args));
    assert_eq!(
        (node(&frozen, "pay"), provider.hits(), stores.ledger.get(&key).await.unwrap().is_none()),
        (NodeState::Paused, 2, true)
    );
    home.command(json!({ "type": "resumeAll" })).await;

    // T1 never changed.
    assert_eq!(stores.checkpointer.list(&t1).await.unwrap(), snapshot);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_rewind_fork_on_memory() {
    approve_rewind_fork("adk-rust", Backend::Memory).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approve_rewind_fork_on_sqlite_with_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    approve_rewind_fork("adk-rust", Backend::Sqlite(dir.path().to_path_buf())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL, a PostgreSQL server; run by hand"]
async fn approve_rewind_fork_on_postgres() {
    let server = std::env::var("DATABASE_URL").expect("DATABASE_URL names a PostgreSQL server");
    // Its own application name keeps this run's sessions, threads, ledger keys and decisions
    // apart from every other run on the same database.
    let app = format!("adk-rust-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    approve_rewind_fork(&app, Backend::Postgres(server)).await;
}
