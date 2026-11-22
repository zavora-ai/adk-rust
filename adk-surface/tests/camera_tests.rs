//! The live camera surface through the engine and the web API (spec 3.7 §8.2, S12): the
//! boards' states are the engine's own projection, only the session's owner sees or acts on a
//! camera, a camera switched off has its frames dropped on the server, and no frame is stored
//! unless it is evidence on a card.
//!
//! The views the board states project are the web fixture's (`web/src/fixture/camera/*.json`),
//! so the screenshots beside the signed-off boards are of the engine's own output;
//! `ADK_SURFACE_WRITE_CAMERA=1` rewrites them.

#![cfg(feature = "web")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService};
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, Event, EventActions, Evidence, InMemoryApprovalStore, ManualClock,
    RequestContext, StoreApprovalService, ToolConfirmationRequest,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::camera::{
    CameraBoard, CameraFrame, CameraState, Cameras, DropReason, Offered, Overlay, RaiseCard,
    ScriptedCameras, StageLook, ZoneChange,
};
use adk_surface::{
    Caller, CardHint, CardHints, CommandOutcome, LocalSource, Origin, Place, SurfaceEngine,
    SurfaceError, SurfaceKind,
};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, FixedOffset};
use serde_json::{Value, json};
use sha2::Digest as _;
use tower::ServiceExt;

const APP: &str = "inspector";
const OWNER: &str = "ana";
const SESSION: &str = "s-line";

fn day() -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339("2026-11-18T00:00:00+00:00").unwrap()
}

/// The stand-in bytes of each picture the boards draw; the web fixture shows its drawing.
fn picture(name: &str) -> Vec<u8> {
    format!("camera-fixture:{name}").into_bytes()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", sha2::Sha256::digest(bytes))
}

/// "Stop line 2", as the Inspector's line-stop call reads.
struct LineStop;

#[async_trait]
impl CardHints for LineStop {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        (request.tool_name == "stop_line").then(|| CardHint::new().with_heading("Stop line 2"))
    }
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

struct Fixture {
    engine: SurfaceEngine,
    cameras: Cameras,
    source: Arc<ScriptedCameras>,
    artifacts: Arc<InMemoryArtifactService>,
    sessions: Arc<dyn SessionService>,
    approvals: Arc<dyn ApprovalService>,
    audit: InMemoryAuditRecorder,
}

async fn fixture(board: CameraBoard) -> Fixture {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    for (user, session) in [(OWNER, SESSION), ("ben", "s-ben")] {
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await
            .unwrap();
    }
    let now = board.now(day());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_clock(Arc::new(ManualClock::new(now)))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let source = Arc::new(
        ScriptedCameras::boards(APP, OWNER, SESSION, board, day())
            .artifacts(artifacts.clone() as Arc<dyn ArtifactService>),
    );
    let cameras = Cameras::new(source.clone());
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(Arc::new(Inspector))
        .session_service(Arc::clone(&sessions))
        .build()
        .unwrap();
    let local = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&approvals))
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .runner(Arc::new(runner))
        .build()
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::new(local))
        .places([Place::web(), Place::phone()])
        .agent_name(APP, "Inspector")
        .utc_offset(FixedOffset::east_opt(0).unwrap())
        .clock(Arc::new(ManualClock::new(now)))
        .card_hints(Arc::new(LineStop))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .cameras(cameras.clone())
        .build();
    Fixture { engine, cameras, source, artifacts, sessions, approvals, audit }
}

/// Offers each camera's frames for `board`, as the boards draw them.
async fn show(fixture: &Fixture, board: CameraBoard) {
    let day = day();
    let line_2 = match board {
        CameraBoard::Live | CameraBoard::SwitchedOff => "line-2-live",
        CameraBoard::SteppedDown => "line-2-down",
        CameraBoard::QuotaExhausted => "line-2-paused",
        CameraBoard::SourceLost => "line-2-live",
        CameraBoard::Later => "line-2-later",
    };
    let frames = |camera: &str, board: CameraBoard, name: &str, at: DateTime<chrono::Utc>| {
        ScriptedCameras::frames(camera, board, &picture(name), "image/png", at)
    };
    let shown = [
        ("line-2", frames("line-2", board, line_2, board.frame_at(day))),
        ("line-3", frames("line-3", board, "line-3", board.now(day) - Duration::seconds(1))),
        (
            "packing-bench",
            frames("packing-bench", board, "packing-bench", board.now(day) - Duration::seconds(1)),
        ),
    ];
    for (camera, frames) in shown {
        ScriptedCameras::show(&fixture.cameras, camera, frames).await;
    }
    if board == CameraBoard::SwitchedOff {
        let at = (day + Duration::hours(11) + Duration::minutes(5)).to_utc();
        fixture.cameras.switch("line-2", false, OWNER, at).await.unwrap();
    }
}

/// The Inspector's held line stop with the frame on the card (board choice C13).
async fn hold_line_stop(fixture: &Fixture) -> ApprovalRequest {
    let held = fixture.cameras.latest("line-2").await.expect("a frame is held");
    let facts = fixture.source.camera("line-2").await.unwrap();
    let mut evidence =
        adk_surface::camera::CameraSource::keep_frame(fixture.source.as_ref(), &facts, &held)
            .await
            .unwrap();
    evidence.caption = Some("Seal tear of 14.3 mm on part T43.".into());
    let call =
        ToolConfirmationRequest::new("stop_line", json!({ "line": 2, "frame": evidence.digest }))
            .with_evidence([Evidence::from(evidence)]);
    let mut request = ApprovalRequest::new(
        "dec-line-2",
        ApprovalScope::new(APP, OWNER, SESSION),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    );
    request.summary = "Seal tear of 14.3 mm on part T43. The frame is on the card.".into();
    request.risk = ApprovalRisk::High;
    request.created_at = (day() + Duration::hours(10) + Duration::minutes(41)).to_utc();
    let mut event = Event::new("inv-1");
    event.author = APP.into();
    event.actions = EventActions { tool_confirmation: Some(call), ..EventActions::default() };
    fixture.sessions.append_event(SESSION, event).await.unwrap();
    fixture.approvals.submit(request).await.unwrap()
}

fn owner(surface: SurfaceKind, origin: Origin) -> Caller {
    Caller::human(OWNER, surface, origin).with_groups(["owner"])
}

fn web() -> Caller {
    owner(SurfaceKind::Web, Origin::Pointer)
}

fn web_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("web/src/fixture/camera/{name}.json"))
}

/// Writes `value` to the web fixture `name` with `ADK_SURFACE_WRITE_CAMERA=1`, else checks the
/// committed one is the same.
fn matches_fixture(name: &str, value: &Value) {
    let path = web_fixture(name);
    if std::env::var_os("ADK_SURFACE_WRITE_CAMERA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(value).unwrap() + "\n").unwrap();
        return;
    }
    let drawn: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        &drawn,
        value,
        "{name}: run with ADK_SURFACE_WRITE_CAMERA=1 to rewrite {}",
        path.display()
    );
}

#[tokio::test]
async fn the_boards_states_are_the_engines_own_projection() {
    let boards = [
        ("live", CameraBoard::Live),
        ("stepped-down", CameraBoard::SteppedDown),
        ("quota-exhausted", CameraBoard::QuotaExhausted),
        ("source-lost", CameraBoard::SourceLost),
        ("switched-off", CameraBoard::SwitchedOff),
    ];
    for (name, board) in boards {
        let fixture = fixture(board).await;
        show(&fixture, board).await;
        if board == CameraBoard::Live {
            hold_line_stop(&fixture).await;
            let several = fixture.engine.cameras_view(&web()).await.unwrap();
            matches_fixture("several-sources", &serde_json::to_value(&several).unwrap());
        }
        let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
        matches_fixture(name, &serde_json::to_value(&view).unwrap());
    }
    // The frozen frame a card is raised from (camera-zone-and-card-web).
    let fixture = fixture(CameraBoard::Later).await;
    show(&fixture, CameraBoard::Later).await;
    let stage = fixture.engine.camera_stage(&web(), "line-2").await.unwrap();
    matches_fixture("later-stage", &serde_json::to_value(&stage).unwrap());
    // Which drawing each stand-in picture is.
    let names =
        ["line-2-live", "line-2-down", "line-2-paused", "line-2-later", "line-3", "packing-bench"];
    let pictures: serde_json::Map<String, Value> =
        names.iter().map(|name| (digest(&picture(name)), json!(name))).collect();
    matches_fixture("pictures", &Value::Object(pictures));
}

#[tokio::test]
async fn live_reads_as_the_board() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    hold_line_stop(&fixture).await;
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Live);
    assert_eq!(view.state_text, "Live");
    assert_eq!(view.ingest, "RTSP · 1920 × 1080 · on the line 2 server");
    assert_eq!(view.agent.line, "Asleep until a defect");
    assert_eq!(view.stage.badge.as_ref().unwrap().text, "Live · 10:41:09");
    assert_eq!(view.stage.rate.as_deref(), Some("Looking 5 a second · overlays 0.1 s behind"));
    let tags: Vec<&str> = view.stage.regions.iter().map(|region| region.tag.as_str()).collect();
    assert_eq!(
        tags,
        [
            "part · T45",
            "part · T43",
            "part · T42",
            "part · T41",
            "",
            "seal tear · 0.91 · 5 frames · 14.3 mm",
            "fiducial"
        ]
    );
    assert_eq!(view.stage.trails.len(), 4, "every part has its last second");
    assert_eq!(view.decisions.len(), 1);
    assert_eq!(view.decisions[0].heading, "Stop line 2");
    assert!(view.decisions[0].media.is_some(), "the frame is on the card");
    let watch = &view.watches[0];
    assert_eq!(
        format!("{}{}{}", watch.before, watch.zone.as_deref().unwrap_or_default(), watch.after),
        "Score 0.80 or more, 3 frames in a row, insideInspection window. Then wait 30 s before waking again for the same track."
    );
    assert_eq!(watch.woke, "Woke the Inspector 3 times today");
    let meters: Vec<(&str, &str)> = view
        .budget
        .meters
        .iter()
        .map(|meter| (meter.value.as_str(), meter.note.as_deref().unwrap_or("")))
        .collect();
    assert_eq!(
        meters,
        [
            ("5 a second", "As set"),
            ("2 in the last minute", "At most 6 a minute · 14 today · $0.14"),
            ("61,240 of 300,000", "Renews at midnight"),
        ]
    );
    assert_eq!(
        view.budget.order.as_deref(),
        Some(
            "Steps down in this order: look less often, then stop asking sam-3.1, then pause the camera until the budget renews at midnight."
        )
    );
    let tracks: Vec<(&str, &str)> =
        view.tracks.iter().map(|track| (track.id.as_str(), track.note.as_str())).collect();
    assert_eq!(
        tracks,
        [
            ("T43", "seal tear 0.91 · in the zone · 5 frames"),
            ("T42", "in the zone"),
            ("T45", "entering"),
            ("T41", "leaving")
        ]
    );
    let sources: Vec<(&str, &str)> =
        view.sources.iter().map(|tile| (tile.name.as_str(), tile.state_text.as_str())).collect();
    assert_eq!(
        sources,
        [
            ("Line 2 camera", "Live"),
            ("Line 3 camera", "Saving frames"),
            ("Packing bench", "Live"),
            ("This computer's camera", "Off")
        ]
    );
    assert!(
        view.actions.raise_card && view.actions.zone && view.actions.switch && view.actions.stop
    );
}

#[tokio::test]
async fn the_governor_steps_down_then_pauses_as_the_boards_say() {
    let fixture = fixture(CameraBoard::SteppedDown).await;
    show(&fixture, CameraBoard::SteppedDown).await;
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.state_text, "Saving frames");
    let notice = view.notice.unwrap();
    assert_eq!(
        format!("{} {}", notice.title, notice.text),
        "Saving frames. Looking 2 times a second instead of 5, and no longer asking sam-3.1 about unclear tracks. Today's frames are 96% used."
    );
    assert_eq!(
        notice.short_text,
        "Looking 2 a second, not asking sam-3.1. 96% of today's frames used."
    );
    let steps: Vec<(&str, &str)> =
        view.budget.steps.iter().map(|step| (step.at.as_str(), step.text.as_str())).collect();
    assert_eq!(
        steps,
        [
            ("14:02", "Looked less often: 5 → 2 a second (90% used)"),
            ("14:31", "Stopped asking sam-3.1 (95% used)"),
            ("next", "At 100%: pause the camera until midnight"),
        ]
    );
    let rows: Vec<&str> = view.budget.rows.iter().map(|row| row.value.as_str()).collect();
    assert_eq!(rows, ["2 a second (was 5)", "Stopped at 14:31", "287,400 of 300,000"]);

    let fixture = self::fixture(CameraBoard::QuotaExhausted).await;
    show(&fixture, CameraBoard::QuotaExhausted).await;
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.state_text, "Paused until midnight");
    assert_eq!(view.stage.look, StageLook::Dimmed);
    assert_eq!(view.stage.badge.unwrap().text, "Last frame · 15:12:03");
    assert!(view.stage.regions.is_empty(), "nothing is looked at while paused");
    let notice = view.notice.unwrap();
    assert_eq!(notice.title, "Parts on line 2 are passing without inspection.");
    assert_eq!(view.budget.limit.as_deref(), Some("Change the daily frame limit"));
    assert_eq!(view.budget.steps.last().unwrap().text, "Paused the camera until midnight");
    assert_eq!(view.tracks_note, "No tracks while paused.");
    assert!(!view.actions.raise_card);
}

#[tokio::test]
async fn a_lost_source_hides_its_credentials_and_offers_to_try_again() {
    let fixture = fixture(CameraBoard::SourceLost).await;
    show(&fixture, CameraBoard::SourceLost).await;
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.stage.look, StageLook::Grey);
    let cover = view.stage.cover.unwrap();
    assert_eq!(cover.title, "Can't reach the line 2 camera");
    assert_eq!(cover.text, "Since 10:42:07 · trying again in 8 s · attempt 3");
    let notice = view.notice.unwrap();
    assert_eq!(
        notice.detail.as_deref(),
        Some("connection refused · rtsp://[credentials hidden]@10.0.4.21:554/stream1")
    );
    assert!(view.actions.retry && !view.actions.zone && !view.actions.raise_card);
    fixture.engine.retry_camera(&web(), "line-2").await.unwrap();
    assert_eq!(fixture.source.log().await, ["retry line-2"]);
}

#[tokio::test]
async fn switching_off_drops_frames_on_the_server() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let frame = |name: &str| {
        CameraFrame::new(picture(name), "image/png", 1920, 1080, CameraBoard::Live.now(day()))
            .with_overlays(vec![Overlay::boxed("part", [1.0, 1.0, 10.0, 10.0]).on_track(9)])
    };
    let held = fixture.cameras.latest("line-2").await.unwrap();
    let view = fixture.engine.switch_camera(&web(), "line-2", false).await.unwrap();
    assert_eq!(view.state, CameraState::Off);
    assert_eq!(view.stage.look, StageLook::Dark);
    assert!(view.stage.frame.is_none(), "nothing is shown");
    assert_eq!(
        view.stage.cover.unwrap().text,
        "Nothing is captured or sent. You switched line 2 off at 10:41."
    );
    assert_eq!(view.watches[0].pill, "Camera off");
    assert!(view.tracks.is_empty(), "tracks cleared");

    // Whatever arrives while it is off is dropped and never held.
    for index in 0..3 {
        let offered = fixture.cameras.offer("line-2", frame(&format!("after off {index}"))).await;
        assert_eq!(offered, Offered::Dropped(DropReason::Off));
    }
    assert_eq!(fixture.cameras.dropped("line-2").await, 3);
    assert!(fixture.cameras.latest("line-2").await.is_none(), "off forgets every frame held");
    let gone = fixture.engine.camera_frame(&web(), "line-2", &held.digest).await;
    assert!(matches!(gone, Err(SurfaceError::NotFound { .. })), "{gone:?}");

    // On again, frames pass; the record says who switched and how.
    fixture.engine.switch_camera(&web(), "line-2", true).await.unwrap();
    assert!(matches!(
        fixture.cameras.offer("line-2", frame("after on")).await,
        Offered::Shown { .. }
    ));
    let switches: Vec<Value> = fixture
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::camera::SURFACE_AUDIT_CAMERA_SWITCH)
        .map(|record| record.ext["on"].clone())
        .collect();
    assert_eq!(switches, [json!(false), json!(true)]);
}

#[tokio::test]
async fn a_stopped_stream_takes_no_frames() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let view = fixture.engine.stop_camera(&web(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Stopped);
    assert!(!view.actions.switch && !view.actions.stop);
    let frame =
        CameraFrame::new(picture("x"), "image/png", 1920, 1080, CameraBoard::Live.now(day()));
    assert_eq!(fixture.cameras.offer("line-2", frame).await, Offered::Dropped(DropReason::Stopped));
}

#[tokio::test]
async fn no_frame_is_stored_unless_it_is_evidence_on_a_card() {
    let fixture = fixture(CameraBoard::Later).await;
    show(&fixture, CameraBoard::Later).await;
    // Frames shown, the camera switched, a zone changed: nothing is written anywhere.
    fixture.engine.switch_camera(&web(), "line-2", false).await.unwrap();
    fixture.engine.switch_camera(&web(), "line-2", true).await.unwrap();
    show(&fixture, CameraBoard::Later).await;
    fixture
        .engine
        .save_camera_zone(
            &web(),
            "line-2",
            "inspection-window",
            &ZoneChange {
                name: "Inspection window".into(),
                corners: [700.0, 200.0, 1600.0, 880.0],
                was: [760.0, 170.0, 1560.0, 910.0],
            },
        )
        .await
        .unwrap();
    let stored = fixture.artifacts.list_user(APP, OWNER).await.unwrap();
    assert!(stored.is_empty(), "{stored:?}");

    // A card keeps exactly the frozen frame, with the regions on it.
    let frozen = fixture.cameras.latest("line-2").await.unwrap();
    let raised = fixture
        .engine
        .raise_camera_card(
            &web(),
            "line-2",
            &RaiseCard {
                frame: frozen.digest.clone(),
                note: "The seal on T47 looks lifted at the edge, but nothing fired.".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(raised.regions, 5, "four parts and the fiducial go with it");
    assert_eq!(raised.kept_until, "Kept as evidence until 18 Dec");
    assert!(
        matches!(raised.outcome, CommandOutcome::Sent { .. } | CommandOutcome::Queued { .. }),
        "{:?}",
        raised.outcome
    );
    let stored = fixture.artifacts.list_user(APP, OWNER).await.unwrap();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].file_name, format!("vision-{}.png", &raised.digest[7..]));
    // No person on this frame: the kept copy is the frame itself.
    assert_eq!(raised.digest, frozen.digest);

    // The card reaches the Inspector as the person's message, naming the evidence by digest.
    let mut text = String::new();
    for _ in 0..100 {
        let session = fixture
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
        text = session
            .events()
            .all()
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| {
                content.parts.iter().filter_map(|part| part.text().map(str::to_string))
            })
            .collect();
        if text.contains("Card from") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(text.contains("Card from the line 2 camera, frame of 10:43:18: The seal on T47 looks lifted at the edge, but nothing fired."), "{text}");
    assert!(text.contains(&raised.digest), "{text}");
    assert!(text.contains("- part T47"), "{text}");
    let card = fixture
        .audit
        .records()
        .into_iter()
        .find(|record| record.action == adk_surface::camera::SURFACE_AUDIT_CAMERA_CARD)
        .expect("the card is on record");
    assert_eq!(card.ext["frame"], json!(frozen.digest));
    assert_eq!(card.ext["regions"], json!(5));
}

#[tokio::test]
async fn a_card_from_a_frame_with_a_person_keeps_it_covered() {
    let fixture = fixture(CameraBoard::Live).await;
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 64, 32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels: Vec<u8> = (0..64 * 32).flat_map(|_| [200u8, 180, 160, 255]).collect();
        encoder.write_header().unwrap().write_image_data(&pixels).unwrap();
    }
    let frame = CameraFrame::new(bytes.clone(), "image/png", 64, 32, CameraBoard::Live.now(day()))
        .with_overlays(vec![Overlay::boxed("person", [4.0, 2.0, 20.0, 30.0]).blurred()]);
    let Offered::Shown { digest: live } = fixture.cameras.offer("packing-bench", frame).await
    else {
        panic!("the bench is on");
    };
    let raised = fixture
        .engine
        .raise_camera_card(
            &web(),
            "packing-bench",
            &RaiseCard { frame: live.clone(), note: "Who left this box?".into() },
        )
        .await
        .unwrap();
    assert_ne!(raised.digest, live, "the kept copy is not the frame as it arrived");
    let stored = fixture.artifacts.list_user(APP, OWNER).await.unwrap();
    assert_eq!(stored.len(), 1);
    let loaded = fixture
        .artifacts
        .load(adk_artifact::LoadRequest {
            app_name: APP.into(),
            user_id: OWNER.into(),
            session_id: SESSION.into(),
            file_name: stored[0].file_name.clone(),
            version: None,
        })
        .await
        .unwrap();
    let adk_core::Part::InlineData { data, .. } = loaded.part else { panic!("an image") };
    assert_eq!(digest(&data), raised.digest);
    let mut reader = png::Decoder::new(std::io::Cursor::new(&data)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    let at = |x: usize, y: usize| &pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 3];
    assert_eq!(at(10, 10), [0, 0, 0], "the person is covered");
    assert_eq!(at(40, 10), [200, 180, 160], "the rest is kept");
}

#[tokio::test]
async fn a_frame_no_longer_held_cannot_be_carded() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let refused = fixture
        .engine
        .raise_camera_card(
            &web(),
            "line-2",
            &RaiseCard { frame: digest(b"gone"), note: "Look".into() },
        )
        .await;
    assert!(matches!(refused, Err(SurfaceError::NotFound { .. })), "{refused:?}");
    let latest = fixture.cameras.latest("line-2").await.unwrap();
    let empty = fixture
        .engine
        .raise_camera_card(&web(), "line-2", &RaiseCard { frame: latest.digest, note: "  ".into() })
        .await;
    assert!(matches!(empty, Err(SurfaceError::Invalid(_))), "{empty:?}");
    assert!(fixture.artifacts.list_user(APP, OWNER).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_zone_is_changed_from_its_next_frame_and_recorded() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let change = ZoneChange {
        name: "Inspection window".into(),
        corners: [700.0, 200.0, 1600.0, 880.0],
        was: [760.0, 170.0, 1560.0, 910.0],
    };
    let saved = fixture
        .engine
        .save_camera_zone(&web(), "line-2", "inspection-window", &change)
        .await
        .unwrap();
    assert_eq!(saved.zone.corners_text, "700, 200 → 1600, 880");
    assert_eq!(saved.text, "Saved. The seal tear watch uses it from the next frame.");
    assert_eq!(
        fixture.source.log().await,
        ["zone line-2/inspection-window [700.0, 200.0, 1600.0, 880.0]"]
    );
    let record = fixture
        .audit
        .records()
        .into_iter()
        .find(|record| record.action == adk_surface::camera::SURFACE_AUDIT_CAMERA_ZONE)
        .unwrap();
    assert_eq!(record.ext["was"], json!([760.0, 170.0, 1560.0, 910.0]));
    // The person saw the old corners: a second save from them is refused, never overwritten.
    let stale =
        fixture.engine.save_camera_zone(&web(), "line-2", "inspection-window", &change).await;
    assert!(matches!(stale, Err(SurfaceError::Invalid(_))), "{stale:?}");
    let outside = ZoneChange { corners: [0.0, 0.0, 2000.0, 900.0], was: change.corners, ..change };
    let refused =
        fixture.engine.save_camera_zone(&web(), "line-2", "inspection-window", &outside).await;
    assert!(matches!(refused, Err(SurfaceError::Invalid(_))), "{refused:?}");
}

#[tokio::test]
async fn only_the_sessions_owner_sees_or_acts_on_a_camera() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let ben = Caller::human("ben", SurfaceKind::Web, Origin::Pointer);
    assert!(fixture.engine.cameras_view(&ben).await.unwrap().tiles.is_empty());
    let read = fixture.engine.camera_view(&ben, "line-2").await;
    assert!(matches!(read, Err(SurfaceError::NotFound { .. })), "{read:?}");
    let off = fixture.engine.switch_camera(&ben, "line-2", false).await;
    assert!(matches!(off, Err(SurfaceError::NotFound { .. })), "{off:?}");
    assert!(fixture.cameras.is_on("line-2").await, "another person cannot switch it off");
    let held = fixture.cameras.latest("line-2").await.unwrap();
    let frame = fixture.engine.camera_frame(&ben, "line-2", &held.digest).await;
    assert!(matches!(frame, Err(SurfaceError::NotFound { .. })), "{frame:?}");
    // Voice has no camera controls.
    let spoken = fixture
        .engine
        .switch_camera(&owner(SurfaceKind::Voice, Origin::Voice), "line-2", false)
        .await;
    assert!(matches!(spoken, Err(SurfaceError::NotHere(_))), "{spoken:?}");
}

fn api(engine: SurfaceEngine, user: &str, device: bool) -> axum::Router {
    let mut metadata = std::collections::HashMap::new();
    if device {
        metadata.insert(adk_kernel::pairing::DEVICE_ID_METADATA.to_string(), "phone-1".to_string());
        metadata.insert(adk_kernel::pairing::DEVICE_KIND_METADATA.to_string(), "phone".to_string());
    }
    axum::Router::new().nest("/api", adk_surface::web::router(engine)).layer(axum::Extension(Some(
        RequestContext {
            user_id: user.into(),
            scopes: vec!["owner".into()],
            metadata: metadata.into_iter().collect(),
        },
    )))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => {
            request.header("content-type", "application/json").body(Body::from(body.to_string()))
        }
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (status, to_bytes(response.into_body(), usize::MAX).await.unwrap().to_vec(), headers)
}

#[tokio::test]
async fn the_web_api_serves_cameras_frames_and_acts() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    let app = api(fixture.engine.clone(), OWNER, false);

    let (status, body, _) = call(&app, "GET", "/api/surface/cameras", None).await;
    assert_eq!(status, StatusCode::OK);
    let grid: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(grid["countText"], "4 sources · 3 on");

    let (status, body, _) = call(&app, "GET", "/api/surface/cameras/line-2", None).await;
    assert_eq!(status, StatusCode::OK);
    let view: Value = serde_json::from_slice(&body).unwrap();
    let href = view["stage"]["frame"]["href"].as_str().unwrap().to_string();
    let (status, body, headers) = call(&app, "GET", &href, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "private, no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(body, picture("line-2-live"));

    // Off over HTTP: the frame route has nothing to serve and frames are dropped.
    let (status, body, _) = call(&app, "POST", "/api/surface/cameras/line-2/off", None).await;
    assert_eq!(status, StatusCode::OK);
    let off: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(off["state"], "off");
    let (status, _, _) = call(&app, "GET", &href, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let frame =
        CameraFrame::new(picture("late"), "image/png", 1920, 1080, CameraBoard::Live.now(day()));
    assert_eq!(fixture.cameras.offer("line-2", frame).await, Offered::Dropped(DropReason::Off));

    // Another person gets 404 for every camera route.
    let ben = api(fixture.engine.clone(), "ben", false);
    for (method, uri) in [
        ("GET", "/api/surface/cameras/line-2"),
        ("GET", "/api/surface/cameras/line-2/feed"),
        ("POST", "/api/surface/cameras/line-2/on"),
        ("POST", "/api/surface/cameras/line-2/stop"),
    ] {
        let (status, _, _) = call(&ben, method, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    // Without a caller, 401.
    let anonymous =
        axum::Router::new().nest("/api", adk_surface::web::router(fixture.engine.clone()));
    let (status, _, _) = call(&anonymous, "GET", "/api/surface/cameras/line-2", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A phone switches on and raises a card; it edits no zone (board choice C14).
    let phone = api(fixture.engine.clone(), OWNER, true);
    let (status, _, _) =
        call(&phone, "POST", "/api/surface/cameras/line-2/on?surface=phone", None).await;
    assert_eq!(status, StatusCode::OK);
    show(&fixture, CameraBoard::Live).await;
    let held = fixture.cameras.latest("line-2").await.unwrap();
    let (status, _, _) = call(
        &phone,
        "POST",
        "/api/surface/cameras/line-2/freeze?surface=phone",
        Some(json!({ "frame": held.digest })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _) = call(
        &phone,
        "POST",
        "/api/surface/cameras/line-2/cards?surface=phone",
        Some(json!({ "frame": held.digest, "note": "Look at T43" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let zone = json!({ "name": "Inspection window", "corners": [700, 200, 1600, 880], "was": [760, 170, 1560, 910] });
    let (status, _, _) = call(
        &phone,
        "PUT",
        "/api/surface/cameras/line-2/zones/inspection-window",
        Some(zone.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) =
        call(&app, "PUT", "/api/surface/cameras/line-2/zones/inspection-window", Some(zone)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn without_cameras_the_routes_say_so() {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let local = LocalSource::builder(APP).sessions(sessions).build().unwrap();
    let engine = SurfaceEngine::builder(Arc::new(local)).build();
    let (status, body, _) =
        call(&api(engine, OWNER, false), "GET", "/api/surface/cameras", None).await;
    assert_ne!(status, StatusCode::OK);
    let problem: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(problem["error"]["code"], "surface.unsupported", "{problem}");
}

#[tokio::test]
async fn a_frozen_frame_is_held_while_the_card_is_written_and_forgotten_when_off() {
    let fixture = fixture(CameraBoard::Later).await;
    show(&fixture, CameraBoard::Later).await;
    let frozen = fixture.cameras.latest("line-2").await.unwrap();
    let stage = fixture.engine.freeze_camera_frame(&web(), "line-2", &frozen.digest).await.unwrap();
    assert_eq!(stage.frame.as_ref().unwrap().at_text, "10:43:18");
    assert_eq!(stage.regions.iter().filter(|region| region.bbox.is_some()).count(), 5);
    // The camera goes on: the frozen frame leaves the latest frames, and is still held.
    for index in 0..adk_surface::camera::HELD_FRAMES + 5 {
        let frame = CameraFrame::new(
            picture(&format!("later {index}")),
            "image/png",
            1920,
            1080,
            CameraBoard::Later.now(day()),
        );
        fixture.cameras.offer("line-2", frame).await;
    }
    assert!(fixture.cameras.held("line-2", &frozen.digest).await.is_some());
    assert!(
        fixture.artifacts.list_user(APP, OWNER).await.unwrap().is_empty(),
        "frozen is not stored"
    );
    let card = RaiseCard { frame: frozen.digest.clone(), note: "Look at T47".into() };
    fixture.engine.raise_camera_card(&web(), "line-2", &card).await.unwrap();
    // Off forgets the frozen frames too.
    let again = fixture.cameras.latest("line-2").await.unwrap();
    fixture.engine.freeze_camera_frame(&web(), "line-2", &again.digest).await.unwrap();
    fixture.engine.switch_camera(&web(), "line-2", false).await.unwrap();
    assert!(fixture.cameras.held("line-2", &again.digest).await.is_none());
    let refused = fixture
        .engine
        .raise_camera_card(
            &web(),
            "line-2",
            &RaiseCard { frame: again.digest, note: "Look".into() },
        )
        .await;
    assert!(matches!(refused, Err(SurfaceError::NotFound { .. })), "{refused:?}");
}

/// Stop all pauses every camera at the hub's gate (spec 3.7 V-I17): a frame offered from then
/// on is dropped, the camera reads "Paused by Stop all", and Resume all opens the gate again.
#[tokio::test]
async fn stop_all_pauses_every_camera_at_the_gate() {
    let fixture = fixture(CameraBoard::Live).await;
    show(&fixture, CameraBoard::Live).await;
    fixture.engine.execute(&web(), adk_surface::Command::StopAll { reason: None }).await.unwrap();
    assert!(fixture.cameras.is_frozen());
    for camera in ["line-2", "line-3", "packing-bench"] {
        let frame = CameraFrame::new(
            picture("after stop all"),
            "image/png",
            1920,
            1080,
            CameraBoard::Live.now(day()),
        );
        assert_eq!(
            fixture.cameras.offer(camera, frame).await,
            Offered::Dropped(DropReason::Frozen),
            "{camera}"
        );
    }
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Frozen);
    assert_eq!(view.state_text, "Paused by Stop all");
    assert_eq!(view.agent.line, "Not watching: Stop all");
    assert!(view.on, "Stop all pauses the camera; the person's switch stays as they left it");
    let cover = view.stage.cover.as_ref().unwrap();
    assert_eq!(cover.title, "Paused by Stop all");
    assert_eq!(cover.text, "Nothing is looked at or sent until someone chooses Resume all.");
    matches_fixture("stop-all", &serde_json::to_value(&view).unwrap());
    let grid = fixture.engine.cameras_view(&web()).await.unwrap();
    assert_eq!(grid.count_text, "4 sources · 3 on");

    fixture.engine.execute(&web(), adk_surface::Command::ResumeAll).await.unwrap();
    assert!(!fixture.cameras.is_frozen());
    let frame =
        CameraFrame::new(picture("resumed"), "image/png", 1920, 1080, CameraBoard::Live.now(day()));
    assert!(matches!(fixture.cameras.offer("line-2", frame).await, Offered::Shown { .. }));
    let view = fixture.engine.camera_view(&web(), "line-2").await.unwrap();
    assert_eq!(view.state, CameraState::Live);
}
