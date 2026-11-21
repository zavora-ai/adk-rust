//! A camera source driven by fixture data (feature `web`): the signed-off boards' content —
//! the line 2 conveyor, line 3, the packing bench and this computer's camera — in every state
//! the boards draw, for the examples, the tests and the screenshots. A frame pipeline
//! implements [`CameraSource`] itself; this one only says what the boards say and keeps a card's
//! frame the way a pipeline must: everyone in it covered first, written once.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use adk_artifact::{ArtifactService, SaveRequest};
use adk_core::{ArtifactLocator, ImageEvidence, Part};
use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use super::facts::{
    CameraFacts, CameraFrame, EscalationFacts, Finding, GovernorFacts, GovernorStep,
    GovernorStepKind, HeldFrame, Ingest, Measurement, Overlay, StreamState, TrackFacts, TrackPhase,
    WatchFacts, ZoneFacts,
};
use super::hub::{CameraSource, Cameras};
use crate::error::{Result, SurfaceError};

/// The line 2 parts as the boards draw them: track and box in the camera's pixels.
const PARTS: [(u64, [f64; 4]); 4] = [
    (45, [330.0, 300.0, 630.0, 530.0]),
    (43, [840.0, 400.0, 1140.0, 630.0]),
    (42, [1290.0, 250.0, 1590.0, 480.0]),
    (41, [1720.0, 470.0, 1920.0, 700.0]),
];
const TEAR: [f64; 4] = [922.0, 490.0, 1024.0, 532.0];
const FIDUCIAL: [f64; 4] = [40.0, 660.0, 285.0, 905.0];
const ZONE: [f64; 4] = [760.0, 170.0, 1560.0, 910.0];
const BENCH_PERSON: [f64; 4] = [200.0, 150.0, 720.0, 720.0];
/// How far a part moves between two frames at five a second, in pixels.
const STEP: f64 = 46.0;

/// A state of the line 2 camera as a signed-off board draws it (spec 3.7 §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraBoard {
    /// `camera-live-*`, `camera-several-sources-*`: live at 10:41:09, a seal tear on T43.
    Live,
    /// `camera-stepped-down-*`: looking 2 a second, not asking `sam-3.1`, 96% used, at 14:40:12.
    SteppedDown,
    /// `camera-quota-exhausted-*`: paused at 15:12 until midnight.
    QuotaExhausted,
    /// `camera-source-lost-*`: no frames since 10:42:07, attempt 3.
    SourceLost,
    /// `camera-switched-off-*`: switched off at 11:05.
    SwitchedOff,
    /// `camera-zone-and-card-web`'s frozen frame: live at 10:43:18, parts T49 to T46.
    Later,
}

impl CameraBoard {
    /// The local time the board shows, on `day` (local midnight).
    pub fn now(self, day: DateTime<FixedOffset>) -> DateTime<Utc> {
        let at = |h: i64, m: i64, s: i64| {
            (day + Duration::hours(h) + Duration::minutes(m) + Duration::seconds(s)).to_utc()
        };
        match self {
            Self::Live => at(10, 41, 12),
            Self::SteppedDown => at(14, 40, 15),
            Self::QuotaExhausted => at(15, 20, 0),
            Self::SourceLost => at(10, 42, 51),
            Self::SwitchedOff => at(11, 6, 0),
            Self::Later => at(10, 43, 20),
        }
    }

    /// When the board's newest frame was captured.
    pub fn frame_at(self, day: DateTime<FixedOffset>) -> DateTime<Utc> {
        let at = |h: i64, m: i64, s: i64| {
            (day + Duration::hours(h) + Duration::minutes(m) + Duration::seconds(s)).to_utc()
        };
        match self {
            Self::Live | Self::SwitchedOff => at(10, 41, 9),
            Self::SteppedDown => at(14, 40, 12),
            Self::QuotaExhausted => at(15, 12, 3),
            Self::SourceLost => at(10, 42, 7),
            Self::Later => at(10, 43, 18),
        }
    }

    /// The parts' tracks on the board's newest frame.
    fn tracks(self) -> [u64; 4] {
        match self {
            Self::SteppedDown | Self::QuotaExhausted => [91, 90, 89, 88],
            Self::Later => [49, 48, 47, 46],
            Self::Live | Self::SourceLost | Self::SwitchedOff => [45, 43, 42, 41],
        }
    }
}

/// A camera source over the boards' fixture content (feature `web`).
///
/// [`ScriptedCameras::boards`] gives the four cameras of the boards with line 2 in one board
/// state; [`show`](Self::show) offers a board's frames to the hub, with the boards' overlays and
/// each track's last second, and [`play`](Self::play) keeps offering them at the camera's rate,
/// as a live pipeline would. A card's frame is kept in the artifact store as `vision-<hex>.png`,
/// every person in it filled solid first.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_surface::camera::{CameraBoard, Cameras, ScriptedCameras};
///
/// let day = chrono::DateTime::parse_from_rfc3339("2026-11-18T00:00:00+00:00").unwrap();
/// let source = Arc::new(ScriptedCameras::boards("home", "ana", "s-line", CameraBoard::Live, day));
/// let cameras = Cameras::new(source.clone());
/// # let _ = cameras;
/// ```
pub struct ScriptedCameras {
    app: String,
    user: String,
    cameras: RwLock<Vec<CameraFacts>>,
    artifacts: Option<Arc<dyn ArtifactService>>,
    /// Every zone saved and every retry, for the tests.
    log: RwLock<Vec<String>>,
}

impl std::fmt::Debug for ScriptedCameras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedCameras").field("app", &self.app).finish_non_exhaustive()
    }
}

impl ScriptedCameras {
    /// No cameras yet, keeping card frames for `user` of `app`.
    pub fn new(app: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            user: user.into(),
            cameras: RwLock::new(Vec::new()),
            artifacts: None,
            log: RwLock::new(Vec::new()),
        }
    }

    /// The boards' four cameras, watched by the Inspector in `session`, line 2 in `board`'s
    /// state, on `day` (local midnight, which sets the boards' local times).
    pub fn boards(
        app: impl Into<String>,
        user: impl Into<String>,
        session: &str,
        board: CameraBoard,
        day: DateTime<FixedOffset>,
    ) -> Self {
        let source = Self::new(app, user);
        let cameras = vec![
            line_2(session, board, day),
            line_3(session, day),
            packing_bench(session, day),
            this_computer(session),
        ];
        Self { cameras: RwLock::new(cameras), ..source }
    }

    /// Keeps card frames in `artifacts`; without it, raising a card is refused.
    #[must_use]
    pub fn artifacts(mut self, artifacts: Arc<dyn ArtifactService>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    /// Adds or replaces camera `facts.id`.
    pub async fn set(&self, facts: CameraFacts) {
        let mut cameras = self.cameras.write().await;
        match cameras.iter_mut().find(|camera| camera.id == facts.id) {
            Some(camera) => *camera = facts,
            None => cameras.push(facts),
        }
    }

    /// Camera `id`'s facts now.
    pub async fn camera(&self, id: &str) -> Option<CameraFacts> {
        self.cameras.read().await.iter().find(|camera| camera.id == id).cloned()
    }

    /// Every zone saved and every retry asked, oldest first.
    pub async fn log(&self) -> Vec<String> {
        self.log.read().await.clone()
    }

    /// The board's frames of `camera` — its last second, oldest first, ending with the board's
    /// frame at `at` — each with `bytes` as its picture and the boards' overlays.
    pub fn frames(
        camera: &str,
        board: CameraBoard,
        bytes: &[u8],
        mime_type: &str,
        at: DateTime<Utc>,
    ) -> Vec<CameraFrame> {
        (0..5)
            .rev()
            .map(|back| {
                let shift = STEP * f64::from(back);
                let overlays = overlays(camera, board, shift, back == 0);
                CameraFrame::new(
                    bytes.to_vec(),
                    mime_type,
                    1920,
                    1080,
                    at - Duration::milliseconds(200 * i64::from(back)),
                )
                .with_overlays(overlays)
                .with_latency_ms(match board {
                    CameraBoard::SteppedDown | CameraBoard::QuotaExhausted => 200,
                    _ => 100,
                })
            })
            .collect()
    }

    /// Offers `frames` of `camera` to `cameras`, in order.
    pub async fn show(cameras: &Cameras, camera: &str, frames: Vec<CameraFrame>) {
        for frame in frames {
            cameras.offer(camera, frame).await;
        }
    }

    /// Keeps offering `picture` with the board's overlays for `camera` to `cameras` every
    /// `interval`, stamped with the time it is offered, as a live pipeline would, until the task
    /// is aborted. The hub drops what it offers while the camera is off.
    pub fn play(
        cameras: Cameras,
        camera: impl Into<String>,
        board: CameraBoard,
        picture: Vec<u8>,
        mime_type: impl Into<String>,
        interval: StdDuration,
    ) -> JoinHandle<()> {
        let camera = camera.into();
        let mime_type = mime_type.into();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            loop {
                ticks.tick().await;
                let frame =
                    CameraFrame::new(picture.clone(), mime_type.clone(), 1920, 1080, Utc::now())
                        .with_overlays(overlays(&camera, board, 0.0, true))
                        .with_latency_ms(100);
                cameras.offer(&camera, frame).await;
            }
        })
    }
}

#[async_trait]
impl CameraSource for ScriptedCameras {
    async fn cameras(&self) -> Result<Vec<CameraFacts>> {
        Ok(self.cameras.read().await.clone())
    }

    async fn retry(&self, camera: &str) -> Result<()> {
        self.log.write().await.push(format!("retry {camera}"));
        Ok(())
    }

    async fn save_zone(&self, camera: &str, zone: &ZoneFacts) -> Result<()> {
        let mut cameras = self.cameras.write().await;
        let facts = cameras
            .iter_mut()
            .find(|facts| facts.id == camera)
            .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))?;
        let entry = facts
            .zones
            .iter_mut()
            .find(|candidate| candidate.id == zone.id)
            .ok_or_else(|| SurfaceError::not_found(format!("zone '{}'", zone.id)))?;
        *entry = zone.clone();
        self.log.write().await.push(format!("zone {camera}/{} {:?}", zone.id, zone.corners));
        Ok(())
    }

    async fn keep_frame(&self, camera: &CameraFacts, frame: &HeldFrame) -> Result<ImageEvidence> {
        let artifacts = self
            .artifacts
            .as_ref()
            .ok_or_else(|| SurfaceError::unsupported("keeping a camera's frames"))?;
        let picture = &frame.frame;
        // Everyone in the frame is covered before it is written (spec 3.7 V-I9, Q3: the whole
        // box of every detected person).
        let people: Vec<[u32; 4]> = picture
            .overlays
            .iter()
            .filter(|overlay| {
                overlay.blurred || overlay.label == "person" || overlay.label == "face"
            })
            .filter_map(|overlay| overlay.bbox)
            .map(|[x0, y0, x1, y1]| {
                let clamp = |value: f64, max: u32| (value.max(0.0) as u32).min(max);
                [
                    clamp(x0.floor(), picture.width),
                    clamp(y0.floor(), picture.height),
                    clamp(x1.ceil(), picture.width),
                    clamp(y1.ceil(), picture.height),
                ]
            })
            .filter(|[x0, y0, x1, y1]| x0 < x1 && y0 < y1)
            .collect();
        let (bytes, mime_type) = if people.is_empty() {
            (picture.bytes.clone(), picture.mime_type.clone())
        } else if picture.mime_type == "image/png" {
            let change = crate::evidence::ImageChange { image: 0, crop: None, cover: people };
            let (covered, _, _) = crate::evidence::changed_png(&picture.bytes, &change)
                .map_err(SurfaceError::Invalid)?;
            (covered, "image/png".to_string())
        } else {
            return Err(SurfaceError::Invalid(
                "this frame has people in it and is not a PNG this source can cover; it is not kept"
                    .into(),
            ));
        };
        let digest = crate::evidence::digest_of(&bytes);
        let extension = match mime_type.as_str() {
            "image/jpeg" => "jpeg",
            "image/webp" => "webp",
            _ => "png",
        };
        let hex = digest.trim_start_matches("sha256:");
        let file_name = format!("vision-{hex}.{extension}");
        let saved = artifacts
            .save(SaveRequest {
                app_name: self.app.clone(),
                user_id: self.user.clone(),
                session_id: camera.session.clone(),
                file_name: file_name.clone(),
                part: Part::InlineData {
                    mime_type: mime_type.clone(),
                    data: bytes,
                    uri: None,
                    annotations: None,
                },
                version: None,
            })
            .await?;
        let regions: Vec<Value> = picture
            .overlays
            .iter()
            .filter_map(|overlay| {
                let [x0, y0, x1, y1] = overlay.bbox?;
                let mut region = json!({
                    "image": digest,
                    "shape": { "kind": "box", "x0": x0, "y0": y0, "x1": x1, "y1": y1 },
                    "label": overlay.label,
                    "origin": { "backend": "camera", "frame": "sourcePixels" },
                });
                if let Some(score) = overlay.score {
                    region["score"] = json!(score);
                }
                if let Some(track) = overlay.track {
                    region["track"] = json!(track);
                }
                Some(region)
            })
            .collect();
        Ok(ImageEvidence::new(
            digest.clone(),
            ArtifactLocator::new(
                self.app.clone(),
                self.user.clone(),
                camera.session.clone(),
                file_name,
                saved.version,
            ),
            mime_type,
            picture.width,
            picture.height,
        )
        .with_regions(json!({
            "image": digest,
            "width": picture.width,
            "height": picture.height,
            "regions": regions,
        }))
        .with_caption(format!("{} at {}", camera.name, picture.at.format("%H:%M:%S"))))
    }
}

// ---------------------------------------------------------------------------
// The boards' content (CHOICES.md S6, S7: invented, numbers illustrative)
// ---------------------------------------------------------------------------

fn at(day: DateTime<FixedOffset>, h: i64, m: i64) -> DateTime<Utc> {
    (day + Duration::hours(h) + Duration::minutes(m)).to_utc()
}

fn line_2(session: &str, board: CameraBoard, day: DateTime<FixedOffset>) -> CameraFacts {
    let midnight = (day + Duration::days(1)).to_utc();
    let [first, second, third, fourth] = board.tracks();
    let part =
        |id: u64, phase: TrackPhase| TrackFacts { id, label: "part".into(), phase, finding: None };
    let escalation = EscalationFacts {
        backend: "sam-3.1".into(),
        last_minute: 2,
        max_per_minute: 6,
        today: 14,
        cost_today_micros: 140_000,
        stopped_at: None,
    };
    let mut governor = GovernorFacts::new(5.0, 300_000, midnight)
        .with_escalation(escalation.clone())
        .with_frames_today(61_240);
    let looked_less = GovernorStep {
        at: at(day, 14, 2),
        kind: GovernorStepKind::LookLessOften { from: 5.0, to: 2.0 },
        used: Some(0.9),
    };
    let stopped_asking =
        GovernorStep { at: at(day, 14, 31), kind: GovernorStepKind::StopAsking, used: Some(0.95) };
    let mut state = StreamState::Live;
    let mut tracks = vec![
        TrackFacts {
            finding: Some(Finding { label: "seal tear".into(), score: 0.91, frames: 5 }),
            ..part(second, TrackPhase::InZone)
        },
        part(third, TrackPhase::InZone),
        part(first, TrackPhase::Entering),
        part(fourth, TrackPhase::Leaving),
    ];
    match board {
        CameraBoard::Live | CameraBoard::Later => {}
        CameraBoard::SteppedDown | CameraBoard::QuotaExhausted => {
            governor = governor
                .with_rate(2.0)
                .with_frames_today(287_400)
                .with_step(looked_less)
                .with_step(stopped_asking);
            governor.escalation = Some(EscalationFacts {
                last_minute: 0,
                stopped_at: Some(at(day, 14, 31)),
                ..escalation
            });
            tracks = vec![
                part(second, TrackPhase::InZone),
                part(third, TrackPhase::InZone),
                part(first, TrackPhase::Entering),
                part(fourth, TrackPhase::Leaving),
            ];
            if board == CameraBoard::QuotaExhausted {
                governor = governor.with_frames_today(300_000).with_step(GovernorStep {
                    at: at(day, 15, 12),
                    kind: GovernorStepKind::Pause,
                    used: Some(1.0),
                });
                state = StreamState::Paused { since: at(day, 15, 12) };
                tracks.clear();
            }
        }
        CameraBoard::SourceLost => {
            let since = board.frame_at(day);
            governor = governor.with_frames_today(64_180);
            if let Some(escalation) = governor.escalation.as_mut() {
                escalation.last_minute = 0;
            }
            state = StreamState::Lost {
                since,
                retry_at: board.now(day) + Duration::seconds(8),
                attempt: 3,
                error: "connection refused · rtsp://[credentials hidden]@10.0.4.21:554/stream1"
                    .into(),
            };
            tracks.clear();
        }
        CameraBoard::SwitchedOff => {
            governor = governor.with_frames_today(64_400);
            if let Some(escalation) = governor.escalation.as_mut() {
                escalation.last_minute = 0;
            }
            tracks.clear();
        }
    }
    CameraFacts::new("line-2", "Line 2 camera", "inspector", session)
        .with_place("line 2")
        .passing("parts")
        .with_ingest(Ingest::rtsp(1920, 1080, "the line 2 server"))
        .with_state(state)
        .with_governor(governor)
        .with_tracks(tracks)
        .with_watches(vec![WatchFacts {
            id: "seal-tear".into(),
            name: "Seal tear".into(),
            label: "seal tear".into(),
            min_score: 0.8,
            frames: 3,
            zone: Some("inspection-window".into()),
            cooldown_secs: 30,
            woke_today: 3,
            wakes_for: Some("a defect".into()),
        }])
        .with_zones(vec![ZoneFacts {
            id: "inspection-window".into(),
            name: "Inspection window".into(),
            corners: ZONE,
        }])
}

fn line_3(session: &str, day: DateTime<FixedOffset>) -> CameraFacts {
    let midnight = (day + Duration::days(1)).to_utc();
    let governor = GovernorFacts::new(5.0, 300_000, midnight)
        .with_escalation(EscalationFacts {
            backend: "sam-3.1".into(),
            last_minute: 0,
            max_per_minute: 6,
            today: 31,
            cost_today_micros: 310_000,
            stopped_at: Some(at(day, 13, 50)),
        })
        .with_rate(2.0)
        .with_frames_today(287_400)
        .with_step(GovernorStep {
            at: at(day, 13, 20),
            kind: GovernorStepKind::LookLessOften { from: 5.0, to: 2.0 },
            used: Some(0.9),
        })
        .with_step(GovernorStep {
            at: at(day, 13, 50),
            kind: GovernorStepKind::StopAsking,
            used: Some(0.95),
        });
    CameraFacts::new("line-3", "Line 3 camera", "inspector", session)
        .with_place("line 3")
        .passing("parts")
        .with_ingest(Ingest::rtsp(1920, 1080, "the line 3 server"))
        .with_governor(governor)
        .with_watches(vec![WatchFacts {
            id: "seal-tear-3".into(),
            name: "Seal tear".into(),
            label: "seal tear".into(),
            min_score: 0.8,
            frames: 3,
            zone: None,
            cooldown_secs: 30,
            woke_today: 0,
            wakes_for: Some("a defect".into()),
        }])
}

fn packing_bench(session: &str, day: DateTime<FixedOffset>) -> CameraFacts {
    let midnight = (day + Duration::days(1)).to_utc();
    CameraFacts::new("packing-bench", "Packing bench", "inspector", session)
        .with_place("the packing bench")
        .with_ingest(Ingest::rtsp(1920, 1080, "the packing server"))
        .with_governor(GovernorFacts::new(1.0, 50_000, midnight).with_frames_today(9_800))
}

fn this_computer(session: &str) -> CameraFacts {
    CameraFacts::new("this-computer", "This computer's camera", "inspector", session)
        .with_place("this computer")
        .with_ingest(Ingest::this_computer(1280, 720))
        .with_governor(GovernorFacts::new(1.0, 0, Utc::now()))
        .from("from the voice screen")
        .switched(false)
}

/// The boards' overlays on a frame of `camera`, every part moved `shift` pixels back along the
/// belt; the masks and the measurement only on the newest frame.
fn overlays(camera: &str, board: CameraBoard, shift: f64, newest: bool) -> Vec<Overlay> {
    let back = |[x0, y0, x1, y1]: [f64; 4]| [x0 - shift, y0, (x1 - shift).min(1920.0), y1];
    match camera {
        "packing-bench" => vec![Overlay::boxed("person", BENCH_PERSON).blurred()],
        "line-2" | "line-3" => {
            let tracks = if camera == "line-3" { [45, 43, 42, 41] } else { board.tracks() };
            let mut overlays: Vec<Overlay> = PARTS
                .iter()
                .zip(tracks)
                .map(|((_, bbox), track)| {
                    Overlay::boxed("part", back(*bbox)).on_track(track).scored(0.96)
                })
                .collect();
            if camera == "line-2"
                && matches!(
                    board,
                    CameraBoard::Live | CameraBoard::SourceLost | CameraBoard::SwitchedOff
                )
                && newest
            {
                let track = tracks[1];
                overlays.push(
                    Overlay { bbox: None, ..Overlay::boxed("seal", [0.0; 4]) }
                        .on_track(track)
                        .outlined(circle(990.0, 515.0, 84.0)),
                );
                overlays.push(
                    Overlay::boxed("seal tear", TEAR)
                        .on_track(track)
                        .scored(0.91)
                        .for_frames(5)
                        .outlined(vec![
                            [926.0, 494.0],
                            [950.0, 502.0],
                            [962.0, 494.0],
                            [979.0, 505.0],
                            [995.0, 498.0],
                            [1010.0, 508.0],
                            [1020.0, 508.0],
                            [1018.0, 518.0],
                            [1000.0, 532.0],
                            [958.0, 532.0],
                            [948.0, 512.0],
                            [930.0, 504.0],
                        ])
                        .measured(Measurement {
                            from: [930.0, 498.0],
                            to: [1016.0, 512.0],
                            millimetres: 14.3,
                        }),
                );
            }
            if camera == "line-2" {
                overlays.push(Overlay::boxed("fiducial", FIDUCIAL));
            }
            overlays
        }
        _ => Vec::new(),
    }
}

/// A circle's outline as a polygon of 48 points.
fn circle(cx: f64, cy: f64, r: f64) -> Vec<[f64; 2]> {
    (0..48)
        .map(|step| {
            let angle = f64::from(step) * std::f64::consts::TAU / 48.0;
            [(cx + r * angle.cos()).round(), (cy + r * angle.sin()).round()]
        })
        .collect()
}
