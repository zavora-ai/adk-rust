//! The kernel-hosted harness the scenario and live tests run (spec 3.3 WP-A scenario).

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore, InMemoryApprovalStore,
    InMemoryRunStore, Llm, RequestContext, StoreApprovalService,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{InMemoryProcessStore, Kernel, KernelConfig, ProcessStore};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::{Caller, LocalSource, Origin, Place, SurfaceEngine, SurfaceKind};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

pub const APP: &str = "home";
pub const OWNER: &str = "ana";

pub struct Scenario {
    pub _dir: tempfile::TempDir,
    pub path: std::path::PathBuf,
    pub model: Arc<dyn Llm>,
    pub kernel: Kernel,
    pub runs: Arc<dyn DurableRunStore>,
    pub service: Arc<dyn ApprovalService>,
    pub sessions: Arc<dyn SessionService>,
    pub audit: InMemoryAuditRecorder,
    pub engine: SurfaceEngine,
}

/// A kernel hosting a harness agent on `model`, with the engine over it.
pub async fn scenario(model: Arc<dyn Llm>) -> Scenario {
    scenario_with(model, ControlPlaneConfig::default().without_socket(), true).await
}

/// The harness of [`scenario`] with its own runner, and a kernel serving `control`; with
/// `hosted` false, the engine runs the harness directly and the kernel is not started.
pub async fn scenario_with(
    model: Arc<dyn Llm>,
    control: ControlPlaneConfig,
    hosted: bool,
) -> Scenario {
    build(model, control, hosted, None).await
}

/// The hosted harness of [`scenario`] on a kernel that writes the `/proc` ledger to `ledger`
/// and serves `control` (spec 3.3 §10 K3, K4).
pub async fn scenario_with_ledger(
    model: Arc<dyn Llm>,
    control: ControlPlaneConfig,
    ledger: adk_kernel::proc::Ledger,
) -> Scenario {
    build(model, control, true, Some(ledger)).await
}

async fn build(
    model: Arc<dyn Llm>,
    control: ControlPlaneConfig,
    hosted: bool,
    ledger: Option<adk_kernel::proc::Ledger>,
) -> Scenario {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([
                    ApproverSelector::group("owner"),
                    ApproverSelector::group("operator"),
                ])
                .allow_self_approval(),
            ),
    );
    let audit = InMemoryAuditRecorder::new();
    let agent = Arc::new(
        HarnessAgent::builder("finance")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(&path))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .approval_service(Arc::clone(&service))
            // The harness's own records (standing rules, spec 3.1 §9.3) beside the surface's.
            .audit_log(Arc::new(audit.clone()))
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(Arc::clone(&sessions)),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness =
        Arc::new(Harness::with_runner(agent, runner).unwrap().with_run_store(Arc::clone(&runs)));
    let mut builder = harness.kernel();
    if let Some(ledger) = ledger {
        builder = builder.ledger(ledger);
    }
    let kernel = builder
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_millis(600))
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(control),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .build()
        .unwrap();
    let mut source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .harness(harness);
    if hosted {
        kernel.start().await.unwrap();
        source = source.kernel(kernel.clone(), APP);
    }
    let source = source.build().unwrap();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .places([Place::web(), Place::phone(), Place::chat("Telegram")])
        .agent_name(APP, "Finance")
        .poll_interval(Duration::from_millis(20))
        .build();
    Scenario { _dir: dir, path, model, kernel, runs, service, sessions, audit, engine }
}

/// Polls `probe` until it answers, for at most `within`.
pub async fn eventually<T, F, Fut>(what: &str, within: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= within {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn web() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

pub fn telegram() -> Caller {
    Caller::human(OWNER, SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
        .with_groups(["owner"])
}

/// The kernel's control plane and the surface API in one router under `/api`, behind a
/// stand-in for the server's auth middleware that authenticates the owner.
pub fn api(kernel: &Kernel, engine: &SurfaceEngine) -> axum::Router {
    let routes =
        adk_kernel::control::router(kernel.clone()).merge(adk_surface::web::router(engine.clone()));
    axum::Router::new().nest("/api", routes).layer(axum::Extension(Some(RequestContext {
        user_id: OWNER.into(),
        scopes: vec!["owner".into(), "operator".into()],
        metadata: Default::default(),
    })))
}

pub async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
