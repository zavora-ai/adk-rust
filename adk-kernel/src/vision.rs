//! Visual wake sources (spec 3.7 §7.10).
//!
//! A camera, screen or browser stream evaluates its watch conditions on the device; the
//! kernel sees only the events that pass. [`VisionWakeSource`] turns each into a
//! [`WakeEvent`] and needs no new [`Wake`](crate::Wake) variant:
//!
//! | Part | How |
//! |------|-----|
//! | Name | The stream's source name, such as `conveyor-1`: a sleeper waits on `Wake::Queue { source: "conveyor-1" }` |
//! | Key | [`keys::vision`]: `queue:<source>:<session>:<track>:<condition>:<event>`, so one track seen for forty frames wakes one process once, and a redelivery wakes nothing |
//! | Fallback | With no sleeper, a [`Trigger`](crate::Trigger) on the same `Wake::Queue` starts a `Triggered` process |
//! | Payload | The event, label, score, track, frame (index, capture time, digest, size, artifact and version) and the top regions with a count of the rest, within [`WAKE_PAYLOAD_LIMIT`] ([`vision_payload`]) |
//! | Delivery | The woken turn reads the payload as delimited data, not instructions ([`WakeReason::notice`](crate::WakeReason::notice)), and loads the frame through the vision tools on the governed path |
//!
//! The payload carries digests and artifact references, never pixels (spec 3.7 V-I15). No
//! principal is set, so standing rules and Auto stay inert for the run a camera starts: a
//! consequential call it makes is held for a person.
//!
//! The kernel takes the source's events through its own types ([`VisualWake`]), so a default
//! build compiles no perception code. Feature `vision` adds the bridge from `adk-vision`:
//! `VisualWake: From<&adk_vision::WatchEvent>`, and `VisionWakeSource` as an
//! `adk_vision::WatchSink`.
//!
//! # Example
//!
//! ```rust
//! use adk_kernel::{VisionWakeSource, VisualFrame, VisualRegion, VisualWake, WakeSource};
//!
//! let source = VisionWakeSource::new("conveyor-1").unwrap();
//! let frame = VisualFrame::new(1203, chrono::Utc::now(), "sha256:0f3a", 1280, 720);
//! let wake = VisualWake::new("shift-1", "torn-seal", "detected", 7, "torn seal", 0.91, frame)
//!     .with_frames(3)
//!     .with_regions([VisualRegion::new([412.0, 220.5, 448.0, 251.0]).with_label("torn seal").with_track(7)]);
//! let event = source.event(&wake).unwrap();
//! assert_eq!(event.source, "conveyor-1");
//! assert_eq!(event.key, "queue:conveyor-1:shift-1:7:torn-seal:detected");
//! assert_eq!(event.payload["track"], 7);
//! assert_eq!(event.payload["regions"][0]["box"][0], 412.0);
//! assert!(event.payload.to_string().len() <= adk_kernel::WAKE_PAYLOAD_LIMIT);
//! assert_eq!(source.name(), "conveyor-1");
//! ```

use adk_core::ArtifactLocator;
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{KernelError, Result};
use crate::wake::{WAKE_PAYLOAD_LIMIT, WakeEvent, WakeHub, WakeSource, keys};

/// The `kind` of every visual wake payload.
pub const VISION_PAYLOAD_KIND: &str = "vision";

/// The most characters of any text a payload carries, so the payload's fixed part stays far
/// below [`WAKE_PAYLOAD_LIMIT`] whatever a detector names its labels.
const PAYLOAD_TEXT_CHARS: usize = 64;

/// The most characters of the frame's artifact file name: a media-store name,
/// `vision-<64 hex>.png`, is 75, and a cut name would name no artifact.
const PAYLOAD_FILE_CHARS: usize = 128;

/// The most bytes of a source name or a key part.
const MAX_KEY_PART: usize = 128;

/// The frame a visual wake names: its place in the stream, the digest of its encoded bytes,
/// its size, and the artifact it is kept as.
///
/// # Example
///
/// ```rust
/// use adk_core::ArtifactLocator;
/// use adk_kernel::VisualFrame;
///
/// let frame = VisualFrame::new(1203, chrono::Utc::now(), "sha256:0f3a", 1280, 720)
///     .with_artifact(ArtifactLocator::new("inspector", "line-1", "shift-1", "frame-1203.jpg", 0));
/// assert_eq!(frame.artifact.unwrap().file_name, "frame-1203.jpg");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VisualFrame {
    /// The frame's index in the stream.
    pub index: u64,
    /// When it was captured.
    pub captured_at: DateTime<Utc>,
    /// `sha256:` of its encoded bytes.
    pub digest: String,
    /// Width in source pixels.
    pub width: u32,
    /// Height in source pixels.
    pub height: u32,
    /// Where it is kept, with its version; `None` while it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<ArtifactLocator>,
}

impl VisualFrame {
    /// A frame not yet kept as an artifact.
    pub fn new(
        index: u64,
        captured_at: DateTime<Utc>,
        digest: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Self {
        Self { index, captured_at, digest: digest.into(), width, height, artifact: None }
    }

    /// Records where the frame is kept.
    #[must_use]
    pub fn with_artifact(mut self, artifact: ArtifactLocator) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

/// One region of a visual wake: its box in source pixels, label, score and track.
///
/// # Example
///
/// ```rust
/// use adk_kernel::VisualRegion;
///
/// let region = VisualRegion::new([10.0, 20.0, 50.0, 60.0]).with_label("seal").with_score(0.8);
/// assert_eq!(region.label.as_deref(), Some("seal"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VisualRegion {
    /// `[x0, y0, x1, y1]` in source pixels.
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
    /// The class or concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The detector's confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// The anonymous track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<u64>,
}

impl VisualRegion {
    /// A region with this box and nothing else.
    pub fn new(bbox: [f32; 4]) -> Self {
        Self { bbox, label: None, score: None, track: None }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the score.
    #[must_use]
    pub fn with_score(mut self, score: f32) -> Self {
        self.score = Some(score);
        self
    }

    /// Sets the track.
    #[must_use]
    pub fn with_track(mut self, track: u64) -> Self {
        self.track = Some(track);
        self
    }
}

/// A watch condition that passed on a stream, as the kernel takes it: the condition, what
/// happened, the track with its label and score, the frame, and the regions that met the
/// condition in it, the track's first.
///
/// `session` is the source session the anonymous track identifiers belong to; it is part of
/// the key, since identifiers reset with it.
///
/// See the [module documentation](self) for an example.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct VisualWake {
    /// The source session.
    pub session: String,
    /// The watch condition's name.
    pub condition: String,
    /// What happened, such as `detected`.
    pub event: String,
    /// The anonymous track.
    pub track: u64,
    /// The track's label.
    pub label: String,
    /// The track's score.
    pub score: f32,
    /// The consecutive frames the track met the condition in.
    pub frames: u32,
    /// The frame.
    pub frame: VisualFrame,
    /// The regions that met the condition, the track's first, then by score.
    pub regions: Vec<VisualRegion>,
}

impl VisualWake {
    /// A wake for `track` under `condition`, with one frame counted and no regions.
    pub fn new(
        session: impl Into<String>,
        condition: impl Into<String>,
        event: impl Into<String>,
        track: u64,
        label: impl Into<String>,
        score: f32,
        frame: VisualFrame,
    ) -> Self {
        Self {
            session: session.into(),
            condition: condition.into(),
            event: event.into(),
            track,
            label: label.into(),
            score,
            frames: 1,
            frame,
            regions: Vec::new(),
        }
    }

    /// Sets the consecutive frames the track met the condition in.
    #[must_use]
    pub fn with_frames(mut self, frames: u32) -> Self {
        self.frames = frames;
        self
    }

    /// Sets the regions, the track's first.
    #[must_use]
    pub fn with_regions(mut self, regions: impl IntoIterator<Item = VisualRegion>) -> Self {
        self.regions = regions.into_iter().collect();
        self
    }
}

/// A [`WakeSource`] for one camera, screen or browser stream (spec 3.7 §7.10): each
/// [`VisualWake`] published becomes a [`WakeEvent`] named after the stream and keyed
/// [`keys::vision`], which `Wake::Queue { source }` matches.
///
/// Register it with `KernelBuilder::wake_source`. Events published while no subscriber is
/// attached are held, up to 1024, for the first subscriber.
///
/// See the [module documentation](self) for an example.
#[derive(Debug)]
pub struct VisionWakeSource {
    name: String,
    hub: WakeHub,
}

impl VisionWakeSource {
    /// A source for the stream named `source`.
    ///
    /// # Errors
    ///
    /// [`KernelError::Config`] for a name that is empty, longer than 128 bytes, or holds
    /// whitespace, a control character, `:` or `#`: the name is the start of every key, and
    /// the kernel reads a key's source up to its first `:`.
    pub fn new(source: impl Into<String>) -> Result<Self> {
        let name = source.into();
        key_part("source name", &name)?;
        if name.contains(':') {
            return Err(KernelError::Config {
                reason: format!(
                    "the visual wake source name `{name}` holds `:`; name the stream without \
                     one, since a key's source ends at its first `:`"
                ),
            });
        }
        Ok(Self { name, hub: WakeHub::default() })
    }

    /// The wake event `wake` makes, without publishing it.
    ///
    /// # Errors
    ///
    /// [`KernelError::Config`] for a session, condition or event that is empty, longer than
    /// 128 bytes, or holds whitespace, a control character or `#`.
    pub fn event(&self, wake: &VisualWake) -> Result<WakeEvent> {
        key_part("session", &wake.session)?;
        key_part("condition", &wake.condition)?;
        key_part("event", &wake.event)?;
        let key = keys::vision(&self.name, &wake.session, wake.track, &wake.condition, &wake.event);
        Ok(WakeEvent::new(self.name.clone(), key, vision_payload(&self.name, wake))
            .with_at(wake.frame.captured_at))
    }

    /// Publishes `wake` to the source's subscribers and returns its event.
    ///
    /// # Errors
    ///
    /// [`event`](Self::event)'s errors; nothing is published then.
    pub fn publish(&self, wake: &VisualWake) -> Result<WakeEvent> {
        let event = self.event(wake)?;
        tracing::debug!(wake.source = %self.name, wake.key = %event.key, "visual wake published");
        self.hub.publish(event.clone());
        Ok(event)
    }
}

#[async_trait]
impl WakeSource for VisionWakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>> {
        Ok(self.hub.subscribe())
    }
}

fn key_part(what: &str, text: &str) -> Result<()> {
    let ok = !text.is_empty()
        && text.len() <= MAX_KEY_PART
        && !text.chars().any(|c| c.is_whitespace() || c.is_control() || c == '#');
    if ok {
        return Ok(());
    }
    Err(KernelError::Config {
        reason: format!(
            "a visual wake's {what} `{}` cannot be part of a key: use 1 to {MAX_KEY_PART} bytes \
             with no whitespace, control character or `#`",
            clip(text, PAYLOAD_TEXT_CHARS)
        ),
    })
}

/// `text` cut to `chars` characters, control characters replaced, so a payload's size is
/// bounded however a detector names things.
fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).map(|c| if c.is_control() { '\u{fffd}' } else { c }).collect()
}

fn rounded(value: f32, decimals: i32) -> Value {
    let scale = 10f64.powi(decimals);
    json!((f64::from(value) * scale).round() / scale)
}

fn region_json(region: &VisualRegion) -> Value {
    let mut entry = json!({ "box": region.bbox.map(|edge| rounded(edge, 1)) });
    if let Some(label) = &region.label {
        entry["label"] = json!(clip(label, PAYLOAD_TEXT_CHARS));
    }
    if let Some(score) = region.score {
        entry["score"] = rounded(score, 2);
    }
    if let Some(track) = region.track {
        entry["track"] = json!(track);
    }
    entry
}

/// The payload of `wake` from the stream `source`, as compact JSON within
/// [`WAKE_PAYLOAD_LIMIT`] bytes: `kind` (`vision`), `source`, `session`, `condition`,
/// `event`, `label`, `score`, `track`, `frames`, `frame` (`index`, `capturedAt`, `digest`,
/// `width`, `height`, `artifact`), `regions` (each `box`, `label`, `score`, `track`) and
/// `moreRegions`, the count of regions left out.
///
/// Texts are cut to 64 characters (the digest to 80, the frame's file name to 128) and boxes
/// rounded to a tenth of a pixel, so the fixed part stays under 3.5 KiB; the regions kept are
/// the longest prefix of
/// `wake.regions` that fits.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{VisualFrame, VisualRegion, VisualWake, WAKE_PAYLOAD_LIMIT, vision_payload};
///
/// let frame = VisualFrame::new(9, chrono::Utc::now(), "sha256:0f3a", 1280, 720);
/// let many = (0..500).map(|i| VisualRegion::new([i as f32, 0.0, i as f32 + 10.0, 10.0]).with_label("scratch"));
/// let wake = VisualWake::new("s1", "scratch", "detected", 1, "scratch", 0.7, frame).with_regions(many);
/// let payload = vision_payload("conveyor-1", &wake);
/// assert!(payload.to_string().len() <= WAKE_PAYLOAD_LIMIT);
/// let kept = payload["regions"].as_array().unwrap().len() as u64;
/// assert_eq!(kept + payload["moreRegions"].as_u64().unwrap(), 500);
/// ```
pub fn vision_payload(source: &str, wake: &VisualWake) -> Value {
    let text = |value: &str| json!(clip(value, PAYLOAD_TEXT_CHARS));
    let frame = &wake.frame;
    let mut frame_json = json!({
        "index": frame.index,
        "capturedAt": frame.captured_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        "digest": clip(&frame.digest, 80),
        "width": frame.width,
        "height": frame.height,
    });
    if let Some(artifact) = &frame.artifact {
        frame_json["artifact"] = json!({
            "appName": text(&artifact.app_name),
            "userId": text(&artifact.user_id),
            "sessionId": text(&artifact.session_id),
            "fileName": clip(&artifact.file_name, PAYLOAD_FILE_CHARS),
            "version": artifact.version,
        });
    }
    let regions: Vec<Value> = wake.regions.iter().map(region_json).collect();
    let total = regions.len();
    let build = |kept: usize| {
        json!({
            "kind": VISION_PAYLOAD_KIND,
            "source": text(source),
            "session": text(&wake.session),
            "condition": text(&wake.condition),
            "event": text(&wake.event),
            "label": text(&wake.label),
            "score": rounded(wake.score, 2),
            "track": wake.track,
            "frames": wake.frames,
            "frame": frame_json,
            "regions": regions[..kept],
            "moreRegions": total - kept,
        })
    };
    let fits = |kept: usize| build(kept).to_string().len() <= WAKE_PAYLOAD_LIMIT;
    // Each region adds more bytes than `moreRegions` loses digits, so the size grows with
    // every region kept: the longest prefix that fits is found by bisection.
    let (mut low, mut high) = (0, total);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    build(low)
}

#[cfg(feature = "vision")]
mod bridge {
    use adk_vision::{VisionError, WatchEvent, WatchSink};
    use async_trait::async_trait;

    use super::{VisionWakeSource, VisualFrame, VisualRegion, VisualWake};

    impl From<&WatchEvent> for VisualWake {
        /// The kernel's form of a watch event; regions without a box are left out, and
        /// masks travel as their boxes (spec 3.7 V-I15).
        fn from(event: &WatchEvent) -> Self {
            let frame = &event.frame;
            let mut visual = VisualFrame::new(
                frame.index,
                frame.captured_at,
                frame.digest.to_string(),
                frame.width,
                frame.height,
            );
            visual.artifact = frame.artifact.clone();
            let regions = event.regions.iter().filter_map(|region| {
                let b = region.shape.bbox()?;
                Some(VisualRegion {
                    bbox: [b.x0, b.y0, b.x1, b.y1],
                    label: region.label.clone(),
                    score: region.score,
                    track: region.track.map(|track| track.0),
                })
            });
            VisualWake::new(
                event.session.clone(),
                event.condition.clone(),
                event.kind.as_str(),
                event.track.0,
                event.label.clone(),
                event.score,
                visual,
            )
            .with_frames(event.frames)
            .with_regions(regions)
        }
    }

    #[async_trait]
    impl WatchSink for VisionWakeSource {
        /// Publishes the event as a wake.
        ///
        /// # Errors
        ///
        /// [`VisionError::InvalidInput`] for an event from another stream than the source's,
        /// or one whose session, condition or event cannot key a wake.
        async fn deliver(&self, event: &WatchEvent) -> adk_vision::Result<()> {
            if event.source.0 != self.name {
                return Err(VisionError::InvalidInput(format!(
                    "a watch event from stream `{}` reached the wake source of `{}`; register \
                     one VisionWakeSource per stream",
                    event.source.0, self.name
                )));
            }
            self.publish(&VisualWake::from(event))
                .map(|_| ())
                .map_err(|error| VisionError::InvalidInput(error.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> VisualFrame {
        VisualFrame::new(4, Utc::now(), "sha256:0f3a", 640, 480)
    }

    #[test]
    fn names_and_key_parts_that_cannot_key_a_wake_are_refused() {
        for name in ["", "rtsp://cam", "two words", "a#b"] {
            assert!(VisionWakeSource::new(name).is_err(), "{name:?}");
        }
        let source = VisionWakeSource::new("cam-1").unwrap();
        for (session, condition) in [("", "seal"), ("s 1", "seal"), ("s1", "se#al")] {
            let wake = VisualWake::new(session, condition, "detected", 1, "seal", 0.9, frame());
            assert!(source.event(&wake).is_err(), "{session:?} {condition:?}");
        }
    }

    #[test]
    fn a_published_wake_reaches_a_later_subscriber_once() {
        use futures::StreamExt;

        let source = VisionWakeSource::new("cam-1").unwrap();
        let wake = VisualWake::new("s1", "seal", "detected", 3, "seal", 0.9, frame());
        let published = source.publish(&wake).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let received = runtime.block_on(async {
            let mut events = source.subscribe().await.unwrap();
            events.next().await.unwrap()
        });
        assert_eq!(received, published);
        assert_eq!(received.key, "queue:cam-1:s1:3:seal:detected");
    }

    #[test]
    fn a_media_store_file_name_reaches_the_payload_whole() {
        let file = format!("vision-{}.png", "0f".repeat(32));
        let frame =
            frame().with_artifact(ArtifactLocator::new("inspector", "line-1", "s1", &file, 0));
        let wake = VisualWake::new("s1", "seal", "detected", 1, "seal", 0.9, frame);
        let payload = vision_payload("cam-1", &wake);
        assert_eq!(payload["frame"]["artifact"]["fileName"], json!(file));
    }

    #[test]
    fn hostile_labels_stay_within_the_limit() {
        let label = "\u{0}\"\\\u{10ffff}".repeat(400);
        let region = VisualRegion::new([0.0, 0.0, 1.0, 1.0]).with_label(label.clone());
        let mut frame = frame();
        frame.digest = label.clone();
        frame.artifact = Some(ArtifactLocator::new(&label, &label, &label, &label, 1));
        let wake = VisualWake::new("s1", "seal", "detected", 1, label, 0.5, frame)
            .with_regions(vec![region; 100]);
        let payload = vision_payload(&"\u{1F600}".repeat(200), &wake);
        assert!(payload.to_string().len() <= WAKE_PAYLOAD_LIMIT, "{}", payload.to_string().len());
        assert!(payload["regions"].as_array().is_some_and(|kept| !kept.is_empty()));
    }
}
