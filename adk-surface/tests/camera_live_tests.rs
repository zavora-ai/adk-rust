//! The live camera surface on the real frame pipeline (spec 3.7 §7.13, §8.2, S12; feature
//! `vision-camera`): what the surface shows is what each camera's `StreamPipeline` measured —
//! the frame with its tracked regions, the governor's rate, quota and steps, the watch
//! conditions and their zones — and a person's acts reach the pipeline: a zone edited on the
//! surface changes what fires, switching off drops frames before the pipeline sees them, Stop
//! all pauses every camera at the hub's gate, and a card keeps the shown frame with everyone in
//! it covered.

#![cfg(all(feature = "web", feature = "vision-camera"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration as StdDuration;

use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest, LoadRequest};
use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, Event, InMemoryApprovalStore, Part,
    StoreApprovalService,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::camera::{
    CameraFacts, CameraSource, CameraState, Cameras, DropReason, GovernorStepKind, Ingest,
    LiveCamera, LiveWatch, Offered, PipelineCameras, RaiseCard, StreamState, ZoneChange, ZoneFacts,
};
use adk_surface::{Caller, Command, LocalSource, Origin, Place, SurfaceEngine, SurfaceKind};
use adk_vision::stream::{
    CameraFeed, FrameBudget, FrameSource, FrameStream, NoRedaction, SurfaceCameraSource,
};
use adk_vision::{
    BBox, BackendDescriptor, CoordFrame, DecodeLimits, DecodedImage, DetectQuery, Detector,
    FrameSourceId, GreedyIouTracker, Locality, RawRegion, Sampling, SentImage, VisionCx,
    VisionError, VisionRuntime, WatchCondition,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

const APP: &str = "inspector";
const OWNER: &str = "ana";
const SESSION: &str = "s-line";
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

/// Finds the white block (a "part") and the red block (a "person") on a frame.
struct Blocks(BackendDescriptor);

fn bounds(image: &DecodedImage, keep: impl Fn([u8; 4]) -> bool) -> Option<BBox> {
    let (width, height) = image.size();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..height {
        for x in 0..width {
            if keep(image.pixel(x, y)) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (x0 < x1).then(|| BBox::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32))
}

#[async_trait]
impl Detector for Blocks {
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
        let mut found = Vec::new();
        if let Some(bbox) = bounds(pixels, |[r, g, b, _]| r > 200 && g > 200 && b > 200) {
            found.push(RawRegion::boxed(bbox, "part", 0.93));
        }
        if let Some(bbox) = bounds(pixels, |[r, g, b, _]| r > 200 && g < 80 && b < 80) {
            found.push(RawRegion::boxed(bbox, "person", 0.9));
        }
        Ok(found)
    }
}

fn runtime() -> Arc<VisionRuntime> {
    let detector = Arc::new(Blocks(BackendDescriptor::new(
        "blocks",
        Locality::InProcess,
        CoordFrame::SentPixels,
    )));
    Arc::new(VisionRuntime::builder().detector(detector).build().unwrap())
}

/// A frame with a white part at `x` and, when asked, a red person on the left.
fn frame(x: u32, person: bool) -> Vec<u8> {
    let mut rgba = DecodedImage::filled(WIDTH, HEIGHT, [30, 30, 30, 255]).rgba().to_vec();
    let mut paint = |x0: u32, y0: u32, x1: u32, y1: u32, colour: [u8; 4]| {
        for y in y0..y1.min(HEIGHT) {
            for px in x0..x1.min(WIDTH) {
                let at = ((y * WIDTH + px) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&colour);
            }
        }
    };
    paint(x, 70, x + 40, 110, [255, 255, 255, 255]);
    if person {
        paint(10, 20, 50, 160, [240, 20, 20, 255]);
    }
    DecodedImage::from_rgba(WIDTH, HEIGHT, rgba).unwrap().encode_png().unwrap()
}

/// 09:00 UTC today: frames carry today's date, as "woke today" counts by it, at an hour whose
/// end the governor's hourly test budgets renew at.
fn start() -> DateTime<Utc> {
    Utc::now().date_naive().and_hms_opt(9, 0, 0).unwrap().and_utc()
}

fn facts() -> CameraFacts {
    CameraFacts::new("line-2", "Line 2 camera", APP, SESSION)
        .with_place("line 2")
        .passing("parts")
        .with_ingest(Ingest::rtsp(WIDTH, HEIGHT, "the test"))
}

fn zone(corners: [f64; 4]) -> ZoneFacts {
    ZoneFacts { id: "window".into(), name: "Inspection window".into(), corners }
}

fn caller() -> Caller {
    Caller::human(OWNER, SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

struct Home {
    engine: SurfaceEngine,
    cameras: Cameras,
    live: Arc<PipelineCameras>,
    feed: CameraFeed,
    artifacts: Arc<InMemoryArtifactService>,
    audit: InMemoryAuditRecorder,
    sessions: Arc<dyn SessionService>,
}

/// The Inspector: each turn says it is looking.
struct Inspector;

#[async_trait]
impl adk_core::Agent for Inspector {
    fn name(&self) -> &str {
        APP
    }

    fn description(&self) -> &str {
        "watches line 2"
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
            Some(adk_core::Content::new("model").with_text("Looking at it."));
        Ok(Box::pin(futures::stream::once(async move { Ok(event) })))
    }
}

/// A home whose line 2 camera reads frames the test pushes, through `configure`'s pipeline.
async fn home(configure: impl FnOnce(LiveCamera) -> LiveCamera) -> Home {
    let (source, feed) = SurfaceCameraSource::new(FrameSourceId("line-2".into()));
    feed.switch(true);
    let camera = configure(
        LiveCamera::stream(facts(), Arc::new(source), Arc::new(NoRedaction))
            .detector(runtime(), DetectQuery::Classes(vec!["part".into(), "person".into()]))
            .tracker(|| Box::new(GreedyIouTracker::new(0.2, 3)))
            .sampling(Sampling { fps: 5.0, max_long_edge: None }),
    );
    home_with(camera, feed).await
}

async fn home_with(camera: LiveCamera, feed: CameraFeed) -> Home {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let approvals: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        ));
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(Arc::new(Inspector))
        .session_service(Arc::clone(&sessions))
        .build()
        .unwrap();
    let governance = runner.governance().clone();
    let audit = InMemoryAuditRecorder::new();
    let live = PipelineCameras::builder(APP, OWNER)
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .governance(governance)
        .audit(AuditTrail::new(Arc::new(audit.clone())), AuditActor::default())
        .camera(camera)
        .build()
        .unwrap();
    let cameras = live.start();
    let local = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(approvals)
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .runner(Arc::new(runner))
        .build()
        .unwrap();
    let engine = SurfaceEngine::builder(Arc::new(local))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Inspector")
        .utc_offset(chrono::FixedOffset::east_opt(0).unwrap())
        .cameras(cameras.clone())
        .build();
    Home { engine, cameras, live, feed, artifacts, audit, sessions }
}

/// Waits, up to five seconds, for `done`.
async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    for _ in 0..500 {
        if done().await {
            return;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

impl Home {
    fn counts(&self) -> adk_surface::camera::FrameCounts {
        self.live.counts("line-2").unwrap()
    }

    /// Pushes `bytes` captured `at` and waits until the pipeline has read it. The camera takes
    /// frames once its pipeline reads it.
    async fn push(&self, bytes: Vec<u8>, at: DateTime<Utc>) {
        let arrived = self.counts().arrived;
        until("the test camera to take the frame", async || {
            self.feed.push_frame(bytes.clone(), at)
        })
        .await;
        until("the frame to arrive", async || self.counts().arrived > arrived).await;
    }

    /// Waits until the pipeline is done with every frame that arrived.
    async fn settle(&self) {
        until("the pipeline to settle", async || {
            let counts = self.counts();
            counts.observed + counts.dropped == counts.arrived
        })
        .await;
    }

    /// Pushes a part moving right 8 pixels a frame, `frames` frames `apart` from `from`.
    async fn belt(&self, from: DateTime<Utc>, x: u32, frames: u32, apart: chrono::Duration) {
        for step in 0..frames {
            self.push(frame(x + step * 8, false), from + apart * i32::try_from(step).unwrap())
                .await;
        }
    }

    async fn facts(&self) -> CameraFacts {
        self.live.cameras().await.unwrap().into_iter().find(|camera| camera.id == "line-2").unwrap()
    }
}

/// S12: the picture, its overlays, the tracks and the governor's numbers are the pipeline's.
#[tokio::test(flavor = "multi_thread")]
async fn the_surface_shows_what_the_pipeline_measured() {
    let home = home(|camera| camera.budget(FrameBudget::new(5.0).with_min_local_fps(1.0))).await;
    home.belt(start(), 100, 6, chrono::Duration::milliseconds(200)).await;
    home.settle().await;
    assert_eq!(home.counts().observed, 6);

    let held = home.cameras.latest("line-2").await.expect("a frame is shown");
    assert_eq!(held.frame.mime_type, "image/jpeg");
    assert_eq!((held.frame.width, held.frame.height), (WIDTH, HEIGHT));
    let part = held.frame.overlays.iter().find(|overlay| overlay.label == "part").unwrap();
    let [x0, y0, x1, y1] = part.bbox.unwrap();
    assert_eq!([x0, y0, x1, y1], [140.0, 70.0, 180.0, 110.0], "the newest frame's part");
    assert!(part.track.is_some(), "an anonymous track");
    // The shown frame is the pipeline's, as a JPEG of the camera's size.
    let (shown, _) = DecodedImage::decode(&held.frame.bytes, &DecodeLimits::default()).unwrap();
    assert_eq!(shown.size(), (WIDTH, HEIGHT));

    let facts = home.facts().await;
    assert_eq!(facts.state, StreamState::Live);
    assert_eq!(facts.tracks.len(), 1);
    assert_eq!(facts.tracks[0].label, "part");
    assert_eq!(facts.governor.rate, 5.0);
    assert_eq!(facts.governor.frames_today, 6, "frames the governor let through");
    assert_eq!(facts.governor.min_rate, Some(1.0));

    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Live);
    assert_eq!(view.stage.regions.len(), 1);
    assert!(view.stage.rate.as_deref().unwrap().starts_with("Looking 5 a second"));
    assert_eq!(view.tracks.len(), 1);
}

/// S11 on the surface: under a small quota the governor steps down at once, and the board's
/// "Saving frames" state, its steps and then "Paused" come from the governor's own numbers.
#[tokio::test(flavor = "multi_thread")]
async fn a_small_quota_steps_the_governor_down_and_the_surface_says_so() {
    let home = home(|camera| {
        camera
            .budget(
                FrameBudget::new(5.0)
                    .with_min_local_fps(1.0)
                    .with_local_quota(6)
                    .with_period_secs(3_600)
                    .with_escalations_per_minute(4)
                    .with_escalation_quota(500),
            )
            .escalation("sam-3.1", 2_000)
    })
    .await;
    home.push(frame(40, false), start()).await;
    home.push(frame(48, false), start() + chrono::Duration::seconds(1)).await;
    until("the first steps", async || home.facts().await.governor.steps.len() >= 2).await;

    let facts = home.facts().await;
    let kinds: Vec<GovernorStepKind> =
        facts.governor.steps.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(
        kinds,
        [GovernorStepKind::LookLessOften { from: 5.0, to: 1.0 }, GovernorStepKind::StopAsking],
        "6 frames cannot last the hour even at 1 a second"
    );
    assert_eq!(facts.governor.rate, 1.0);
    assert_eq!(facts.governor.frames_limit, 6);
    assert_eq!(facts.governor.renews_at, start() + chrono::Duration::hours(1));
    let escalation = facts.governor.escalation.as_ref().unwrap();
    assert_eq!(escalation.stopped_at, Some(start()));
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::SavingFrames);
    let notice = view.notice.unwrap();
    assert_eq!(notice.title, "Saving frames.");
    assert!(
        notice.text.starts_with("Looking once a second instead of 5, and no longer asking sam-3.1"),
        "{}",
        notice.text
    );
    let steps: Vec<&str> = view.budget.steps.iter().map(|step| step.text.as_str()).collect();
    assert_eq!(
        steps,
        [
            "Looked less often: 5 → 1 a second (0% used)",
            "Stopped asking sam-3.1 (0% used)",
            "At 100%: pause the camera until 10:00",
        ]
    );

    // An escalation is refused once the governor stopped asking.
    let refused =
        home.live.escalate_at("line-2", start() + chrono::Duration::seconds(1), 50).await.unwrap();
    assert!(
        matches!(refused, adk_vision::stream::EscalationVerdict::Refused { .. }),
        "{refused:?}"
    );

    // The sixth frame spends the quota: the camera pauses until the hour renews.
    for second in 2..8 {
        home.push(
            frame(40 + second * 8, false),
            start() + chrono::Duration::seconds(i64::from(second)),
        )
        .await;
    }
    until("the pause", async || matches!(home.facts().await.state, StreamState::Paused { .. }))
        .await;
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Paused);
    assert_eq!(view.state_text, "Paused until 10:00");
    assert_eq!(home.facts().await.governor.frames_today, 6);
}

/// A zone a person edits on the surface becomes the watch condition's zone from the next frame:
/// a part outside the old zone fires once the zone covers it.
#[tokio::test(flavor = "multi_thread")]
async fn a_zone_edited_on_the_surface_changes_what_fires() {
    let home = home(|camera| {
        camera
            .budget(FrameBudget::new(5.0).with_min_local_fps(1.0))
            .zone(zone([0.0, 0.0, 80.0, 180.0]))
            .watch(
                LiveWatch::new(
                    "part",
                    "Part",
                    WatchCondition::new("part", ["part"]).with_min_frames(3),
                )
                .in_zone("window"),
            )
    })
    .await;
    let apart = chrono::Duration::milliseconds(200);
    // The part passes right of the zone: nothing fires.
    home.belt(start(), 200, 6, apart).await;
    until("the frames observed", async || home.counts().observed >= 5).await;
    assert_eq!(home.counts().fired, 0);
    let facts = home.facts().await;
    assert_eq!(facts.watches[0].woke_today, 0);
    assert_eq!(facts.tracks[0].phase, adk_surface::camera::TrackPhase::InView);

    // The person moves the zone over the part's path.
    let saved = home
        .engine
        .save_camera_zone(
            &caller(),
            "line-2",
            "window",
            &ZoneChange {
                name: "Inspection window".into(),
                corners: [180.0, 40.0, 320.0, 140.0],
                was: [0.0, 0.0, 80.0, 180.0],
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.zone.corners, [180.0, 40.0, 320.0, 140.0]);
    home.belt(start() + chrono::Duration::seconds(2), 200, 6, apart).await;
    until("the watch to fire", async || home.counts().fired == 1).await;
    let facts = home.facts().await;
    assert_eq!(facts.zones[0].corners, [180.0, 40.0, 320.0, 140.0]);
    assert_eq!(facts.watches[0].woke_today, 1);
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.watches[0].woke, "Woke the Inspector once today");
    // The new condition and its firing are recorded.
    let actions: Vec<String> =
        home.audit.records().into_iter().map(|record| record.action).collect();
    assert_eq!(
        actions.iter().filter(|a| *a == "vision.watch.registered").count(),
        2,
        "{actions:?}"
    );
    assert_eq!(actions.iter().filter(|a| *a == "vision.watch.fired").count(), 1, "{actions:?}");
}

/// S12: switching a camera off drops its frames on the server before the pipeline sees them,
/// and nothing is held for the surface.
#[tokio::test(flavor = "multi_thread")]
async fn switching_off_drops_frames_on_the_server() {
    let home = home(|camera| camera).await;
    home.belt(start(), 100, 2, chrono::Duration::milliseconds(200)).await;
    home.settle().await;
    home.engine.switch_camera(&caller(), "line-2", false).await.unwrap();
    assert!(home.cameras.latest("line-2").await.is_none(), "off forgets the frames held");
    let before = home.counts();
    // The source is closed, so the test camera takes nothing; a frame offered from anywhere
    // is dropped at the gate.
    let later = start() + chrono::Duration::seconds(1);
    assert!(!home.feed.push_frame(frame(120, false), later), "the source captures nothing");
    let late =
        adk_surface::camera::CameraFrame::new(frame(120, false), "image/png", WIDTH, HEIGHT, later);
    assert_eq!(home.cameras.offer("line-2", late).await, Offered::Dropped(DropReason::Off));
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    let after = home.counts();
    assert_eq!(after.observed, before.observed, "the pipeline looked at nothing");
    assert_eq!(after.arrived, before.arrived, "nothing arrived");
    assert!(home.cameras.latest("line-2").await.is_none());
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Off);
    assert!(view.tracks.is_empty());
    // Nothing was stored.
    let stored = home
        .artifacts
        .list(ListRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: SESSION.into(),
        })
        .await
        .unwrap();
    assert!(stored.file_names.is_empty(), "{:?}", stored.file_names);

    home.engine.switch_camera(&caller(), "line-2", true).await.unwrap();
    home.belt(start() + chrono::Duration::seconds(2), 140, 2, chrono::Duration::milliseconds(200))
        .await;
    until("frames again", async || home.counts().observed > after.observed).await;
}

/// V-I17: Stop all pauses every camera at the hub's gate and in the pipeline; Resume all opens
/// it again.
#[tokio::test(flavor = "multi_thread")]
async fn stop_all_pauses_every_camera() {
    let home = home(|camera| camera).await;
    home.belt(start(), 100, 2, chrono::Duration::milliseconds(200)).await;
    home.settle().await;
    home.engine.execute(&caller(), Command::StopAll { reason: None }).await.unwrap();
    assert!(home.cameras.is_frozen());
    let before = home.counts();
    home.belt(start() + chrono::Duration::seconds(1), 120, 3, chrono::Duration::milliseconds(200))
        .await;
    assert_eq!(home.counts().observed, before.observed, "no frame is looked at");
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Frozen);
    assert_eq!(view.state_text, "Paused by Stop all");
    assert_eq!(view.stage.cover.unwrap().title, "Paused by Stop all");
    // A frame offered from anywhere is dropped at the gate.
    let offered = home
        .cameras
        .offer(
            "line-2",
            adk_surface::camera::CameraFrame::new(vec![1], "image/jpeg", 1, 1, start()),
        )
        .await;
    assert_eq!(offered, Offered::Dropped(DropReason::Frozen));

    home.engine.execute(&caller(), Command::ResumeAll).await.unwrap();
    assert!(!home.cameras.is_frozen());
    home.belt(start() + chrono::Duration::seconds(2), 140, 2, chrono::Duration::milliseconds(200))
        .await;
    until("frames after Resume all", async || home.counts().observed > before.observed).await;
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Live);
}

/// No frame is stored unless it is evidence on a card, and then everyone the detector found
/// in it is covered — on the screen and in the kept copy.
#[tokio::test(flavor = "multi_thread")]
async fn a_card_keeps_the_shown_frame_with_people_covered() {
    let home = home(|camera| camera).await;
    home.push(frame(200, true), start()).await;
    home.push(frame(208, true), start() + chrono::Duration::milliseconds(200)).await;
    until("a frame observed", async || home.counts().observed >= 1).await;
    let held = home.cameras.latest("line-2").await.unwrap();
    let person = held.frame.overlays.iter().find(|overlay| overlay.label == "person").unwrap();
    assert!(person.blurred, "a person is covered before the frame is shown");
    let (shown, _) = DecodedImage::decode(&held.frame.bytes, &DecodeLimits::default()).unwrap();
    let [r, g, b, _] = shown.pixel(30, 90);
    assert!(r < 30 && g < 30 && b < 30, "the person's pixels are filled: {:?}", [r, g, b]);

    let list =
        || ListRequest { app_name: APP.into(), user_id: OWNER.into(), session_id: SESSION.into() };
    assert!(home.artifacts.list(list()).await.unwrap().file_names.is_empty(), "nothing kept yet");
    home.engine.freeze_camera_frame(&caller(), "line-2", &held.digest).await.unwrap();
    let raised = home
        .engine
        .raise_camera_card(
            &caller(),
            "line-2",
            &RaiseCard { frame: held.digest.clone(), note: "Is this a person on the belt?".into() },
        )
        .await
        .unwrap();
    let kept = home.artifacts.list(list()).await.unwrap().file_names;
    let evidence: Vec<&String> = kept.iter().filter(|name| !name.ends_with(".json")).collect();
    assert_eq!(evidence.len(), 1, "exactly the card's frame: {kept:?}");
    let loaded = home
        .artifacts
        .load(LoadRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: SESSION.into(),
            file_name: evidence[0].clone(),
            version: None,
        })
        .await
        .unwrap();
    let Part::InlineData { data, .. } = loaded.part else { panic!("an image") };
    assert_eq!(format!("sha256:{}", sha256(&data)), raised.digest);
    let (image, _) = DecodedImage::decode(&data, &DecodeLimits::default()).unwrap();
    assert_eq!(image.pixel(30, 90)[..3], [0, 0, 0], "covered again before it was written");
    let actions: Vec<String> =
        home.audit.records().into_iter().map(|record| record.action).collect();
    assert!(actions.contains(&"vision.evidence.kept".to_string()), "{actions:?}");
    // The card reached the Inspector as the person's message naming the frame by digest.
    until("the card in the session", async || {
        card_texts(&home).await.iter().any(|text| text.contains(&raised.digest))
    })
    .await;
}

async fn card_texts(home: &Home) -> Vec<String> {
    let session = home
        .sessions
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: SESSION.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    session
        .events()
        .all()
        .into_iter()
        .filter_map(|event| event.llm_response.content)
        .flat_map(|content| content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text),
            _ => None,
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The voice screen's camera goes through `SurfaceCameraSource`: off, the hub drops what the
/// screen sends; on, the frames reach the pipeline and come back with what it found.
#[tokio::test(flavor = "multi_thread")]
async fn the_voice_screens_camera_goes_through_the_pipeline() {
    let camera = LiveCamera::surface(
        CameraFacts::new("this-computer", "This computer's camera", APP, SESSION)
            .with_ingest(Ingest::this_computer(WIDTH, HEIGHT)),
        Arc::new(NoRedaction),
    )
    .detector(runtime(), DetectQuery::Classes(vec!["part".into()]))
    .sampling(Sampling { fps: 5.0, max_long_edge: None });
    let (_unused, feed) = SurfaceCameraSource::new(FrameSourceId("unused".into()));
    let home = home_with(camera, feed).await;
    let cameras = &home.cameras;
    assert_eq!(cameras.voice_camera().as_deref(), Some("this-computer"));
    let facts = home.live.cameras().await.unwrap();
    assert_eq!(facts[0].from.as_deref(), Some("from the web home"));
    assert!(!facts[0].on, "the voice screen's camera starts off");
    assert_eq!(
        cameras.ingest("this-computer", frame(100, false), start()).await,
        Offered::Dropped(DropReason::Off)
    );
    home.engine.switch_camera(&caller(), "this-computer", true).await.unwrap();
    assert_eq!(
        cameras.ingest("this-computer", frame(100, false), start()).await,
        Offered::Forwarded
    );
    until("the frame shown", async || cameras.latest("this-computer").await.is_some()).await;
    let held = cameras.latest("this-computer").await.unwrap();
    assert_eq!(held.frame.overlays[0].label, "part");
    let counts = home.live.counts("this-computer").unwrap();
    assert_eq!((counts.arrived, counts.observed), (1, 1));
}

/// A source that refuses to open twice, its error naming a URL with credentials, then reads
/// the test's frames.
struct Flaky {
    id: FrameSourceId,
    failures: AtomicU32,
    inner: SurfaceCameraSource,
}

#[async_trait]
impl FrameSource for Flaky {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
        let failing = self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok();
        if failing {
            return Err(VisionError::InvalidInput(
                "connection refused · rtsp://admin:hunter2@10.0.4.21:554/stream1".into(),
            ));
        }
        self.inner.frames(sampling).await
    }
}

/// A lost source is reported with its attempt and a redacted error, tried again at once from
/// "Try again now", and live again with its first frame.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_source_is_retried_and_reported() {
    let (inner, feed) = SurfaceCameraSource::new(FrameSourceId("line-2".into()));
    feed.switch(true);
    let source = Flaky { id: FrameSourceId("line-2".into()), failures: AtomicU32::new(2), inner };
    let camera = LiveCamera::stream(facts(), Arc::new(source), Arc::new(NoRedaction))
        .detector(runtime(), DetectQuery::Classes(vec!["part".into()]))
        .sampling(Sampling { fps: 5.0, max_long_edge: None });
    let home = home_with(camera, feed).await;
    until("the camera lost", async || matches!(home.facts().await.state, StreamState::Lost { .. }))
        .await;
    let StreamState::Lost { attempt, error, .. } = home.facts().await.state else { unreachable!() };
    assert_eq!(attempt, 1);
    assert!(
        error.ends_with("connection refused · rtsp://[credentials hidden]@10.0.4.21:554/stream1"),
        "{error}"
    );
    assert!(!error.contains("hunter2"), "{error}");
    let view = home.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Lost);
    assert!(view.actions.retry);

    // "Try again now" twice: the second attempt opens the source.
    home.engine.retry_camera(&caller(), "line-2").await.unwrap();
    until("the second attempt", async || {
        matches!(home.facts().await.state, StreamState::Lost { attempt: 2, .. })
    })
    .await;
    home.engine.retry_camera(&caller(), "line-2").await.unwrap();
    // The stream opens; its first frame makes the camera live again.
    for step in 0..50u32 {
        if home.feed.push_frame(
            frame(100, false),
            start() + chrono::Duration::milliseconds(200 * i64::from(step)),
        ) {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    }
    until("live again", async || home.facts().await.state == StreamState::Live).await;
}

/// A stream camera's source that counts the streams it holds open and those it ever opened.
struct Counted {
    id: FrameSourceId,
    inner: SurfaceCameraSource,
    open: Arc<AtomicU32>,
    opened: Arc<AtomicU32>,
}

/// One open stream of a [`Counted`] source, until it is dropped.
struct Open(Arc<AtomicU32>);

impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl FrameSource for Counted {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
        use futures::StreamExt as _;
        let stream = self.inner.frames(sampling).await?;
        self.opened.fetch_add(1, Ordering::SeqCst);
        self.open.fetch_add(1, Ordering::SeqCst);
        let open = Open(Arc::clone(&self.open));
        Ok(stream
            .map(move |frame| {
                let _held = &open;
                frame
            })
            .boxed())
    }
}

/// The `reason` of every `action` record the cameras' trail holds.
fn reasons(home: &Home, action: &str) -> Vec<serde_json::Value> {
    home.audit
        .records()
        .into_iter()
        .filter(|record| record.action == action)
        .map(|record| record.ext.get("reason").cloned().unwrap_or_default())
        .collect()
}

/// Off stops a stream camera's capture at its source — the stream is closed before the switch
/// answers, so a device's ffmpeg exits — and nothing opens it until the camera is on again;
/// each is recorded as `vision.source.stopped` and `.started`.
#[tokio::test(flavor = "multi_thread")]
async fn switching_off_closes_the_source_and_on_opens_it_again() {
    let (inner, feed) = SurfaceCameraSource::new(FrameSourceId("line-2".into()));
    feed.switch(true);
    let open = Arc::new(AtomicU32::new(0));
    let opened = Arc::new(AtomicU32::new(0));
    let source = Counted {
        id: FrameSourceId("line-2".into()),
        inner,
        open: Arc::clone(&open),
        opened: Arc::clone(&opened),
    };
    let camera = LiveCamera::stream(facts(), Arc::new(source), Arc::new(NoRedaction))
        .detector(runtime(), DetectQuery::Classes(vec!["part".into()]))
        .sampling(Sampling { fps: 5.0, max_long_edge: None });
    let home = home_with(camera, feed).await;
    home.belt(start(), 100, 2, chrono::Duration::milliseconds(200)).await;
    home.settle().await;
    assert_eq!(home.live.is_capturing("line-2"), Some(true));
    assert_eq!((open.load(Ordering::SeqCst), opened.load(Ordering::SeqCst)), (1, 1));

    home.engine.switch_camera(&caller(), "line-2", false).await.unwrap();
    assert_eq!(open.load(Ordering::SeqCst), 0, "closed before the switch answered");
    assert_eq!(home.live.is_capturing("line-2"), Some(false));
    assert!(!home.feed.push_frame(frame(120, false), start()), "nothing reads the source");
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    assert_eq!(opened.load(Ordering::SeqCst), 1, "nothing opens it while the camera is off");
    assert_eq!(
        reasons(&home, adk_vision::stream::VISION_SOURCE_STOPPED),
        [serde_json::json!("switchedOff")]
    );
    assert!(
        reasons(&home, adk_vision::stream::VISION_SOURCE_STARTED)
            .iter()
            .all(|reason| reason.is_null())
    );

    home.engine.switch_camera(&caller(), "line-2", true).await.unwrap();
    until("the source opened again", async || open.load(Ordering::SeqCst) == 1).await;
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    let before = home.counts();
    home.belt(start() + chrono::Duration::seconds(2), 140, 2, chrono::Duration::milliseconds(200))
        .await;
    until("frames again", async || home.counts().observed > before.observed).await;
    assert_eq!(home.live.is_capturing("line-2"), Some(true));
    let started = reasons(&home, adk_vision::stream::VISION_SOURCE_STARTED);
    assert_eq!(started.iter().filter(|reason| *reason == "switchedOn").count(), 1, "{started:?}");

    // Stopping the stream closes it for good.
    home.engine.stop_camera(&caller(), "line-2").await.unwrap();
    until("the source closed", async || open.load(Ordering::SeqCst) == 0).await;
}

fn api(engine: SurfaceEngine, device: bool) -> axum::Router {
    let mut metadata = std::collections::HashMap::new();
    if device {
        metadata.insert(adk_kernel::pairing::DEVICE_ID_METADATA.to_string(), "phone-1".to_string());
        metadata.insert(adk_kernel::pairing::DEVICE_KIND_METADATA.to_string(), "phone".to_string());
    }
    axum::Router::new().nest("/api", adk_surface::web::router(engine)).layer(axum::Extension(Some(
        adk_core::RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: metadata.into_iter().collect(),
        },
    )))
}

/// Sends `bytes` as `mime` to `uri`; the status and the answer.
async fn send(
    app: &axum::Router,
    uri: &str,
    mime: &str,
    bytes: Vec<u8>,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt as _;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", mime)
        .body(axum::body::Body::from(bytes))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null))
}

/// The web home's own camera, fed from the camera page with no voice screen and no realtime
/// model.
async fn browser_home() -> Home {
    let camera = LiveCamera::surface(
        CameraFacts::new("this-computer", "This computer's camera", APP, SESSION)
            .with_ingest(Ingest::this_computer(WIDTH, HEIGHT)),
        Arc::new(NoRedaction),
    )
    .detector(runtime(), DetectQuery::Classes(vec!["part".into()]))
    .sampling(Sampling { fps: 5.0, max_long_edge: None });
    let (_unused, feed) = SurfaceCameraSource::new(FrameSourceId("unused".into()));
    home_with(camera, feed).await
}

/// The browser's camera drives the camera's pipeline through `POST
/// /surface/cameras/{camera}/frames`: only for a camera that takes a screen's frames, from its
/// owner at a screen, as an image; switched off it sends nothing, and the answer says so.
#[tokio::test(flavor = "multi_thread")]
async fn the_browsers_camera_feeds_its_camera_through_the_frames_route() {
    let home = browser_home().await;
    let view = home.engine.camera_view(&caller(), "this-computer").await.unwrap();
    assert!(view.actions.send_frames, "the page offers this browser's camera");
    let app = api(home.engine.clone(), false);
    let uri = "/api/surface/cameras/this-computer/frames?surface=web";

    // Off: dropped on the server, and the answer says why.
    let (status, body) = send(&app, uri, "image/png", frame(100, false)).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "taken": false, "dropped": "off" }));

    // On: the frame reaches the pipeline and comes back with what it found.
    home.engine.switch_camera(&caller(), "this-computer", true).await.unwrap();
    let (status, body) = send(&app, uri, "image/png", frame(100, false)).await;
    assert_eq!((status, body), (axum::http::StatusCode::OK, serde_json::json!({ "taken": true })));
    until("the frame shown", async || home.cameras.latest("this-computer").await.is_some()).await;
    let held = home.cameras.latest("this-computer").await.unwrap();
    assert_eq!(held.frame.overlays[0].label, "part");
    assert_eq!(home.live.counts("this-computer").unwrap().observed, 1);

    // Not an image, or not the image its type says: refused before anything sees it.
    let (status, body) = send(&app, uri, "text/plain", b"hello".to_vec()).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "surface.invalid");
    let (status, _) = send(&app, uri, "image/jpeg", frame(100, false)).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "a PNG sent as a JPEG");
    let (status, _) = send(&app, uri, "image/png", Vec::new()).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "an empty frame");
    // Over the size limit: the body is refused.
    let mut huge = frame(100, false);
    huge.resize(adk_surface::camera::MAX_SENT_FRAME_BYTES + 1, 0);
    let (status, _) = send(&app, uri, "image/png", huge).await;
    assert_eq!(status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);

    // Switched off, it takes nothing again; a paired phone switches it on and sends with the
    // decide scope.
    home.engine.switch_camera(&caller(), "this-computer", false).await.unwrap();
    let (_, body) = send(&app, uri, "image/png", frame(110, false)).await;
    assert_eq!(body, serde_json::json!({ "taken": false, "dropped": "off" }));
    assert_eq!(home.live.counts("this-computer").unwrap().observed, 1);
}

/// Frames come at most `MAX_SCREEN_FPS` a second from a screen; a camera that reads its own
/// source takes none; another person's camera is not found.
#[tokio::test(flavor = "multi_thread")]
async fn sent_frames_are_paced_owned_and_only_for_a_screens_camera() {
    let home = browser_home().await;
    home.engine.switch_camera(&caller(), "this-computer", true).await.unwrap();
    let at = start() + chrono::Duration::hours(1);
    // The hub takes one frame per interval, whatever the screen sends.
    let gap =
        chrono::Duration::milliseconds(1_000 / i64::from(adk_surface::camera::MAX_SCREEN_FPS));
    assert_ne!(
        home.cameras.ingest("this-computer", frame(100, false), at).await,
        Offered::Dropped(DropReason::TooSoon)
    );
    assert_eq!(
        home.cameras.ingest("this-computer", frame(100, false), at + gap / 2).await,
        Offered::Dropped(DropReason::TooSoon)
    );
    assert_ne!(
        home.cameras.ingest("this-computer", frame(100, false), at + gap).await,
        Offered::Dropped(DropReason::TooSoon)
    );

    // Over HTTP, a frame too soon is 429 surface.rate_limited: an engine on a still clock.
    let clock = Arc::new(adk_core::ManualClock::new(at + gap * 4));
    let local = LocalSource::builder(APP).sessions(Arc::clone(&home.sessions)).build().unwrap();
    let still = SurfaceEngine::builder(Arc::new(local))
        .places([Place::web(), Place::phone()])
        .clock(clock)
        .cameras(home.cameras.clone())
        .build();
    let uri = "/api/surface/cameras/this-computer/frames?surface=web";
    let app = api(still.clone(), false);
    let (status, body) = send(&app, uri, "image/png", frame(100, false)).await;
    assert_eq!((status, body), (axum::http::StatusCode::OK, serde_json::json!({ "taken": true })));
    let (status, body) = send(&app, uri, "image/png", frame(100, false)).await;
    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "surface.rate_limited");

    // A paired phone sends with the decide scope, which the server's device map requires.
    let phone = api(still.clone(), true);
    let (status, body) = send(
        &phone,
        "/api/surface/cameras/this-computer/frames?surface=phone",
        "image/png",
        frame(100, false),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::TOO_MANY_REQUESTS, "{body}");

    // Another person's camera, or one that reads its own source.
    let ben = Caller::human("ben", SurfaceKind::Web, Origin::Pointer);
    let refused =
        home.engine.send_camera_frame(&ben, "this-computer", "image/png", frame(100, false)).await;
    assert!(matches!(refused, Err(adk_surface::SurfaceError::NotFound { .. })), "{refused:?}");
    let spoken = Caller::human(OWNER, SurfaceKind::Voice, Origin::Voice);
    let refused = home
        .engine
        .send_camera_frame(&spoken, "this-computer", "image/png", frame(100, false))
        .await;
    assert!(matches!(refused, Err(adk_surface::SurfaceError::NotHere(_))), "{refused:?}");
    let stream = home_with(
        LiveCamera::stream(
            facts(),
            Arc::new(SurfaceCameraSource::new(FrameSourceId("line-2".into())).0),
            Arc::new(NoRedaction),
        ),
        SurfaceCameraSource::new(FrameSourceId("unused".into())).1,
    )
    .await;
    let view = stream.engine.camera_view(&caller(), "line-2").await.unwrap();
    assert!(!view.actions.send_frames);
    let refused =
        stream.engine.send_camera_frame(&caller(), "line-2", "image/png", frame(100, false)).await;
    assert!(matches!(refused, Err(adk_surface::SurfaceError::Unsupported { .. })), "{refused:?}");
}
