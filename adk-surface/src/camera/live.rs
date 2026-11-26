//! The live camera surface on the frame pipeline (spec 3.7 §7.13, §8.2, S12; feature
//! `vision-camera`).
//!
//! [`PipelineCameras`] runs each camera through adk-vision's
//! [`StreamPipeline`](adk_vision::stream::StreamPipeline) — source, frame governor, privacy,
//! detector, tracker — and is the [`CameraSource`] the hub reads: what the surface shows is what
//! the pipeline measured.
//!
//! | The surface shows | From |
//! |---|---|
//! | The picture and its overlays | Each observation's frame, after the camera's privacy hook and, with [`LiveCamera::cover_people`], everyone the detector calls a person covered, with its tracked regions; offered to the hub, whose gate drops it while the camera is off, stopped or paused by Stop all |
//! | "Looking", "Frames today", the steps | The pipeline's [`GovernorHandle`] and the `vision.governor.step`s it took |
//! | "Asking sam-3.1" | Escalations granted through [`PipelineCameras::escalate`] |
//! | "Tracks now" | The tracker's anonymous identifiers on the newest frame (V-I8) |
//! | "Watching for", zones | Each camera's [`WatchCondition`]s, held against every frame here; a zone a person edits becomes the conditions' zone from the next frame |
//! | "Can't reach the camera" | The source failing, retried with a growing wait or at once from "Try again now" |
//!
//! Switching a stream camera off stops capture at its source: the stream is dropped — a
//! [`DeviceSource`](adk_vision::stream::DeviceSource)'s ffmpeg exits, an RTSP connection
//! closes — recorded as `vision.source.stopped`, and on opens it again (`vision.source.started`).
//! Stop all closes every stream camera's source the same way (reason `stopAll`), and Resume all
//! opens again each one that is on (`resumeAll`), so a freeze leaves no camera capturing
//! (spec 3.7 V-I17).
//!
//! The web home's own camera is a [`LiveCamera::surface`]: frames the voice screen or the camera
//! page ("Use this browser's camera") capture reach the hub through [`Cameras::ingest`], which
//! drops them while the camera is off, and enter the pipeline through adk-vision's
//! [`SurfaceCameraSource`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use adk_artifact::ArtifactService;
use adk_core::ImageEvidence;
use adk_core::audit_log::{
    AuditActor, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, AuditTrail,
};
use adk_vision::stream::{
    CameraFeed, EscalationVerdict, FrameBudget, FrameKeeper, FramePrivacy, FrameSource,
    FrameStream, GovernorChange, GovernorHandle, KeepReason, NoRedaction, Observation,
    ObservationSink, PrivacyHook, RedactedFrame, SourceEvent, StreamPipeline, SurfaceCameraSource,
    VISION_SOURCE_STARTED, VISION_SOURCE_STOPPED,
};
use adk_vision::{
    BBox, CallOptions, DecodeLimits, DecodedImage, DetectQuery, Frame, FrameMeta, FramePixels,
    FrameSourceId, MediaScope, Region, Sampling, Shape, Tracker, VisionCx, VisionError,
    VisionRuntime, WatchCondition, WatchEvaluator, WatchSink, Zone, watch_registered_draft,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use futures::StreamExt;
use serde_json::json;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::facts::{
    CameraFacts, EscalationFacts, Finding, GovernorFacts, GovernorStep, GovernorStepKind,
    HeldFrame, Overlay, StreamState, TrackFacts, TrackPhase, WatchFacts, ZoneFacts,
};
use super::hub::{CameraSource, Cameras, FrameIntake, WeakCameras};
use crate::error::{Result, SurfaceError};

/// How good a shown frame's JPEG is.
const SHOWN_QUALITY: u8 = 82;

/// The labels a frame's people go by: covered before a frame is shown or kept under
/// [`LiveCamera::cover_people`], the whole box of every detected person.
const PEOPLE: [&str; 2] = ["person", "face"];

/// How long a camera waits before trying a lost source again, at first and at most.
const RETRY_FIRST: StdDuration = StdDuration::from_secs(2);
const RETRY_MOST: StdDuration = StdDuration::from_secs(30);

/// How long switching a stream camera off waits for its source to close before answering.
const STOP_WAIT: StdDuration = StdDuration::from_secs(3);

fn vision(error: VisionError) -> SurfaceError {
    SurfaceError::Source(error.into())
}

/// A watch condition on a live camera (spec 3.7 §7.10), as the surface names it.
///
/// # Example
///
/// ```rust
/// use adk_surface::camera::LiveWatch;
/// use adk_vision::WatchCondition;
///
/// let watch = LiveWatch::new(
///     "crack",
///     "Crack",
///     WatchCondition::new("crack", ["crack"]).with_min_score(0.6).with_min_frames(3),
/// )
/// .in_zone("inspection-window")
/// .wakes_for("a defect");
/// # let _ = watch;
/// ```
#[derive(Debug, Clone)]
pub struct LiveWatch {
    id: String,
    name: String,
    condition: WatchCondition,
    zone: Option<String>,
    wakes_for: Option<String>,
}

impl LiveWatch {
    /// The watch `id`, shown as `name`, holding `condition`. Its zone, when it has one, is a
    /// zone of the camera ([`in_zone`](Self::in_zone)), never the condition's own.
    pub fn new(id: impl Into<String>, name: impl Into<String>, condition: WatchCondition) -> Self {
        Self { id: id.into(), name: name.into(), condition, zone: None, wakes_for: None }
    }

    /// Looks only inside the camera's zone `zone`.
    #[must_use]
    pub fn in_zone(mut self, zone: impl Into<String>) -> Self {
        self.zone = Some(zone.into());
        self
    }

    /// What the agent sleeps until, as its line says: "a defect" ("Asleep until a defect").
    #[must_use]
    pub fn wakes_for(mut self, what: impl Into<String>) -> Self {
        self.wakes_for = Some(what.into());
        self
    }
}

/// What a camera's pipeline did with its frames since it started, for a test or a probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameCounts {
    /// Frames the source delivered.
    pub arrived: u64,
    /// Frames dropped before the pipeline looked at them: the camera off, or Stop all.
    pub dropped: u64,
    /// Frames the pipeline looked at and offered to the hub.
    pub observed: u64,
    /// Watch events.
    pub fired: u64,
}

/// Builds a fresh tracker for each run of a camera's pipeline.
type TrackerFactory = Arc<dyn Fn() -> Box<dyn Tracker> + Send + Sync>;

enum Input {
    Stream(Arc<dyn FrameSource>),
    Screen { source: Arc<SurfaceCameraSource>, feed: CameraFeed },
}

/// One camera on the frame pipeline: its facts, its source, and the pipeline's stages.
///
/// Frames are shown and kept as the camera sends them. Two opt-ins restrict that: a privacy
/// hook ([`with_privacy`](Self::with_privacy)) redacts every frame before any other stage
/// sees it, and [`cover_people`](Self::cover_people) covers every region the detector labels
/// `person` or `face` before a frame is shown or kept.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_surface::camera::{CameraFacts, Ingest, LiveCamera, LiveWatch, ZoneFacts};
/// use adk_vision::stream::{FrameBudget, SurfaceCameraSource};
/// use adk_vision::{FrameSourceId, GreedyIouTracker, WatchCondition};
///
/// let (source, _feed) = SurfaceCameraSource::new(FrameSourceId("line-2".into()));
/// let camera = LiveCamera::stream(
///     CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")
///         .with_ingest(Ingest::rtsp(640, 360, "the line 2 server")),
///     Arc::new(source),
/// )
/// .tracker(|| Box::new(GreedyIouTracker::new(0.3, 4)))
/// .budget(FrameBudget::new(5.0).with_min_local_fps(1.0).with_local_quota(200_000))
/// .zone(ZoneFacts {
///     id: "inspection-window".into(),
///     name: "Inspection window".into(),
///     corners: [200.0, 140.0, 440.0, 280.0],
/// })
/// .watch(LiveWatch::new("crack", "Crack", WatchCondition::new("crack", ["crack"])).in_zone("inspection-window"));
/// # let _ = camera;
/// ```
pub struct LiveCamera {
    facts: CameraFacts,
    input: Input,
    privacy: Arc<dyn PrivacyHook>,
    cover_people: bool,
    detection: Option<(Arc<VisionRuntime>, DetectQuery, CallOptions)>,
    tracker: Option<TrackerFactory>,
    budget: Option<FrameBudget>,
    sampling: Option<Sampling>,
    watches: Vec<LiveWatch>,
    escalation: Option<(String, u64)>,
}

impl std::fmt::Debug for LiveCamera {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCamera").field("id", &self.facts.id).finish_non_exhaustive()
    }
}

impl LiveCamera {
    /// A camera whose frames `source` reads: an RTSP camera, a file, a screen. It starts on
    /// unless `facts` says otherwise.
    pub fn stream(facts: CameraFacts, source: Arc<dyn FrameSource>) -> Self {
        Self::with_input(facts, Input::Stream(source))
    }

    /// The web home's own camera (spec 3.7 §7.13 `SurfaceCameraSource`): frames the voice
    /// screen or the camera page captures reach it through [`Cameras::ingest`] — the camera
    /// page sends them to `POST /surface/cameras/{camera}/frames` — with no realtime model. It
    /// starts off, says it comes "from the web home", and takes nothing until a person
    /// switches it on.
    pub fn surface(facts: CameraFacts) -> Self {
        let (source, feed) = SurfaceCameraSource::new(FrameSourceId(facts.id.clone()));
        let facts = match facts.from {
            Some(_) => facts,
            None => facts.from("from the web home"),
        }
        .switched(false);
        Self::with_input(facts, Input::Screen { source: Arc::new(source), feed })
    }

    fn with_input(facts: CameraFacts, input: Input) -> Self {
        let zones = facts.zones.clone();
        Self {
            facts: CameraFacts { zones, ..facts },
            input,
            privacy: Arc::new(NoRedaction),
            cover_people: false,
            detection: None,
            tracker: None,
            budget: None,
            sampling: None,
            watches: Vec::new(),
            escalation: None,
        }
    }

    /// Passes every frame through `hook` before any other stage sees it, such as
    /// `RedactorPrivacy::blur_faces`; a frame the hook fails on is dropped.
    #[must_use]
    pub fn with_privacy(mut self, hook: Arc<dyn PrivacyHook>) -> Self {
        self.privacy = hook;
        self
    }

    /// Covers every region the detector labels `person` or `face`, its whole box, before a
    /// frame is shown or kept.
    #[must_use]
    pub fn cover_people(mut self) -> Self {
        self.cover_people = true;
        self
    }

    /// Detects `query` on every frame through `runtime`'s detection route.
    #[must_use]
    pub fn detector(mut self, runtime: Arc<VisionRuntime>, query: DetectQuery) -> Self {
        self.detection = Some((runtime, query, CallOptions::default()));
        self
    }

    /// The detection call's options: a route, a region of interest.
    #[must_use]
    pub fn detect_options(mut self, options: CallOptions) -> Self {
        if let Some((_, _, current)) = &mut self.detection {
            *current = options;
        }
        self
    }

    /// Tracks detections across frames with a tracker `make` builds for each run of the
    /// pipeline, so identifiers start again with each source session (V-I8).
    #[must_use]
    pub fn tracker(mut self, make: impl Fn() -> Box<dyn Tracker> + Send + Sync + 'static) -> Self {
        self.tracker = Some(Arc::new(make));
        self
    }

    /// Holds the camera to `budget` through a frame governor (spec 3.7 §7.9).
    #[must_use]
    pub fn budget(mut self, budget: FrameBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Samples the source at `sampling`; the budget's local rate by default.
    #[must_use]
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.sampling = Some(sampling);
        self
    }

    /// Adds zone `zone`, in the camera's pixels.
    #[must_use]
    pub fn zone(mut self, zone: ZoneFacts) -> Self {
        self.facts.zones.retain(|existing| existing.id != zone.id);
        self.facts.zones.push(zone);
        self
    }

    /// Adds watch condition `watch`.
    #[must_use]
    pub fn watch(mut self, watch: LiveWatch) -> Self {
        self.watches.push(watch);
        self
    }

    /// Escalates unclear tracks to `backend`, such as `sam-3.1`, each billed frame costing
    /// `micros_per_frame` micro-USD: [`PipelineCameras::escalate`] asks the governor first.
    #[must_use]
    pub fn escalation(mut self, backend: impl Into<String>, micros_per_frame: u64) -> Self {
        self.escalation = Some((backend.into(), micros_per_frame));
        self
    }
}

/// The share of a camera's frames the surface needs back from the pipeline, behind one lock.
struct RunState {
    stream: StreamState,
    /// The camera's size in pixels: the frames' when they are not downscaled.
    size: (u32, u32),
    governor: Option<GovernorHandle>,
    steps: Vec<GovernorStep>,
    paused_since: Option<DateTime<Utc>>,
    escalation_stopped_at: Option<DateTime<Utc>>,
    /// Escalations granted: when, and the frames billed.
    grants: VecDeque<(DateTime<Utc>, u64)>,
    tracks: Vec<TrackFacts>,
    /// Each track's centre on the frame before, for which way it moves.
    centres: HashMap<u64, f64>,
    zones: Vec<ZoneFacts>,
    evaluator: WatchEvaluator,
    /// Frames in a row each (watch, track) met its condition.
    streaks: HashMap<(usize, u64), u32>,
    woke: Vec<u32>,
    woke_on: Option<NaiveDate>,
    last_frame: Option<DateTime<Utc>>,
    attempt: u32,
}

/// One camera and the task running its pipeline.
struct Run {
    base: CameraFacts,
    id: FrameSourceId,
    input: Input,
    privacy: Arc<dyn PrivacyHook>,
    cover_people: bool,
    detection: Option<(Arc<VisionRuntime>, DetectQuery, CallOptions)>,
    tracker: Option<TrackerFactory>,
    budget: Option<FrameBudget>,
    sampling: Option<Sampling>,
    watches: Vec<LiveWatch>,
    escalation: Option<(String, u64)>,
    /// The camera's switch: a stream camera's source captures only while it is on.
    switch: watch::Sender<bool>,
    /// Whether the camera's source holds an open stream: a device captures, a connection reads.
    capturing: watch::Sender<bool>,
    stopped: AtomicBool,
    arrived: AtomicU64,
    dropped: AtomicU64,
    observed: AtomicU64,
    fired: AtomicU64,
    cancel: CancellationToken,
    retry_now: Notify,
    state: Mutex<RunState>,
}

impl Run {
    fn lock(&self) -> std::sync::MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_on(&self) -> bool {
        *self.switch.borrow()
    }

    fn feed(&self) -> Option<&CameraFeed> {
        match &self.input {
            Input::Screen { feed, .. } => Some(feed),
            Input::Stream(_) => None,
        }
    }

    /// The watches' conditions, each zone the camera's zone of that id now.
    fn conditions(&self, zones: &[ZoneFacts]) -> Vec<WatchCondition> {
        self.watches
            .iter()
            .map(|watch| {
                let zone = watch
                    .zone
                    .as_deref()
                    .and_then(|id| zones.iter().find(|zone| zone.id == id))
                    .and_then(|zone| {
                        let [x0, y0, x1, y1] = zone.corners.map(|value| value as f32);
                        Zone::new(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]).ok()
                    });
                let mut condition = watch.condition.clone();
                condition.zone = zone;
                condition
            })
            .collect()
    }

    fn facts(&self, now: DateTime<Utc>) -> CameraFacts {
        let state = self.lock();
        let mut facts = self.base.clone();
        facts.ingest.width = state.size.0;
        facts.ingest.height = state.size.1;
        facts.state = match state.paused_since {
            Some(since) => StreamState::Paused { since },
            None => state.stream.clone(),
        };
        facts.governor = self.governor_facts(&state, now);
        facts.tracks = match facts.state {
            StreamState::Live if self.is_on() => state.tracks.clone(),
            StreamState::Live | StreamState::Paused { .. } | StreamState::Lost { .. } => Vec::new(),
        };
        let today = now.date_naive();
        facts.watches = self
            .watches
            .iter()
            .enumerate()
            .map(|(index, watch)| WatchFacts {
                id: watch.id.clone(),
                name: watch.name.clone(),
                label: watch.condition.labels.first().cloned().unwrap_or_default(),
                min_score: f64::from(watch.condition.min_score),
                frames: watch.condition.min_frames,
                zone: watch.zone.clone(),
                cooldown_secs: u32::try_from(watch.condition.cooldown.as_secs())
                    .unwrap_or(u32::MAX),
                woke_today: if state.woke_on == Some(today) {
                    state.woke.get(index).copied().unwrap_or(0)
                } else {
                    0
                },
                wakes_for: watch.wakes_for.clone(),
            })
            .collect();
        facts.zones = state.zones.clone();
        facts
    }

    fn governor_facts(&self, state: &RunState, now: DateTime<Utc>) -> GovernorFacts {
        let Some(budget) = &self.budget else {
            let rate = self.sampling.map_or(1.0, |sampling| f64::from(sampling.fps));
            return GovernorFacts::new(rate, 0, now);
        };
        let (rate, frames_today, renews_at) = match &state.governor {
            Some(governor) => {
                let now = governor.state();
                (f64::from(now.fps), now.local_frames, now.period_ends_at)
            }
            None => {
                let period = i64::try_from(budget.period_secs).unwrap_or(i64::MAX).max(1);
                let end = (now.timestamp().div_euclid(period) + 1).saturating_mul(period);
                (f64::from(budget.local_fps), 0, DateTime::from_timestamp(end, 0).unwrap_or(now))
            }
        };
        let mut facts = GovernorFacts::new(
            f64::from(budget.local_fps),
            budget.local_frames_per_period.unwrap_or(0),
            renews_at,
        )
        .with_rate(rate)
        .with_frames_today(frames_today);
        facts.min_rate = Some(f64::from(budget.min_local_fps));
        facts.look_less_at = state.steps.iter().find_map(|step| step.used).unwrap_or(0.9);
        facts.steps = state.steps.clone();
        if let Some((backend, micros)) = &self.escalation {
            let minute_ago = now - chrono::Duration::seconds(60);
            let billed: u64 = state.grants.iter().map(|(_, frames)| frames).sum();
            facts.escalation = Some(EscalationFacts {
                backend: backend.clone(),
                last_minute: u32::try_from(
                    state.grants.iter().filter(|(at, _)| *at > minute_ago).count(),
                )
                .unwrap_or(u32::MAX),
                max_per_minute: budget.escalations_per_minute.unwrap_or(0),
                today: u32::try_from(state.grants.len()).unwrap_or(u32::MAX),
                cost_today_micros: billed.saturating_mul(*micros),
                stopped_at: state.escalation_stopped_at,
            });
        }
        facts
    }
}

struct Shared {
    app: String,
    user: String,
    artifacts: Option<Arc<dyn ArtifactService>>,
    cx: VisionCx,
    audit: Option<(AuditTrail, AuditActor)>,
    wakes: Option<Arc<dyn WatchSink>>,
    /// Stop all, as the hub told the source: while it is in force every stream camera's source
    /// is closed.
    frozen: watch::Sender<bool>,
}

impl Shared {
    async fn audit(&self, draft: AuditRecordDraft) {
        if let Some((trail, actor)) = &self.audit
            && let Err(error) = trail.record(draft.actor(actor.clone())).await
        {
            tracing::warn!(error = %error, "a camera's audit record was not written");
        }
    }
}

/// The cameras of a home on the frame pipeline (spec 3.7 §7.13, §8.2): the [`CameraSource`]
/// that reports what each camera's pipeline measured.
///
/// [`start`](Self::start) makes the hub over it and starts every camera's pipeline; each runs
/// until its camera is stopped or the hub is dropped. A camera switched off has its frames
/// dropped before the pipeline looks at them, and the hub's gate drops anything still on its
/// way; Stop all does the same for every camera (V-I17), and a frozen kill switch in the
/// context pauses each pipeline at its next frame with a `vision.source.paused` record.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::camera::{CameraFacts, LiveCamera, PipelineCameras};
/// use adk_vision::VisionCx;
///
/// # async fn example(source: Arc<dyn adk_vision::stream::FrameSource>) {
/// let live = PipelineCameras::builder("inspector", "ana")
///     .cx(VisionCx::new())
///     .camera(LiveCamera::stream(
///         CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line"),
///         source,
///     ))
///     .build()
///     .unwrap();
/// let cameras = live.start();
/// # let _ = cameras;
/// # }
/// ```
pub struct PipelineCameras {
    shared: Arc<Shared>,
    runs: Vec<Arc<Run>>,
    governance: Option<adk_core::GovernanceControl>,
}

impl std::fmt::Debug for PipelineCameras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineCameras")
            .field("cameras", &self.runs.iter().map(|run| &run.base.id).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Drop for PipelineCameras {
    fn drop(&mut self) {
        for run in &self.runs {
            run.cancel.cancel();
        }
    }
}

/// Builds [`PipelineCameras`].
pub struct PipelineCamerasBuilder {
    app: String,
    user: String,
    artifacts: Option<Arc<dyn ArtifactService>>,
    cx: VisionCx,
    governance: Option<adk_core::GovernanceControl>,
    audit: Option<(AuditTrail, AuditActor)>,
    wakes: Option<Arc<dyn WatchSink>>,
    cameras: Vec<LiveCamera>,
}

impl PipelineCamerasBuilder {
    /// Keeps card evidence and wake payloads in `artifacts`, in each camera's session of the
    /// builder's app and user; without it, raising a card is refused.
    #[must_use]
    pub fn artifacts(mut self, artifacts: Arc<dyn ArtifactService>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// Runs every pipeline under `cx`: its meter, its audit trail, its cancellation.
    #[must_use]
    pub fn cx(mut self, cx: VisionCx) -> Self {
        self.cx = cx;
        self
    }

    /// The kill switch every pipeline and the hub follow (spec 3.7 V-I17): while it is frozen,
    /// each pipeline drops its frames unprocessed and the hub's gate drops every frame.
    #[must_use]
    pub fn governance(mut self, control: adk_core::GovernanceControl) -> Self {
        self.cx = self.cx.with_governance(control.clone());
        self.governance = Some(control);
        self
    }

    /// Writes the cameras' `vision.*` records — the pipelines' and each watch's
    /// `vision.watch.registered` and `.fired` — to `trail` as `actor`.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail, actor: AuditActor) -> Self {
        self.cx = self.cx.with_audit(trail.clone(), actor.clone());
        self.audit = Some((trail, actor));
        self
    }

    /// Hands every watch event to `wakes`, such as the kernel's `VisionWakeSource`, with its
    /// frame kept redacted as the wake payload (needs [`artifacts`](Self::artifacts)).
    #[must_use]
    pub fn wakes(mut self, wakes: Arc<dyn WatchSink>) -> Self {
        self.wakes = Some(wakes);
        self
    }

    /// Adds `camera`.
    #[must_use]
    pub fn camera(mut self, camera: LiveCamera) -> Self {
        self.cameras.push(camera);
        self
    }

    /// The cameras, not yet started.
    ///
    /// # Errors
    ///
    /// `surface.invalid` for two cameras with one id, a watch on a zone the camera does not
    /// have, or conditions the watch evaluator refuses.
    pub fn build(self) -> Result<Arc<PipelineCameras>> {
        let shared = Arc::new(Shared {
            app: self.app,
            user: self.user,
            artifacts: self.artifacts,
            cx: self.cx,
            audit: self.audit,
            wakes: self.wakes,
            frozen: watch::Sender::new(false),
        });
        let mut runs: Vec<Arc<Run>> = Vec::with_capacity(self.cameras.len());
        for camera in self.cameras {
            if runs.iter().any(|run| run.base.id == camera.facts.id) {
                return Err(SurfaceError::Invalid(format!(
                    "two cameras are named '{}'; each id names one camera's routes",
                    camera.facts.id
                )));
            }
            for watch in &camera.watches {
                if let Some(zone) = &watch.zone
                    && !camera.facts.zones.iter().any(|candidate| &candidate.id == zone)
                {
                    return Err(SurfaceError::Invalid(format!(
                        "watch '{}' of camera '{}' looks inside zone '{zone}', which the camera does not have",
                        watch.id, camera.facts.id
                    )));
                }
            }
            let cancel = shared.cx.cancellation().child_token();
            let size = (camera.facts.ingest.width, camera.facts.ingest.height);
            let zones = camera.facts.zones.clone();
            let mut run = Run {
                id: FrameSourceId(camera.facts.id.clone()),
                switch: watch::Sender::new(camera.facts.on),
                capturing: watch::Sender::new(false),
                base: camera.facts,
                input: camera.input,
                privacy: camera.privacy,
                cover_people: camera.cover_people,
                detection: camera.detection,
                tracker: camera.tracker,
                budget: camera.budget,
                sampling: camera.sampling,
                escalation: camera.escalation,
                stopped: AtomicBool::new(false),
                arrived: AtomicU64::new(0),
                dropped: AtomicU64::new(0),
                observed: AtomicU64::new(0),
                fired: AtomicU64::new(0),
                cancel,
                retry_now: Notify::new(),
                state: Mutex::new(RunState {
                    stream: StreamState::Live,
                    size,
                    governor: None,
                    steps: Vec::new(),
                    paused_since: None,
                    escalation_stopped_at: None,
                    grants: VecDeque::new(),
                    tracks: Vec::new(),
                    centres: HashMap::new(),
                    zones: Vec::new(),
                    evaluator: WatchEvaluator::new(Vec::new()).map_err(vision)?,
                    streaks: HashMap::new(),
                    woke: vec![0; camera.watches.len()],
                    woke_on: None,
                    last_frame: None,
                    attempt: 0,
                }),
                watches: camera.watches,
            };
            let conditions = run.conditions(&zones);
            let state = run.state.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.evaluator = WatchEvaluator::new(conditions).map_err(vision)?;
            state.zones = zones;
            if let Some(feed) = run.feed() {
                feed.switch(run.is_on());
            }
            runs.push(Arc::new(run));
        }
        Ok(Arc::new(PipelineCameras { shared, runs, governance: self.governance }))
    }
}

impl PipelineCameras {
    /// A builder of the cameras of `user` in `app`.
    pub fn builder(app: impl Into<String>, user: impl Into<String>) -> PipelineCamerasBuilder {
        PipelineCamerasBuilder {
            app: app.into(),
            user: user.into(),
            artifacts: None,
            cx: VisionCx::new(),
            governance: None,
            audit: None,
            wakes: None,
            cameras: Vec::new(),
        }
    }

    /// The hub over these cameras, following the kill switch they were built with, and every
    /// camera's pipeline started. A pipeline holds the hub weakly: dropping every clone of the
    /// hub and of these cameras stops them.
    pub fn start(self: &Arc<Self>) -> Cameras {
        let mut hub = Cameras::new(Arc::clone(self) as Arc<dyn CameraSource>);
        if let Some(control) = &self.governance {
            hub = hub.with_governance(control.clone());
        }
        for run in &self.runs {
            tokio::spawn(drive(Arc::clone(run), Arc::clone(&self.shared), hub.downgrade()));
        }
        hub
    }

    fn run(&self, camera: &str) -> Result<&Arc<Run>> {
        self.runs
            .iter()
            .find(|run| run.base.id == camera)
            .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))
    }

    /// What camera `camera`'s pipeline did with its frames so far.
    pub fn counts(&self, camera: &str) -> Option<FrameCounts> {
        let run = self.run(camera).ok()?;
        Some(FrameCounts {
            arrived: run.arrived.load(Ordering::SeqCst),
            dropped: run.dropped.load(Ordering::SeqCst),
            observed: run.observed.load(Ordering::SeqCst),
            fired: run.fired.load(Ordering::SeqCst),
        })
    }

    /// Whether camera `camera`'s source holds an open stream now: a device capturing, a
    /// connection reading. A stream camera switched off holds none.
    pub fn is_capturing(&self, camera: &str) -> Option<bool> {
        Some(*self.run(camera).ok()?.capturing.borrow())
    }

    /// Camera `camera`'s frame governor, once its pipeline runs.
    pub fn governor(&self, camera: &str) -> Option<GovernorHandle> {
        self.run(camera).ok()?.lock().governor.clone()
    }

    /// Asks camera `camera`'s governor to escalate `frames` frames of an unclear track now:
    /// granted with the frames billed, or refused with the reason. A grant counts in "Asking …
    /// about unclear tracks".
    ///
    /// # Errors
    ///
    /// `surface.not_found` for an unknown camera, `surface.unsupported` before its pipeline
    /// runs or without a budget, and the audit trail's error when it fails closed.
    pub async fn escalate(&self, camera: &str, frames: u64) -> Result<EscalationVerdict> {
        self.escalate_at(camera, Utc::now(), frames).await
    }

    /// As [`escalate`](Self::escalate), asked at `at`: for a replayed stream, whose governor
    /// runs on the frames' capture times.
    ///
    /// # Errors
    ///
    /// As [`escalate`](Self::escalate).
    pub async fn escalate_at(
        &self,
        camera: &str,
        at: DateTime<Utc>,
        frames: u64,
    ) -> Result<EscalationVerdict> {
        let run = self.run(camera)?;
        let governor = run.lock().governor.clone().ok_or_else(|| {
            SurfaceError::unsupported("escalating before the camera's governor runs")
        })?;
        let verdict =
            governor.request_escalation_at(&self.shared.cx, at, frames).await.map_err(vision)?;
        if let EscalationVerdict::Granted { billed_frames } = verdict {
            let period = governor.state();
            let length = run.budget.as_ref().map_or(86_400, |budget| budget.period_secs);
            let period_start = period.period_ends_at
                - chrono::Duration::seconds(i64::try_from(length).unwrap_or(86_400));
            let mut state = run.lock();
            state.grants.retain(|(granted, _)| *granted >= period_start);
            state.grants.push_back((at, billed_frames));
        }
        Ok(verdict)
    }
}

#[async_trait]
impl CameraSource for PipelineCameras {
    async fn cameras(&self) -> Result<Vec<CameraFacts>> {
        let now = Utc::now();
        Ok(self.runs.iter().map(|run| run.facts(now)).collect())
    }

    /// Off stops a stream camera's capture at its source: its stream is dropped — a
    /// [`DeviceSource`](adk_vision::stream::DeviceSource)'s ffmpeg exits, a connection closes —
    /// and nothing opens it until the camera is on again; the answer comes once the stream is
    /// closed, or after [`STOP_WAIT`] whatever the source does. On opens it again. The hub's
    /// gate and the pipeline's own check still drop any frame on its way.
    async fn switch(&self, camera: &str, on: bool) -> Result<()> {
        let run = self.run(camera)?;
        run.switch.send_replace(on);
        if let Some(feed) = run.feed() {
            feed.switch(on);
        }
        if !on {
            {
                let mut state = run.lock();
                state.tracks.clear();
                state.centres.clear();
                state.streaks.clear();
            }
            if run.feed().is_none() {
                let mut capturing = run.capturing.subscribe();
                if tokio::time::timeout(STOP_WAIT, capturing.wait_for(|open| !*open)).await.is_err()
                {
                    tracing::warn!(camera.id = %camera, "the camera's source did not close in time; its frames are still dropped");
                }
            }
        }
        Ok(())
    }

    async fn stop(&self, camera: &str) -> Result<()> {
        let run = self.run(camera)?;
        run.stopped.store(true, Ordering::SeqCst);
        run.switch.send_replace(false);
        if let Some(feed) = run.feed() {
            feed.switch(false);
        }
        run.cancel.cancel();
        run.lock().tracks.clear();
        Ok(())
    }

    async fn retry(&self, camera: &str) -> Result<()> {
        self.run(camera)?.retry_now.notify_one();
        Ok(())
    }

    async fn save_zone(&self, camera: &str, zone: &ZoneFacts) -> Result<()> {
        let run = self.run(camera)?;
        let changed: Vec<WatchCondition> = {
            let mut state = run.lock();
            let entry = state
                .zones
                .iter_mut()
                .find(|candidate| candidate.id == zone.id)
                .ok_or_else(|| SurfaceError::not_found(format!("zone '{}'", zone.id)))?;
            *entry = zone.clone();
            let conditions = run.conditions(&state.zones);
            // Counting starts again with the new corners: a track met the old zone, not this one.
            state.evaluator = WatchEvaluator::new(conditions.clone()).map_err(vision)?;
            state.streaks.clear();
            run.watches
                .iter()
                .zip(conditions)
                .filter(|(watch, _)| watch.zone.as_deref() == Some(zone.id.as_str()))
                .map(|(_, condition)| condition)
                .collect()
        };
        for condition in &changed {
            self.shared.audit(watch_registered_draft(&run.id, condition)).await;
        }
        tracing::info!(camera.id = %camera, zone.id = %zone.id, watches = changed.len(), "zone changed");
        Ok(())
    }

    /// Stop all closes each stream camera's source as off does — a device's ffmpeg exits, a
    /// connection closes — and answers once each has closed, or after [`STOP_WAIT`] whatever a
    /// source does; Resume all opens again each one that is on. The voice screen's camera
    /// captures in the browser, which stops its own track: its frames are dropped.
    async fn freeze(&self, frozen: bool) -> Result<()> {
        self.shared.frozen.send_replace(frozen);
        if frozen {
            let closing = self.runs.iter().filter(|run| run.feed().is_none()).map(|run| async move {
                let mut capturing = run.capturing.subscribe();
                if tokio::time::timeout(STOP_WAIT, capturing.wait_for(|open| !*open)).await.is_err()
                {
                    tracing::warn!(camera.id = %run.base.id, "the camera's source did not close in time after Stop all; its frames are still dropped");
                }
            });
            futures::future::join_all(closing).await;
        }
        Ok(())
    }

    fn intake(&self, camera: &str) -> Option<Arc<dyn FrameIntake>> {
        let feed = self.run(camera).ok()?.feed()?.clone();
        Some(Arc::new(FeedIntake(feed)))
    }

    fn voice_camera(&self) -> Option<String> {
        self.runs.iter().find(|run| run.feed().is_some()).map(|run| run.base.id.clone())
    }

    async fn keep_frame(&self, camera: &CameraFacts, frame: &HeldFrame) -> Result<ImageEvidence> {
        let run = self.run(&camera.id)?;
        let artifacts = self
            .shared
            .artifacts
            .clone()
            .ok_or_else(|| SurfaceError::unsupported("keeping a camera's frames"))?;
        let shown = &frame.frame;
        let (pixels, _) =
            DecodedImage::decode(&shown.bytes, &DecodeLimits::default()).map_err(vision)?;
        let (width, height) = pixels.size();
        let scale = (
            f64::from(width) / f64::from(shown.width.max(1)),
            f64::from(height) / f64::from(shown.height.max(1)),
        );
        // What the hook redacted, and under `cover_people` everyone on the frame, is covered
        // again before it is written, whatever was shown.
        let people: Vec<BBox> = shown
            .overlays
            .iter()
            .filter(|overlay| {
                overlay.blurred || (run.cover_people && PEOPLE.contains(&overlay.label.as_str()))
            })
            .filter_map(|overlay| overlay.bbox)
            .map(|[x0, y0, x1, y1]| {
                BBox::new(
                    (x0 * scale.0) as f32,
                    (y0 * scale.1) as f32,
                    (x1 * scale.0) as f32,
                    (y1 * scale.1) as f32,
                )
            })
            .collect();
        let redacted =
            cover(&self.shared.cx, &run.id, shown.at, pixels, people.clone(), "camera-card")
                .await
                .map_err(vision)?;
        let keeper = FrameKeeper::new(
            artifacts,
            MediaScope::new(
                self.shared.app.clone(),
                self.shared.user.clone(),
                camera.session.clone(),
            ),
            run.id.clone(),
        );
        let kept = keeper
            .keep(&self.shared.cx, &redacted, KeepReason::CardEvidence)
            .await
            .map_err(vision)?;
        let digest = kept.image.digest.to_string();
        let regions: Vec<serde_json::Value> = people
            .iter()
            .map(|bbox| {
                json!({
                    "image": digest,
                    "shape": { "kind": "box", "x0": bbox.x0, "y0": bbox.y0, "x1": bbox.x1, "y1": bbox.y1 },
                    "label": "person",
                    "origin": { "backend": "camera", "frame": "sourcePixels" },
                })
            })
            .collect();
        Ok(ImageEvidence::new(
            digest.clone(),
            kept.image.artifact.clone(),
            kept.image.mime_type.clone(),
            kept.image.width,
            kept.image.height,
        )
        .with_regions(json!({
            "image": digest,
            "width": kept.image.width,
            "height": kept.image.height,
            "regions": regions,
        }))
        .with_caption(format!("{} at {}", camera.name, shown.at.format("%H:%M:%S"))))
    }
}

/// The push side of the voice screen's camera, behind the hub's gate.
struct FeedIntake(CameraFeed);

impl FrameIntake for FeedIntake {
    fn push(&self, bytes: Vec<u8>, at: DateTime<Utc>) -> bool {
        self.0.push_frame(bytes, at)
    }
}

/// A privacy step that fills boxes solid, for people the detector found after the camera's own
/// privacy hook ran.
struct Cover {
    name: String,
    boxes: Vec<BBox>,
}

#[async_trait]
impl PrivacyHook for Cover {
    fn name(&self) -> &str {
        &self.name
    }

    async fn redact(
        &self,
        _cx: &VisionCx,
        _frame: &FrameMeta,
        pixels: &mut DecodedImage,
    ) -> adk_vision::Result<Vec<BBox>> {
        if self.boxes.is_empty() {
            return Ok(Vec::new());
        }
        let (width, height) = pixels.size();
        let mut rgba = pixels.rgba().to_vec();
        for bbox in &self.boxes {
            let clamp = |value: f32, max: u32| (value.max(0.0) as u32).min(max);
            let (x0, y0) = (clamp(bbox.x0.floor(), width), clamp(bbox.y0.floor(), height));
            let (x1, y1) = (clamp(bbox.x1.ceil(), width), clamp(bbox.y1.ceil(), height));
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = ((y * width + x) * 4) as usize;
                    rgba[at..at + 3].copy_from_slice(&[0, 0, 0]);
                    rgba[at + 3] = 255;
                }
            }
        }
        *pixels = DecodedImage::from_rgba(width, height, rgba)?;
        Ok(self.boxes.clone())
    }
}

/// `pixels` of `source` captured `at`, `people` filled, as the only kind of frame a
/// [`FrameKeeper`] stores.
async fn cover(
    cx: &VisionCx,
    source: &FrameSourceId,
    at: DateTime<Utc>,
    pixels: DecodedImage,
    people: Vec<BBox>,
    name: &str,
) -> adk_vision::Result<RedactedFrame> {
    let frame = Frame {
        source: source.clone(),
        index: 0,
        pts: StdDuration::ZERO,
        captured_at: at,
        pixels: FramePixels::Ephemeral(Arc::new(pixels)),
    };
    FramePrivacy::new(Arc::new(Cover { name: name.to_string(), boxes: people }))
        .apply(cx, frame)
        .await
}

/// A JPEG of `image` for the screen.
fn jpeg(image: &DecodedImage) -> std::result::Result<Vec<u8>, String> {
    let (width, height) = image.size();
    let rgba = image::RgbaImage::from_raw(width, height, image.rgba().to_vec())
        .ok_or_else(|| "a frame's pixels do not match its size".to_string())?;
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, SHOWN_QUALITY)
        .encode_image(&rgb)
        .map_err(|error| error.to_string())?;
    Ok(out)
}

/// Replaces the user and password of any URL in `text`: an error a source reports never shows
/// a credential (spec 3.7 §9.1).
fn without_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(scheme) = rest.find("://") {
        let (head, tail) = rest.split_at(scheme + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) => {
                out.push_str("[credentials hidden]");
                rest = &tail[at..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

/// Runs camera `run`'s pipeline until it is stopped or the hub goes, building it again after a
/// stage fails.
async fn drive(run: Arc<Run>, shared: Arc<Shared>, hub: WeakCameras) {
    let conditions = run.lock().evaluator.conditions().to_vec();
    for condition in &conditions {
        shared.audit(watch_registered_draft(&run.id, condition)).await;
    }
    let source: Arc<dyn FrameSource> = match &run.input {
        Input::Stream(source) => Arc::clone(source),
        Input::Screen { source, .. } => Arc::clone(source) as Arc<dyn FrameSource>,
    };
    let watched: Arc<dyn FrameSource> = Arc::new(Watched {
        id: run.id.clone(),
        stops: matches!(run.input, Input::Stream(_)),
        inner: source,
        run: Arc::clone(&run),
        shared: Arc::clone(&shared),
        hub: hub.clone(),
    });
    let sink =
        Arc::new(Sink { run: Arc::clone(&run), shared: Arc::clone(&shared), hub: hub.clone() });
    let cx = shared.cx.clone().with_cancellation(run.cancel.clone());
    loop {
        if run.cancel.is_cancelled() || hub.upgrade().is_none() {
            return;
        }
        // A frozen kill switch refuses a run at its start: wait for the thaw rather than
        // building a pipeline whose governor would start its count again.
        if let Err(VisionError::Cancelled) = cx.check() {
            return;
        }
        if cx.check().is_err() {
            tokio::select! {
                () = run.cancel.cancelled() => return,
                () = tokio::time::sleep(StdDuration::from_millis(500)) => continue,
            }
        }
        let mut builder = StreamPipeline::builder(Arc::clone(&watched))
            .with_privacy(Arc::clone(&run.privacy))
            .sink(Arc::clone(&sink) as Arc<dyn ObservationSink>);
        if let Some(sampling) = run.sampling {
            builder = builder.sampling(sampling);
        }
        if let Some(budget) = &run.budget {
            builder = builder.budget(budget.clone());
        }
        if let Some((runtime, query, options)) = &run.detection {
            builder = builder
                .detector(Arc::clone(runtime), query.clone())
                .detect_options(options.clone());
        }
        if let Some(make) = &run.tracker {
            builder = builder.tracker(make());
        }
        let pipeline = match builder.build() {
            Ok(pipeline) => pipeline,
            Err(error) => {
                tracing::error!(camera.id = %run.id.0, error = %error, "the camera's pipeline cannot be built");
                lost(&run, &hub, &error.to_string()).await;
                return;
            }
        };
        {
            let mut state = run.lock();
            state.governor = pipeline.governor();
            state.steps.clear();
            state.paused_since = None;
            state.escalation_stopped_at = None;
        }
        match pipeline.run(&cx).await {
            Ok(summary) => {
                tracing::info!(camera.id = %run.id.0, frames.received = summary.received, frames.observed = summary.observations, "camera stream ended");
            }
            Err(error) => {
                tracing::warn!(camera.id = %run.id.0, error = %error, "camera stream failed");
                if run.cancel.is_cancelled() {
                    return;
                }
                let wait = lost(&run, &hub, &error.to_string()).await;
                tokio::select! {
                    () = run.cancel.cancelled() => return,
                    () = run.retry_now.notified() => {}
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
}

/// Marks `run` lost with `error` and tells the hub; returns how long to wait before trying
/// again.
async fn lost(run: &Run, hub: &WeakCameras, error: &str) -> StdDuration {
    let now = Utc::now();
    let wait = {
        let mut state = run.lock();
        state.attempt = state.attempt.saturating_add(1);
        let wait =
            RETRY_FIRST.saturating_mul(1 << state.attempt.saturating_sub(1).min(5)).min(RETRY_MOST);
        state.stream = StreamState::Lost {
            since: state.last_frame.unwrap_or(now),
            retry_at: now + chrono::Duration::from_std(wait).unwrap_or_default(),
            attempt: state.attempt,
            error: without_credentials(error),
        };
        state.tracks.clear();
        state.centres.clear();
        wait
    };
    if let Some(hub) = hub.upgrade() {
        hub.changed(&run.base.id);
    }
    wait
}

/// The `vision.source.stopped` or `.started` record of a person's switch closing or opening a
/// camera's source (spec 3.7 §9.3); the person's own act is `surface.camera.switch`.
fn switch_draft(action: &str, source: &FrameSourceId, reason: &str) -> AuditRecordDraft {
    AuditRecordDraft::new(
        AuditRecordKind::Other(adk_vision::audit::VISION_AUDIT_KIND.to_string()),
        action,
    )
    .outcome(AuditRecordOutcome::Succeeded)
    .ext("source", json!(source.0))
    .ext("reason", json!(reason))
}

/// A camera's source as its pipeline reads it: reopened with a growing wait when it fails or
/// ends, so the pipeline — and its governor's count — outlives a lost connection; closed while
/// a stream camera is off and opened again when it is on; frames dropped while the camera is
/// off or Stop all is in force, before anything looks at them; and numbered on across
/// reconnections, so watch conditions keep counting.
struct Watched {
    id: FrameSourceId,
    /// Off closes the source: a stream camera's. The voice screen's camera captures in the
    /// browser, which stops its own track; its source only queues what the browser sends.
    stops: bool,
    inner: Arc<dyn FrameSource>,
    run: Arc<Run>,
    shared: Arc<Shared>,
    hub: WeakCameras,
}

struct Reading {
    inner: Arc<dyn FrameSource>,
    stops: bool,
    switch: watch::Receiver<bool>,
    /// Stop all: in force, it closes the source as off does.
    frozen: watch::Receiver<bool>,
    /// The switch or Stop all closed the source; opening it again is recorded, with what
    /// opened it: `switchedOn` or `resumeAll`.
    reopened_by: Option<&'static str>,
    sampling: Sampling,
    current: Option<FrameStream>,
    next_index: u64,
    pts_base: StdDuration,
    last_pts: StdDuration,
    run: Arc<Run>,
    shared: Arc<Shared>,
    hub: WeakCameras,
}

#[async_trait]
impl FrameSource for Watched {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
        adk_vision::stream::Sampler::new(sampling)?;
        let reading = Reading {
            inner: Arc::clone(&self.inner),
            stops: self.stops,
            switch: self.run.switch.subscribe(),
            frozen: self.shared.frozen.subscribe(),
            reopened_by: None,
            sampling,
            current: None,
            next_index: 0,
            pts_base: StdDuration::ZERO,
            last_pts: StdDuration::ZERO,
            run: Arc::clone(&self.run),
            shared: Arc::clone(&self.shared),
            hub: self.hub.clone(),
        };
        let frames = futures::stream::unfold(reading, |mut reading| async move {
            loop {
                // Off and Stop all stop capture at the source: the stream is dropped, and with
                // it any process or connection it holds, until the camera is on and nothing is
                // stopped.
                let off = !*reading.switch.borrow_and_update();
                let frozen = *reading.frozen.borrow_and_update();
                if reading.stops && (off || frozen) {
                    reading.close(Some(if off { "switchedOff" } else { "stopAll" })).await;
                    tokio::select! {
                        () = reading.run.cancel.cancelled() => return None,
                        changed = reading.switch.changed() => {
                            if changed.is_err() {
                                return None;
                            }
                            if *reading.switch.borrow() && reading.reopened_by.is_some() {
                                reading.reopened_by = Some("switchedOn");
                            }
                        }
                        changed = reading.frozen.changed() => {
                            if changed.is_err() {
                                return None;
                            }
                            if !*reading.frozen.borrow() && reading.reopened_by.is_some() {
                                reading.reopened_by = Some("resumeAll");
                            }
                        }
                    }
                    continue;
                }
                if reading.current.is_none() {
                    match reading.inner.frames(reading.sampling).await {
                        Ok(stream) => {
                            reading.current = Some(stream);
                            reading.pts_base = reading.last_pts;
                            reading.run.capturing.send_replace(true);
                            reading.opened().await;
                        }
                        Err(error) => {
                            wait_after(&reading.run, &reading.hub, &error.to_string()).await;
                            continue;
                        }
                    }
                }
                let next = match reading.current.as_mut() {
                    Some(stream) if reading.stops => tokio::select! {
                        next = stream.next() => next,
                        // Switched off or stopped while waiting for a frame: closed at the top.
                        _ = reading.switch.wait_for(|on| !*on) => continue,
                        _ = reading.frozen.wait_for(|frozen| *frozen) => continue,
                    },
                    Some(stream) => stream.next().await,
                    None => continue,
                };
                match next {
                    Some(Ok(mut frame)) => {
                        reading.arrived(frame.captured_at);
                        reading.run.arrived.fetch_add(1, Ordering::SeqCst);
                        if !reading.run.is_on() || *reading.shared.frozen.borrow() {
                            reading.run.dropped.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                        frame.index = reading.next_index;
                        reading.next_index += 1;
                        frame.pts = reading.pts_base + frame.pts;
                        reading.last_pts = frame.pts;
                        return Some((Ok(frame), reading));
                    }
                    Some(Err(error)) => {
                        reading.close(None).await;
                        wait_after(&reading.run, &reading.hub, &error.to_string()).await;
                    }
                    None => {
                        reading.close(None).await;
                        wait_after(&reading.run, &reading.hub, "the camera's stream ended").await;
                    }
                }
            }
        });
        Ok(frames.boxed())
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        self.run.capturing.send_replace(false);
    }
}

impl Reading {
    /// Drops the source's stream, if one is open; `reason`, when a person closed it — the
    /// switch (`switchedOff`) or Stop all (`stopAll`) — which is recorded as
    /// `vision.source.stopped`.
    async fn close(&mut self, reason: Option<&'static str>) {
        let Some(stream) = self.current.take() else { return };
        drop(stream);
        self.run.capturing.send_replace(false);
        if let Some(reason) = reason {
            self.reopened_by = Some(if reason == "stopAll" { "resumeAll" } else { "switchedOn" });
            tracing::info!(camera.id = %self.run.base.id, close.reason = reason, "camera source stopped capturing");
            self.shared.audit(switch_draft(VISION_SOURCE_STOPPED, &self.run.id, reason)).await;
        }
    }

    /// The source opened: after a person closed it, that is recorded as
    /// `vision.source.started` with what opened it again.
    async fn opened(&mut self) {
        if let Some(reason) = self.reopened_by.take() {
            tracing::info!(camera.id = %self.run.base.id, open.reason = reason, "camera source captures again");
            self.shared.audit(switch_draft(VISION_SOURCE_STARTED, &self.run.id, reason)).await;
        }
    }

    /// A frame arrived: the camera is live again.
    fn arrived(&self, at: DateTime<Utc>) {
        let was_lost = {
            let mut state = self.run.lock();
            state.last_frame = Some(at);
            state.attempt = 0;
            let was_lost = matches!(state.stream, StreamState::Lost { .. });
            state.stream = StreamState::Live;
            was_lost
        };
        if was_lost && let Some(hub) = self.hub.upgrade() {
            tracing::info!(camera.id = %self.run.base.id, "camera back");
            hub.changed(&self.run.base.id);
        }
    }
}

/// `run`'s source failed: marks the camera lost, then waits — less if a person asks to try
/// again.
async fn wait_after(run: &Run, hub: &WeakCameras, error: &str) {
    tracing::warn!(camera.id = %run.base.id, error = %without_credentials(error), "camera lost");
    let wait = lost(run, hub, error).await;
    tokio::select! {
        () = run.retry_now.notified() => {}
        () = tokio::time::sleep(wait) => {}
    }
}

/// What a camera's pipeline hands the surface: each observation as a shown frame, the tracks
/// and watch counts on it, and the governor's steps.
struct Sink {
    run: Arc<Run>,
    shared: Arc<Shared>,
    hub: WeakCameras,
}

fn is_person(region: &Region) -> bool {
    region.label.as_deref().is_some_and(|label| PEOPLE.contains(&label))
}

/// What a privacy hook's boxes are called on screen, from its name: `redactor:faces` → `face`.
fn hidden_label(hook: &str) -> &'static str {
    if hook.contains("face") { "face" } else { "person" }
}

#[async_trait]
impl ObservationSink for Sink {
    async fn observe(&self, cx: &VisionCx, observation: Observation) -> adk_vision::Result<()> {
        let Some(hub) = self.hub.upgrade() else { return Ok(()) };
        let run = &self.run;
        let captured = observation.frame.captured_at;
        let pixels = observation.image.pixels();
        let (frame_width, frame_height) = pixels.size();
        let (width, height) = {
            let mut state = run.lock();
            // Regions are in source pixels: without downscaling the frame's size is the camera's.
            if run.sampling.is_none_or(|sampling| sampling.max_long_edge.is_none()) {
                state.size = (frame_width, frame_height);
            }
            state.size
        };
        let to_frame = (
            f64::from(frame_width) / f64::from(width.max(1)),
            f64::from(frame_height) / f64::from(height.max(1)),
        );
        let people: Vec<BBox> = observation
            .regions
            .iter()
            .filter(|region| run.cover_people && is_person(region))
            .filter_map(|region| region.shape.bbox())
            .map(|bbox| {
                BBox::new(
                    (f64::from(bbox.x0) * to_frame.0) as f32,
                    (f64::from(bbox.y0) * to_frame.1) as f32,
                    (f64::from(bbox.x1) * to_frame.0) as f32,
                    (f64::from(bbox.y1) * to_frame.1) as f32,
                )
            })
            .collect();
        // Under `cover_people`, everyone the detector found is covered before the frame is
        // shown or kept.
        let image: Arc<RedactedFrame> = if people.is_empty() {
            Arc::clone(&observation.image)
        } else {
            let name = format!("{}+people", observation.image.hook());
            Arc::new(cover(cx, &observation.source, captured, pixels.clone(), people, &name).await?)
        };

        // Watch conditions, with each (watch, track)'s frames in a row.
        let tracked = observation.tracked();
        let (events, conditions) = {
            let mut state = run.lock();
            let events = state.evaluator.observe(&tracked);
            let conditions = state.evaluator.conditions().to_vec();
            let mut seen = Vec::new();
            for (index, condition) in conditions.iter().enumerate() {
                for region in observation.regions.iter().filter(|region| condition.matches(region))
                {
                    if let Some(track) = region.track {
                        let key = (index, track.0);
                        if !seen.contains(&key) {
                            *state.streaks.entry(key).or_insert(0) += 1;
                            seen.push(key);
                        }
                    }
                }
            }
            state.streaks.retain(|key, _| seen.contains(key));
            if !events.is_empty() {
                let today = captured.date_naive();
                if state.woke_on != Some(today) {
                    state.woke_on = Some(today);
                    state.woke.iter_mut().for_each(|count| *count = 0);
                }
                for event in &events {
                    if let Some(index) =
                        conditions.iter().position(|condition| condition.name == event.condition)
                        && let Some(count) = state.woke.get_mut(index)
                    {
                        *count += 1;
                    }
                }
            }
            (events, conditions)
        };
        for event in &events {
            tracing::info!(camera.id = %run.base.id, watch = %event.condition, track = event.track.0, score = event.score, frames = event.frames, "watch fired");
            if let Some(condition) = conditions.iter().find(|c| c.name == event.condition) {
                self.shared.audit(event.audit_draft(condition.digest())).await;
            }
            if let (Some(wakes), Some(artifacts)) = (&self.shared.wakes, &self.shared.artifacts) {
                let keeper = FrameKeeper::new(
                    Arc::clone(artifacts),
                    MediaScope::new(
                        self.shared.app.clone(),
                        self.shared.user.clone(),
                        run.base.session.clone(),
                    ),
                    run.id.clone(),
                );
                let kept = keeper.keep(cx, &image, KeepReason::WakePayload).await?;
                if let Err(error) =
                    wakes.deliver(&event.clone().with_frame_artifact(kept.image.artifact)).await
                {
                    tracing::warn!(camera.id = %run.base.id, error = %error, "a watch event was not delivered");
                }
            }
        }

        // The overlays, in the camera's pixels.
        let streak_of = |state: &RunState, region: &Region| -> Option<u32> {
            let track = region.track?.0;
            conditions
                .iter()
                .enumerate()
                .filter(|(_, condition)| condition.matches(region))
                .filter_map(|(index, _)| state.streaks.get(&(index, track)).copied())
                .max()
        };
        let to_camera = (1.0 / to_frame.0, 1.0 / to_frame.1);
        let overlays: Vec<Overlay>;
        {
            let mut state = run.lock();
            let zones = state.zones.clone();
            let mut centres = HashMap::new();
            let mut rows: Vec<TrackFacts> = Vec::new();
            let mut drawn = Vec::with_capacity(observation.regions.len());
            for region in &observation.regions {
                let Some(bbox) = region.shape.bbox() else { continue };
                let corners = [
                    f64::from(bbox.x0),
                    f64::from(bbox.y0),
                    f64::from(bbox.x1),
                    f64::from(bbox.y1),
                ];
                let label = region.label.clone().unwrap_or_else(|| "object".into());
                let mut overlay = Overlay::boxed(label.clone(), corners);
                overlay.score = region.score.map(f64::from);
                overlay.track = region.track.map(|track| track.0);
                if let Shape::Polygon(polygon) = &region.shape {
                    overlay.outline = polygon
                        .points
                        .iter()
                        .map(|[x, y]| [f64::from(*x), f64::from(*y)])
                        .collect();
                }
                let streak = streak_of(&state, region);
                overlay.frames = streak;
                if run.cover_people && is_person(region) {
                    overlay.blurred = true;
                } else if let Some(track) = region.track {
                    let centre = f64::midpoint(corners[0], corners[2]);
                    let inside = zones.iter().any(|zone| {
                        let [x0, y0, x1, y1] = zone.corners;
                        let cy = f64::midpoint(corners[1], corners[3]);
                        centre >= x0 && centre < x1 && cy >= y0 && cy < y1
                    });
                    let moving = state.centres.get(&track.0).map(|before| centre - before);
                    let edge = f64::from(width) * 0.04;
                    let phase = if inside {
                        TrackPhase::InZone
                    } else if corners[0] <= edge {
                        if moving.is_some_and(|dx| dx < 0.0) {
                            TrackPhase::Leaving
                        } else {
                            TrackPhase::Entering
                        }
                    } else if corners[2] >= f64::from(width) - edge {
                        if moving.is_some_and(|dx| dx < 0.0) {
                            TrackPhase::Entering
                        } else {
                            TrackPhase::Leaving
                        }
                    } else {
                        TrackPhase::InView
                    };
                    centres.insert(track.0, centre);
                    let finding = streak.map(|frames| Finding {
                        label: label.clone(),
                        score: region.score.map_or(0.0, f64::from),
                        frames,
                    });
                    match rows.iter_mut().find(|row| row.id == track.0) {
                        Some(row) => {
                            if row.finding.is_none() {
                                row.finding = finding;
                            }
                        }
                        None => rows.push(TrackFacts {
                            id: track.0,
                            label: label.clone(),
                            phase,
                            finding,
                        }),
                    }
                }
                drawn.push(overlay);
            }
            // What the camera's privacy hook covered, in the camera's pixels.
            let hidden = hidden_label(observation.image.hook());
            for bbox in observation.image.redacted() {
                drawn.push(
                    Overlay::boxed(
                        hidden,
                        [
                            f64::from(bbox.x0) * to_camera.0,
                            f64::from(bbox.y0) * to_camera.1,
                            f64::from(bbox.x1) * to_camera.0,
                            f64::from(bbox.y1) * to_camera.1,
                        ],
                    )
                    .blurred(),
                );
            }
            state.centres = centres;
            state.tracks = rows;
            overlays = drawn;
        }

        let shown_pixels = image.pixels().clone();
        let bytes = tokio::task::spawn_blocking(move || jpeg(&shown_pixels))
            .await
            .map_err(|error| {
                VisionError::Decode(format!("a shown frame's encoder panicked: {error}"))
            })?
            .map_err(VisionError::Decode)?;
        let latency = (Utc::now() - captured).num_milliseconds().clamp(0, i64::from(u32::MAX));
        let frame = super::facts::CameraFrame::new(bytes, "image/jpeg", width, height, captured)
            .with_overlays(overlays)
            .with_latency_ms(u32::try_from(latency).unwrap_or(u32::MAX));
        hub.offer(&run.base.id, frame).await;
        run.observed.fetch_add(1, Ordering::SeqCst);
        run.fired.fetch_add(events.len() as u64, Ordering::SeqCst);
        if !events.is_empty() {
            hub.changed(&run.base.id);
        }
        Ok(())
    }

    async fn source_event(&self, _cx: &VisionCx, event: &SourceEvent) -> adk_vision::Result<()> {
        let run = &self.run;
        {
            let mut state = run.lock();
            match event {
                SourceEvent::Governor { step } => {
                    let used = step
                        .state
                        .local_quota
                        .filter(|quota| *quota > 0)
                        .map(|quota| step.state.local_frames as f64 / quota as f64);
                    let mut push =
                        |kind| state.steps.push(GovernorStep { at: step.at, kind, used });
                    match &step.change {
                        GovernorChange::RateLowered { from_fps, to_fps } => {
                            push(GovernorStepKind::LookLessOften {
                                from: f64::from(*from_fps),
                                to: f64::from(*to_fps),
                            });
                        }
                        GovernorChange::EscalationStopped { .. } => {
                            push(GovernorStepKind::StopAsking);
                            state.escalation_stopped_at = Some(step.at);
                        }
                        GovernorChange::Paused { .. } => {
                            push(GovernorStepKind::Pause);
                            state.paused_since = Some(step.at);
                            state.tracks.clear();
                        }
                        // A new period: today's steps start again.
                        GovernorChange::Resumed => {
                            state.paused_since = None;
                            state.steps.clear();
                        }
                        GovernorChange::RateRestored { .. } => state.steps.clear(),
                        GovernorChange::EscalationResumed => {
                            state.escalation_stopped_at = None;
                            state.steps.clear();
                        }
                        _ => {}
                    }
                    tracing::info!(camera.id = %run.base.id, change = ?step.change, frames.today = step.state.local_frames, "governor step");
                }
                SourceEvent::Stopped { .. } => {
                    state.tracks.clear();
                    state.centres.clear();
                }
                _ => {}
            }
        }
        if let Some(hub) = self.hub.upgrade() {
            hub.changed(&run.base.id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_error_never_shows_a_credential() {
        assert_eq!(
            without_credentials("connection refused · rtsp://admin:hunter2@10.0.4.21:554/stream1"),
            "connection refused · rtsp://[credentials hidden]@10.0.4.21:554/stream1"
        );
        assert_eq!(without_credentials("rtsp://10.0.4.21/s"), "rtsp://10.0.4.21/s");
        assert_eq!(without_credentials("no url here"), "no url here");
    }

    #[test]
    fn a_privacy_hooks_boxes_are_named_for_what_it_hides() {
        assert_eq!(hidden_label("redactor:faces"), "face");
        assert_eq!(hidden_label("redactor:people"), "person");
        assert_eq!(hidden_label("none"), "person");
    }
}
