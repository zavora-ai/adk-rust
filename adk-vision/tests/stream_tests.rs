//! The stream pipeline (spec 3.7 §7.13): the frame governor (S11), the privacy step before
//! detection and storage (S18, V-I9), retention (Q13), tracking, the kill switch (V-I17)
//! and the `vision.*` records, over scripted sources.

#![cfg(feature = "stream")]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest};
use adk_core::GovernanceControl;
use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_vision::stream::{
    EscalationRefusal, EscalationVerdict, FrameBudget, FrameKeeper, FramePrivacy, FrameSource,
    FrameStream, GovernorChange, KeepReason, NoRedaction, Observation, ObservationSink,
    PrivacyHook, PurgeCause, RetentionPolicy, SourceEvent, StopCause, StreamPipeline,
    VISION_EVIDENCE_KEPT, VISION_EVIDENCE_PURGED, VISION_GOVERNOR_STEP, VISION_SOURCE_PAUSED,
    VISION_SOURCE_RESUMED, VISION_SOURCE_STARTED, VISION_SOURCE_STOPPED,
};
use adk_vision::{
    BBox, DecodedImage, DetectQuery, Frame, FrameMeta, FramePixels, FrameSourceId,
    GreedyIouTracker, InputLimits, MediaScope, MediaStore, Sampling, VisionCx, VisionError,
    VisionRuntime,
};
use chrono::{DateTime, TimeDelta, Utc};
use futures::StreamExt;
use support::{ColourDetector, find_colour, palette};
use tokio::sync::mpsc;

fn source_id() -> FrameSourceId {
    FrameSourceId("line-1".into())
}

/// A white frame with solid boxes of the named colours.
fn boxes(width: u32, height: u32, drawn: &[(&str, BBox)]) -> DecodedImage {
    let colours = palette();
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    for (label, b) in drawn {
        for y in b.y0 as u32..b.y1 as u32 {
            for x in b.x0 as u32..b.x1 as u32 {
                let i = ((y * width + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&colours[label]);
            }
        }
    }
    DecodedImage::from_rgba(width, height, rgba).unwrap()
}

fn frame(index: u64, pts: Duration, captured_at: DateTime<Utc>, pixels: DecodedImage) -> Frame {
    Frame {
        source: source_id(),
        index,
        pts,
        captured_at,
        pixels: FramePixels::Ephemeral(Arc::new(pixels)),
    }
}

/// Frames handed in up front.
struct Scripted {
    id: FrameSourceId,
    frames: Mutex<Option<Vec<Frame>>>,
}

impl Scripted {
    fn new(frames: Vec<Frame>) -> Arc<Self> {
        Arc::new(Self { id: source_id(), frames: Mutex::new(Some(frames)) })
    }
}

#[async_trait::async_trait]
impl FrameSource for Scripted {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }
    async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let frames = self.frames.lock().unwrap().take().unwrap_or_default();
        Ok(futures::stream::iter(frames.into_iter().map(Ok)).boxed())
    }
}

/// Frames sent one at a time through a channel; counts how often the pipeline asks for the
/// next one, which it does only after it has finished the frame before.
struct Channel {
    id: FrameSourceId,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Frame>>>,
    polls: Arc<AtomicUsize>,
}

impl Channel {
    fn new() -> (Arc<Self>, mpsc::UnboundedSender<Frame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Arc::new(Self { id: source_id(), rx: Mutex::new(Some(rx)), polls: Arc::default() }), tx)
    }
}

#[async_trait::async_trait]
impl FrameSource for Channel {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }
    async fn frames(&self, _sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let rx = self.rx.lock().unwrap().take().expect("one stream");
        let polls = Arc::clone(&self.polls);
        Ok(futures::stream::unfold(rx, move |mut rx| {
            polls.fetch_add(1, Ordering::SeqCst);
            async move { rx.recv().await.map(|f| (Ok(f), rx)) }
        })
        .boxed())
    }
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Keeps every observation and event.
#[derive(Default)]
struct Collect {
    observations: Mutex<Vec<Observation>>,
    events: Mutex<Vec<SourceEvent>>,
}

#[async_trait::async_trait]
impl ObservationSink for Collect {
    async fn observe(&self, _cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        self.observations.lock().unwrap().push(observation);
        Ok(())
    }
    async fn source_event(&self, _cx: &VisionCx, event: &SourceEvent) -> adk_vision::Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn audited() -> (InMemoryAuditRecorder, VisionCx) {
    let recorder = InMemoryAuditRecorder::new();
    let cx = VisionCx::new()
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())), AuditActor::default());
    (recorder, cx)
}

fn actions(recorder: &InMemoryAuditRecorder) -> Vec<String> {
    recorder.records().into_iter().map(|r| r.action).collect()
}

fn records(recorder: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    recorder.records().into_iter().filter(|r| r.action == action).collect()
}

fn change_kind(record: &AuditRecordDraft) -> String {
    record.ext["change"]["kind"].as_str().unwrap().to_string()
}

/// A period boundary: 1,000,000 whole minutes after the epoch.
fn period_start() -> DateTime<Utc> {
    DateTime::from_timestamp(60 * 1_000_000, 0).unwrap()
}

#[tokio::test]
async fn s11_the_governor_lowers_the_rate_before_it_pauses_and_resumes_next_period() {
    // Two one-minute periods of a 5 fps camera against 50 frames a minute: 1 fps cannot last
    // the minute either, so the rate drops to the floor, escalation stops, and the source
    // pauses at its 50th frame; the next minute resumes it.
    let start = period_start();
    let frames: Vec<Frame> = (0..600u64)
        .map(|i| {
            let pts = Duration::from_millis(i * 200);
            frame(
                i,
                pts,
                start + TimeDelta::milliseconds(i as i64 * 200),
                DecodedImage::filled(16, 16, [9; 4]),
            )
        })
        .collect();
    let budget =
        FrameBudget::new(5.0).with_min_local_fps(1.0).with_local_quota(50).with_period_secs(60);
    let sink = Arc::new(Collect::default());
    let (recorder, cx) = audited();
    let summary = StreamPipeline::builder(Scripted::new(frames), Arc::new(NoRedaction))
        .budget(budget)
        .sink(sink.clone())
        .build()
        .unwrap()
        .run(&cx)
        .await
        .unwrap();

    assert_eq!(summary.received, 600);
    assert_eq!(summary.observations, 100, "50 a period");
    assert_eq!(summary.paused, 2 * 54, "49.2 s to 59.8 s of each minute");
    assert_eq!(summary.received, summary.observations + summary.thinned + summary.paused);

    let steps: Vec<String> =
        records(&recorder, VISION_GOVERNOR_STEP).iter().map(change_kind).collect();
    assert_eq!(
        steps,
        [
            "rateLowered",
            "escalationStopped",
            "paused",
            "resumed",
            "rateRestored",
            "escalationResumed",
            "rateLowered",
            "escalationStopped",
            "paused",
        ]
    );
    let lowered = &records(&recorder, VISION_GOVERNOR_STEP)[0];
    assert_eq!(lowered.ext["change"]["fromFps"], 5.0);
    assert_eq!(lowered.ext["change"]["toFps"], 1.0);
    assert_eq!(lowered.ext["quota"]["localQuota"], 50);
    assert_eq!(records(&recorder, VISION_SOURCE_PAUSED).len(), 2);
    assert_eq!(records(&recorder, VISION_SOURCE_RESUMED).len(), 1);
    let all = actions(&recorder);
    assert_eq!(all.first().map(String::as_str), Some(VISION_SOURCE_STARTED));
    assert_eq!(all.last().map(String::as_str), Some(VISION_SOURCE_STOPPED));
    // Every observation's spacing honours the rate it was admitted at: 1 fps.
    let observations = sink.observations.lock().unwrap();
    for pair in observations.windows(2) {
        let gap = pair[1].frame.captured_at - pair[0].frame.captured_at;
        assert!(gap >= TimeDelta::milliseconds(875), "{gap}");
    }
    // The sink saw the same story as the trail.
    let events = sink.events.lock().unwrap();
    assert!(matches!(events.first(), Some(SourceEvent::Started { .. })));
    assert!(matches!(events.last(), Some(SourceEvent::Stopped { cause: StopCause::Ended })));
    assert_eq!(events.iter().filter(|e| matches!(e, SourceEvent::Governor { .. })).count(), 9);
}

#[tokio::test]
async fn s11_escalation_stops_at_its_quota_bills_at_least_fifty_frames_and_resumes_next_period() {
    let budget = FrameBudget::new(5.0)
        .with_escalations_per_minute(10)
        .with_escalation_quota(120)
        .with_period_secs(3_600);
    let pipeline = StreamPipeline::builder(Scripted::new(Vec::new()), Arc::new(NoRedaction))
        .budget(budget)
        .build()
        .unwrap();
    let governor = pipeline.governor().unwrap();
    let (recorder, cx) = audited();
    let at = DateTime::from_timestamp(3_600 * 500_000, 0).unwrap();
    // A 12-frame track is billed the 50-frame minimum.
    assert_eq!(
        governor.request_escalation_at(&cx, at, 12).await.unwrap(),
        EscalationVerdict::Granted { billed_frames: 50 }
    );
    assert_eq!(
        governor.request_escalation_at(&cx, at + TimeDelta::seconds(5), 30).await.unwrap(),
        EscalationVerdict::Granted { billed_frames: 50 }
    );
    assert_eq!(governor.state().escalated_frames, 100);
    // 20 frames left cannot cover a minimum request: escalation stops, and says so.
    assert!(matches!(
        governor.request_escalation_at(&cx, at + TimeDelta::seconds(10), 5).await.unwrap(),
        EscalationVerdict::Refused { refusal: EscalationRefusal::Stopped { .. } }
    ));
    assert_eq!(
        records(&recorder, VISION_GOVERNOR_STEP).iter().map(change_kind).collect::<Vec<_>>(),
        ["escalationStopped"]
    );
    // The next hour resumes it.
    assert_eq!(
        governor.request_escalation_at(&cx, at + TimeDelta::hours(1), 50).await.unwrap(),
        EscalationVerdict::Granted { billed_frames: 50 }
    );
    let steps: Vec<String> =
        records(&recorder, VISION_GOVERNOR_STEP).iter().map(change_kind).collect();
    assert_eq!(steps, ["escalationStopped", "escalationResumed"]);
}

#[tokio::test]
async fn the_kill_switch_pauses_the_stream_at_its_next_frame_and_resumes_after_the_thaw() {
    let (source, tx) = Channel::new();
    let polls = Arc::clone(&source.polls);
    let control = GovernanceControl::new();
    let sink = Arc::new(Collect::default());
    let (recorder, cx) = audited();
    let cx = cx.with_governance(control.clone());
    let run = tokio::spawn({
        let sink = sink.clone();
        async move {
            StreamPipeline::builder(source, Arc::new(NoRedaction))
                .sampling(Sampling { fps: 100.0, max_long_edge: None })
                .sink(sink)
                .build()
                .unwrap()
                .run(&cx)
                .await
        }
    });
    let send = |i: u64| {
        tx.send(frame(i, Duration::from_secs(i), Utc::now(), DecodedImage::filled(4, 4, [0; 4])))
            .unwrap()
    };
    send(0);
    until("frame 0", || sink.observations.lock().unwrap().len() == 1).await;
    control.freeze("incident 7");
    send(1);
    send(2);
    // The pipeline asks for a fourth frame only once it has dropped frames 1 and 2.
    until("frames 1 and 2", || polls.load(Ordering::SeqCst) >= 4).await;
    control.unfreeze();
    send(3);
    until("frame 3", || sink.observations.lock().unwrap().len() == 2).await;
    drop(tx);
    let summary = run.await.unwrap().unwrap();
    assert_eq!(summary.frozen, 2);
    assert_eq!(summary.observations, 2);
    let seen: Vec<u64> = sink.observations.lock().unwrap().iter().map(|o| o.frame.index).collect();
    assert_eq!(seen, vec![0, 3]);
    let paused = records(&recorder, VISION_SOURCE_PAUSED);
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].ext["reason"], "frozen");
    assert_eq!(records(&recorder, VISION_SOURCE_RESUMED)[0].ext["reason"], "thawed");
    let events = sink.events.lock().unwrap();
    assert!(events.contains(&SourceEvent::Frozen) && events.contains(&SourceEvent::Thawed));
}

#[tokio::test]
async fn a_frozen_control_refuses_the_run_before_any_frame() {
    let control = GovernanceControl::new();
    control.freeze("maintenance");
    let cx = VisionCx::new().with_governance(control);
    let err = StreamPipeline::builder(Scripted::new(Vec::new()), Arc::new(NoRedaction))
        .build()
        .unwrap()
        .run(&cx)
        .await
        .unwrap_err();
    assert_eq!(err.category(), adk_core::ErrorCategory::Forbidden, "{err}");
}

/// Blacks out the left half of every frame.
struct LeftHalf;

#[async_trait::async_trait]
impl PrivacyHook for LeftHalf {
    fn name(&self) -> &str {
        "left-half"
    }
    async fn redact(
        &self,
        _cx: &VisionCx,
        _frame: &FrameMeta,
        pixels: &mut DecodedImage,
    ) -> adk_vision::Result<Vec<BBox>> {
        let (w, h) = pixels.size();
        let mut rgba = pixels.rgba().to_vec();
        for y in 0..h {
            for x in 0..w / 2 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        *pixels = DecodedImage::from_rgba(w, h, rgba)?;
        Ok(vec![BBox::new(0.0, 0.0, (w / 2) as f32, h as f32)])
    }
}

#[tokio::test]
async fn s18_every_frame_is_redacted_before_detection_and_before_it_is_kept() {
    // A red box on the left (to be redacted) and a blue box on the right.
    let drawn = boxes(
        64,
        48,
        &[("red", BBox::new(4.0, 4.0, 20.0, 20.0)), ("blue", BBox::new(40.0, 20.0, 56.0, 36.0))],
    );
    let detector = Arc::new(ColourDetector::local("colours"));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector.clone()).build().unwrap());
    let sink = Arc::new(Collect::default());
    let (recorder, cx) = audited();
    StreamPipeline::builder(
        Scripted::new(vec![frame(0, Duration::ZERO, Utc::now(), drawn)]),
        Arc::new(LeftHalf),
    )
    .detector(runtime, DetectQuery::Classes(vec!["red".into(), "blue".into()]))
    .sink(sink.clone())
    .build()
    .unwrap()
    .run(&cx)
    .await
    .unwrap();
    let observation = sink.observations.lock().unwrap().pop().unwrap();
    // The detector saw only the redacted frame: the red box is gone.
    let labels: Vec<_> = observation.regions.iter().filter_map(|r| r.label.clone()).collect();
    assert_eq!(labels, vec!["blue".to_string()]);
    assert_eq!(observation.regions[0].image, observation.digest);
    assert_eq!(observation.image.hook(), "left-half");

    // What is kept is the redacted frame, under the digest the regions name.
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let keeper =
        FrameKeeper::new(artifacts.clone(), MediaScope::new("inspector", "u1", "s1"), source_id());
    let kept = keeper.keep(&cx, &observation.image, KeepReason::CardEvidence).await.unwrap();
    assert_eq!(kept.image.digest, observation.digest);
    assert_eq!(kept.privacy, "left-half");
    let stored = MediaStore::new(artifacts).load(&kept.image).await.unwrap();
    assert_eq!(stored.pixels().pixel(10, 10), [0, 0, 0, 255], "the red box was never written");
    assert!(find_colour(stored.pixels(), palette()["red"]).is_none());
    let record = &records(&recorder, VISION_EVIDENCE_KEPT)[0];
    assert_eq!(record.payload_ref.as_deref(), Some(observation.digest.to_string().as_str()));
    assert_eq!(record.ext["privacy"], "left-half");
    assert_eq!(record.ext["redacted"], 1);
    assert_eq!(record.ext["retentionSecs"], 30 * 86_400);
}

/// Fails on odd frames.
struct FlakyHook;

#[async_trait::async_trait]
impl PrivacyHook for FlakyHook {
    fn name(&self) -> &str {
        "flaky"
    }
    async fn redact(
        &self,
        _cx: &VisionCx,
        frame: &FrameMeta,
        _pixels: &mut DecodedImage,
    ) -> adk_vision::Result<Vec<BBox>> {
        if frame.index % 2 == 1 {
            return Err(VisionError::Unavailable {
                backend: "face-detector".into(),
                message: "weights missing".into(),
            });
        }
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_failing_privacy_hook_drops_its_frame_and_nothing_downstream_sees_it() {
    let frames = (0..6u64)
        .map(|i| frame(i, Duration::from_secs(i), Utc::now(), DecodedImage::filled(4, 4, [1; 4])))
        .collect();
    let sink = Arc::new(Collect::default());
    let summary = StreamPipeline::builder(Scripted::new(frames), Arc::new(FlakyHook))
        .sink(sink.clone())
        .build()
        .unwrap()
        .run(&VisionCx::new())
        .await
        .unwrap();
    assert_eq!(summary.privacy_failures, 3);
    let seen: Vec<u64> = sink.observations.lock().unwrap().iter().map(|o| o.frame.index).collect();
    assert_eq!(seen, vec![0, 2, 4]);
}

#[tokio::test]
async fn regions_are_in_source_pixels_and_track_ids_reset_with_each_session() {
    // A blue box moving right; the detector takes at most 64 px, so every frame is downscaled.
    let moving = |i: u64| {
        let x = 20.0 + i as f32 * 8.0;
        boxes(320, 160, &[("blue", BBox::new(x, 40.0, x + 60.0, 100.0))])
    };
    let detector = Arc::new(ColourDetector::with(
        adk_vision::BackendDescriptor::new(
            "small",
            adk_vision::Locality::InProcess,
            adk_vision::CoordFrame::SentPixels,
        )
        .with_limits(InputLimits::long_edge(64)),
    ));
    let runtime = Arc::new(VisionRuntime::builder().detector(detector).build().unwrap());
    for session in 0..2 {
        let frames =
            (0..5u64).map(|i| frame(i, Duration::from_secs(i), Utc::now(), moving(i))).collect();
        let sink = Arc::new(Collect::default());
        StreamPipeline::builder(Scripted::new(frames), Arc::new(NoRedaction))
            .detector(runtime.clone(), DetectQuery::Classes(vec!["blue".into()]))
            .tracker(Box::new(GreedyIouTracker::new(0.3, 2)))
            .sink(sink.clone())
            .build()
            .unwrap()
            .run(&VisionCx::new())
            .await
            .unwrap();
        let observations = sink.observations.lock().unwrap();
        for (i, observation) in observations.iter().enumerate() {
            let region = &observation.regions[0];
            let bbox = region.shape.bbox().unwrap();
            let x = 20.0 + i as f32 * 8.0;
            assert!(
                (bbox.x0 - x).abs() <= 6.0 && (bbox.x1 - (x + 60.0)).abs() <= 6.0,
                "session {session} frame {i}: {bbox:?}"
            );
            assert!(!region.origin.chain.is_identity(), "the downscale is on record");
            assert_eq!(
                region.track.map(|t| t.0),
                Some(0),
                "one anonymous track, restarted each session"
            );
        }
    }
}

#[tokio::test]
async fn cancellation_stops_the_stream_and_records_why() {
    let (source, _tx) = Channel::new();
    let token = tokio_util::sync::CancellationToken::new();
    let (recorder, cx) = audited();
    let cx = cx.with_cancellation(token.clone());
    let run = tokio::spawn(async move {
        StreamPipeline::builder(source, Arc::new(NoRedaction)).build().unwrap().run(&cx).await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    token.cancel();
    run.await.unwrap().unwrap();
    let stopped = &records(&recorder, VISION_SOURCE_STOPPED)[0];
    assert_eq!(stopped.ext["cause"]["kind"], "cancelled");
}

async fn redacted(index: u64, shade: u8) -> adk_vision::stream::RedactedFrame {
    let frame = frame(
        index,
        Duration::from_secs(index),
        Utc::now(),
        DecodedImage::filled(8, 8, [shade, shade, shade, 255]),
    );
    FramePrivacy::new(Arc::new(NoRedaction)).apply(&VisionCx::new(), frame).await.unwrap()
}

#[tokio::test]
async fn kept_frames_expire_on_schedule_survive_a_restart_and_erase_by_scope() {
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let scope = MediaScope::new("inspector", "u1", "s1");
    let policy = RetentionPolicy::default()
        .with_reason(KeepReason::PersonKept, Duration::from_secs(90 * 86_400));
    let keeper =
        FrameKeeper::new(artifacts.clone(), scope.clone(), source_id()).with_policy(policy);
    let (recorder, cx) = audited();
    let t0 = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let wake = redacted(1, 10).await;
    let sample = redacted(2, 200).await;

    // One frame kept for a wake and, a day later, for a card: one artifact, both reasons.
    let first = keeper.keep_at(&cx, &wake, KeepReason::WakePayload, t0).await.unwrap();
    let again = keeper
        .keep_at(&cx, &wake, KeepReason::CardEvidence, t0 + TimeDelta::days(1))
        .await
        .unwrap();
    assert_eq!(again.image, first.image);
    assert_eq!(again.reasons, vec![KeepReason::WakePayload, KeepReason::CardEvidence]);
    assert_eq!(again.expires_at, t0 + TimeDelta::days(31));
    let kept_sample = keeper.keep_at(&cx, &sample, KeepReason::PersonKept, t0).await.unwrap();
    assert_eq!(keeper.kept().await.unwrap().len(), 2);

    // Day 30: nothing is due yet. Day 31: the evidence frame goes, the person's sample stays.
    assert!(keeper.purge_expired(&cx, t0 + TimeDelta::days(30)).await.unwrap().is_empty());
    let purged = keeper.purge_expired(&cx, t0 + TimeDelta::days(31)).await.unwrap();
    assert_eq!(purged.len(), 1);
    assert_eq!(purged[0].entry.image.digest, first.image.digest);
    assert_eq!(purged[0].cause, PurgeCause::Expired);
    let at = &first.image.artifact;
    let gone = artifacts
        .load(LoadRequest {
            app_name: at.app_name.clone(),
            user_id: at.user_id.clone(),
            session_id: at.session_id.clone(),
            file_name: at.file_name.clone(),
            version: None,
        })
        .await;
    assert!(gone.is_err(), "the purged frame's artifact is deleted");

    // A new keeper over the same store finds what the first one kept.
    let restarted = FrameKeeper::new(artifacts.clone(), scope, source_id());
    let left = restarted.kept().await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].image, kept_sample.image);
    let erased = restarted.erase(&cx).await.unwrap();
    assert_eq!(erased.len(), 1);
    assert_eq!(erased[0].cause, PurgeCause::Erased);
    assert!(restarted.kept().await.unwrap().is_empty());

    assert_eq!(records(&recorder, VISION_EVIDENCE_KEPT).len(), 3);
    let purges = records(&recorder, VISION_EVIDENCE_PURGED);
    assert_eq!(
        purges.iter().map(|r| r.ext["cause"].as_str().unwrap()).collect::<Vec<_>>(),
        ["expired", "erased"]
    );
    // Records carry digests, never pixels (V-I15).
    for record in recorder.records() {
        let text = serde_json::to_string(&record.ext).unwrap();
        assert!(text.len() < 2_000, "{} is {} bytes", record.action, text.len());
    }
}

#[tokio::test]
async fn the_keeper_refuses_a_frame_of_another_source() {
    let keeper = FrameKeeper::new(
        Arc::new(InMemoryArtifactService::new()),
        MediaScope::new("inspector", "u1", "s1"),
        FrameSourceId("line-2".into()),
    );
    let err = keeper
        .keep(&VisionCx::new(), &redacted(0, 5).await, KeepReason::PersonKept)
        .await
        .unwrap_err();
    assert!(matches!(err, VisionError::InvalidInput(_)), "{err}");
}

#[tokio::test]
async fn governor_steps_reach_the_sink_and_the_trail_in_the_same_order() {
    let start = period_start();
    let frames = (0..20u64)
        .map(|i| {
            frame(
                i,
                Duration::from_secs(i),
                start + TimeDelta::seconds(i as i64),
                DecodedImage::filled(4, 4, [3; 4]),
            )
        })
        .collect();
    let sink = Arc::new(Collect::default());
    let (recorder, cx) = audited();
    StreamPipeline::builder(Scripted::new(frames), Arc::new(NoRedaction))
        .budget(
            FrameBudget::new(1.0).with_min_local_fps(1.0).with_local_quota(5).with_period_secs(60),
        )
        .sink(sink.clone())
        .build()
        .unwrap()
        .run(&cx)
        .await
        .unwrap();
    let from_sink: Vec<GovernorChange> = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SourceEvent::Governor { step } => Some(step.change.clone()),
            _ => None,
        })
        .collect();
    let from_trail: Vec<GovernorChange> = records(&recorder, VISION_GOVERNOR_STEP)
        .iter()
        .map(|r| serde_json::from_value(r.ext["change"].clone()).unwrap())
        .collect();
    assert_eq!(from_sink, from_trail);
    // The floor is the rate: nothing to lower, so escalation stops and then the source pauses.
    assert!(matches!(
        from_sink.as_slice(),
        [GovernorChange::EscalationStopped { .. }, GovernorChange::Paused { .. }]
    ));
}

/// Holds watch conditions against each observation, keeps each event's frame as its wake
/// payload, and collects the events with the frame's artifact attached.
struct Watch {
    evaluator: Mutex<adk_vision::WatchEvaluator>,
    keeper: FrameKeeper,
    events: Mutex<Vec<adk_vision::WatchEvent>>,
}

#[async_trait::async_trait]
impl ObservationSink for Watch {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        let tracked = observation.tracked();
        assert_eq!(tracked.frame.digest, observation.digest);
        assert_eq!((tracked.frame.width, tracked.frame.height), (320, 160));
        let events = self.evaluator.lock().unwrap().observe(&tracked);
        for event in events {
            let kept = self.keeper.keep(cx, &observation.image, KeepReason::WakePayload).await?;
            self.events.lock().unwrap().push(event.with_frame_artifact(kept.image.artifact));
        }
        Ok(())
    }
}

#[tokio::test]
async fn observations_feed_watch_conditions_and_the_wake_frame_is_kept_under_their_digest() {
    let frames = (0..6u64)
        .map(|i| {
            let x = 20.0 + i as f32 * 8.0;
            frame(
                i,
                Duration::from_secs(i),
                Utc::now(),
                boxes(320, 160, &[("blue", BBox::new(x, 40.0, x + 60.0, 100.0))]),
            )
        })
        .collect();
    let runtime = Arc::new(
        VisionRuntime::builder()
            .detector(Arc::new(ColourDetector::local("colours")))
            .build()
            .unwrap(),
    );
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let watch = Arc::new(Watch {
        evaluator: Mutex::new(
            adk_vision::WatchEvaluator::new(vec![
                adk_vision::WatchCondition::new("blue-part", ["blue"]).with_min_frames(3),
            ])
            .unwrap(),
        ),
        keeper: FrameKeeper::new(
            artifacts.clone(),
            MediaScope::new("inspector", "u1", "s1"),
            source_id(),
        ),
        events: Mutex::default(),
    });
    let (recorder, cx) = audited();
    StreamPipeline::builder(Scripted::new(frames), Arc::new(NoRedaction))
        .detector(runtime, DetectQuery::Classes(vec!["blue".into()]))
        .tracker(Box::new(GreedyIouTracker::new(0.3, 2)))
        .sink(watch.clone())
        .build()
        .unwrap()
        .run(&cx)
        .await
        .unwrap();
    let events = watch.events.lock().unwrap().clone();
    assert_eq!(events.len(), 1, "one track, one event");
    let event = &events[0];
    assert_eq!(event.frame.index, 2);
    let session =
        records(&recorder, VISION_SOURCE_STARTED)[0].ext["session"].as_str().unwrap().to_string();
    assert_eq!(event.session, session, "the event's session is the run's");
    let artifact = event.frame.artifact.clone().expect("the wake frame is kept");
    let kept = watch.keeper.kept().await.unwrap();
    assert_eq!(kept[0].image.artifact, artifact);
    assert_eq!(
        kept[0].image.digest, event.frame.digest,
        "stored under the digest the regions name"
    );
    assert_eq!(kept[0].reasons, vec![KeepReason::WakePayload]);
}
