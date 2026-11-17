//! A 3D scene an agent drew, on the web home over a scripted kernel (spec 3.3 §6.1 `genui`,
//! extension catalogs, §7.4 rule 5): what `adk-surface/web/e2e/genui-scenes.spec.ts` opens.
//!
//! An extension catalog, `scenes` 1.0, adds `Yard`: its home view is the scene it is given,
//! drawn by the home. A kernel hosts the harness agent Logistics in "Ask first" with the render
//! tools drawing in that catalog and `reserve_dock`, which appends each call it runs to
//! `reserved.txt` in the workspace. Its model is `ScriptedLlm`: the first turn draws a yard of
//! four docks, a Dock picker the yard picks into, and **Reserve**, then says "Here is the
//! yard…"; after the person allows the call, it says "Reserved.". The kernel, its durable worker,
//! the approval inbox, the session store and the engine are real; a stand-in extractor admits
//! every request as the owner.
//!
//! ```sh
//! cargo run -p adk-surface --features web,genui --example scripted_genui_scenes -- 8807 /tmp/scenes-workspace
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
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
use adk_surface::genui::{
    Catalog, ChildSlot, DrawToolset, DrawnScene, ExtensionComponent, ExtensionSpec, HomeNode,
    HomeScene, ResolvedComponent, SceneSelect, Validator,
};
use adk_surface::{GrantedUse, LocalSource, Place, SurfaceEngine, words};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "home";
const OWNER: &str = "ana";
const SCENES_ID: &str = "https://example.com/a2ui/catalogs/scenes/1.0/catalog.json";
const COMMON: &str = "https://a2ui.org/specification/v0_9/common_types.json#/$defs";

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

/// `Yard`: shows the scene it is given; `pickInto` names the choice field a pick fills.
struct Yard;

impl ExtensionComponent for Yard {
    fn name(&self) -> &'static str {
        "Yard"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "$ref": format!("{COMMON}/ComponentId") },
                "component": { "const": "Yard" },
                "scene": { "type": "object" },
                "pickInto": { "type": "string" }
            },
            "required": ["id", "component", "scene"],
            "additionalProperties": false
        })
    }

    fn children(&self) -> &'static [ChildSlot] {
        &[]
    }

    fn usage(&self) -> &'static str {
        "A yard of docks in 3D; picking a dock fills the choice field `pickInto` names."
    }

    fn home(&self, component: &ResolvedComponent<'_>) -> Result<HomeNode, String> {
        let written = component.get("scene").cloned().unwrap_or_default();
        let mut scene: DrawnScene =
            serde_json::from_value(written).map_err(|error| error.to_string())?;
        if let Some(path) = component.str("pickInto") {
            // The server writes the note.
            scene.select = Some(SceneSelect { path: path.to_string(), note: String::new() });
        }
        Ok(HomeNode::Scene(Box::new(HomeScene::new(scene))))
    }
}

static SCENES: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::extend(ExtensionSpec {
        id: SCENES_ID,
        name: "scenes",
        version: "1.0",
        components: vec![Arc::new(Yard) as Arc<dyn ExtensionComponent>],
    })
    .expect("the scenes catalog compiles")
});

/// `reserve_dock`: appends each call it runs to `reserved.txt`, so a test counts them.
struct ReserveDock {
    log: PathBuf,
}

#[async_trait]
impl Tool for ReserveDock {
    fn name(&self) -> &str {
        "reserve_dock"
    }

    fn description(&self) -> &str {
        "Reserves a loading dock."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "dock": { "type": "array", "items": { "type": "string" } } },
            "required": ["dock"]
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
        Ok(json!({ "reserved": true }))
    }
}

/// The yard: four docks in a row, each with its state and a value the Dock picker offers.
fn yard() -> Value {
    let dock = |n: u8, state: &str, label: &str| {
        json!({
            "id": format!("dock-{n}"), "label": format!("Dock {n} · {label}"), "state": state,
            "shape": { "type": "box", "size": [2.4, 1.2, 2.4] },
            "at": [f64::from(n) * 3.4 - 8.5, 0.6, 0], "value": format!("dock-{n}")
        })
    };
    let scene = json!({
        "kind": "space",
        "objects": [
            { "id": "floor", "shape": { "type": "plane", "width": 16, "depth": 8 }, "look": { "color": "muted" } },
            dock(1, "warning", "loading"),
            dock(2, "warning", "loading"),
            dock(3, "ok", "free"),
            dock(4, "critical", "closed")
        ],
        "camera": { "preset": "iso" },
        "lighting": "studio",
        "motion": [{ "target": "dock-3", "kind": "pulse", "axis": "y", "seconds": 2.4, "amount": 0.08 }],
        "summary": "written by the server"
    });
    let option = |n: u8| json!({ "label": format!("Dock {n}"), "value": format!("dock-{n}") });
    json!({
        "surfaceId": "yard",
        "title": "Yard B",
        "components": [
            { "id": "root", "component": "Column", "children": ["docks", "dock", "reserve"] },
            { "id": "docks", "component": "Yard", "scene": scene, "pickInto": "/dock" },
            {
                "id": "dock", "component": "ChoicePicker", "label": "Dock", "value": { "path": "/dock" },
                "options": [option(1), option(2), option(3), option(4)]
            },
            {
                "id": "reserve", "component": "Button", "child": "reserve-label", "variant": "primary",
                "action": { "event": { "name": "reserve" } }
            },
            { "id": "reserve-label", "component": "Text", "text": "Reserve" }
        ],
        "dataModel": { "dock": [] },
        "actions": [{
            "id": "reserve", "label": "Reserve", "tool": "reserve_dock",
            "args": { "dock": { "path": "/dock" } }
        }]
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().map_or(Ok(8807), |port| port.parse())?;
    let workspace = args.next().map_or_else(std::env::temp_dir, PathBuf::from);
    std::fs::create_dir_all(&workspace)?;

    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("render_screen", yard()),
            ScriptedResponse::text("Here is the yard. Pick a free dock and press Reserve."),
            ScriptedResponse::text("Reserved."),
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
        HarnessAgent::builder("logistics")
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
            .tool(Arc::new(ReserveDock { log: workspace.join("reserved.txt") }))
            .toolset(Arc::new(DrawToolset::new(Validator::new(&SCENES))))
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
                        .with_worker_id("genui-scenes")
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
        .with_catalog(&SCENES)
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Logistics")
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
    println!("scripted genui scenes home on http://127.0.0.1:{port}/ui/");
    axum::serve(listener, app).await?;
    Ok(())
}
