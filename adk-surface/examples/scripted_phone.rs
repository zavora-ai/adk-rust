//! The phone and the web home over a scripted kernel (spec 3.3 §5.4, §8.2, §8.3): what the
//! phone's end-to-end test in `adk-surface/web/e2e/phone.spec.ts` opens.
//!
//! A kernel hosts a harness agent in "Ask first" whose model is `ScriptedLlm`: each of two
//! turns writes a file, a call the harness holds for the person and the approval router rates
//! high risk, so it is approved only after "Confirm it's you". The kernel pairs devices in
//! memory: "Add a phone or app" in the home issues a phone's code whose link opens
//! `<home>/ui/pair`, and a paired phone reads and answers with its device token behind the
//! kernel's device guard. The phone's routes serve its passkey and "Confirm it's you" for the
//! phone and for the person's passkey on this computer. The home's URL is
//! `http://localhost:<port>`, the WebAuthn relying party `localhost`. Requests without a
//! device token are the owner's, as a loopback-only server admits them. No wake-up is sent.
//!
//! ```sh
//! cargo run -p adk-surface --features mobile --example scripted_phone -- 8799 /tmp/phone-workspace
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalCall, ApprovalPolicy, ApprovalRisk, ApprovalRoute, ApprovalService, ApproverSelector,
    DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, RequestContext, SingleAgentLoader,
    StoreApprovalService,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{DeviceTokenExtractor, InMemoryDeviceStore, Pairing};
use adk_kernel::proc::{InMemoryLedgerStore, Ledger, LedgerKey};
use adk_kernel::{InMemoryProcessStore, KernelConfig, PartitionMode, ProcessStore};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::DurableWorkerConfig;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::mobile::{Phone, PushDoorbell, PushKey};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use serde_json::json;

const APP: &str = "home";
const OWNER: &str = "ana";

/// Admits every request without a device token as the owner, as a loopback-only server's
/// extractor does.
struct Owner;

#[async_trait]
impl RequestContextExtractor for Owner {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        Ok(RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into(), "operator".into()],
            metadata: Default::default(),
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8799), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;
    let home = format!("http://localhost:{port}");

    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "release.txt", "content": "v3.0.0" }),
            ),
            ScriptedResponse::text("The release notes are written."),
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "notes.txt", "content": "Notes" }),
            ),
            ScriptedResponse::text("The notes are written."),
        ],
    ));
    // Every held call is high risk, so its approval needs the person's passkey (spec 3.3 §7.2).
    let doorbell = Arc::new(PushDoorbell::new());
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(|_: &ApprovalCall| {
                let owner =
                    ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval();
                Some(ApprovalRoute::new(owner).with_risk(ApprovalRisk::High))
            })
            .with_notifier(doorbell.clone()),
    );
    let agent = Arc::new(
        HarnessAgent::builder("release")
            .model(model)
            .workspace(adk_devtools::Workspace::new(&workspace))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Ask)
            .approval_service(Arc::clone(&approvals))
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()?;
    let harness =
        Arc::new(Harness::with_runner(agent.clone(), runner)?.with_run_store(Arc::clone(&runs)));

    // Codes open the installed app's pairing screen; devices reach the surface's routes.
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()))
        .with_home_url(format!("{home}/ui"))
        .with_device_routes(Arc::new(adk_surface::web::device_scope_for));
    let kernel = harness
        .kernel()
        .ledger(Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::generate()?))
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("phone")
                        .with_poll_interval(Duration::from_millis(50)),
                )
                .with_control(
                    ControlPlaneConfig::default()
                        .without_socket()
                        .with_pairing(pairing.clone())
                        .with_start_mode("default", PartitionMode::Ask),
                ),
        )
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .build()?;
    kernel.start().await?;

    let source = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .approvals(Arc::clone(&approvals))
        .harness(harness)
        .kernel(kernel.clone(), APP)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Release")
        .build();
    let phone = Phone::builder(engine.clone(), pairing.clone())
        .home_url(home.clone())
        .push_key(PushKey::generate()?)
        .doorbell(doorbell)
        .approvals(approvals)
        .kernel(kernel.clone())
        .build()?;

    let extractor = DeviceTokenExtractor::new(pairing.clone()).with_fallback(Arc::new(Owner));
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(extractor));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_kernel::control::router(kernel.clone()))
        .add_api_routes(
            adk_surface::web::router_with_devices(engine.clone(), pairing)
                .merge(adk_surface::web::devices_router(engine)),
        )
        .add_api_routes(adk_surface::mobile::router(phone))
        .add_root_routes(adk_kernel::control::claim_router(kernel.clone()))
        .add_root_routes(adk_surface::web::home())
        .build();
    let app = adk_kernel::control::guard_devices(&kernel, app);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(port, "scripted phone listening");
    println!("scripted phone on {home}/ui/");
    axum::serve(listener, app).await?;
    Ok(())
}
