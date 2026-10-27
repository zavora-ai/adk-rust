//! The web home over a scripted kernel with something in Coming up to snooze (spec 3.3 §2 item
//! 7, §12 "One verb, three origins"): what `web/e2e/verbs.spec.ts` opens.
//!
//! A kernel hosts Finance, a harness agent with the screen-action tools, in Go ahead, over
//! `ScriptedLlm`: its turn reads Coming up and asks to snooze News digest an hour, which waits
//! for the person whatever the mode. The agenda holds News digest (Research's follow-up) and
//! Water plants, each asleep until a time eight and nine hours from now. The kernel, its
//! worker, the approval inbox, the registry of screen actions, the engine and the server are
//! real; a stand-in extractor admits every request as the owner.
//!
//! ```sh
//! cargo run -p adk-surface --features web,verb-ui --example scripted_verbs -- 8795 /tmp/verbs-workspace
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApprovalStore, ApproverSelector, DurableRunStore,
    InMemoryApprovalStore, InMemoryRunStore, RequestContext, SingleAgentLoader,
    StoreApprovalService,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    AgendaHandle, AgendaItem, AgendaOrigin, AgendaState, InMemoryProcessStore, KernelConfig,
    PartitionId, ProcessStore, ProcessTable, Wake, WakeSet,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::DurableWorkerConfig;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::verbs::{KernelAgenda, VerbGate, VerbRegistry};
use adk_surface::{LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use chrono::{Timelike as _, Utc};
use serde_json::json;

const APP: &str = "adk-rust";
const OWNER: &str = "ana";

/// Admits every request as the owner, as a loopback-only server's extractor does.
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
    let port: u16 = args.next().map_or(Ok(8795), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;

    // News digest wakes at the whole hour eight hours from now; Water plants an hour later.
    let at = (Utc::now() + chrono::Duration::hours(8))
        .with_minute(0)
        .and_then(|at| at.with_second(0))
        .and_then(|at| at.with_nanosecond(0))
        .ok_or("no whole hour")?;
    let to = at + chrono::Duration::hours(1);
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("verb_coming_up", json!({})),
            ScriptedResponse::function_call(
                "verb_snooze",
                json!({ "item": "news-digest", "to": to.to_rfc3339() }),
            ),
            ScriptedResponse::text("I asked you to move News digest an hour later."),
            ScriptedResponse::text("Done."),
            ScriptedResponse::text("Done."),
        ],
    ));
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let store: Arc<dyn ApprovalStore> = Arc::new(InMemoryApprovalStore::new());
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let audit = InMemoryAuditRecorder::new();
    let handle = AgendaHandle::new();
    let registry = VerbRegistry::builder(
        APP,
        Arc::new(KernelAgenda::new(
            handle.clone(),
            ProcessTable::new(Arc::clone(&runs), Arc::clone(&processes)),
        )),
    )
    .approval_store(Arc::clone(&store))
    .audit(AuditTrail::new(Arc::new(audit.clone())))
    .sessions(Arc::clone(&sessions))
    .agent_name("finance", "Finance")
    .build()?;
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::clone(&store))
            .with_run_store(Arc::clone(&runs))
            .with_router(registry.router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            )),
    );
    let area = PartitionId::default();
    let agent = Arc::new(
        HarnessAgent::builder("finance")
            .model(model)
            .workspace(adk_devtools::Workspace::new(&workspace))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..InstructionsConfig::default()
            })
            .without_compaction()
            .without_default_members()
            .mode(HarnessMode::Auto)
            .approval_service(Arc::clone(&approvals))
            .toolset(Arc::new(registry.toolset(area.clone())))
            .tool_classes(Arc::new(registry.classes()))
            .add_operator_policy(Arc::new(registry.policy(area)))
            .build()?,
    );
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
        )
        .build()?;
    let harness =
        Arc::new(Harness::with_runner(agent.clone(), runner)?.with_run_store(Arc::clone(&runs)));
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("verbs")
                        .with_poll_interval(Duration::from_millis(50)),
                )
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(processes)
        .agenda_handle(handle)
        .build()?;
    kernel.start().await?;
    // Coming up: News digest (Research's follow-up) and Water plants, each asleep until its time.
    for (id, title, when) in
        [("news-digest", "News digest", at), ("water-plants", "Water plants", to)]
    {
        let item = AgendaItem::new(
            id,
            SubjectRef::human(OWNER),
            title,
            "A follow-up Research set",
            AgendaOrigin::User,
            Utc::now(),
        )
        .with_state(AgendaState::Approved);
        kernel.agenda().store().put(item).await?;
        kernel
            .agenda()
            .schedule(id, WakeSet::any_of([Wake::At(when)]), &SubjectRef::human(OWNER))
            .await?;
    }

    let source = LocalSource::builder(APP)
        .sessions(sessions.clone())
        .approvals(approvals)
        .harness(harness)
        .kernel(kernel.clone(), APP)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit)))
        .places([Place::web(), Place::phone()])
        .agent_name("finance", "Finance")
        .card_hints(Arc::new(registry.hints()))
        .grant(registry.grant())
        .build();
    let gate = VerbGate::new(registry, engine.clone());

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_kernel::control::router(kernel))
        .add_api_routes(adk_surface::web::router(engine))
        .add_api_routes(adk_surface::web::verbs::router(gate))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("scripted screen actions on http://127.0.0.1:{port}/ui/");
    axum::serve(listener, app).await?;
    Ok(())
}
