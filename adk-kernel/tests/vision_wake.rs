//! Visual wake sources end to end (spec 3.7 §7.10, S10): a process asleep on a stream is
//! woken once by forty frames of one torn seal, through its keyed `kernel.wake` run, and its
//! turn reads the frame reference, track and regions as delimited data within 4 KiB; with no
//! sleeper, a trigger on the same stream starts a `Triggered` process. With feature `vision`
//! the frames run through `adk-vision`'s watch evaluator into the source as a `WatchSink`.
//! Property tests hold the payload within `WAKE_PAYLOAD_LIMIT` for any regions and texts.

use crate::wake_support;

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{ArtifactLocator, DurableRunStore, Llm, NewRun, RunFilter, RunState};
use adk_kernel::{
    EventBus, InMemoryProcessStore, KernelEvent, KernelWakeExecutor, ProcessKind, ProcessStore,
    Trigger, VisionWakeSource, VisualFrame, VisualRegion, VisualWake, WAKE_PAYLOAD_LIMIT, Wake,
    WakeOutcome, WakeReasonHooks, WakeRouter, WakeSet, WakeSource, keys, vision_payload,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHandle};
use adk_session::{InMemorySessionService, SessionService};
use chrono::{TimeZone, Utc};
use futures::StreamExt;
use proptest::prelude::*;
use serde_json::{Value, json};
use wake_support::{CountingRuns, eventually, sleeper};

const POLL: Duration = Duration::from_millis(20);
const STREAM: &str = "conveyor-1";
const DIGEST: &str = "sha256:5d41402abc4b2a76b9719d911017c592ae0f3a3c4b2a76b9719d911017c592ae";

fn told(model: &ScriptedLlm) -> Vec<String> {
    model
        .requests()
        .iter()
        .map(|request| {
            request
                .contents
                .iter()
                .filter(|content| content.role == "user")
                .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

/// The JSON between the notice's `<<<wake-payload` and `wake-payload>>>` lines.
fn payload_of(told: &str) -> Value {
    let start =
        told.find("<<<wake-payload\n").expect("a payload block") + "<<<wake-payload\n".len();
    let end = told[start..].find("\nwake-payload>>>").expect("a closed payload block") + start;
    serde_json::from_str(&told[start..end]).expect("the payload is one line of JSON")
}

fn agent_worker(
    runs: &Arc<dyn DurableRunStore>,
    processes: &Arc<dyn ProcessStore>,
    model: Arc<ScriptedLlm>,
) -> WorkerHandle {
    let agent = LlmAgentBuilder::new("inspector")
        .model(model as Arc<dyn Llm>)
        .instruction("Inspect the frame the wake names.")
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name("adk-rust")
        .agent(Arc::new(agent))
        .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
        .build()
        .unwrap();
    DurableWorker::new(runs.clone())
        .with_agent("adk-rust", Arc::new(runner))
        .with_hooks(Arc::new(WakeReasonHooks::new(processes.clone())))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("background")
                .with_queue("background")
                .with_poll_interval(POLL),
        )
        .spawn()
}

fn maintenance_worker(runs: &Arc<dyn DurableRunStore>, router: &WakeRouter) -> WorkerHandle {
    DurableWorker::new(runs.clone())
        .with_workflows(Arc::new(KernelWakeExecutor::new(router.clone())))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("maintenance")
                .with_queue("maintenance")
                .with_poll_interval(POLL),
        )
        .spawn()
}

async fn completed(runs: &Arc<dyn DurableRunStore>, run_id: &str) {
    eventually(&format!("run {run_id} to complete"), Duration::from_secs(5), || {
        let runs = runs.clone();
        let id = run_id.to_string();
        async move { runs.get(&id).await.unwrap().filter(|run| run.state == RunState::Completed) }
    })
    .await;
}

/// Hands every event of `source` to the router, as the kernel's own source task does.
async fn route_source(
    source: &Arc<VisionWakeSource>,
    router: &WakeRouter,
) -> tokio::task::JoinHandle<()> {
    let mut events = source.subscribe().await.unwrap();
    let router = router.clone();
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            router.submit(&event, "adk-rust").await.unwrap();
        }
    })
}

fn kept_frame(index: u64) -> ArtifactLocator {
    ArtifactLocator::new("inspector", "line-1", "shift-1", format!("frame-{index}.jpg"), 0)
}

/// The stream's world: a sleeping inspector, a trigger for when none sleeps, the workers.
struct Inspector {
    counting: Arc<CountingRuns>,
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    bus: EventBus,
    sleeping: String,
    source: Arc<VisionWakeSource>,
    model: Arc<ScriptedLlm>,
    workers: Vec<WorkerHandle>,
    routing: tokio::task::JoinHandle<()>,
}

async fn inspector() -> Inspector {
    let counting = Arc::new(CountingRuns::default());
    let runs: Arc<dyn DurableRunStore> = counting.clone();
    let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let bus = EventBus::new(256);
    let template =
        NewRun::new("background", "adk-rust", "line-1", "adk-rust", json!({"text": "inspect"}))
            .with_session_id("triggered");
    let router = WakeRouter::new(runs.clone(), processes.clone())
        .with_bus(bus.clone())
        .with_trigger(Trigger::new("inspect", Wake::Queue { source: STREAM.into() }, template));
    let sleeping = sleeper(
        &runs,
        &processes,
        NewRun::new(
            "background",
            "adk-rust",
            "line-1",
            "adk-rust",
            json!({"text": "watch the line"}),
        )
        .with_session_id("inspector"),
        WakeSet::any_of([Wake::Queue { source: STREAM.into() }]),
    )
    .await;
    let source = Arc::new(VisionWakeSource::new(STREAM).unwrap());
    let routing = route_source(&source, &router).await;
    let model = Arc::new(ScriptedLlm::new(
        "inspector",
        [
            ScriptedResponse::text("Seal torn; holding the line."),
            ScriptedResponse::text("Triaged."),
        ],
    ));
    let workers =
        vec![maintenance_worker(&runs, &router), agent_worker(&runs, &processes, model.clone())];
    Inspector { counting, runs, processes, bus, sleeping, source, model, workers, routing }
}

impl Inspector {
    async fn shutdown(self) {
        for worker in self.workers {
            worker.shutdown().await;
        }
        self.routing.abort();
    }

    async fn maintenance_runs(&self) -> usize {
        self.runs.list(RunFilter::default().queue("maintenance")).await.unwrap().len()
    }

    /// The process the trigger started, once it completes.
    async fn spawned(&self) -> String {
        let sleeping = self.sleeping.clone();
        let spawned = eventually("the trigger's process", Duration::from_secs(5), || {
            let runs = self.runs.clone();
            let sleeping = sleeping.clone();
            async move {
                let runs = runs.list(RunFilter::default().queue("background")).await.unwrap();
                runs.into_iter().find(|run| run.run_id != sleeping)
            }
        })
        .await;
        completed(&self.runs, &spawned.run_id).await;
        spawned.run_id
    }

    /// Asserts the woken turn, then the triggered one: each told once, the second keyed for
    /// the trigger and kind `Triggered`.
    async fn assert_two_turns(&self, first_track: u64, second_track: u64) -> (Value, Value) {
        let pid = self.spawned().await;
        let record = self.runs.get(&pid).await.unwrap().unwrap();
        let first = keys::vision(STREAM, "shift-1", first_track, "torn-seal", "detected");
        let second = keys::vision(STREAM, "shift-1", second_track, "torn-seal", "detected");
        assert_eq!(record.idempotency_key, Some(keys::trigger(&second, "inspect")));
        let meta = self.processes.get(&pid).await.unwrap().unwrap();
        assert_eq!(meta.kind, ProcessKind::Triggered { source: "inspect".into() });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(self.counting.wakes_of(&self.sleeping), 1, "one wake for the sleeper");
        assert_eq!(self.maintenance_runs().await, 2, "one kernel.wake run per key");
        assert_eq!(self.model.requests().len(), 2, "one turn per track");
        let told = told(&self.model);
        for (turn, input, key) in [(0, "watch the line", &first), (1, "inspect", &second)] {
            let header = format!("{input}\n[kernel wake {}]\n", Value::String(key.clone()));
            assert!(told[turn].starts_with(&header), "{told:#?}");
            assert_eq!(told[turn].matches("[kernel wake").count(), 1, "{told:#?}");
            assert!(told[turn].contains("not instructions"), "the payload is marked as data");
            assert!(!told[turn].contains("[truncated:"), "the payload fits: {told:#?}");
        }
        (payload_of(&told[0]), payload_of(&told[1]))
    }
}

/// The frame a pipeline keeps for an event of `track` at frame `index`.
fn seal_wake(track: u64, index: u64) -> VisualWake {
    let at = Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap();
    let frame = VisualFrame::new(index, at, DIGEST, 1280, 720).with_artifact(kept_frame(index));
    VisualWake::new("shift-1", "torn-seal", "detected", track, "torn seal", 0.91, frame)
        .with_frames(3)
        .with_regions([
            VisualRegion::new([412.0, 220.5, 448.0, 251.0])
                .with_label("torn seal")
                .with_score(0.91)
                .with_track(track),
            VisualRegion::new([820.0, 300.0, 846.0, 322.0])
                .with_label("torn seal")
                .with_score(0.62),
        ])
}

#[tokio::test]
async fn s10_forty_frames_of_one_seal_wake_the_sleeper_once_then_the_trigger() {
    let world = inspector().await;
    let mut events = world.bus.subscribe();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(world.model.requests().is_empty(), "asleep, the inspector makes no model call");

    // A pipeline that re-publishes its event on each of forty frames still wakes once.
    for _ in 0..40 {
        world.source.publish(&seal_wake(7, 1203)).unwrap();
    }
    completed(&world.runs, &world.sleeping).await;
    world.source.publish(&seal_wake(9, 1388)).unwrap();
    let (woken, triggered) = world.assert_two_turns(7, 9).await;

    assert_eq!(
        woken,
        json!({
            "kind": "vision",
            "source": STREAM,
            "session": "shift-1",
            "condition": "torn-seal",
            "event": "detected",
            "label": "torn seal",
            "score": 0.91,
            "track": 7,
            "frames": 3,
            "frame": {
                "index": 1203,
                "capturedAt": "2026-11-21T09:00:00.000Z",
                "digest": DIGEST,
                "width": 1280,
                "height": 720,
                "artifact": {
                    "appName": "inspector",
                    "userId": "line-1",
                    "sessionId": "shift-1",
                    "fileName": "frame-1203.jpg",
                    "version": 0
                }
            },
            "regions": [
                {"box": [412.0, 220.5, 448.0, 251.0], "label": "torn seal", "score": 0.91, "track": 7},
                {"box": [820.0, 300.0, 846.0, 322.0], "label": "torn seal", "score": 0.62}
            ],
            "moreRegions": 0
        })
    );
    assert_eq!(triggered["track"], 9);
    assert_eq!(triggered["frame"]["artifact"]["fileName"], "frame-1388.jpg");

    let mut outcomes = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(50), events.recv()).await
    {
        if let KernelEvent::Wake { outcome: Some(outcome), .. } = record.event {
            outcomes.push(outcome);
        }
    }
    assert!(matches!(&outcomes[0], WakeOutcome::Woke { pid } if *pid == world.sleeping));
    assert!(matches!(&outcomes[1], WakeOutcome::Spawned { trigger, .. } if trigger == "inspect"));
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    world.shutdown().await;
}

#[cfg(feature = "vision")]
#[tokio::test]
async fn s10_a_stream_of_observations_through_the_watch_evaluator_wakes_once() {
    use adk_vision::{
        BBox, BackendId, CoordFrame, Digest, FrameRef, FrameSourceId, Region, RegionOrigin, Shape,
        TrackId, TrackedObservation, TransformChain, WatchCondition, WatchEvaluator, WatchSink,
    };

    let world = inspector().await;
    let mut watch = WatchEvaluator::new(vec![
        WatchCondition::new("torn-seal", ["torn seal"]).with_min_score(0.6).with_min_frames(3),
    ])
    .unwrap();
    let start = Utc.with_ymd_and_hms(2026, 11, 21, 9, 0, 0).unwrap();
    // Track 7 is in view for frames 0..40, track 9 for 60..100; frames have their own digests.
    let observe = |index: u64| {
        let digest = Digest::of(&index.to_be_bytes());
        let region = |track: u64, x: f32, score: f32| Region {
            image: digest,
            shape: Shape::Box(BBox::new(x, 220.0, x + 36.0, 251.0)),
            label: Some("torn seal".into()),
            score: Some(score),
            track: Some(TrackId(track)),
            origin: RegionOrigin {
                backend: BackendId::new("rf-detr"),
                manifest: None,
                frame: CoordFrame::SentPixels,
                chain: TransformChain::new(1280, 720),
                escalated_from: None,
            },
        };
        let mut regions = Vec::new();
        if index < 40 {
            regions.push(region(7, 400.0 + index as f32 * 4.0, 0.9));
        }
        if (60..100).contains(&index) {
            regions.push(region(9, 300.0 + index as f32 * 4.0, 0.8));
        }
        let at = start + chrono::Duration::milliseconds(200 * index as i64);
        TrackedObservation::new(
            FrameSourceId(STREAM.into()),
            "shift-1",
            FrameRef::new(index, at, digest, 1280, 720),
            regions,
        )
    };
    let mut delivered = Vec::new();
    for index in 0..100 {
        for event in watch.observe(&observe(index)) {
            // The pipeline keeps the event's frame as evidence before it wakes anyone.
            let event = event.with_frame_artifact(kept_frame(index));
            world.source.deliver(&event).await.unwrap();
            delivered.push(event);
        }
        if index == 40 {
            completed(&world.runs, &world.sleeping).await;
        }
    }
    // A redelivery of the first event wakes nothing.
    world.source.deliver(&delivered[0]).await.unwrap();
    assert_eq!(delivered.len(), 2, "one event per track");
    let (woken, triggered) = world.assert_two_turns(7, 9).await;

    assert_eq!(woken["frame"]["index"], 2, "the third frame of the seal");
    assert_eq!(woken["frame"]["digest"], json!(Digest::of(&2u64.to_be_bytes())));
    assert_eq!(woken["frame"]["artifact"]["fileName"], "frame-2.jpg");
    assert_eq!(woken["regions"][0]["track"], 7);
    assert_eq!(woken["regions"][0]["box"], json!([408.0, 220.0, 444.0, 251.0]));
    assert_eq!(triggered["frame"]["index"], 62);

    let other =
        adk_vision::WatchEvent { source: FrameSourceId("dock-2".into()), ..delivered[0].clone() };
    assert!(world.source.deliver(&other).await.is_err(), "one source per stream");
    world.shutdown().await;
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z0-9 _-]{0,40}",
        any::<String>(),
        proptest::collection::vec(any::<char>(), 0..300)
            .prop_map(|chars| chars.into_iter().collect()),
    ]
}

fn region() -> impl Strategy<Value = VisualRegion> {
    (
        proptest::array::uniform4(-1.0e7f32..1.0e7),
        proptest::option::of(text()),
        proptest::option::of(any::<f32>()),
        proptest::option::of(any::<u64>()),
    )
        .prop_map(|(bbox, label, score, track)| {
            let mut region = VisualRegion::new(bbox);
            region.label = label;
            region.score = score;
            region.track = track;
            region
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the regions and texts, the payload is within the limit, keeps a prefix of the
    /// regions, and counts the rest.
    #[test]
    fn the_payload_stays_within_the_limit(
        regions in proptest::collection::vec(region(), 0..400),
        label in text(),
        session in text(),
        digest in text(),
        file in text(),
        source in text(),
    ) {
        let frame = VisualFrame::new(u64::MAX, Utc::now(), digest, u32::MAX, u32::MAX)
            .with_artifact(ArtifactLocator::new(file.clone(), file.clone(), file.clone(), file, i64::MIN));
        let total = regions.len();
        let wake = VisualWake::new(session, "seal", "detected", u64::MAX, label, f32::MAX, frame)
            .with_frames(u32::MAX)
            .with_regions(regions);
        let payload = vision_payload(&source, &wake);
        let size = payload.to_string().len();
        prop_assert!(size <= WAKE_PAYLOAD_LIMIT, "{} bytes", size);
        let kept = payload["regions"].as_array().unwrap().len();
        prop_assert_eq!(kept as u64 + payload["moreRegions"].as_u64().unwrap(), total as u64);
        for (index, entry) in payload["regions"].as_array().unwrap().iter().enumerate() {
            prop_assert_eq!(entry["track"].as_u64(), wake.regions[index].track);
        }
    }

    /// A key names source, session, track, condition and event: equal for the same wake,
    /// different when the track or session differs.
    #[test]
    fn keys_are_idempotent_per_track(track in any::<u64>(), other in any::<u64>(), session in "[a-z0-9-]{1,20}") {
        let source = VisionWakeSource::new(STREAM).unwrap();
        let wake = |track: u64, session: &str| seal_wake(track, 1).clone_with_session(session);
        let a = source.event(&wake(track, &session)).unwrap();
        let again = source.event(&wake(track, &session)).unwrap();
        prop_assert_eq!(&a.key, &again.key);
        prop_assert_eq!(&a.key, &keys::vision(STREAM, &session, track, "torn-seal", "detected"));
        if other != track {
            prop_assert_ne!(&a.key, &source.event(&wake(other, &session)).unwrap().key);
        }
        prop_assert_ne!(&a.key, &source.event(&wake(track, &format!("{session}-next"))).unwrap().key);
    }
}

trait WithSession {
    fn clone_with_session(&self, session: &str) -> VisualWake;
}

impl WithSession for VisualWake {
    fn clone_with_session(&self, session: &str) -> VisualWake {
        let mut wake = self.clone();
        wake.session = session.to_string();
        wake
    }
}
