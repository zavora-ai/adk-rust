//! S12, the live camera surface: the web home's camera board, driven without a browser as
//! `adk-surface`'s `live_camera_home` example builds it. The conveyor clip streams over RTSP in
//! real time into `PipelineCameras`, whose pipeline detects, tracks and shows each frame with
//! its overlays; the operator switches the camera off and on, raises a card from a frame, and
//! approves the Inspector's line stop from the decision card.
//!
//! The Inspector's model is a stand-in that reads the card and asks to stop the line with the
//! card's frame; the camera, pipeline, hub, surface engine, approvals and runner are the real
//! ones.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest};
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, ArtifactLocator, Evidence, ImageEvidence,
    InMemoryApprovalStore, Llm, LlmRequest, LlmResponseStream, Part, StoreApprovalService, Tool,
    ToolContext,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::camera::{
    CameraFacts, CameraUpdate, Ingest, LiveCamera, PipelineCameras, RaiseCard,
};
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, LocalSource, Origin, Place, SurfaceEngine, SurfaceKind,
};
use adk_vision::stream::fixtures::{ConveyorClip, ConveyorSpec};
use adk_vision::stream::{FrameBudget, RtspSource};
use adk_vision::{
    BBox, BackendDescriptor, DetectQuery, Detector, FrameSourceId, GreedyIouTracker, RawRegion,
    SentImage, VisionCx, VisionRuntime,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::Checks;
use crate::conveyor::{BeltDetector, serve};

const APP: &str = "inspector";
const OWNER: &str = "ana";
const SESSION: &str = "s-line";
const CAMERA: &str = "line-2";
const FPS: u32 = 5;

/// When something happened on a frame, and the labelled boxes on it.
type Moment<B> = (Instant, Vec<(String, B)>);

/// The detector, timed: when each detection finished, and what it found.
struct Timed {
    inner: BeltDetector,
    done: Mutex<Vec<Moment<BBox>>>,
}

impl Timed {
    fn done(&self) -> Vec<Moment<BBox>> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl Detector for Timed {
    fn descriptor(&self) -> &BackendDescriptor {
        self.inner.descriptor()
    }

    async fn detect(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let found = self.inner.detect(cx, image, query).await?;
        let boxes =
            found.iter().filter_map(|r| Some((r.label.clone()?, r.shape.bbox()?))).collect();
        self.done.lock().unwrap_or_else(|e| e.into_inner()).push((Instant::now(), boxes));
        Ok(found)
    }
}

/// Stops a line: held for the person, with the card's frame as its evidence.
struct StopLine(Arc<AtomicUsize>);

#[async_trait::async_trait]
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
        vec![
            ImageEvidence::new(
                text("frame"),
                ArtifactLocator::new(
                    APP,
                    OWNER,
                    SESSION,
                    text("file"),
                    i64::try_from(number("version")).unwrap_or_default(),
                ),
                "image/png",
                u32::try_from(number("width")).unwrap_or_default(),
                u32::try_from(number("height")).unwrap_or_default(),
            )
            .with_caption("The frame the card was raised from.")
            .into(),
        ]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "stopped": args["line"] }))
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
    Some(
        json!({ "line": 2, "frame": digest, "file": file, "version": version, "width": width, "height": height }),
    )
}

/// The Inspector's model, a stand-in that reads the card a person raised and asks to stop the
/// line with its frame.
struct CardReader;

#[async_trait::async_trait]
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
                content.parts.iter().filter_map(|part| part.text()).collect::<Vec<_>>().join("")
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

fn caller() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

async fn until(limit: Duration, mut done: impl AsyncFnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() <= limit {
        if done().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn stored(artifacts: &InMemoryArtifactService) -> Result<Vec<String>> {
    Ok(artifacts
        .list(ListRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: SESSION.into(),
        })
        .await?
        .file_names
        .into_iter()
        .filter(|name| !name.ends_with(".json"))
        .collect())
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note(
        "the conveyor clip over RTSP in real time at 5 frames a second into PipelineCameras; \
         the surface engine without a browser; BeltDetector; a stand-in Inspector model",
    );
    // A quarter of the parts cracked, so a crack passes every few seconds.
    let clip =
        Arc::new(ConveyorClip::new(ConveyorSpec { defective: 50, ..ConveyorSpec::default() }));
    let spec = clip.truth().spec.clone();
    // Two minutes of the clip at 5 frames a second, in real time.
    let server = serve(&clip, 0, 2, 600, FPS, 1.0).await?;

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
    let stops = Arc::new(AtomicUsize::new(0));
    let agent = Arc::new(
        LlmAgentBuilder::new(APP)
            .description("watches line 2 for cracks")
            .instruction("Stop the line when a card shows a defect.")
            .model(Arc::new(CardReader))
            .tool(Arc::new(StopLine(stops.clone())))
            .require_tool_confirmation("stop_line")
            .build()?,
    );
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(Arc::clone(&sessions))
        .artifact_service(artifacts.clone())
        .run_config(adk_core::RunConfig::builder().approval_service(Arc::clone(&approvals)).build())
        .build()?;

    let timed = Arc::new(Timed { inner: BeltDetector::new(), done: Mutex::default() });
    let audit = InMemoryAuditRecorder::new();
    let camera = LiveCamera::stream(
        CameraFacts::new(CAMERA, "Line 2 camera", APP, SESSION)
            .with_place("line 2")
            .passing("parts")
            .with_ingest(Ingest::rtsp(spec.width, spec.height, "this computer")),
        Arc::new(RtspSource::new(FrameSourceId(CAMERA.into()), &server.url())?),
    )
    .detector(
        Arc::new(VisionRuntime::builder().detector(timed.clone()).build()?),
        DetectQuery::Classes(vec!["part".into(), "crack".into()]),
    )
    .tracker(|| Box::new(GreedyIouTracker::new(0.3, 3)))
    .budget(FrameBudget::new(FPS as f32).with_min_local_fps(1.0).with_local_quota(500_000));
    let live = PipelineCameras::builder(APP, OWNER)
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .cx(VisionCx::new())
        .governance(runner.governance().clone())
        .audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default())
        .camera(camera)
        .build()?;
    let cameras = live.start();
    // Every frame the hub shows, with when it was shown and its overlays.
    let shown: Arc<Mutex<Vec<Moment<[f64; 4]>>>> = Arc::default();
    let mut updates = cameras.subscribe();
    let watcher = tokio::spawn({
        let shown = shown.clone();
        async move {
            loop {
                let update = match updates.recv().await {
                    Ok(update) => update,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if let CameraUpdate::Frame { camera, frame } = update
                    && camera == CAMERA
                {
                    let overlays = frame
                        .frame
                        .overlays
                        .iter()
                        .filter_map(|o| Some((o.label.clone(), o.bbox?)))
                        .collect();
                    shown
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((Instant::now(), overlays));
                }
            }
        }
    });
    let local = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&approvals))
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .runner(Arc::new(runner))
        .build()?;
    let engine = SurfaceEngine::builder(Arc::new(local))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Inspector")
        .cameras(cameras.clone())
        .build();
    let counts = || live.counts(CAMERA).unwrap_or_default();

    // Overlays: the hub shows each frame, with what was found on it, within a frame interval
    // of the detection.
    // At least forty frames, and until a cracked part has passed.
    let watched = until(Duration::from_secs(90), async || {
        counts().observed >= 40
            && shown
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|(_, overlays)| overlays.iter().any(|(label, _)| label == "crack"))
    })
    .await;
    let detections = timed.done();
    let frames = shown.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let interval = Duration::from_millis(1000 / u64::from(FPS));
    let paired = detections.len().min(frames.len());
    let mut worst = Duration::ZERO;
    let mut same = true;
    for ((found_at, found), (shown_at, overlays)) in detections.iter().zip(&frames).take(paired) {
        worst = worst.max(shown_at.saturating_duration_since(*found_at));
        same &= found.len() == overlays.len()
            && found.iter().all(|(label, b)| {
                overlays.iter().any(|(shown_label, s)| {
                    label == shown_label
                        && [b.x0, b.y0, b.x1, b.y1]
                            .iter()
                            .zip(s)
                            .all(|(a, b)| (f64::from(*a) - b).abs() < 0.5)
                })
            });
    }
    let cracks = frames.iter().filter(|(_, o)| o.iter().any(|(l, _)| l == "crack")).count();
    checks.check(
        &format!(
            "every frame is shown with its detections as overlays within one frame interval of \
             the detection ({paired} frames, {cracks} with a crack; slowest {worst:.0?} of {interval:?})"
        ),
        watched && paired >= 40 && same && worst < interval && cracks > 0,
    );
    checks.check(
        "nothing is stored while the operator only watches",
        stored(&artifacts).await?.is_empty(),
    );

    // Off: the server drops frames before the pipeline looks at them.
    engine.switch_camera(&caller(), CAMERA, false).await?;
    let before = counts();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = counts();
    let held = cameras.latest(CAMERA).await;
    checks.check(
        &format!(
            "switched off, the server drops frames: {} arrived and dropped, none looked at or shown",
            after.dropped - before.dropped
        ),
        after.observed == before.observed
            && (after.dropped > before.dropped || after.arrived == before.arrived)
            && held.is_none()
            && stored(&artifacts).await?.is_empty(),
    );

    // On again; the operator raises a card from a frame showing a crack.
    engine.switch_camera(&caller(), CAMERA, true).await?;
    let mut cracked = None;
    let found = until(Duration::from_secs(60), async || {
        if let Some(held) = cameras.latest(CAMERA).await
            && held.frame.overlays.iter().any(|o| o.label == "crack")
        {
            cracked = Some(held);
            return true;
        }
        false
    })
    .await;
    let held = cracked.filter(|_| found).context("a crack on screen after switching on")?;
    engine.freeze_camera_frame(&caller(), CAMERA, &held.digest).await?;
    let raised = engine
        .raise_camera_card(
            &caller(),
            CAMERA,
            &RaiseCard {
                frame: held.digest.clone(),
                note: "Crack on this part; stop the line?".into(),
            },
        )
        .await?;
    let kept = stored(&artifacts).await?;
    checks.check(
        &format!("a card keeps exactly its frame: {} image stored", kept.len()),
        kept.len() == 1
            && kept[0] == format!("vision-{}.png", raised.digest.trim_start_matches("sha256:"))
            && audit.records().iter().filter(|r| r.action == "vision.evidence.kept").count() == 1,
    );

    // The Inspector asks to stop the line; the call is held with the card's frame.
    let mut decision = None;
    let held_call = until(Duration::from_secs(30), async || {
        decision = engine
            .decisions(&caller())
            .await
            .ok()
            .and_then(|cards| cards.into_iter().find(|card| card.call.tool == "stop_line"));
        decision.is_some()
    })
    .await;
    let decision = decision.filter(|_| held_call).context("the line stop is held")?;
    let shows = decision
        .media
        .as_ref()
        .and_then(|media| media.images.first())
        .map(|image| image.digest.clone());
    checks.check(
        "the Inspector's line stop is held on a decision card showing the card's frame",
        stops.load(Ordering::SeqCst) == 0 && shows.as_deref() == Some(raised.digest.as_str()),
    );
    let outcome = engine
        .execute(
            &caller(),
            Command::Decide {
                decision_id: decision.id.clone(),
                version: decision.version,
                answer: Answer::AllowOnce,
                edited_args: None,
                reason: None,
                expanded: true,
                evidence: vec![raised.digest.clone()],
            },
        )
        .await?;
    let ran = until(Duration::from_secs(30), async || stops.load(Ordering::SeqCst) > 0).await;
    checks.check(
        "the operator's approval on the card stops the line, once",
        matches!(outcome, CommandOutcome::Answered { .. })
            && ran
            && stops.load(Ordering::SeqCst) == 1,
    );
    checks.check("still only the card's frame is stored", stored(&artifacts).await?.len() == 1);

    engine.stop_camera(&caller(), CAMERA).await.ok();
    watcher.abort();
    drop(cameras);
    server.shutdown().await;
    Ok(checks)
}
