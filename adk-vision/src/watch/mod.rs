//! Watch conditions: what a stream wakes an agent for (spec 3.7 §7.10).
//!
//! A stream's pipeline runs source, sampler, privacy, detector and tracker, then hands each
//! frame's tracked regions to a [`WatchEvaluator`] as a [`TrackedObservation`]. The
//! evaluator holds each [`WatchCondition`] against every track, on the device, and returns a
//! [`WatchEvent`] only when one passes — a track that met the condition in enough consecutive
//! frames, outside the condition's cooldown. A [`WatchSink`] takes the events on; the
//! kernel's `VisionWakeSource` (`adk-kernel`, feature `vision`) is one, turning each into a
//! wake.
//!
//! | Property | Holds by |
//! |----------|----------|
//! | One track, one event | A track fires once per condition and session, however long it stays in view |
//! | Idempotent | An observation delivered twice, or out of order, changes nothing; each event's [`key`](WatchEvent::key) is its source, session, track, condition and kind |
//! | Bounded | State is kept only for the tracks the tracker still holds |
//! | Pure | No clock, no I/O: time is each frame's capture time |
//!
//! Events carry digests and artifact references, never pixels (V-I15).
//!
//! # Example
//!
//! ```
//! use adk_vision::{
//!     BBox, BackendId, CoordFrame, Digest, FrameRef, FrameSourceId, Region, RegionOrigin,
//!     Shape, TrackId, TrackedObservation, TransformChain, WatchCondition, WatchEvaluator,
//! };
//!
//! let mut watch = WatchEvaluator::new(vec![
//!     WatchCondition::new("torn-seal", ["torn seal"]).with_min_frames(3),
//! ])
//! .unwrap();
//! let seal = |index: u64| {
//!     let frame = Digest::of(&index.to_be_bytes());
//!     let region = Region {
//!         image: frame,
//!         shape: Shape::Box(BBox::new(400.0, 200.0, 440.0, 230.0)),
//!         label: Some("torn seal".into()),
//!         score: Some(0.9),
//!         track: Some(TrackId(7)),
//!         origin: RegionOrigin {
//!             backend: BackendId::new("rf-detr"),
//!             manifest: None,
//!             frame: CoordFrame::SentPixels,
//!             chain: TransformChain::new(1280, 720),
//!             escalated_from: None,
//!         },
//!     };
//!     TrackedObservation::new(
//!         FrameSourceId("conveyor-1".into()),
//!         "shift-1",
//!         FrameRef::new(index, chrono::Utc::now(), frame, 1280, 720),
//!         vec![region],
//!     )
//! };
//! // Forty frames of one torn seal: one event, on the third frame.
//! let events: Vec<_> = (0..40).flat_map(|i| watch.observe(&seal(i))).collect();
//! assert_eq!(events.len(), 1);
//! assert_eq!(events[0].frame.index, 2);
//! assert_eq!(events[0].key(), "shift-1:7:torn-seal:detected");
//! ```

mod condition;
mod evaluator;

use adk_core::audit_log::{AuditRecordDraft, AuditRecordKind, AuditRecordOutcome};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use condition::{WatchCondition, Zone, ZoneAnchor};
pub use evaluator::WatchEvaluator;

use crate::audit::VISION_AUDIT_KIND;
use crate::digest::Digest;
use crate::error::Result;
use crate::geometry::{Region, TrackId};
use crate::media::{ArtifactLocator, FrameSourceId, ImageRef};

/// Action of the record a watch condition registered on a source writes.
pub const VISION_WATCH_REGISTERED: &str = "vision.watch.registered";
/// Action of the record a watch event writes.
pub const VISION_WATCH_FIRED: &str = "vision.watch.fired";

/// The frame an observation was made on: its place in the stream, the digest of its encoded
/// bytes, its size, and the artifact it is stored as once it is kept.
///
/// Frames stay in memory unless they become evidence (spec 3.7 §7.13), so `artifact` is
/// `None` until the pipeline keeps the frame; a pipeline keeps the frame of each event and
/// attaches its locator ([`WatchEvent::with_frame_artifact`]) before delivering the event.
///
/// # Example
///
/// ```
/// use adk_vision::{ArtifactLocator, Digest, FrameRef};
///
/// let frame = FrameRef::new(1203, chrono::Utc::now(), Digest::of(b"jpeg"), 1280, 720);
/// assert!(frame.artifact.is_none());
/// let kept = frame.with_artifact(ArtifactLocator::new("inspector", "line-1", "shift-1", "frame-1203.jpg", 0));
/// assert_eq!(kept.artifact.unwrap().version, 0);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameRef {
    /// The frame's index in the stream.
    pub index: u64,
    /// When the frame was captured.
    pub captured_at: DateTime<Utc>,
    /// `sha256:` of the frame's encoded bytes, the digest its regions name.
    pub digest: Digest,
    /// Width in source pixels.
    pub width: u32,
    /// Height in source pixels.
    pub height: u32,
    /// Where the frame is stored, with its version; `None` while it is ephemeral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactLocator>,
}

impl FrameRef {
    /// An ephemeral frame.
    pub fn new(
        index: u64,
        captured_at: DateTime<Utc>,
        digest: Digest,
        width: u32,
        height: u32,
    ) -> Self {
        Self { index, captured_at, digest, width, height, artifact: None }
    }

    /// A frame kept as `image`.
    pub fn stored(index: u64, captured_at: DateTime<Utc>, image: &ImageRef) -> Self {
        Self {
            index,
            captured_at,
            digest: image.digest,
            width: image.width,
            height: image.height,
            artifact: Some(image.artifact.clone()),
        }
    }

    /// Records where the frame is stored.
    #[must_use]
    pub fn with_artifact(mut self, artifact: ArtifactLocator) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

/// One frame's tracked regions, as the stream pipeline hands them to a [`WatchEvaluator`].
///
/// `session` names the source session the track identifiers belong to: they are anonymous
/// and reset with it (V-I8), so a new session starts every condition afresh and its events
/// have their own keys. Within a session a tracker never reuses an identifier.
///
/// # Example
///
/// ```
/// use adk_vision::{Digest, FrameRef, FrameSourceId, TrackId, TrackedObservation};
///
/// let frame = FrameRef::new(0, chrono::Utc::now(), Digest::of(b"frame"), 640, 480);
/// let empty = TrackedObservation::new(FrameSourceId("cam".into()), "s1", frame, Vec::new())
///     .with_tracks([TrackId(3)]);
/// assert_eq!(empty.tracks, vec![TrackId(3)]);
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedObservation {
    /// The frame source.
    pub source: FrameSourceId,
    /// The source session the track identifiers belong to.
    pub session: String,
    /// The frame.
    pub frame: FrameRef,
    /// The frame's regions, in source pixels; the tracker set each one's `track`. Regions
    /// without a track are not watched.
    pub regions: Vec<Region>,
    /// The tracks the tracker still holds after this frame, seen in it or briefly missed.
    /// The evaluator forgets a track that is neither listed nor seen in the frame.
    pub tracks: Vec<TrackId>,
}

impl TrackedObservation {
    /// An observation whose live tracks are the ones its regions carry.
    pub fn new(
        source: FrameSourceId,
        session: impl Into<String>,
        frame: FrameRef,
        regions: Vec<Region>,
    ) -> Self {
        let mut tracks: Vec<TrackId> = regions.iter().filter_map(|r| r.track).collect();
        tracks.sort_unstable();
        tracks.dedup();
        Self { source, session: session.into(), frame, regions, tracks }
    }

    /// Sets the tracks the tracker still holds, such as tracks missed in this frame that it
    /// keeps alive; the tracks of the regions stay listed.
    #[must_use]
    pub fn with_tracks(mut self, tracks: impl IntoIterator<Item = TrackId>) -> Self {
        self.tracks.extend(tracks);
        self.tracks.sort_unstable();
        self.tracks.dedup();
        self
    }
}

/// What happened to a track under a condition.
///
/// # Example
///
/// ```
/// use adk_vision::WatchEventKind;
///
/// assert_eq!(WatchEventKind::Detected.as_str(), "detected");
/// assert_eq!(serde_json::to_value(WatchEventKind::Detected).unwrap(), "detected");
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchEventKind {
    /// The track met the condition in its minimum number of consecutive frames.
    Detected,
}

impl WatchEventKind {
    /// The kind as it appears in event keys and payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
        }
    }
}

/// A condition that passed: the track, its label and score, the frame and the regions that
/// met the condition in it.
///
/// `regions` lists the firing track's region first, then the frame's other regions that met
/// the condition, tracked or not, highest score first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchEvent {
    /// The frame source.
    pub source: FrameSourceId,
    /// The source session.
    pub session: String,
    /// The condition's name.
    pub condition: String,
    /// What happened.
    pub kind: WatchEventKind,
    /// The track.
    pub track: TrackId,
    /// The track's label in this frame.
    pub label: String,
    /// The track's score in this frame.
    pub score: f32,
    /// The consecutive frames the track had met the condition in.
    pub frames: u32,
    /// The frame.
    pub frame: FrameRef,
    /// The regions that met the condition in the frame, the track's first.
    pub regions: Vec<Region>,
}

impl WatchEvent {
    /// The event's identity within its source: `<session>:<track>:<condition>:<kind>`. The
    /// kernel keys the wake `queue:<source>:<key>`, so a redelivered event wakes nothing.
    pub fn key(&self) -> String {
        format!("{}:{}:{}:{}", self.session, self.track.0, self.condition, self.kind.as_str())
    }

    /// Records where the event's frame is stored, once the pipeline has kept it.
    #[must_use]
    pub fn with_frame_artifact(mut self, artifact: ArtifactLocator) -> Self {
        self.frame.artifact = Some(artifact);
        self
    }

    /// The `vision.watch.fired` audit record of the event: source, condition, track, frame
    /// digest and artifact, by reference only (V-I15).
    ///
    /// `condition` is the digest of the condition that fired ([`WatchCondition::digest`]).
    pub fn audit_draft(&self, condition: Digest) -> AuditRecordDraft {
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(VISION_AUDIT_KIND.to_string()),
            VISION_WATCH_FIRED,
        )
        .outcome(AuditRecordOutcome::Succeeded)
        .payload_ref(self.frame.digest.to_string())
        .ext("source", json!(self.source))
        .ext("session", json!(self.session))
        .ext("condition", json!(self.condition))
        .ext("conditionDigest", json!(condition))
        .ext("event", json!(self.kind))
        .ext("track", json!(self.track))
        .ext("frameDigest", json!(self.frame.digest))
        .ext("frameIndex", json!(self.frame.index));
        if let Some(artifact) = &self.frame.artifact {
            draft = draft.ext("artifact", json!(artifact));
        }
        draft
    }
}

/// The `vision.watch.registered` audit record of `condition` registered on `source`.
///
/// # Example
///
/// ```
/// use adk_vision::{FrameSourceId, VISION_WATCH_REGISTERED, WatchCondition, watch_registered_draft};
///
/// let condition = WatchCondition::new("torn-seal", ["torn seal"]);
/// let draft = watch_registered_draft(&FrameSourceId("conveyor-1".into()), &condition);
/// assert_eq!(draft.action, VISION_WATCH_REGISTERED);
/// ```
pub fn watch_registered_draft(
    source: &FrameSourceId,
    condition: &WatchCondition,
) -> AuditRecordDraft {
    AuditRecordDraft::new(
        AuditRecordKind::Other(VISION_AUDIT_KIND.to_string()),
        VISION_WATCH_REGISTERED,
    )
    .outcome(AuditRecordOutcome::Succeeded)
    .payload_ref(condition.digest().to_string())
    .ext("source", json!(source))
    .ext("condition", json!(condition.name))
    .ext("conditionDigest", json!(condition.digest()))
    .ext("labels", json!(condition.labels))
    .ext("minScore", json!(condition.min_score))
    .ext("minFrames", json!(condition.min_frames))
    .ext("zone", json!(condition.zone.is_some()))
    .ext("cooldownMs", json!(u64::try_from(condition.cooldown.as_millis()).unwrap_or(u64::MAX)))
}

/// Takes the events a [`WatchEvaluator`] passes, such as the kernel's `VisionWakeSource`
/// (`adk-kernel`, feature `vision`), which turns each into a wake.
///
/// # Example
///
/// ```
/// use std::sync::Mutex;
/// use adk_vision::{WatchEvent, WatchSink};
///
/// #[derive(Default)]
/// struct Collect(Mutex<Vec<String>>);
///
/// #[async_trait::async_trait]
/// impl WatchSink for Collect {
///     async fn deliver(&self, event: &WatchEvent) -> adk_vision::Result<()> {
///         self.0.lock().unwrap().push(event.key());
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait WatchSink: Send + Sync {
    /// Takes one event.
    ///
    /// # Errors
    ///
    /// Returns the sink's error, such as an event for a source it does not serve.
    async fn deliver(&self, event: &WatchEvent) -> Result<()>;
}
