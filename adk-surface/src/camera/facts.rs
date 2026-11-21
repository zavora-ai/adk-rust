//! What a camera source reports (spec 3.7 §7.13, §8.2): the facts the live camera surface
//! projects into words. Numbers and times, never sentences, so a pipeline reports what it
//! measured and the surface says it the same way on every screen.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One camera as its source reports it.
///
/// # Example
///
/// ```rust
/// use adk_surface::camera::{CameraFacts, GovernorFacts, Ingest, StreamState};
///
/// let facts = CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")
///     .with_place("line 2")
///     .with_ingest(Ingest::rtsp(1920, 1080, "the line 2 server"))
///     .with_governor(GovernorFacts::new(5.0, 300_000, chrono::Utc::now()));
/// assert_eq!(facts.state, StreamState::Live);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraFacts {
    /// The camera's id, as routes name it: `line-2`.
    pub id: String,
    /// The name people know it by: "Line 2 camera".
    pub name: String,
    /// What it looks at, as a sentence names it: "line 2" ("Nothing on line 2 is being checked").
    pub place: String,
    /// The agent that watches it, by its name in the runtime.
    pub agent: String,
    /// The agent's session the camera belongs to: only that session's owner sees or acts on
    /// the camera (spec 3.3 §11 S-I8).
    pub session: String,
    /// How frames arrive.
    pub ingest: Ingest,
    /// The stream's state as the pipeline sees it; a person's switch and stop are the hub's.
    pub state: StreamState,
    /// The frame governor (spec 3.7 §7.9).
    pub governor: GovernorFacts,
    /// The tracks now, anonymous (V-I8).
    #[serde(default)]
    pub tracks: Vec<TrackFacts>,
    /// The watch conditions on this camera.
    #[serde(default)]
    pub watches: Vec<WatchFacts>,
    /// The zones on this camera, in its pixels.
    #[serde(default)]
    pub zones: Vec<ZoneFacts>,
    /// The switch when the camera is first seen: on unless the source says otherwise.
    #[serde(default = "on")]
    pub on: bool,
    /// Where the camera comes from when it is not a stream the agent watches, such as "from the
    /// voice screen".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What passes the camera, plural, for the warning when nothing checks it: "parts" ("Parts on
    /// line 2 are passing without inspection").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passing: Option<String>,
}

fn on() -> bool {
    true
}

impl CameraFacts {
    /// A live camera with no tracks, watches or zones, watched by `agent` in `session`.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        agent: impl Into<String>,
        session: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            id: id.into(),
            place: name.to_lowercase(),
            name,
            agent: agent.into(),
            session: session.into(),
            ingest: Ingest::rtsp(1920, 1080, ""),
            state: StreamState::Live,
            governor: GovernorFacts::new(1.0, 0, Utc::now()),
            tracks: Vec::new(),
            watches: Vec::new(),
            zones: Vec::new(),
            on: true,
            from: None,
            passing: None,
        }
    }

    /// What sentences call it: "line 2".
    #[must_use]
    pub fn with_place(mut self, place: impl Into<String>) -> Self {
        self.place = place.into();
        self
    }

    /// How frames arrive.
    #[must_use]
    pub fn with_ingest(mut self, ingest: Ingest) -> Self {
        self.ingest = ingest;
        self
    }

    /// The stream's state.
    #[must_use]
    pub fn with_state(mut self, state: StreamState) -> Self {
        self.state = state;
        self
    }

    /// The frame governor.
    #[must_use]
    pub fn with_governor(mut self, governor: GovernorFacts) -> Self {
        self.governor = governor;
        self
    }

    /// The tracks now.
    #[must_use]
    pub fn with_tracks(mut self, tracks: Vec<TrackFacts>) -> Self {
        self.tracks = tracks;
        self
    }

    /// The watch conditions.
    #[must_use]
    pub fn with_watches(mut self, watches: Vec<WatchFacts>) -> Self {
        self.watches = watches;
        self
    }

    /// The zones.
    #[must_use]
    pub fn with_zones(mut self, zones: Vec<ZoneFacts>) -> Self {
        self.zones = zones;
        self
    }

    /// The switch when the camera is first seen.
    #[must_use]
    pub fn switched(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Where the camera comes from, such as "from the voice screen".
    #[must_use]
    pub fn from(mut self, from: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self
    }

    /// What passes the camera, plural: "parts".
    #[must_use]
    pub fn passing(mut self, what: impl Into<String>) -> Self {
        self.passing = Some(what.into());
        self
    }
}

/// How a camera's frames arrive (spec 3.7 §7.13's sources).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ingest {
    /// `RTSP`, `File`, `Screen`, `Browser` or `This computer`.
    pub kind: String,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Where it is decoded, such as "the line 2 server"; empty when it says nothing.
    #[serde(default)]
    pub at: String,
}

impl Ingest {
    /// An RTSP camera of `width` × `height`, decoded `at`.
    pub fn rtsp(width: u32, height: u32, at: impl Into<String>) -> Self {
        Self { kind: "RTSP".into(), width, height, at: at.into() }
    }

    /// The web home's own camera (spec 3.7 §7.13 `SurfaceCameraSource`).
    pub fn this_computer(width: u32, height: u32) -> Self {
        Self { kind: "This computer".into(), width, height, at: String::new() }
    }
}

/// A stream's state as the pipeline sees it. Off and stopped are a person's, kept by the hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum StreamState {
    /// Frames arrive and are looked at.
    Live,
    /// The governor paused the camera until its budget renews (board choice C5).
    Paused {
        /// When it paused.
        since: DateTime<Utc>,
    },
    /// The source cannot be reached and is retried (board choice C6).
    Lost {
        /// When the last frame arrived.
        since: DateTime<Utc>,
        /// The next attempt.
        retry_at: DateTime<Utc>,
        /// The attempts so far.
        attempt: u32,
        /// What went wrong, credentials already replaced by the source (spec 3.7 §9.1).
        error: String,
    },
}

/// The frame governor's state for one camera (spec 3.7 §7.9, board choice C3): how often it
/// looks, whether it asks a cloud model about unclear tracks, and today's frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorFacts {
    /// Frames looked at a second now.
    pub rate: f64,
    /// Frames a second as set.
    pub set_rate: f64,
    /// Escalation of unclear tracks, when this camera escalates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<EscalationFacts>,
    /// Frames looked at today.
    pub frames_today: u64,
    /// The daily frame limit.
    pub frames_limit: u64,
    /// When the budget renews.
    pub renews_at: DateTime<Utc>,
    /// The share of the limit at which the governor looks less often.
    pub look_less_at: f64,
    /// The share at which it stops escalating.
    pub stop_asking_at: f64,
    /// Today's steps, oldest first.
    #[serde(default)]
    pub steps: Vec<GovernorStep>,
}

impl GovernorFacts {
    /// Looking `rate` times a second, as set, against `frames_limit` frames a day renewing at
    /// `renews_at`, with the steps at 90% and 95%.
    pub fn new(rate: f64, frames_limit: u64, renews_at: DateTime<Utc>) -> Self {
        Self {
            rate,
            set_rate: rate,
            escalation: None,
            frames_today: 0,
            frames_limit,
            renews_at,
            look_less_at: 0.9,
            stop_asking_at: 0.95,
            steps: Vec::new(),
        }
    }

    /// Escalation of unclear tracks.
    #[must_use]
    pub fn with_escalation(mut self, escalation: EscalationFacts) -> Self {
        self.escalation = Some(escalation);
        self
    }

    /// Frames looked at today.
    #[must_use]
    pub fn with_frames_today(mut self, frames: u64) -> Self {
        self.frames_today = frames;
        self
    }

    /// The rate now, after the governor lowered it.
    #[must_use]
    pub fn with_rate(mut self, rate: f64) -> Self {
        self.rate = rate;
        self
    }

    /// A step the governor took.
    #[must_use]
    pub fn with_step(mut self, step: GovernorStep) -> Self {
        self.steps.push(step);
        self
    }

    /// The share of today's frames used, `0.0..=1.0`; `0.0` without a limit.
    pub fn used(&self) -> f64 {
        if self.frames_limit == 0 {
            return 0.0;
        }
        (self.frames_today as f64 / self.frames_limit as f64).min(1.0)
    }

    /// Whether the governor stepped down today and has not paused.
    pub fn stepped_down(&self) -> bool {
        self.steps.iter().any(|step| !matches!(step.kind, GovernorStepKind::Pause))
    }
}

/// Escalation of unclear tracks to a cloud model (spec 3.7 §7.13 "Tracking").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationFacts {
    /// The model asked, such as `sam-3.1`.
    pub backend: String,
    /// Requests in the last minute.
    pub last_minute: u32,
    /// The most a minute.
    pub max_per_minute: u32,
    /// Requests today.
    pub today: u32,
    /// What today's requests cost, in micro-USD.
    pub cost_today_micros: u64,
    /// When the governor stopped escalating, while it is stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<DateTime<Utc>>,
}

/// One step the governor took (spec 3.7 §9.3 `vision.governor.step`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorStep {
    /// When.
    pub at: DateTime<Utc>,
    /// What it did.
    pub kind: GovernorStepKind,
    /// The share of the limit used when it did, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
}

/// What a governor step did, in the fixed order it steps down (board choice C3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum GovernorStepKind {
    /// Looked less often.
    LookLessOften {
        /// Frames a second before.
        from: f64,
        /// Frames a second after.
        to: f64,
    },
    /// Stopped escalating unclear tracks.
    StopAsking,
    /// Paused the camera until the budget renews.
    Pause,
}

/// One anonymous track (spec 3.7 V-I8): its id resets with its source session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackFacts {
    /// The track's number: `43` shows as `T43`.
    pub id: u64,
    /// What it is: "part".
    pub label: String,
    /// Where it is.
    pub phase: TrackPhase,
    /// A defect a watch condition looks for, seen on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<Finding>,
}

/// Where a track is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackPhase {
    /// Coming into view.
    Entering,
    /// Inside a zone.
    InZone,
    /// Leaving the view.
    Leaving,
    /// In view, outside every zone.
    InView,
}

/// What a detector found on a track, such as a seal tear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// "seal tear".
    pub label: String,
    /// Its score.
    pub score: f64,
    /// Frames in a row it was seen.
    pub frames: u32,
}

/// A watch condition (spec 3.7 §7.10): what wakes the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchFacts {
    /// Its id.
    pub id: String,
    /// "Seal tear".
    pub name: String,
    /// The label it looks for: "seal tear".
    pub label: String,
    /// The least score.
    pub min_score: f64,
    /// Frames in a row.
    pub frames: u32,
    /// The zone it looks inside, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// Seconds before it wakes again for the same track.
    pub cooldown_secs: u32,
    /// Times it woke the agent today.
    pub woke_today: u32,
    /// What the agent sleeps until, as its line says: "a defect" ("Asleep until a defect");
    /// "a seal tear" when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wakes_for: Option<String>,
}

/// A zone, in the camera's pixels (board choice C12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneFacts {
    /// Its id.
    pub id: String,
    /// "Inspection window".
    pub name: String,
    /// `[x0, y0, x1, y1]` in the camera's pixels.
    pub corners: [f64; 4],
}

/// One frame offered to the hub: the encoded picture, in the camera's pixels, with what was
/// found on it (spec 3.7 §7.13: frames stay ephemeral).
#[derive(Debug, Clone, PartialEq)]
pub struct CameraFrame {
    /// The encoded picture: `image/jpeg` or `image/png`.
    pub bytes: Vec<u8>,
    /// Its media type.
    pub mime_type: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// When it was captured.
    pub at: DateTime<Utc>,
    /// How long after capture its overlays were ready, in milliseconds (board choice C10).
    pub latency_ms: u32,
    /// What was found on it, in its pixels.
    pub overlays: Vec<Overlay>,
}

impl CameraFrame {
    /// A JPEG or PNG frame of `width` × `height` captured `at`, nothing found on it yet.
    pub fn new(
        bytes: Vec<u8>,
        mime_type: impl Into<String>,
        width: u32,
        height: u32,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            bytes,
            mime_type: mime_type.into(),
            width,
            height,
            at,
            latency_ms: 0,
            overlays: Vec::new(),
        }
    }

    /// What was found on it.
    #[must_use]
    pub fn with_overlays(mut self, overlays: Vec<Overlay>) -> Self {
        self.overlays = overlays;
        self
    }

    /// How long after capture its overlays were ready.
    #[must_use]
    pub fn with_latency_ms(mut self, latency_ms: u32) -> Self {
        self.latency_ms = latency_ms;
        self
    }
}

/// Something found on a frame, in the frame's pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Overlay {
    /// "part", "seal tear", "fiducial", "person".
    pub label: String,
    /// `[x0, y0, x1, y1]`; absent for a mask drawn without its box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    /// Its score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// The anonymous track it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<u64>,
    /// A mask's outline, as a polygon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outline: Vec<[f64; 2]>,
    /// A measured length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Measurement>,
    /// Frames in a row it was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<u32>,
    /// A person covered before the frame was shown (board choice C9).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blurred: bool,
}

impl Overlay {
    /// A box labelled `label`.
    pub fn boxed(label: impl Into<String>, bbox: [f64; 4]) -> Self {
        Self {
            label: label.into(),
            bbox: Some(bbox),
            score: None,
            track: None,
            outline: Vec::new(),
            measurement: None,
            frames: None,
            blurred: false,
        }
    }

    /// Of track `track`.
    #[must_use]
    pub fn on_track(mut self, track: u64) -> Self {
        self.track = Some(track);
        self
    }

    /// With its score.
    #[must_use]
    pub fn scored(mut self, score: f64) -> Self {
        self.score = Some(score);
        self
    }

    /// With a mask's outline.
    #[must_use]
    pub fn outlined(mut self, outline: Vec<[f64; 2]>) -> Self {
        self.outline = outline;
        self
    }

    /// With a measured length.
    #[must_use]
    pub fn measured(mut self, measurement: Measurement) -> Self {
        self.measurement = Some(measurement);
        self
    }

    /// Seen `frames` frames in a row.
    #[must_use]
    pub fn for_frames(mut self, frames: u32) -> Self {
        self.frames = Some(frames);
        self
    }

    /// A person, covered before the frame was shown.
    #[must_use]
    pub fn blurred(mut self) -> Self {
        self.blurred = true;
        self
    }
}

/// A length measured on a frame, valid in the calibrated plane (spec 3.7 §7.13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    /// One end, in the frame's pixels.
    pub from: [f64; 2],
    /// The other end.
    pub to: [f64; 2],
    /// The length in millimetres.
    pub millimetres: f64,
}

/// A frame the hub holds in memory, with its digest: what a card keeps (board choice C11).
#[derive(Debug, Clone, PartialEq)]
pub struct HeldFrame {
    /// `sha256:` of its bytes.
    pub digest: String,
    /// The frame.
    pub frame: CameraFrame,
}
