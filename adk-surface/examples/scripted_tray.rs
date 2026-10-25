//! The tray panel over a scripted kernel with two agents (spec 3.3 §5.5, §12 phase 5): what
//! the tray's end-to-end tests open, in Chromium (`adk-surface/web/e2e/tray.spec.ts`) and in the
//! desktop shell on WebKitGTK (`adk-surface/desktop/app/tests/webkitgtk.rs`).
//!
//! A kernel hosts two harness agents in "Ask first" on `ScriptedLlm`, sharing one approval
//! service and inbox:
//!
//! | Agent | Target | Script | Its held call |
//! |---|---|---|---|
//! | Finance | `finance`, the primary agent new messages run on | writes `invoice.txt`, then answers in words | `write_file`, medium risk |
//! | Code | `code`, started in process with `--code-turn` | publishes a release | `publish_release`, high risk |
//!
//! Quiet time is 22:00–07:00 and the daily spending limit $5.00. Stop all needs the operator,
//! as under `adk-rust up`; a stand-in extractor admits every request as the owner and operator.
//! The server is `adk-server`'s `ServerBuilder` with the engine under `/api/surface`, the
//! kernel's routes under `/api/kernel` and the page at `/ui/` (the panel at `/ui/tray`).
//!
//! ```sh
//! cargo run -p adk-surface --features web --example scripted_tray -- 8792 /tmp/adk-tray-ws --code-turn
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApprovalService, ApproverSelector,
    DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, NewRun, RequestContext, Result,
    SingleAgentLoader, StoreApprovalService, Tool, ToolContext,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    InMemoryProcessStore, KernelConfig, PartitionMode, ProcessKind, ProcessSpec, ProcessStore,
    QueueClass, QuietHours,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::DurableWorkerConfig;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use chrono::NaiveTime;
use serde_json::{Value, json};

const APP: &str = "tray";
const OWNER: &str = "ana";
const DAILY_LIMIT_MICRO_USD: u64 = 5_000_000;

/// Admits every request as the owner and operator, as a loopback-only server's extractor does.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> std::result::Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into(), "operator".into()],
            metadata: Default::default(),
        })
    }
}

/// Publishes a release: a call that cannot be taken back, held at high risk.
struct PublishRelease;

#[async_trait]
impl Tool for PublishRelease {
    fn name(&self) -> &str {
        "publish_release"
    }

    fn description(&self) -> &str {
        "Publishes a release of the project"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "version": { "type": "string" } },
            "required": ["version"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        Ok(json!({ "published": args["version"] }))
    }
}

/// A harness agent in "Ask first" on `responses`, holding its calls in `approvals`.
fn agent(
    name: &str,
    responses: Vec<ScriptedResponse>,
    workspace: &std::path::Path,
    approvals: &Arc<dyn ApprovalService>,
    tools: Vec<Arc<dyn Tool>>,
) -> std::result::Result<Arc<HarnessAgent>, Box<dyn std::error::Error>> {
    let mut builder = HarnessAgent::builder(name)
        .model(Arc::new(ScriptedLlm::new("scripted", responses)))
        .workspace(adk_devtools::Workspace::new(workspace))
        .project_instructions(InstructionsConfig {
            user_file: None,
            require_trust: false,
            ..InstructionsConfig::default()
        })
        .without_compaction()
        .without_default_members()
        .mode(HarnessMode::Ask)
        .approval_service(Arc::clone(approvals));
    for tool in tools {
        builder = builder.tool(tool);
    }
    Ok(Arc::new(builder.build()?))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8792), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    let code_turn = args.any(|arg| arg == "--code-turn");
    std::fs::create_dir_all(&workspace)?;

    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let owner = ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval();
    // Writing a file needs a look; publishing a release a careful one (spec 3.3 §7.1).
    let router = move |call: &ApprovalCall| {
        let risk = match call.request.tool_name.as_str() {
            "publish_release" => ApprovalRisk::High,
            _ => ApprovalRisk::Medium,
        };
        Some(ApprovalRoute::new(owner.clone()).with_risk(risk))
    };
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(router),
    );

    // Finance's script runs each message it is sent: the invoice, then the queued follow-up.
    let finance = agent(
        "finance",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "invoice.txt", "content": "INV-1" }),
            ),
            ScriptedResponse::text("The invoice is written."),
            ScriptedResponse::text("The November invoice is next."),
            ScriptedResponse::text("Done."),
            ScriptedResponse::text("Done."),
        ],
        &workspace,
        &approvals,
        Vec::new(),
    )?;
    let code = agent(
        "code",
        vec![
            ScriptedResponse::function_call("publish_release", json!({ "version": "3.0.0" })),
            ScriptedResponse::text("The release is published."),
            ScriptedResponse::text("Done."),
        ],
        &workspace,
        &approvals,
        vec![Arc::new(PublishRelease)],
    )?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = |agent: &Arc<HarnessAgent>| {
        agent.configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
    };
    let code_runner = Arc::new(runner(&code).build()?);
    let harness = Arc::new(
        Harness::with_runner(finance.clone(), runner(&finance).build()?)?
            .with_run_store(Arc::clone(&runs)),
    );
    // The harness's own runner, hosted as the `finance` target beside the harness's targets.
    let (_, finance_runner) = harness.goal_target();
    let kernel = harness
        .kernel()
        .agent("finance", finance_runner)
        .agent("code", code_runner)
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("tray")
                        .with_poll_interval(Duration::from_millis(50)),
                )
                .with_control(
                    ControlPlaneConfig::default()
                        .without_socket()
                        .with_start_mode("default", PartitionMode::Ask),
                ),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .build()?;
    kernel.start().await?;

    let source = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .approvals(approvals)
        .harness(harness)
        .kernel(kernel.clone(), "finance")
        .daily_limit(DAILY_LIMIT_MICRO_USD)
        .build()?;
    let quiet = QuietHours::new(
        NaiveTime::from_hms_opt(22, 0, 0).unwrap_or_default(),
        NaiveTime::from_hms_opt(7, 0, 0).unwrap_or_default(),
    );
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web(), Place::tray()])
        .agent_name("finance", "Finance")
        .agent_name("code", "Code")
        .quiet_hours(quiet)
        .stops_need("operator")
        .build();

    if code_turn {
        // Code's release push, held for a decision from the start (spec 3.3 §12 phase 5).
        let session = "code-release";
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: OWNER.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await?;
        let run = NewRun::new(
            QueueClass::Interactive.as_str(),
            APP,
            OWNER,
            "code",
            json!({ "text": "Publish the 3.0.0 release" }),
        )
        .with_session_id(session)
        .with_authenticated_user();
        kernel.processes().spawn(run, ProcessSpec::new(ProcessKind::Interactive)).await?;
    }

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(finance)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_kernel::control::router(kernel))
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, code_turn, "scripted tray listening");
    println!("scripted tray on http://127.0.0.1:{port}/ui/tray");
    axum::serve(listener, app).await?;
    Ok(())
}
