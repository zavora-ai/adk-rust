//! The governance kill switch: admin endpoints, the runners the server builds, scheduling,
//! and the freeze delegate a kernel registers (spec 3.2 §10 S4).

#![cfg(feature = "background")]

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::AuditActor;
use adk_core::{
    Agent, DurableRunStore, EventStream, GovernanceControl, InvocationContext, Llm, NewRun,
    Result as AdkResult, RunFilter, RunRecord, RunState, SingleAgentLoader, Tool, ToolContext,
    ToolEffect,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::{BackgroundState, CronState, DurableWorkerConfig};
use adk_server::{FreezeDelegate, FreezeReport, ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// Counts how often it runs.
struct CountingAgent {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for CountingAgent {
    fn name(&self) -> &str {
        "counting_agent"
    }
    fn description(&self) -> &str {
        "counts runs"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Accepts three bearer tokens: `good`, the administrator `operator` with the `admin` scope;
/// `member`, an authenticated user without it; and `ops`, a user with the `ops` scope.
struct TokenExtractor;

#[async_trait]
impl RequestContextExtractor for TokenExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let (user_id, scope) =
            match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
                Some("Bearer good") => ("operator", "admin"),
                Some("Bearer member") => ("member", "user"),
                Some("Bearer ops") => ("ops-lead", "ops"),
                Some(_) => {
                    return Err(RequestContextError::InvalidToken("unknown token".to_string()));
                }
                None => return Err(RequestContextError::MissingAuth),
            };
        Ok(adk_core::RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![scope.to_string()],
            metadata: Default::default(),
        })
    }
}

/// Freezes the control itself and reports six parked runs, as a kernel's freeze would; records
/// each call with the acting administrator.
struct ParkingDelegate {
    control: GovernanceControl,
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl FreezeDelegate for ParkingDelegate {
    async fn freeze(&self, reason: &str, by: AuditActor) -> AdkResult<FreezeReport> {
        self.control.freeze_recorded(reason, by.clone()).await?;
        let actor = by.on_behalf_of.map(|subject| subject.id).unwrap_or_default();
        self.calls.lock().unwrap().push(format!("freeze:{reason}:{actor}"));
        Ok(FreezeReport { parked_runs: 6, cancelled_runs: 0 })
    }

    async fn unfreeze(&self, by: AuditActor) -> AdkResult<()> {
        self.control.unfreeze_recorded(by.clone()).await?;
        let actor = by.on_behalf_of.map(|subject| subject.id).unwrap_or_default();
        self.calls.lock().unwrap().push(format!("unfreeze:{actor}"));
        Ok(())
    }
}

struct Fixture {
    app: Router,
    control: GovernanceControl,
    runs: Arc<AtomicUsize>,
    cron: CronState,
}

async fn fixture() -> Fixture {
    fixture_with(|config| config, |_| None).await
}

/// A fixture whose server configuration `configure` adjusts, and whose freezes go to the
/// delegate `delegate` builds over the server's control.
async fn fixture_with(
    configure: impl FnOnce(ServerConfig) -> ServerConfig,
    delegate: impl FnOnce(&GovernanceControl) -> Option<Arc<dyn FreezeDelegate>>,
) -> Fixture {
    let runs = Arc::new(AtomicUsize::new(0));
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "counting_agent".to_string(),
            user_id: "operator".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let control = GovernanceControl::new();
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(CountingAgent { runs: runs.clone() })));
    let config = configure(
        ServerConfig::new(loader, sessions as Arc<dyn SessionService>)
            .with_request_context(Arc::new(TokenExtractor))
            .with_governance(control.clone()),
    );
    let background = BackgroundState::new();
    let cron = CronState::new(background.clone());
    let mut builder = ServerBuilder::new(config)
        .with_background_runs(background)
        .with_cron_jobs(cron.clone())
        .enable_governance_endpoints();
    if let Some(delegate) = delegate(&control) {
        builder = builder.with_freeze_delegate(delegate);
    }
    Fixture { app: builder.build(), control, runs, cron }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder =
        Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response =
        app.clone().oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_admin_endpoints_require_authentication() {
    let fixture = fixture().await;
    let (status, _) =
        call(&fixture.app, "POST", "/api/admin/freeze", None, json!({ "reason": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!fixture.control.is_frozen());
}

/// Only an administrator freezes or unfreezes: an authenticated caller without the admin scope
/// is refused `403`, and neither the control nor the freeze delegate changes; the status stays
/// readable to that caller.
#[tokio::test]
async fn a_caller_without_the_admin_scope_can_neither_freeze_nor_unfreeze() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let fixture = fixture_with(
        |config| config,
        move |control| {
            let delegate = ParkingDelegate { control: control.clone(), calls: recorded };
            Some(Arc::new(delegate) as Arc<dyn FreezeDelegate>)
        },
    )
    .await;
    let code = |body: &Value| body["error"]["code"].clone();

    let (status, body) = call(
        &fixture.app,
        "POST",
        "/api/admin/freeze",
        Some("member"),
        json!({ "reason": "incident 30" }),
    )
    .await;
    assert_eq!((status, code(&body)), (StatusCode::FORBIDDEN, json!("governance.forbidden")));
    assert!(!fixture.control.is_frozen());
    assert!(calls.lock().unwrap().is_empty(), "the delegate is not reached");

    let (status, body) =
        call(&fixture.app, "GET", "/api/admin/governance", Some("member"), json!({})).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({ "frozen": false, "mirrored": false, "stale": false, "runningRuns": "park" })
        )
    );

    let (status, _) = call(
        &fixture.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 30" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/unfreeze", Some("member"), json!({})).await;
    assert_eq!((status, code(&body)), (StatusCode::FORBIDDEN, json!("governance.forbidden")));
    assert!(fixture.control.is_frozen(), "the freeze stays in force");
    assert_eq!(calls.lock().unwrap().as_slice(), ["freeze:incident 30:operator"]);
}

/// Without a `RequestContextExtractor` no request has a caller: the freeze and the unfreeze are
/// refused `401` and the control stays as it was; the status stays readable.
#[tokio::test]
async fn without_an_extractor_anonymous_callers_can_neither_freeze_nor_unfreeze() {
    let fixture = fixture_with(
        |mut config| {
            config.request_context_extractor = None;
            config
        },
        |_| None,
    )
    .await;
    let code = |body: &Value| body["error"]["code"].clone();

    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/freeze", None, json!({ "reason": "x" })).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNAUTHORIZED, json!("governance.unauthenticated"))
    );
    assert!(!fixture.control.is_frozen());

    fixture.control.freeze("incident 31");
    let (status, body) = call(&fixture.app, "POST", "/api/admin/unfreeze", None, json!({})).await;
    assert_eq!(
        (status, code(&body)),
        (StatusCode::UNAUTHORIZED, json!("governance.unauthenticated"))
    );
    assert!(fixture.control.is_frozen(), "the freeze stays in force");

    let (status, body) = call(&fixture.app, "GET", "/api/admin/governance", None, json!({})).await;
    assert_eq!(
        (status, body),
        (
            StatusCode::OK,
            json!({
                "frozen": true, "reason": "incident 31", "mirrored": false, "stale": false,
                "runningRuns": "cancel"
            })
        )
    );
}

/// `SecurityConfig::admin_scope` names the scope that makes a caller an administrator.
#[tokio::test]
async fn the_admin_scope_is_the_one_the_security_config_names() {
    let fixture = fixture_with(
        |mut config| {
            config.security.admin_scope = "ops".to_string();
            config
        },
        |_| None,
    )
    .await;

    let (status, _) =
        call(&fixture.app, "POST", "/api/admin/freeze", Some("good"), json!({ "reason": "x" }))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "`admin` is not the configured scope");
    assert!(!fixture.control.is_frozen());

    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/freeze", Some("ops"), json!({ "reason": "x" }))
            .await;
    assert_eq!((status, body["frozen"].clone()), (StatusCode::OK, json!(true)));
    assert!(fixture.control.is_frozen());
}

#[tokio::test]
async fn freezing_stops_runs_and_pauses_scheduling_until_unfrozen() {
    let fixture = fixture().await;
    let run_body = json!({
        "appName": "counting_agent",
        "userId": "operator",
        "sessionId": "s1",
        "newMessage": { "role": "user", "parts": [{ "text": "go" }] }
    });

    let (status, body) = call(
        &fixture.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 4012" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "frozen": true, "reason": "incident 4012", "mirrored": false, "stale": false,
            "runningRuns": "cancel", "parkedRuns": 0, "cancelledRuns": 0
        })
    );
    assert!(fixture.control.is_frozen());
    assert!(fixture.cron.scheduler().tick().await.unwrap().frozen, "schedules do not fire");
    assert!(
        fixture.cron.background_state.governance().unwrap().same_as(&fixture.control),
        "background runs obey the server's kill switch"
    );

    let (status, _) = call(&fixture.app, "POST", "/api/run", Some("good"), run_body.clone()).await;
    assert_ne!(status, StatusCode::OK, "a frozen server must not run the agent");
    assert_eq!(fixture.runs.load(Ordering::SeqCst), 0);

    let submit = json!({ "workflowId": "report", "input": {} });
    let (status, _) = call(&fixture.app, "POST", "/api/runs", Some("good"), submit).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/unfreeze", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "frozen": false, "mirrored": false, "stale": false, "runningRuns": "cancel" })
    );
    assert!(!fixture.cron.scheduler().tick().await.unwrap().frozen);

    let (status, _) = call(&fixture.app, "POST", "/api/run", Some("good"), run_body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.runs.load(Ordering::SeqCst), 1);
}

/// Gate (spec 3.2 §10 S4): with a freeze delegate the endpoints hand the freeze and the
/// unfreeze to it as the authenticated administrator and report `parkedRuns` and
/// `cancelledRuns` from what it returns; `GET /api/admin/governance` says running runs are
/// parked.
#[tokio::test]
async fn a_freeze_delegate_parks_instead_of_cancelling_and_the_status_says_so() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    let fixture = fixture_with(
        |config| config,
        move |control| {
            let delegate = ParkingDelegate { control: control.clone(), calls: recorded };
            Some(Arc::new(delegate) as Arc<dyn FreezeDelegate>)
        },
    )
    .await;

    let (status, body) =
        call(&fixture.app, "GET", "/api/admin/governance", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "frozen": false, "mirrored": false, "stale": false, "runningRuns": "park" })
    );

    let (status, body) = call(
        &fixture.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 9" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "frozen": true, "reason": "incident 9", "mirrored": false, "stale": false,
            "runningRuns": "park", "parkedRuns": 6, "cancelledRuns": 0
        }),
        "the counts are the delegate's report"
    );
    assert!(fixture.control.is_frozen(), "the delegate froze the server's control");
    assert!(fixture.cron.scheduler().tick().await.unwrap().frozen, "schedules do not fire");
    assert_eq!(calls.lock().unwrap().as_slice(), ["freeze:incident 9:operator"]);

    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/unfreeze", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "frozen": false, "mirrored": false, "stale": false, "runningRuns": "park" })
    );
    assert!(!fixture.control.is_frozen());
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["freeze:incident 9:operator", "unfreeze:operator"]
    );
}

/// A non-idempotent charge that blocks inside the call until the test adds a permit.
struct BlockingCharge {
    charges: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

#[async_trait]
impl Tool for BlockingCharge {
    fn name(&self) -> &str {
        "charge_card"
    }

    fn description(&self) -> &str {
        "charges the customer's card"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        self.release.acquire().await.expect("the semaphore stays open").forget();
        Ok(json!({ "status": "charged" }))
    }
}

/// Freezes the server's control as a kernel's freeze does and reports the runs that were
/// running as parked: the workers, which share the control, park each at its next model-call
/// boundary and wake it on the unfreeze.
struct KernelLikeDelegate {
    control: GovernanceControl,
    runs: Arc<dyn DurableRunStore>,
}

#[async_trait]
impl FreezeDelegate for KernelLikeDelegate {
    async fn freeze(&self, reason: &str, by: AuditActor) -> AdkResult<FreezeReport> {
        self.control.freeze_recorded(reason, by).await?;
        let running = self.runs.list(RunFilter::default().states([RunState::Running])).await?;
        Ok(FreezeReport { parked_runs: running.len(), cancelled_runs: 0 })
    }

    async fn unfreeze(&self, by: AuditActor) -> AdkResult<()> {
        self.control.unfreeze_recorded(by).await
    }
}

/// A server whose embedded background worker runs a billing agent over the server's control.
struct Billing {
    app: Router,
    runs: Arc<dyn DurableRunStore>,
    charges: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

impl Billing {
    async fn new(kernel: bool) -> Self {
        let control = GovernanceControl::new();
        let sessions = Arc::new(InMemorySessionService::new());
        let charges = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Semaphore::new(0));
        let model = ScriptedLlm::new(
            "billing-script",
            [
                ScriptedResponse::function_call("charge_card", json!({ "amountCents": 1999 })),
                ScriptedResponse::text("charge status: charged"),
            ],
        );
        let agent = LlmAgentBuilder::new("billing")
            .model(Arc::new(model) as Arc<dyn Llm>)
            .tool(Arc::new(BlockingCharge {
                charges: Arc::clone(&charges),
                release: Arc::clone(&release),
            }))
            .build()
            .unwrap();
        let runner = Runner::builder()
            .app_name("billing")
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
            .governance(control.clone())
            .build()
            .unwrap();
        let background =
            BackgroundState::new().with_agent("billing", Arc::new(runner)).with_worker_config(
                DurableWorkerConfig::default()
                    .with_lease_ttl(Duration::from_millis(300))
                    .with_poll_interval(Duration::from_millis(20)),
            );
        let runs = Arc::clone(background.run_store());
        let loader = Arc::new(SingleAgentLoader::new(Arc::new(CountingAgent {
            runs: Arc::new(AtomicUsize::new(0)),
        })));
        let config = ServerConfig::new(loader, sessions as Arc<dyn SessionService>)
            .with_request_context(Arc::new(TokenExtractor))
            .with_governance(control.clone());
        let builder = ServerBuilder::new(config)
            .with_background_runs(background)
            .enable_governance_endpoints();
        let builder = if kernel {
            builder.with_freeze_delegate(Arc::new(KernelLikeDelegate {
                control,
                runs: Arc::clone(&runs),
            }))
        } else {
            builder
        };
        Self { app: builder.build(), runs, charges, release }
    }

    /// Enqueues a charge and waits until the worker is executing it.
    async fn charging(&self) -> RunRecord {
        let run =
            NewRun::new("default", "billing", "customer-1", "billing", json!("charge my card"));
        let run = self.runs.enqueue(run).await.unwrap().record().cloned().unwrap();
        self.eventually("the charge to start", |billing| {
            Box::pin(async move { (billing.charges.load(Ordering::SeqCst) == 1).then_some(()) })
        })
        .await;
        run
    }

    async fn run_in_state(&self, run_id: &str, state: RunState) -> RunRecord {
        let id = run_id.to_string();
        self.eventually(&format!("run {run_id} to be {state:?}"), move |billing| {
            let id = id.clone();
            Box::pin(async move {
                billing.runs.get(&id).await.ok().flatten().filter(|run| run.state == state)
            })
        })
        .await
    }

    /// Polls `probe` every 10 ms until it returns `Some`, for at most five seconds.
    async fn eventually<'a, T>(
        &'a self,
        what: &str,
        probe: impl Fn(&'a Self) -> futures::future::BoxFuture<'a, Option<T>>,
    ) -> T {
        let started = tokio::time::Instant::now();
        while started.elapsed() <= Duration::from_secs(5) {
            if let Some(value) = probe(self).await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }
}

/// Gate (spec 3.2 §10 S4): under a freeze delegate, `POST /api/admin/freeze` leaves a running
/// background run to park at its next model-call boundary rather than cancelling it, reports
/// it in `parkedRuns`, and `POST /api/admin/unfreeze` resumes it; the charge runs once.
#[tokio::test]
async fn under_a_freeze_delegate_the_admin_freeze_parks_a_running_run_and_unfreeze_resumes_it() {
    let billing = Billing::new(true).await;
    let run = billing.charging().await;

    let (status, body) = call(
        &billing.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 21" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "frozen": true, "reason": "incident 21", "mirrored": false, "stale": false,
            "runningRuns": "park", "parkedRuns": 1, "cancelledRuns": 0
        })
    );
    billing.release.add_permits(1);
    let parked = billing.run_in_state(&run.run_id, RunState::Waiting).await;
    assert_eq!(
        (parked.cancel_requested, parked.lease_owner.as_deref()),
        (false, None),
        "{parked:?}"
    );

    let (status, body) =
        call(&billing.app, "POST", "/api/admin/unfreeze", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["frozen"], json!(false));
    let finished = billing.run_in_state(&run.run_id, RunState::Completed).await;
    assert_eq!((finished.attempt, billing.charges.load(Ordering::SeqCst)), (1, 1));
}

/// Gate (spec 3.2 §10 S4): without a delegate, `POST /api/admin/freeze` keeps the cancel — a
/// terminate, not a pause — and the unfreeze does not revive the run.
#[tokio::test]
async fn without_a_freeze_delegate_the_admin_freeze_cancels_a_running_run() {
    let billing = Billing::new(false).await;
    let run = billing.charging().await;

    let (status, body) = call(
        &billing.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 22" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cancelledRuns"], json!(1), "{body}");
    billing.release.add_permits(1);
    billing.run_in_state(&run.run_id, RunState::Cancelled).await;

    let (status, _) =
        call(&billing.app, "POST", "/api/admin/unfreeze", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let still = billing.runs.get(&run.run_id).await.unwrap().unwrap();
    assert_eq!(still.state, RunState::Cancelled);
    assert_eq!(billing.charges.load(Ordering::SeqCst), 1);
}
