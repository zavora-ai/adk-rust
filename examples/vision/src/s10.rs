//! S10, wake on what matters: the Inspector is a harness process asleep in the kernel on the
//! line's camera. The conveyor clip streams over RTSP through the operator's watch
//! (`FrameSources`, `vision_watch`'s registry); the first defect's event keeps its frame and
//! reaches the kernel through `VisionWakeSource`. The woken turn reads the payload as data,
//! loads the frame through the vision pack's `vision_detect` by its digest, and asks to stop
//! the line with the frame as the call's evidence: in Ask mode the call is held for the
//! person, whose one approval lets it run.
//!
//! The Inspector's model is a stand-in that plans from what the turn shows it: the payload,
//! then the tool's answer. The stream, watch, keeper, wake source, kernel, harness, pack and
//! approval are the real ones.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ArtifactLocator, BudgetTracker, Content, DurableRunStore, Evidence, FinishReason,
    ImageEvidence, InMemoryRunStore, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    ModelCallScope, NewRun, Part, RunBudget, RunState, SessionId, Tool, ToolContext, UserId,
};
use adk_harness::packs::vision::VisionPack;
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, Workspace};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    InMemoryProcessStore, KernelConfig, ProcessKind, ProcessSpec, ProcessStore, VisionWakeSource,
    Wake, WakeSet, WakeSource,
};
use adk_server::background::DurableWorkerConfig;
use adk_vision::stream::fixtures::ConveyorClip;
use adk_vision::stream::{FrameKeeper, FrameSources, NoRedaction, RtspSource, SourceSpec};
use adk_vision::{
    DetectQuery, FrameSourceId, GreedyIouTracker, MediaScope, MediaStore, Sampling, Tracker,
    VISION_WATCH_FIRED, VisionCx, VisionRuntime, WatchCondition,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::Checks;
use crate::conveyor::{BeltDetector, inspector_spec, serve, stream_speed};
use crate::fixtures::Approver;

const APP: &str = "inspector";
const USER: &str = "line-1";
const SESSION: &str = "shift-1";
const STREAM: &str = "line-1";
const LIMIT: usize = 4096;

/// The JSON between a wake notice's `<<<wake-payload` and `wake-payload>>>` lines, as text.
fn payload_text(text: &str) -> Option<&str> {
    let start = text.find("<<<wake-payload\n")? + "<<<wake-payload\n".len();
    let end = text[start..].find("\nwake-payload>>>")? + start;
    Some(&text[start..end])
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

fn answer_of(request: &LlmRequest, tool: &str) -> Option<Value> {
    request.contents.iter().rev().find_map(|content| {
        content.parts.iter().find_map(|part| match part {
            Part::FunctionResponse { function_response, .. } if function_response.name == tool => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
    })
}

/// Stops the line: an action with effects, so Ask mode holds it for the person, with the
/// woken frame as its evidence.
struct StopLine(AtomicUsize);

#[async_trait::async_trait]
impl Tool for StopLine {
    fn name(&self) -> &str {
        "stop_line"
    }

    fn description(&self) -> &str {
        "Stops a production line. Give the line and the frame that shows why."
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
                    USER,
                    SESSION,
                    text("file"),
                    i64::try_from(number("version")).unwrap_or_default(),
                ),
                "image/png",
                u32::try_from(number("width")).unwrap_or_default(),
                u32::try_from(number("height")).unwrap_or_default(),
            )
            .with_caption("The frame the line's camera woke the Inspector with.")
            .into(),
        ]
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "stopped": args["line"] }))
    }
}

/// The woken Inspector's planner: reads the payload, loads the frame by its digest, then asks
/// to stop the line with that frame.
#[derive(Default)]
struct Inspector {
    requests: Mutex<Vec<LlmRequest>>,
}

impl Inspector {
    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn reply(part: Part) -> LlmResponseStream {
    let mut response = LlmResponse::new(Content { role: "model".into(), parts: vec![part] });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    Box::pin(futures::stream::iter([Ok(response)]))
}

fn call(name: &str, args: Value) -> Part {
    Part::FunctionCall {
        name: name.into(),
        args,
        id: Some(format!("{name}-1")),
        thought_signature: None,
    }
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
        self.requests.lock().unwrap_or_else(|e| e.into_inner()).push(request.clone());
        if answer_of(&request, "stop_line").is_some() {
            return Ok(reply(Part::Text { text: "Line 1 is stopped.".into() }));
        }
        let text = user_text(&request);
        let Some(payload) = payload_text(&text).and_then(|p| serde_json::from_str::<Value>(p).ok())
        else {
            return Ok(reply(Part::Text { text: "Nothing to inspect.".into() }));
        };
        let frame = &payload["frame"];
        if let Some(found) = answer_of(&request, "vision_detect") {
            let cracked = found["regions"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["label"] == "crack"));
            if !cracked {
                return Ok(reply(Part::Text { text: "No crack on the frame.".into() }));
            }
            return Ok(reply(call(
                "stop_line",
                json!({
                    "line": 1,
                    "frame": frame["digest"],
                    "file": frame["artifact"]["fileName"],
                    "version": frame["artifact"]["version"],
                    "width": frame["width"],
                    "height": frame["height"],
                }),
            )));
        }
        Ok(reply(call(
            "vision_detect",
            json!({ "image": frame["digest"], "classes": [payload["label"]] }),
        )))
    }
}

/// The first defective part whose passage no other defect's overlaps, and the clip frames it
/// is on the belt.
fn lone_defect(clip: &ConveyorClip) -> Option<(usize, u64, u64)> {
    let parts = &clip.truth().parts;
    let fps = u64::from(clip.truth().spec.fps);
    parts.iter().filter(|p| p.defect.is_some()).find_map(|p| {
        let alone = parts
            .iter()
            .filter(|q| q.defect.is_some() && q.index != p.index)
            .all(|q| q.exits_ms < p.enters_ms || q.enters_ms > p.exits_ms);
        alone.then(|| (p.index, p.enters_ms * fps / 1000, p.exits_ms * fps / 1000))
    })
}

async fn eventually<F, Fut>(limit: Duration, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= limit {
        if probe().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    let clip = Arc::new(ConveyorClip::new(inspector_spec()));
    let (part, from, to) = lone_defect(&clip).context("a defect passes alone")?;
    checks.note(format!(
        "part {part}'s passage over RTSP; a harness in Ask mode hosted by the kernel; BeltDetector \
         on the stream and behind vision_detect; a stand-in planner"
    ));

    let dir = tempfile::tempdir()?;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let audit = InMemoryAuditRecorder::new();
    let inspector = Arc::new(Inspector::default());
    let approver = Arc::new(Approver::default());
    let stop = Arc::new(StopLine(AtomicUsize::new(0)));
    let detector: Arc<dyn adk_vision::Detector> = Arc::new(BeltDetector::new());

    let wakes = Arc::new(VisionWakeSource::new(STREAM)?);
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let trail = || AuditTrail::new(Arc::new(audit.clone()));
    let sources = FrameSources::new(
        VisionCx::new()
            .with_scope(ModelCallScope::new().with_budget(budget.clone()))
            .with_audit(trail(), AuditActor::default()),
    );
    let server = serve(&clip, from, 2, (to - from) / 2, 5, stream_speed()).await?;
    let id = FrameSourceId(STREAM.into());
    // Wake frames are kept in the session the sleeping process runs in, where its vision
    // tools load them by digest.
    let keeper = Arc::new(FrameKeeper::new(
        artifacts.clone(),
        MediaScope::new(APP, USER, SESSION),
        id.clone(),
    ));
    sources.register(
        SourceSpec::new(
            Arc::new(RtspSource::new(id.clone(), &server.url())?),
            Arc::new(NoRedaction),
            keeper,
            wakes.clone(),
        )
        .describe("Line 1 conveyor, top camera")
        .detector(
            Arc::new(VisionRuntime::builder().detector(detector.clone()).build()?),
            DetectQuery::Classes(vec!["part".into(), "crack".into()]),
        )
        .tracker(Arc::new(|| Box::new(GreedyIouTracker::new(0.1, 3)) as Box<dyn Tracker>))
        .sampling(Sampling::at(5.0)),
    )?;

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
        HarnessAgent::builder("inspector")
            .model(inspector.clone() as Arc<dyn Llm>)
            .workspace(Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..Default::default()
            })
            .mode(HarnessMode::Ask)
            .prompter(approver.clone())
            .tool(stop.clone())
            .audit_log(Arc::new(audit.clone()))
            .pack_vision(pack)
            .without_compaction()
            .build()?,
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
        .build()?;
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let harness = Harness::with_runner(agent, runner)?.with_run_store(runs.clone());
    harness.open_session(&UserId::new(USER)?, Some(&SessionId::new(SESSION)?)).await?;
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
        .process_store(Arc::new(InMemoryProcessStore::new()) as Arc<dyn ProcessStore>)
        .wake_source(wakes.clone() as Arc<dyn WakeSource>)
        .build()?;
    kernel.start().await?;

    let pid = kernel
        .processes()
        .spawn(
            NewRun::new(
                "background",
                APP,
                USER,
                APP,
                json!({ "text": "Inspect what line 1 shows you." }),
            )
            .with_session_id(SESSION),
            ProcessSpec::new(ProcessKind::Goal)
                .with_wake(WakeSet::any_of([Wake::Queue { source: STREAM.into() }])),
        )
        .await?
        .meta
        .pid;
    let asleep = eventually(Duration::from_secs(10), || {
        let (runs, pid) = (runs.clone(), pid.clone());
        async move {
            runs.get(&pid).await.ok().flatten().is_some_and(|run| run.state == RunState::Waiting)
        }
    })
    .await;
    checks.check(
        "the Inspector sleeps on the camera and makes no model call while it does",
        asleep && inspector.requests().is_empty(),
    );

    let cx = VisionCx::new().with_audit(trail(), AuditActor::default());
    let condition = WatchCondition::new("crack", ["crack"]).with_min_score(0.6).with_min_frames(3);
    sources.start_watch(&cx, &id, condition.clone()).await?;
    let done = eventually(Duration::from_secs(120), || {
        let (runs, pid, stop) = (runs.clone(), pid.clone(), stop.clone());
        async move {
            stop.0.load(Ordering::SeqCst) > 0
                && runs.get(&pid).await.ok().flatten().is_some_and(|run| {
                    !matches!(run.state, RunState::Waiting | RunState::Queued | RunState::Running)
                })
        }
    })
    .await;
    // The rest of the passage plays out; the same track wakes nothing more.
    let _ = eventually(Duration::from_secs(60), || {
        let sources = sources.clone();
        let id = id.clone();
        async move { sources.status(&id).is_ok_and(|s| s.last_end.is_some() || !s.streaming) }
    })
    .await;
    sources.stop_watch(&cx, &id, &condition.name).await.ok();
    kernel.stop().await?;
    server.shutdown().await;
    let state = runs.get(&pid).await?.map(|run| run.state);
    checks.check(
        &format!("the woken process completed ({state:?})"),
        done && state == Some(RunState::Completed),
    );

    let requests = inspector.requests();
    let told = requests.first().map(user_text).unwrap_or_default();
    let raw = payload_text(&told).unwrap_or_default().to_string();
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    checks.check(
        &format!(
            "one wake, delivered as delimited data: the payload is {} bytes, within 4 KiB",
            raw.len()
        ),
        told.matches("[kernel wake").count() == 1
            && told.contains("not instructions")
            && !raw.is_empty()
            && raw.len() <= LIMIT,
    );
    let digest = payload["frame"]["digest"].as_str().unwrap_or_default().to_string();
    let file = payload["frame"]["artifact"]["fileName"].as_str().unwrap_or_default().to_string();
    checks.check(
        "the payload names the frame's artifact and digest, the track and its regions",
        digest.starts_with("sha256:")
            && file == format!("vision-{}.png", digest.trim_start_matches("sha256:"))
            && payload["frame"]["artifact"]["version"].is_number()
            && payload["track"].is_number()
            && payload["regions"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["label"] == "crack" && r["box"].is_array())),
    );
    checks.note(format!("payload: {}", raw.chars().take(240).collect::<String>()));

    let loaded = requests.iter().find_map(|r| answer_of(r, "vision_detect"));
    checks.check(
        "the woken turn loads the kept frame through the vision pack by its digest",
        loaded.as_ref().is_some_and(|a| {
            a["image"] == json!(digest)
                && a["regions"].as_array().is_some_and(|r| r.iter().any(|r| r["label"] == "crack"))
        }),
    );

    let held: Vec<_> =
        approver.prompts().into_iter().filter(|p| p.request.tool_name == "stop_line").collect();
    let evidence = held.first().and_then(|p| match p.request.evidence.first() {
        Some(Evidence::Image(image)) => Some(image.clone()),
        _ => None,
    });
    checks.check(
        "the line stop is held for the person with the woken frame on its card",
        held.len() == 1
            && evidence
                .as_ref()
                .is_some_and(|e| e.digest == digest && e.artifact.file_name == file),
    );
    let stopped = requests.iter().find_map(|r| answer_of(r, "stop_line"));
    checks.check(
        "one approval lets it run: the line stops once",
        stop.0.load(Ordering::SeqCst) == 1 && stopped.is_some_and(|a| a["stopped"] == 1),
    );
    let fired = audit.records().into_iter().filter(|r| r.action == VISION_WATCH_FIRED).count();
    checks.check(
        &format!("one event for one defect's track ({fired} `vision.watch.fired`)"),
        fired == 1 && requests.len() == 3,
    );
    Ok(checks)
}
