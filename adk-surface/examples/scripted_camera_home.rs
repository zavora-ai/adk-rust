//! The live camera surface on the web home over scripted cameras (spec 3.7 §8.2, S12).
//!
//! Four cameras as the signed-off boards draw them — the line 2 conveyor with a seal tear on
//! part T43, line 3 saving frames, the packing bench with a person blurred, and this computer's
//! camera off — are served by `ScriptedCameras`, which offers each camera's frames to the hub at
//! its rate as a pipeline would. The Inspector's line-stop call is held with the line 2 frame
//! as its evidence. The hub, the engine, every route, the approval inbox, the artifact store
//! and the session are the real ones: switching a camera off drops its frames on the server,
//! and raising a card keeps exactly the frozen frame and sends it to the Inspector.
//!
//! The frames are the boards' drawings (`examples/fixtures/camera/`, `fixtures/viewer/`), all
//! content invented. A stand-in extractor admits every request as the owner, with presence
//! confirmed, so "Confirm it's you and approve" answers in the browser.
//!
//! ```sh
//! cargo run -p adk-surface --features web --example scripted_camera_home -- 8794
//! ```
//!
//! Then open `http://127.0.0.1:8794/ui/cameras/line-2`.

use std::sync::Arc;
use std::time::Duration;

use adk_artifact::{ArtifactService, InMemoryArtifactService};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, Content, Event, EventActions, Evidence, InMemoryApprovalStore,
    RequestContext, SingleAgentLoader, StoreApprovalService, ToolConfirmationRequest,
};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::camera::{CameraBoard, CameraSource, Cameras, ScriptedCameras};
use adk_surface::{CardHint, CardHints, LocalSource, Place, SurfaceEngine};
use async_trait::async_trait;
use chrono::{Local, TimeZone as _};
use serde_json::json;

const APP: &str = "inspector";
const OWNER: &str = "ana";
const SESSION: &str = "s-line";
const LINE_2: &[u8] = include_bytes!("fixtures/viewer/line-2-10-41-07.png");
const LINE_3: &[u8] = include_bytes!("fixtures/camera/line-3.png");
const BENCH: &[u8] = include_bytes!("fixtures/camera/packing-bench.png");

/// Admits every request as the owner, presence confirmed, as a loopback example's extractor may.
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
            metadata: [(
                adk_surface::web::PRESENCE_METADATA_KEY.to_string(),
                "confirmed".to_string(),
            )]
            .into_iter()
            .collect(),
        })
    }
}

/// The Inspector: each card it gets, it says it is looking at the frame.
struct Inspector;

#[async_trait]
impl adk_core::Agent for Inspector {
    fn name(&self) -> &str {
        APP
    }

    fn description(&self) -> &str {
        "watches line 2 for seal tears"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let mut event = Event::new("inv-look");
        event.author = APP.into();
        event.llm_response.content =
            Some(Content::new("model").with_text("Looking at the frame you raised."));
        Ok(Box::pin(futures::stream::once(async move { Ok(event) })))
    }
}

/// "Stop line 2", as the Inspector's line-stop call reads.
struct LineStop;

#[async_trait]
impl CardHints for LineStop {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        (request.tool_name == "stop_line").then(|| CardHint::new().with_heading("Stop line 2"))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let port: u16 = std::env::args().nth(1).map_or(Ok(8794), |port| port.parse())?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await?;
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ));
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let agent = Arc::new(Inspector);
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent.clone())
        .session_service(Arc::clone(&sessions))
        .artifact_service(artifacts.clone())
        .build()?;

    // Today's local midnight sets the boards' budget renewal.
    let today = Local::now().date_naive();
    let midnight = Local
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap_or_default())
        .single()
        .unwrap_or_else(Local::now)
        .fixed_offset();
    let source = Arc::new(
        ScriptedCameras::boards(APP, OWNER, SESSION, CameraBoard::Live, midnight)
            .artifacts(artifacts.clone() as Arc<dyn ArtifactService>),
    );
    let cameras = Cameras::new(source.clone());
    let playing = [
        ScriptedCameras::play(
            cameras.clone(),
            "line-2",
            CameraBoard::Live,
            LINE_2.to_vec(),
            "image/png",
            Duration::from_millis(200),
        ),
        ScriptedCameras::play(
            cameras.clone(),
            "line-3",
            CameraBoard::Live,
            LINE_3.to_vec(),
            "image/png",
            Duration::from_millis(500),
        ),
        ScriptedCameras::play(
            cameras.clone(),
            "packing-bench",
            CameraBoard::Live,
            BENCH.to_vec(),
            "image/png",
            Duration::from_secs(1),
        ),
    ];

    // The Inspector's line stop, held with the line 2 frame on the card (board choice C13).
    tokio::time::sleep(Duration::from_millis(300)).await;
    if let (Some(held), Some(facts)) =
        (cameras.latest("line-2").await, source.camera("line-2").await)
    {
        let mut evidence = source.keep_frame(&facts, &held).await?;
        evidence.caption = Some("Seal tear of 14.3 mm on part T43.".into());
        let call = ToolConfirmationRequest::new(
            "stop_line",
            json!({ "line": 2, "frame": evidence.digest }),
        )
        .with_evidence([Evidence::from(evidence)]);
        let mut request = ApprovalRequest::new(
            "dec-line-2",
            ApprovalScope::new(APP, OWNER, SESSION),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        );
        request.summary = "Seal tear of 14.3 mm on part T43. The frame is on the card.".into();
        request.risk = ApprovalRisk::High;
        let mut event = Event::new("inv-1");
        event.author = APP.into();
        event.actions = EventActions { tool_confirmation: Some(call), ..EventActions::default() };
        sessions.append_event(SESSION, event).await?;
        approvals.submit(request).await?;
    }

    let local = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&approvals))
        .artifacts(artifacts as Arc<dyn ArtifactService>)
        .runner(Arc::new(runner))
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(local))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Inspector")
        .card_hints(Arc::new(LineStop))
        .cameras(cameras)
        .build();

    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("live camera surface on http://127.0.0.1:{port}/ui/cameras/line-2");
    axum::serve(listener, app).await?;
    for task in playing {
        task.abort();
    }
    Ok(())
}
