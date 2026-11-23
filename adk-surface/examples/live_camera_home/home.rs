//! The home `live_camera_home` serves, with any detector: two cameras over the conveyor clip on
//! the frame pipeline, the Inspector that stops the line from a card, and the web home.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_artifact::{ArtifactService, InMemoryArtifactService};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, ArtifactLocator, Evidence, ImageEvidence,
    InMemoryApprovalStore, Llm, LlmRequest, LlmResponseStream, Part, RequestContext,
    SingleAgentLoader, StoreApprovalService, Tool, ToolContext,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::camera::{CameraFacts, Ingest, LiveCamera, LiveWatch, PipelineCameras, ZoneFacts};
use adk_surface::{CardHint, CardHints, LocalSource, Place, SurfaceEngine};
use adk_vision::stream::fixtures::{
    ConveyorClip, ConveyorSpec, RtspTestOptions, RtspTestServer, TestVideo,
};
use adk_vision::stream::{FrameBudget, NoRedaction, RtspSource};
use adk_vision::{DetectQuery, FrameSourceId, GreedyIouTracker, VisionCx, VisionRuntime};
use async_trait::async_trait;
use serde_json::{Value, json};

pub const APP: &str = "inspector";
pub const OWNER: &str = "ana";
pub const SESSION: &str = "s-line";

/// What the cameras detect and what line 2 watches for.
pub struct Detection {
    /// The detection route both cameras call.
    pub runtime: Arc<VisionRuntime>,
    /// What they ask it for.
    pub query: DetectQuery,
    /// Line 2's watch, inside `zone`.
    pub watch: LiveWatch,
    /// Line 2's zone, in the clip's pixels.
    pub zone: ZoneFacts,
}

/// The clip: parts on a belt, a quarter of them cracked so a crack passes every few seconds.
pub fn clip() -> ConveyorClip {
    ConveyorClip::new(ConveyorSpec { defective: 50, ..ConveyorSpec::default() })
}

/// How many frames of the clip play in a loop: thirty seconds, about five cracked parts.
const LOOP_FRAMES: u64 = 300;

/// Frames `0..count` of `clip`, drawn on every core.
fn drawn(clip: &ConveyorClip, count: u64) -> Vec<adk_vision::DecodedImage> {
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get) as u64;
    let mut frames: Vec<(u64, adk_vision::DecodedImage)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..count)
                        .step_by(threads as usize)
                        .map(|index| (index, clip.render(index)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
    });
    frames.sort_by_key(|(index, _)| *index);
    frames.into_iter().map(|(_, frame)| frame).collect()
}

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

/// Stops a line: held for the person, with the frame the card raised as its evidence.
struct StopLine;

#[async_trait]
impl Tool for StopLine {
    fn name(&self) -> &str {
        "stop_line"
    }

    fn description(&self) -> &str {
        "Stops a production line. Give the line and the frame of the card as evidence."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "line": { "type": "integer" },
                "frame": { "type": "string" },
                "file": { "type": "string" },
                "version": { "type": "integer" },
                "width": { "type": "integer" },
                "height": { "type": "integer" },
            },
            "required": ["line", "frame", "file", "version", "width", "height"],
        }))
    }

    fn approval_evidence(&self, args: &Value) -> Vec<Evidence> {
        let text = |key: &str| args[key].as_str().unwrap_or_default().to_string();
        let number = |key: &str| args[key].as_u64().unwrap_or_default();
        let locator = ArtifactLocator::new(
            APP,
            OWNER,
            SESSION,
            text("file"),
            i64::try_from(number("version")).unwrap_or_default(),
        );
        vec![
            ImageEvidence::new(
                text("frame"),
                locator,
                "image/png",
                u32::try_from(number("width")).unwrap_or_default(),
                u32::try_from(number("height")).unwrap_or_default(),
            )
            .with_caption("The frame the card was raised from.")
            .into(),
        ]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(json!({ "stopped": args["line"] }))
    }
}

/// The Inspector's model, a stand-in that reads the card a person raised: the frame's digest,
/// artifact and size, then asks to stop the line with that frame as evidence.
struct CardReader;

#[async_trait]
impl Llm for CardReader {
    fn name(&self) -> &str {
        "card-reader"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let last = request.contents.last();
        let answered = last.is_some_and(|content| {
            content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
        });
        let text: String = last
            .map(|content| {
                content
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let response = match card(&text) {
            Some(args) if !answered => ScriptedResponse::function_call("stop_line", args),
            _ if answered => ScriptedResponse::text("Line 2 is stopped."),
            _ => ScriptedResponse::text("Looking at the line."),
        };
        ScriptedLlm::new("card-reader", [response]).generate_content(request, stream).await
    }
}

/// The frame a card names: "… sha256:<hex> (artifact <file> v<version>, <w> × <h>, …".
fn card(text: &str) -> Option<Value> {
    if !text.starts_with("Card from") {
        return None;
    }
    let at = text.find("sha256:")?;
    let digest: String =
        text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == ':').collect();
    let rest = &text[at..];
    let artifact = rest.find("(artifact ")? + "(artifact ".len();
    let mut words = rest[artifact..].split_whitespace();
    let file = words.next()?.to_string();
    let version: i64 = words.next()?.trim_start_matches('v').trim_end_matches(',').parse().ok()?;
    let width: u32 = words.next()?.parse().ok()?;
    words.next()?;
    let height: u32 = words.next()?.trim_end_matches(',').parse().ok()?;
    Some(json!({
        "line": 2,
        "frame": digest,
        "file": file,
        "version": version,
        "width": width,
        "height": height,
    }))
}

/// "Stop line 2", as the Inspector's line-stop call reads.
struct LineStop;

#[async_trait]
impl CardHints for LineStop {
    async fn hint(&self, request: &adk_core::ApprovalRequest) -> Option<CardHint> {
        (request.tool_name == "stop_line").then(|| CardHint::new().with_heading("Stop line 2"))
    }
}

/// Serves the home on `port`: the clip over an in-process RTSP server, line 2 watched with
/// `detection` at `line_2_fps` frames a second against a generous daily budget, line 3 on the
/// same clip held to `line_3_quota` frames a day, and the voice screen's camera, off.
///
/// # Errors
///
/// Whatever fails to start.
pub async fn serve(
    port: u16,
    detection: Detection,
    line_3_quota: u64,
    line_2_fps: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    adk_core::ensure_crypto_provider();
    let clip = clip();
    let spec = clip.truth().spec.clone();
    // The clip's first thirty seconds, drawn once up front and played in a loop.
    let frames = Arc::new(drawn(&clip, LOOP_FRAMES.min(clip.frame_count())));
    let count = frames.len() as u64;
    let server = RtspTestServer::start_with(
        TestVideo::Mjpeg {
            render: Arc::new(move |index| frames[(index % count) as usize].clone()),
            frames: count,
            fps: spec.fps,
            quality: 85,
        },
        RtspTestOptions { looping: true, ..RtspTestOptions::default() },
    )
    .await?;
    let url = server.url();

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await?;
    let store = Arc::new(InMemoryApprovalStore::new());
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(store.clone()).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ));
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let agent = Arc::new(
        LlmAgentBuilder::new(APP)
            .description("watches line 2 for cracks")
            .instruction("Stop the line when a card shows a defect.")
            .model(Arc::new(CardReader))
            .tool(Arc::new(StopLine))
            .require_tool_confirmation("stop_line")
            .build()?,
    );
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent.clone())
        .session_service(Arc::clone(&sessions))
        .artifact_service(artifacts.clone())
        // The line stop is held in the inbox the home reads, through its router.
        .run_config(adk_core::RunConfig::builder().approval_service(Arc::clone(&approvals)).build())
        .build()?;

    let stream = |id: &str| -> Result<Arc<RtspSource>, adk_vision::VisionError> {
        Ok(Arc::new(RtspSource::new(FrameSourceId(id.into()), &url)?))
    };
    let line_2 = LiveCamera::stream(
        CameraFacts::new("line-2", "Line 2 camera", APP, SESSION)
            .with_place("line 2")
            .passing("parts")
            .with_ingest(Ingest::rtsp(spec.width, spec.height, "this computer")),
        stream("line-2")?,
        Arc::new(NoRedaction),
    )
    .detector(Arc::clone(&detection.runtime), detection.query.clone())
    .tracker(|| Box::new(GreedyIouTracker::new(0.3, 3)))
    .budget(
        FrameBudget::new(line_2_fps)
            .with_min_local_fps(1.0_f32.min(line_2_fps))
            .with_local_quota(500_000),
    )
    .zone(detection.zone)
    .watch(detection.watch);
    let line_3 = LiveCamera::stream(
        CameraFacts::new("line-3", "Line 3 camera", APP, SESSION)
            .with_place("line 3")
            .passing("parts")
            .with_ingest(Ingest::rtsp(spec.width, spec.height, "this computer")),
        stream("line-3")?,
        Arc::new(NoRedaction),
    )
    .detector(Arc::clone(&detection.runtime), detection.query)
    .tracker(|| Box::new(GreedyIouTracker::new(0.3, 3)))
    .budget(
        FrameBudget::new(5.0)
            .with_min_local_fps(1.0)
            .with_local_quota(line_3_quota)
            .with_escalations_per_minute(6)
            .with_escalation_quota(5_000),
    )
    .escalation("sam-3.1", 2_000);
    let this_computer = LiveCamera::surface(
        CameraFacts::new("this-computer", "This computer's camera", APP, SESSION)
            .with_place("this computer")
            .with_ingest(Ingest::this_computer(1280, 720)),
        Arc::new(NoRedaction),
    );
    let live = PipelineCameras::builder(APP, OWNER)
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .cx(VisionCx::new())
        .governance(runner.governance().clone())
        .camera(line_2)
        .camera(line_3)
        .camera(this_computer)
        .build()?;
    let cameras = live.start();

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
        .cameras(cameras.clone())
        .build();

    // What each pipeline did with its frames and what the hub dropped, for the end-to-end test.
    let probe = axum::Router::new().route(
        "/e2e/cameras",
        axum::routing::get(move || {
            let live = Arc::clone(&live);
            let cameras = cameras.clone();
            async move {
                let mut out = serde_json::Map::new();
                for id in ["line-2", "line-3", "this-computer"] {
                    out.insert(
                        id.into(),
                        json!({
                            "pipeline": live.counts(id),
                            "hubDropped": cameras.dropped(id).await,
                            "on": cameras.is_on(id).await,
                            "capturing": live.is_capturing(id),
                        }),
                    );
                }
                axum::Json(Value::Object(out))
            }
        }),
    );
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions)
        .with_request_context(Arc::new(Owner));
    let app = ServerBuilder::new(config)
        .add_api_routes(adk_surface::web::router(engine))
        .add_root_routes(adk_surface::web::home())
        .add_root_routes(probe)
        .build();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("live camera surface on http://127.0.0.1:{port}/ui/cameras/line-2");
    axum::serve(listener, app).await?;
    server.shutdown().await;
    Ok(())
}
