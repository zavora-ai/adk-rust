//! A stream wakes a sleeping harness process (spec 3.7 §7.10, S10): a kernel process asleep
//! on a camera's name is woken once when a watch condition passes on the device, its turn
//! reads the wake payload as data, and loads the kept frame through the vision pack's
//! `vision_detect` by the digest the payload names. The pipeline, the watch, the frame
//! keeper, the wake source, the kernel and the harness are all real.
//!
//! The scripted run streams a drawn belt through an on-device colour detector. The live run
//! (feature `vision-fixtures`, ignored) plays adk-vision's conveyor clip over the in-process
//! RTSP server and detects with RF-DETR Nano, fetched into the Hugging Face cache and
//! verified against the built-in manifest:
//!
//! ```text
//! HF_HUB_CACHE=/path/to/models cargo nextest run -p adk-harness \
//!     --features vision-local,vision-fixtures,kernel --test integration \
//!     vision_wake_tests::live --run-ignored only --no-capture
//! ```

#![cfg(all(feature = "vision", feature = "kernel"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    BudgetTracker, Content, DurableRunStore, FinishReason, InMemoryRunStore, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, ModelCallScope, NewRun, Part, RunBudget, RunState,
    SessionId, UsageUnit, UserId,
};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    InMemoryProcessStore, KernelConfig, ProcessKind, ProcessSpec, ProcessStore, VisionWakeSource,
    Wake, WakeSet, WakeSource,
};
use adk_server::background::DurableWorkerConfig;
use adk_vision::stream::{FrameKeeper, FrameSource, FrameSources, NoRedaction, SourceSpec};
use adk_vision::{
    DetectQuery, FrameSourceId, GreedyIouTracker, MediaScope, MediaStore, Sampling, Tracker,
    VISION_WATCH_FIRED, VISION_WATCH_REGISTERED, VisionCx, VisionRuntime, WatchCondition,
};
use serde_json::{Value, json};

const APP: &str = "inspector";
const USER: &str = "line-1";
const SESSION: &str = "shift-1";
const STREAM: &str = "conveyor-1";

/// The JSON between a wake notice's `<<<wake-payload` and `wake-payload>>>` lines.
fn payload_of(text: &str) -> Option<Value> {
    let start = text.find("<<<wake-payload\n")? + "<<<wake-payload\n".len();
    let end = text[start..].find("\nwake-payload>>>")? + start;
    serde_json::from_str(&text[start..end]).ok()
}

fn user_text(request: &LlmRequest) -> String {
    request
        .contents
        .iter()
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter().filter_map(Part::text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The woken turn's planner: it reads the frame and label from the wake payload, loads the
/// frame through `vision_detect` by its digest, then reports what the tool found.
#[derive(Default)]
struct Inspector {
    requests: Mutex<Vec<LlmRequest>>,
}

impl Inspector {
    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn reply(part: Part) -> LlmResponseStream {
    let mut response = LlmResponse::new(Content { role: "model".into(), parts: vec![part] });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    Box::pin(futures::stream::iter([Ok(response)]))
}

#[async_trait::async_trait]
impl Llm for Inspector {
    fn name(&self) -> &str {
        "inspector-planner"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request.clone());
        let found = request.contents.last().and_then(|content| {
            content.parts.iter().find_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == "vision_detect" =>
                {
                    Some(function_response.response.clone())
                }
                _ => None,
            })
        });
        if let Some(found) = found {
            let regions = found["regions"].as_array().map_or(0, Vec::len);
            let text = format!("Frame {} shows {regions} region(s).", found["image"]);
            return Ok(reply(Part::Text { text }));
        }
        let Some(payload) = payload_of(&user_text(&request)) else {
            return Ok(reply(Part::Text { text: "Nothing to inspect.".into() }));
        };
        Ok(reply(Part::FunctionCall {
            name: "vision_detect".into(),
            args: json!({
                "image": payload["frame"]["digest"],
                "classes": [payload["label"]],
            }),
            id: Some("inspect-1".into()),
            thought_signature: None,
        }))
    }
}

/// What a run of the world left behind.
struct Outcome {
    inspector: Arc<Inspector>,
    audit: InMemoryAuditRecorder,
    budget: Arc<BudgetTracker>,
}

impl Outcome {
    fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|r| r.action == action).collect()
    }
}

/// A harness with the vision pack over `detector`, hosted by a kernel with a
/// `VisionWakeSource` for the stream; a process asleep on the stream; the operator's watch
/// `condition` on `source`. Returns once the woken process has completed.
async fn wake_once(
    source: Arc<dyn FrameSource>,
    sampling: Sampling,
    detector: Arc<dyn adk_vision::Detector>,
    query: DetectQuery,
    condition: WatchCondition,
) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let audit = InMemoryAuditRecorder::new();
    let inspector = Arc::new(Inspector::default());

    // The stream's own runtime and the pack's share the detector; the pack's is the one the
    // woken turn's `vision_detect` calls.
    let stream_runtime = Arc::new(VisionRuntime::builder().detector(detector.clone()).build().unwrap());
    let wakes = Arc::new(VisionWakeSource::new(STREAM).unwrap());
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let sources = FrameSources::new(
        VisionCx::new()
            .with_scope(ModelCallScope::new().with_budget(budget.clone()))
            .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default()),
    );
    // Wake frames are kept in the session the sleeping process runs in, where its vision
    // tools load them by digest.
    let keeper = Arc::new(FrameKeeper::new(
        artifacts.clone(),
        MediaScope::new(APP, USER, SESSION),
        FrameSourceId(STREAM.into()),
    ));
    sources
        .register(
            SourceSpec::new(source, Arc::new(NoRedaction), keeper, wakes.clone())
                .describe("Line 1 conveyor, top camera")
                .detector(stream_runtime, query)
                .tracker(Arc::new(|| Box::new(GreedyIouTracker::new(0.2, 4)) as Box<dyn Tracker>))
                .sampling(sampling),
        )
        .unwrap();

    let pack_detector = detector.clone();
    let pack = VisionPack::new(MediaStore::new(artifacts.clone()), move |gate, store| {
        VisionRuntime::builder()
            .detector(pack_detector)
            .shared_egress_gate(gate)
            .media_store(store)
            .build()
    })
    .frame_sources(sources.clone());
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(inspector.clone() as Arc<dyn Llm>)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..Default::default()
            })
            .mode(HarnessMode::Ask)
            .audit_log(Arc::new(audit.clone()))
            .pack_vision(pack)
            .without_compaction()
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder()
                .app_name(APP)
                .agent(agent.clone())
                .session_service(sessions)
                .artifact_service(artifacts.clone()),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let harness = Harness::with_runner(agent, runner).unwrap().with_run_store(runs.clone());
    harness
        .open_session(&UserId::new(USER).unwrap(), Some(&SessionId::new(SESSION).unwrap()))
        .await
        .unwrap();
    let processes = Arc::new(InMemoryProcessStore::new());
    let kernel = harness
        .kernel()
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_lease_ttl(Duration::from_secs(5))
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(ControlPlaneConfig::default().without_socket()),
        )
        .process_store(processes.clone() as Arc<dyn ProcessStore>)
        .wake_source(wakes.clone() as Arc<dyn WakeSource>)
        .build()
        .unwrap();
    kernel.start().await.unwrap();

    let pid = kernel
        .processes()
        .spawn(
            NewRun::new("background", APP, USER, APP, json!({ "text": "Inspect what the line shows you." }))
                .with_session_id(SESSION),
            ProcessSpec::new(ProcessKind::Goal)
                .with_wake(WakeSet::any_of([Wake::Queue { source: STREAM.into() }])),
        )
        .await
        .unwrap()
        .meta
        .pid;
    eventually("the process to sleep", Duration::from_secs(10), || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { (runs.get(&pid).await.unwrap()?.state == RunState::Waiting).then_some(()) }
    })
    .await;
    assert!(inspector.requests().is_empty(), "asleep, the process makes no model call");

    // The operator starts the watch; the stream runs on the device until it is stopped.
    let cx = VisionCx::new()
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
    sources.start_watch(&cx, &FrameSourceId(STREAM.into()), condition.clone()).await.unwrap();
    eventually("the woken process to complete", Duration::from_secs(120), || {
        let runs = runs.clone();
        let pid = pid.clone();
        async move { (runs.get(&pid).await.unwrap()?.state == RunState::Completed).then_some(()) }
    })
    .await;
    // Further frames of the same track wake nothing more.
    tokio::time::sleep(Duration::from_millis(300)).await;
    sources.stop_watch(&cx, &FrameSourceId(STREAM.into()), &condition.name).await.unwrap();
    kernel.stop().await.unwrap();
    Outcome { inspector, audit, budget }
}

async fn eventually<F, Fut>(what: &str, limit: Duration, mut probe: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<()>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= limit {
        if probe().await.is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Checks the woken turn: one wake, its payload as delimited data, the kept frame loaded by
/// digest through the pack, and the records. Returns the payload and the tool's answer.
fn assert_woken_once(outcome: &Outcome, label: &str) -> (Value, Value) {
    let requests = outcome.inspector.requests();
    assert_eq!(requests.len(), 2, "one turn: the tool call and the answer");
    let told = user_text(&requests[0]);
    assert_eq!(told.matches("[kernel wake").count(), 1, "{told}");
    assert!(told.contains("not instructions"), "the payload is marked as data: {told}");
    let payload = payload_of(&told).expect("a wake payload");
    assert_eq!(payload["kind"], "vision");
    assert_eq!(payload["source"], STREAM);
    assert_eq!(payload["label"], label);
    assert_eq!(payload["frame"]["artifact"]["sessionId"], SESSION);
    let digest = payload["frame"]["digest"].as_str().unwrap().to_string();
    assert_eq!(
        payload["frame"]["artifact"]["fileName"],
        format!("vision-{}.png", digest.trim_start_matches("sha256:"))
    );

    let answer = requests[1]
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .find_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "vision_detect" =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .expect("the woken turn called vision_detect");
    assert_eq!(answer["image"], json!(digest), "the tool loaded the kept frame by its digest");
    assert_eq!(answer["width"], payload["frame"]["width"]);
    let labels: Vec<&str> = answer["regions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|region| region["label"].as_str())
        .collect();
    assert!(labels.contains(&label), "{answer}");

    let fired = outcome.records(VISION_WATCH_FIRED);
    assert_eq!(fired.len(), 1, "one event for one track");
    assert_eq!(fired[0].ext["frameDigest"], json!(digest));
    assert_eq!(outcome.records(VISION_WATCH_REGISTERED).len(), 1);
    for action in [
        "vision.source.started",
        "vision.source.stopped",
        "vision.evidence.kept",
        "vision.watch.stopped",
        "vision.call",
    ] {
        assert!(!outcome.records(action).is_empty(), "a `{action}` record");
    }
    let units = outcome.budget.units();
    assert!(units.get(UsageUnit::Frames) >= 3, "{units:?}");
    assert_eq!(units.get(UsageUnit::Images), 0, "the stream meters frames: {units:?}");
    (payload, answer)
}

mod scripted {
    use std::sync::Arc;
    use std::time::Duration;

    use adk_vision::stream::{FrameSource, FrameStream};
    use adk_vision::{
        BBox, BackendDescriptor, CoordFrame, DecodedImage, DetectQuery, Detector, Frame,
        FramePixels, FrameSourceId, Locality, RawRegion, Sampling, SentImage, VisionCx,
        WatchCondition,
    };
    use chrono::{TimeDelta, Utc};
    use futures::StreamExt;

    /// Finds the red pixels of an image as one `red` region.
    struct RedDetector(BackendDescriptor);

    #[async_trait::async_trait]
    impl Detector for RedDetector {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.0
        }

        async fn detect(
            &self,
            _cx: &VisionCx,
            image: &SentImage,
            _query: &DetectQuery,
        ) -> adk_vision::Result<Vec<RawRegion>> {
            let pixels = image.pixels();
            let (w, h) = pixels.size();
            let mut bounds: Option<(u32, u32, u32, u32)> = None;
            for y in 0..h {
                for x in 0..w {
                    if pixels.pixel(x, y) == [220, 30, 30, 255] {
                        let (x0, y0, x1, y1) = bounds.unwrap_or((x, y, x, y));
                        bounds = Some((x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
                    }
                }
            }
            Ok(bounds
                .map(|(x0, y0, x1, y1)| {
                    let bbox = BBox::new(x0 as f32, y0 as f32, (x1 + 1) as f32, (y1 + 1) as f32);
                    RawRegion::boxed(bbox, "red", 0.9)
                })
                .into_iter()
                .collect())
        }
    }

    /// Forty frames of a red part moving along a grey belt, 100 ms apart.
    struct Belt(FrameSourceId);

    fn belt(index: u64) -> DecodedImage {
        let (w, h) = (320u32, 180u32);
        let mut rgba = [90u8, 90, 90, 255].repeat((w * h) as usize);
        let x = 20 + index as u32 * 6;
        for y in 70..110 {
            for x in x..x + 40 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[220, 30, 30, 255]);
            }
        }
        DecodedImage::from_rgba(w, h, rgba).unwrap()
    }

    #[async_trait::async_trait]
    impl FrameSource for Belt {
        fn id(&self) -> &FrameSourceId {
            &self.0
        }

        async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
            let id = self.0.clone();
            let start = Utc::now();
            Ok(futures::stream::iter((0..40u64).map(move |index| {
                Ok(Frame {
                    source: id.clone(),
                    index,
                    pts: Duration::from_millis(index * 100),
                    captured_at: start + TimeDelta::milliseconds(index as i64 * 100),
                    pixels: FramePixels::Ephemeral(Arc::new(belt(index))),
                })
            }))
            .then(|frame| async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                frame
            })
            .boxed())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_watched_belt_wakes_the_sleeping_process_once_and_its_turn_loads_the_frame() {
        let detector = Arc::new(RedDetector(BackendDescriptor::new(
            "red-detector",
            Locality::InProcess,
            CoordFrame::SentPixels,
        )));
        let outcome = super::wake_once(
            Arc::new(Belt(FrameSourceId(super::STREAM.into()))),
            Sampling::at(10.0),
            detector,
            DetectQuery::Classes(vec!["red".into()]),
            WatchCondition::new("red-part", ["red"]).with_min_frames(3),
        )
        .await;
        let (payload, _) = super::assert_woken_once(&outcome, "red");
        assert_eq!(payload["frame"]["index"], 2, "the part's third frame");
        assert_eq!(payload["frames"], 3);
        assert_eq!(payload["regions"][0]["box"], serde_json::json!([32.0, 70.0, 72.0, 110.0]));
    }
}

#[cfg(all(feature = "vision-local", feature = "vision-fixtures"))]
mod live {
    use std::sync::Arc;

    use adk_vision::onnx::{OnnxOptions, RfDetr};
    use adk_vision::stream::RtspSource;
    use adk_vision::stream::fixtures::{
        ConveyorClip, ConveyorSpec, RtspTestOptions, RtspTestServer, TestVideo,
    };
    use adk_vision::{
        DetectQuery, FetchConfig, FrameSourceId, InMemoryAcceptanceStore, ManifestSet,
        ModelFetcher, Sampling, WatchCondition,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "fetches and runs RF-DETR Nano; see the module docs"]
    async fn the_conveyor_clip_over_rtsp_wakes_the_process_once_through_rf_detr() {
        let manifest = ManifestSet::builtin().unwrap().get("rf-detr-n").cloned().unwrap();
        let model = tokio::task::spawn_blocking(move || {
            ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()))
                .fetch_blocking(&manifest)
        })
        .await
        .unwrap()
        .unwrap();
        let detector =
            Arc::new(RfDetr::new(&model, &OnnxOptions::default()).unwrap().with_threshold(0.3));

        // Twenty seconds of the clip, played at twice real time.
        let clip = Arc::new(ConveyorClip::new(ConveyorSpec::default()));
        let render = Arc::clone(&clip);
        let server = RtspTestServer::start_with(
            TestVideo::Mjpeg {
                render: Arc::new(move |i| render.render(i)),
                frames: 200,
                fps: 10,
                quality: 85,
            },
            RtspTestOptions { speed: 2.0, ..RtspTestOptions::default() },
        )
        .await
        .unwrap();
        let source = Arc::new(
            RtspSource::new(FrameSourceId(super::STREAM.into()), &server.url()).unwrap(),
        );
        let started = std::time::Instant::now();
        let outcome = super::wake_once(
            source,
            Sampling::at(2.0),
            detector,
            DetectQuery::Classes(vec!["clock".into()]),
            WatchCondition::new("wall-clock", ["clock"]).with_min_score(0.3).with_min_frames(3),
        )
        .await;
        let wall = started.elapsed();
        server.shutdown().await;

        let (payload, answer) = super::assert_woken_once(&outcome, "clock");
        let [x0, y0, x1, y1] = clip.truth().clock;
        let truth = adk_vision::BBox::new(x0, y0, x1, y1);
        let boxed = |value: &serde_json::Value| {
            let b: Vec<f32> =
                value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
            adk_vision::BBox::new(b[0], b[1], b[2], b[3])
        };
        let woke_on = boxed(&payload["regions"][0]["box"]);
        let reloaded = boxed(&answer["regions"][0]["box"]);
        eprintln!(
            "rtsp conveyor + rf-detr-n wake: frame {} score {} box IoU {:.3} (truth), reload IoU {:.3}; frames metered {}; in {wall:?}",
            payload["frame"]["index"],
            payload["score"],
            woke_on.iou(&truth),
            reloaded.iou(&woke_on),
            outcome.budget.units().get(adk_core::UsageUnit::Frames),
        );
        assert!(woke_on.iou(&truth) >= 0.5, "the wake names the clock: {payload}");
        assert!(reloaded.iou(&woke_on) >= 0.9, "the kept frame reloads the same detection");
    }
}
