//! A stream wakes an agent (spec 3.7 §7.8, §7.10, X3): the pipeline's observations go
//! through the watch conditions, the frame of each event is kept as its wake payload, the
//! event names the kept artifact and reaches the wake sink, each detector call is metered in
//! frames, and `vision_watch` starts and stops watches on registered sources through
//! `Runner` and `LlmAgent` with a scripted planner. Every stage is real except the scripted
//! sources and the colour detector.

#![cfg(feature = "stream")]

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_artifact::InMemoryArtifactService;
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    BudgetTracker, Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    ModelCallScope, Part, RunBudget, UsageUnit,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::stream::{
    FrameKeeper, FrameSource, FrameSources, FrameStream, NoRedaction, SourceSpec, StopCause,
    StreamPipeline, StreamWatch, TrackerFactory, VISION_EVIDENCE_KEPT, VISION_SOURCE_STARTED,
    VISION_SOURCE_STOPPED, WatchChange,
};
use adk_vision::{
    BBox, DecodedImage, DetectQuery, DeterministicEditor, Frame, FramePixels, FrameSourceId,
    GreedyIouTracker, MediaScope, MediaStore, Sampling, Tracker, VISION_WATCH_FIRED,
    VISION_WATCH_REGISTERED, VISION_WATCH_STOPPED, VisionCx, VisionRuntime, VisionToolset,
    WatchCondition, WatchEvaluator, WatchEvent, WatchSink,
};
use chrono::{TimeDelta, Utc};
use futures::StreamExt;
use serde_json::{Value, json};
use support::{ColourDetector, palette};

const APP: &str = "inspector";
const USER: &str = "line-1";
const SESSION: &str = "shift-1";

fn line() -> FrameSourceId {
    FrameSourceId("line-1".into())
}

/// A white 320x240 frame with solid boxes of the named colours.
fn drawn(boxes: &[(&str, BBox)]) -> DecodedImage {
    let colours = palette();
    let (width, height) = (320u32, 240u32);
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for (label, b) in boxes {
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = ((y * width + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&colours[label]);
            }
        }
    }
    DecodedImage::from_rgba(width, height, rgba).unwrap()
}

/// A red part moving along the belt in frames 0 to 11, missed by the camera in frame 6, then
/// an empty belt to frame 19.
fn belt(index: u64) -> DecodedImage {
    match index {
        0..=11 if index != 6 => {
            let x = 20.0 + index as f32 * 8.0;
            drawn(&[("red", BBox::new(x, 100.0, x + 40.0, 140.0))])
        }
        _ => drawn(&[]),
    }
}

/// `frames` frames of `paint`, 100 ms apart, then the end of the stream; or without end
/// when `frames` is `None`, one every 5 ms of wall time.
struct Belt {
    id: FrameSourceId,
    frames: Option<u64>,
    paint: fn(u64) -> DecodedImage,
}

#[async_trait::async_trait]
impl FrameSource for Belt {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let (id, paint, frames) = (self.id.clone(), self.paint, self.frames);
        let start = Utc::now();
        Ok(futures::stream::unfold(0u64, move |index| {
            let id = id.clone();
            async move {
                match frames {
                    Some(last) if index >= last => return None,
                    Some(_) => {}
                    None => tokio::time::sleep(Duration::from_millis(5)).await,
                }
                let frame = Frame {
                    source: id,
                    index,
                    pts: Duration::from_millis(index * 100),
                    captured_at: start + TimeDelta::milliseconds(index as i64 * 100),
                    pixels: FramePixels::Ephemeral(Arc::new(paint(index))),
                };
                Some((Ok(frame), index + 1))
            }
        })
        .boxed())
    }
}

/// Keeps every event it is handed, as the kernel's `VisionWakeSource` would wake on it.
#[derive(Default)]
struct Wakes(Mutex<Vec<WatchEvent>>);

impl Wakes {
    fn events(&self) -> Vec<WatchEvent> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl WatchSink for Wakes {
    async fn deliver(&self, event: &WatchEvent) -> adk_vision::Result<()> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|r| r.action == action).collect()
}

fn detector() -> Arc<VisionRuntime> {
    Arc::new(
        VisionRuntime::builder()
            .detector(Arc::new(ColourDetector::local("colour")))
            .build()
            .unwrap(),
    )
}

fn tracker() -> TrackerFactory {
    Arc::new(|| Box::new(GreedyIouTracker::new(0.1, 2)) as Box<dyn Tracker>)
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_stream_wakes_once_per_track_with_its_kept_frame_metered_in_frames() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let keeper =
        Arc::new(FrameKeeper::new(artifacts.clone(), MediaScope::new(APP, USER, SESSION), line()));
    let wakes = Arc::new(Wakes::default());
    let condition = WatchCondition::new("red-part", ["red"]).with_min_frames(3);
    let watch = Arc::new(StreamWatch::new(
        WatchEvaluator::new(vec![condition.clone()]).unwrap(),
        keeper,
        wakes.clone(),
    ));
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let audit = InMemoryAuditRecorder::new();
    let cx = VisionCx::new()
        .with_scope(ModelCallScope::new().with_budget(budget.clone()))
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());

    let summary = StreamPipeline::builder(
        Arc::new(Belt { id: line(), frames: Some(20), paint: belt }),
        Arc::new(NoRedaction),
    )
    .sampling(Sampling::at(10.0))
    .detector(detector(), DetectQuery::Classes(vec!["red".into(), "blue".into()]))
    .tracker(tracker()())
    .sink(watch.clone())
    .build()
    .unwrap()
    .run(&cx)
    .await
    .unwrap();

    assert_eq!(summary.observations, 20);
    // One part, one event, on its third frame — the frame the camera missed did not end the
    // track, so the part did not fire again when it came back.
    let events = wakes.events();
    assert_eq!(events.len(), 1, "{events:#?}");
    assert_eq!(watch.fired(), 1);
    let event = &events[0];
    assert_eq!(event.frame.index, 2);
    assert_eq!(event.key(), format!("{}:0:red-part:detected", event.session));
    assert_eq!(event.label, "red");
    assert_eq!(event.frames, 3);

    // The event names the kept frame, stored redacted under its digest, and it loads back.
    let artifact = event.frame.artifact.clone().expect("the event's frame is kept");
    assert_eq!((artifact.app_name.as_str(), artifact.session_id.as_str()), (APP, SESSION));
    assert_eq!(artifact.file_name, format!("vision-{}.png", event.frame.digest.hex()));
    let store = MediaStore::new(artifacts);
    let kept = adk_vision::ImageRef {
        artifact: artifact.clone(),
        digest: event.frame.digest,
        mime_type: "image/png".into(),
        width: event.frame.width,
        height: event.frame.height,
        provenance: adk_vision::Provenance::Camera { source: line() },
    };
    assert_eq!(store.load(&kept).await.unwrap().digest(), event.frame.digest);

    // The records: the stream's life, the kept frame, the firing — by digest.
    assert_eq!(records(&audit, VISION_SOURCE_STARTED).len(), 1);
    assert_eq!(records(&audit, VISION_SOURCE_STOPPED).len(), 1);
    let kept_records = records(&audit, VISION_EVIDENCE_KEPT);
    assert_eq!(kept_records.len(), 1);
    assert_eq!(kept_records[0].ext["reason"], json!("wakePayload"));
    let fired = records(&audit, VISION_WATCH_FIRED);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].ext["frameDigest"], json!(event.frame.digest));
    assert_eq!(fired[0].ext["conditionDigest"], json!(condition.digest()));
    assert_eq!(fired[0].ext["artifact"], json!(artifact));
    assert_eq!(fired[0].payload_ref.as_deref(), Some(event.frame.digest.to_string().as_str()));

    // Every detector call was a frame, at zero cost on the device; none was an image.
    let units = budget.units();
    assert_eq!(units.get(UsageUnit::Frames), 20, "{units:?}");
    assert_eq!(units.get(UsageUnit::Images), 0, "{units:?}");
    assert_eq!(budget.usage().cost_micro_usd, 0);
}

fn registry_source(artifacts: &Arc<InMemoryArtifactService>, wakes: &Arc<Wakes>) -> SourceSpec {
    let keeper =
        Arc::new(FrameKeeper::new(artifacts.clone(), MediaScope::new(APP, USER, SESSION), line()));
    SourceSpec::new(
        Arc::new(Belt {
            id: line(),
            frames: None,
            paint: |_| drawn(&[("red", BBox::new(100.0, 100.0, 140.0, 140.0))]),
        }),
        Arc::new(NoRedaction),
        keeper,
        wakes.clone(),
    )
    .describe("Line 1 conveyor, top camera")
    .detector(detector(), DetectQuery::Classes(vec!["red".into(), "blue".into()]))
    .tracker(tracker())
    .sampling(Sampling::at(1000.0))
}

#[tokio::test]
async fn the_registry_streams_from_the_first_watch_to_the_last() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let wakes = Arc::new(Wakes::default());
    let audit = InMemoryAuditRecorder::new();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let cx = VisionCx::new()
        .with_scope(ModelCallScope::new().with_budget(budget.clone()))
        .with_audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default());
    let sources = FrameSources::new(cx.clone());
    sources.register(registry_source(&artifacts, &wakes)).unwrap();
    assert!(sources.register(registry_source(&artifacts, &wakes)).is_err(), "one per name");

    let status = sources.status(&line()).unwrap();
    assert!(!status.streaming, "nothing streams until a watch is registered");
    assert_eq!(status.labels, vec!["red", "blue"]);

    let red = WatchCondition::new("red-part", ["red"]).with_min_frames(2);
    let started = sources.start_watch(&cx, &line(), red.clone()).await.unwrap();
    assert_eq!(started.change, WatchChange::Registered);
    assert!(started.status.streaming);
    let again = sources.start_watch(&cx, &line(), red.clone()).await.unwrap();
    assert_eq!(again.change, WatchChange::Unchanged, "idempotent");

    // Refused: other settings under a registered name, a label the detector never reports,
    // and a source nobody registered.
    let changed = red.clone().with_min_score(0.8);
    assert!(sources.start_watch(&cx, &line(), changed).await.is_err());
    let green = WatchCondition::new("green-part", ["green"]);
    let refusal = sources.start_watch(&cx, &line(), green).await.unwrap_err().to_string();
    assert!(refusal.contains("green"), "{refusal}");
    let nowhere = FrameSourceId("dock-2".into());
    assert!(sources.start_watch(&cx, &nowhere, red.clone()).await.is_err());

    until("the red part's wake", || !wakes.events().is_empty()).await;
    // A second condition joins the running stream with its own streaks.
    let any = WatchCondition::new("any-part", Vec::<String>::new()).with_min_frames(1);
    let joined = sources.start_watch(&cx, &line(), any.clone()).await.unwrap();
    assert_eq!(joined.change, WatchChange::Registered);
    assert_eq!(joined.status.conditions, vec![red.clone(), any.clone()]);
    until("the second condition's wake", || {
        wakes.events().iter().any(|e| e.condition == "any-part")
    })
    .await;

    let stopped = sources.stop_watch(&cx, &line(), "red-part").await.unwrap();
    assert_eq!(stopped.change, WatchChange::Removed);
    assert!(stopped.status.streaming, "one watch is left");
    let gone = sources.stop_watch(&cx, &line(), "red-part").await.unwrap();
    assert_eq!(gone.change, WatchChange::NotWatched);
    let last = sources.stop_watch(&cx, &line(), "any-part").await.unwrap();
    assert_eq!(last.change, WatchChange::Removed);
    assert!(!last.status.streaming, "the last watch stops the stream");
    let end = last.status.last_end.expect("the stream's end is recorded");
    assert_eq!(end.cause, StopCause::Cancelled);
    assert!(end.summary.observations >= 2);

    // One event per (condition, track): one track, two conditions.
    let mut keys: Vec<String> = wakes.events().iter().map(WatchEvent::key).collect();
    keys.dedup();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(records(&audit, VISION_WATCH_REGISTERED).len(), 2);
    assert_eq!(records(&audit, VISION_WATCH_STOPPED).len(), 2);
    assert_eq!(records(&audit, VISION_WATCH_FIRED).len(), 2);
    assert_eq!(records(&audit, VISION_SOURCE_STARTED).len(), 1);
    let stops = records(&audit, VISION_SOURCE_STOPPED);
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].ext["cause"]["kind"], json!("cancelled"));
    assert_eq!(budget.units().get(UsageUnit::Frames), end.summary.observations);
}

/// A planner that answers each request with its next scripted step: a tool call, or text.
struct Planner {
    steps: Mutex<VecDeque<(String, Value)>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl Planner {
    fn new(steps: Vec<(&str, Value)>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().map(|(n, a)| (n.to_string(), a)).collect()),
            requests: Mutex::default(),
        }
    }
}

#[async_trait::async_trait]
impl Llm for Planner {
    fn name(&self) -> &str {
        "planner"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let part = match self.steps.lock().unwrap().pop_front() {
            Some((name, args)) if name != "text" => Part::FunctionCall {
                name,
                args,
                id: Some(format!("call-{}", self.requests.lock().unwrap().len())),
                thought_signature: None,
            },
            Some((_, text)) => Part::Text { text: text.as_str().unwrap_or_default().into() },
            None => Part::Text { text: "done".into() },
        };
        let mut response = LlmResponse::new(Content { role: "model".into(), parts: vec![part] });
        response.finish_reason = Some(FinishReason::Stop);
        response.turn_complete = true;
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

async fn turn(runner: &Runner, text: &str) -> Vec<adk_core::Event> {
    let mut stream = runner
        .run(
            adk_core::UserId::try_from(USER).unwrap(),
            adk_core::SessionId::try_from(SESSION).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

fn answers(events: &[adk_core::Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.clone())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "vision_watch" =>
            {
                Some(function_response.response)
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn vision_watch_starts_and_stops_a_watch_through_runner_and_llm_agent() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let wakes = Arc::new(Wakes::default());
    let stream_audit = InMemoryAuditRecorder::new();
    let budget = Arc::new(BudgetTracker::new(RunBudget::new()));
    let sources = FrameSources::new(
        VisionCx::new()
            .with_scope(ModelCallScope::new().with_budget(budget.clone()))
            .with_audit(AuditTrail::new(Arc::new(stream_audit.clone())), AuditActor::default()),
    );
    sources.register(registry_source(&artifacts, &wakes)).unwrap();
    let runtime = VisionRuntime::builder()
        .editor(Arc::new(DeterministicEditor::new()))
        .media_store(MediaStore::new(artifacts.clone()))
        .build()
        .unwrap();
    let toolset =
        VisionToolset::builder(Arc::new(runtime)).frame_sources(sources.clone()).build().unwrap();
    assert!(toolset.tool_names().contains(&"vision_watch"));

    let start = json!({
        "action": "start",
        "source": "line-1",
        "name": "red-part",
        "labels": ["red"],
        "minScore": 0.6,
        "minFrames": 2,
        "zone": [[0, 0], [320, 0], [320, 240], [0, 240]],
        "zoneAnchor": "bottomCenter",
        "cooldownSeconds": 30,
    });
    let planner = Arc::new(Planner::new(vec![
        ("vision_watch", json!({ "action": "list" })),
        ("vision_watch", start.clone()),
        ("text", json!("Watching line 1 for red parts.")),
        ("vision_watch", json!({ "action": "stop", "source": "line-1", "name": "red-part" })),
        ("text", json!("Stopped.")),
    ]));
    let agent = LlmAgentBuilder::new("watcher")
        .model(planner.clone() as Arc<dyn Llm>)
        .toolset(Arc::new(toolset.clone()))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let run_audit = InMemoryAuditRecorder::new();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .artifact_service(artifacts.clone())
        .audit_log(Arc::new(run_audit.clone()))
        .build()
        .unwrap();

    let first = answers(&turn(&runner, "Watch line 1 for red parts").await);
    assert_eq!(first.len(), 2, "{first:#?}");
    let listed = &first[0]["sources"][0];
    assert_eq!(listed["source"], "line-1");
    assert_eq!(listed["description"], "Line 1 conveyor, top camera");
    assert_eq!(listed["labels"], json!(["red", "blue"]));
    assert_eq!(listed["streaming"], false);
    assert_eq!(first[1]["change"], "registered");
    assert_eq!(first[1]["status"]["streaming"], true);
    let registered = &first[1]["status"]["conditions"][0];
    assert_eq!(registered["minFrames"], 2);
    assert_eq!(registered["cooldownMs"], 30_000);
    assert_eq!(registered["zone"]["anchor"], "bottomCenter");

    until("the watch to wake", || !wakes.events().is_empty()).await;
    let event = wakes.events()[0].clone();
    assert_eq!(event.condition, "red-part");
    assert!(event.frame.artifact.is_some());

    let second = answers(&turn(&runner, "Stop watching").await);
    assert_eq!(second.len(), 1, "{second:#?}");
    assert_eq!(second[0]["change"], "removed");
    assert_eq!(second[0]["status"]["streaming"], false);
    assert_eq!(second[0]["status"]["lastEnd"]["cause"]["kind"], "cancelled");

    // Registering and stopping are on the run's trail with the acting agent; the stream's
    // records are on the registry's.
    let registered = records(&run_audit, VISION_WATCH_REGISTERED);
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].ext["source"], json!("line-1"));
    assert_eq!(
        registered[0].actor.agent.as_ref().map(ToString::to_string).as_deref(),
        Some("watcher")
    );
    assert_eq!(records(&run_audit, VISION_WATCH_STOPPED).len(), 1);
    assert_eq!(records(&stream_audit, VISION_SOURCE_STARTED).len(), 1);
    assert_eq!(records(&stream_audit, VISION_SOURCE_STOPPED).len(), 1);
    assert_eq!(records(&stream_audit, VISION_WATCH_FIRED).len(), 1);
    assert!(budget.units().get(UsageUnit::Frames) >= 2);
}
