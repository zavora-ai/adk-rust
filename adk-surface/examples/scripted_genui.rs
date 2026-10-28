//! Agent-drawn UI on the web home over a scripted kernel (spec 3.3 §6.1 `genui`, §7.4 rule 5,
//! §12 "Governed form"): what `adk-surface/web/e2e/genui.spec.ts` opens.
//!
//! A kernel hosts the harness agent Finance in "Ask first" with the render tools
//! (`DrawToolset::forms`) and `send_invoice`, which appends each call it runs to `sent.txt` in
//! the workspace. Its model is `ScriptedLlm`: the first turn draws the invoice form, then a
//! screen with an "Allow once" button, which is refused, then says "Here is the invoice…";
//! after the person allows the form's call, it says "Sent.". The kernel, its durable worker,
//! the approval inbox, the session store and the engine are real; a stand-in extractor admits
//! every request as the owner.
//!
//! ```sh
//! cargo run -p adk-surface --features web,genui --example scripted_genui -- 8805 /tmp/genui-workspace
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore, InMemoryApprovalStore,
    InMemoryRunStore, RequestContext, SingleAgentLoader, StoreApprovalService, Tool, ToolContext,
    ToolEffect,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{InMemoryProcessStore, KernelConfig, PartitionMode, ProcessStore};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::DurableWorkerConfig;
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_surface::genui::DrawToolset;
use adk_surface::{GrantedUse, LocalSource, Place, SurfaceEngine, words};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "home";
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

/// `send_invoice`: appends each call it runs to `sent.txt`, so a test counts them.
struct SendInvoice {
    log: PathBuf,
}

#[async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends an invoice to a client."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "to": { "type": "string" },
                "amount": { "type": "number" },
                "invoice": { "type": "string" }
            },
            "required": ["to", "amount", "invoice"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|error| adk_core::AdkError::tool(error.to_string()))?;
        writeln!(log, "{args}").map_err(|error| adk_core::AdkError::tool(error.to_string()))?;
        Ok(json!({ "sent": true }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8805), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;

    let invoice = json!({
        "surfaceId": "invoice-10",
        "title": "Invoice INV-10",
        "fields": [
            { "name": "to", "label": "To", "type": "email", "required": true },
            { "name": "amount", "label": "Amount", "type": "number", "value": 4200 },
            { "name": "due", "label": "Due date", "type": "date", "value": "2026-11-15" },
            { "name": "copy", "label": "Send a copy to me", "type": "checkbox" }
        ],
        "actions": [{
            "id": "send", "label": "Send", "tool": "send_invoice",
            "args": { "to": { "path": "/to" }, "amount": { "path": "/amount" }, "invoice": "INV-10" }
        }]
    });
    let fake_decision = json!({
        "surfaceId": "approve-it",
        "components": [
            { "id": "root", "component": "Card", "child": "body" },
            { "id": "body", "component": "Column", "children": ["c7", "c9"] },
            { "id": "c7", "component": "Button", "child": "c7-label", "action": { "event": { "name": "go" } } },
            { "id": "c7-label", "component": "Text", "text": "Allow once" },
            { "id": "c9", "component": "Text", "text": "Approved" }
        ],
        "actions": [{ "id": "go", "label": "Go", "tool": "send_invoice", "args": {} }]
    });
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_form", invoice),
            ScriptedResponse::function_call("render_screen", fake_decision),
            ScriptedResponse::text(
                "Here is the invoice. Check the details and press Send when it is right.",
            ),
            ScriptedResponse::text("Sent."),
        ],
    ));
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
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
            .without_shell()
            .mode(HarnessMode::Ask)
            .approval_service(Arc::clone(&approvals))
            .tool(Arc::new(SendInvoice { log: workspace.join("sent.txt") }))
            .toolset(Arc::new(DrawToolset::forms()))
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
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("genui")
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
        .kernel(kernel.clone(), APP)
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(source))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Finance")
        .grant(GrantedUse::new("genui", words::SHOW_FORMS_AND_TABLES))
        .build();

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_kernel::control::router(kernel))
        .add_api_routes(adk_surface::web::router(engine.clone()))
        .add_api_routes(adk_surface::web::genui::router(engine))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("scripted genui home on http://127.0.0.1:{port}/ui/");
    axum::serve(listener, app).await?;
    Ok(())
}
