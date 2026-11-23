//! The hub every camera frame passes before anyone sees it (spec 3.7 §8.2, §7.13).
//!
//! The browser stops a track when its switch goes off and a pipeline stops sampling, but the
//! server does not rely on either: [`Cameras::offer`] drops every frame of a camera that is off
//! or stopped, whatever arrives, as `voice::VisualInputs` does for the voice screen. A frame
//! that passes is held in memory only — the latest few, so a person can freeze one and raise a
//! card from it — and never written anywhere. The one way a frame is kept is
//! [`CameraSource::keep_frame`], from a card, blurred first.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_core::GovernanceControl;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::{RwLock, broadcast};

use super::facts::{CameraFacts, CameraFrame, HeldFrame, ZoneFacts};
use crate::error::{Result, SurfaceError};

/// How many frames of each camera the hub holds, newest last: about two seconds at five a
/// second, enough to freeze the frame on screen and draw each track's trail.
pub const HELD_FRAMES: usize = 10;

/// How many frozen frames of each camera the hub holds while people write their cards.
pub const FROZEN_FRAMES: usize = 4;

/// How long a frozen frame is held for its card, in minutes.
pub const FROZEN_MINUTES: i64 = 15;

/// The most frames a second a screen hands one camera through [`Cameras::ingest`]: twice the
/// rate a camera's pipeline looks by default. A frame sooner after the last one taken is
/// dropped ([`DropReason::TooSoon`]).
pub const MAX_SCREEN_FPS: u32 = 10;

/// Where the live camera surface reads cameras and acts on them (spec 3.7 §8.2): the seam
/// between the surface and a frame pipeline.
///
/// The pipeline reports facts — the stream's state, the governor's numbers, tracks, watches,
/// zones — and offers frames to the [`Cameras`] hub; the surface turns them into words. A
/// person's switch and stop reach the source after the hub has already closed its gate, so a
/// source that keeps sending sends to nobody.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_surface::camera::{CameraFacts, CameraSource, Cameras, HeldFrame, ZoneFacts};
///
/// struct OneCamera;
///
/// #[async_trait::async_trait]
/// impl CameraSource for OneCamera {
///     async fn cameras(&self) -> adk_surface::Result<Vec<CameraFacts>> {
///         Ok(vec![CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")])
///     }
///     async fn keep_frame(
///         &self,
///         camera: &CameraFacts,
///         frame: &HeldFrame,
///     ) -> adk_surface::Result<adk_core::ImageEvidence> {
///         let _ = (camera, frame);
///         Err(adk_surface::SurfaceError::unsupported("keeping frames"))
///     }
/// }
///
/// let cameras = Cameras::new(Arc::new(OneCamera));
/// # let _ = cameras;
/// ```
#[async_trait]
pub trait CameraSource: Send + Sync {
    /// Every camera this source serves, with its facts now.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn cameras(&self) -> Result<Vec<CameraFacts>>;

    /// Turns `camera` on or off at the source: off, it captures and sends nothing. The hub has
    /// already closed or opened its gate. The default does nothing more.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn switch(&self, camera: &str, on: bool) -> Result<()> {
        let _ = (camera, on);
        Ok(())
    }

    /// Ends `camera`'s stream. The default does nothing more than the hub's gate.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn stop(&self, camera: &str) -> Result<()> {
        let _ = camera;
        Ok(())
    }

    /// Tries a lost source again now (board choice C6).
    ///
    /// # Errors
    ///
    /// `surface.unsupported` by default.
    async fn retry(&self, camera: &str) -> Result<()> {
        let _ = camera;
        Err(SurfaceError::unsupported("trying a camera again"))
    }

    /// Changes zone `zone` of `camera`: every watch that uses it reads the new corners from its
    /// next frame (board choice C12).
    ///
    /// # Errors
    ///
    /// `surface.unsupported` by default.
    async fn save_zone(&self, camera: &str, zone: &ZoneFacts) -> Result<()> {
        let _ = (camera, zone);
        Err(SurfaceError::unsupported("changing a camera's zones"))
    }

    /// Pauses (`true`) or resumes (`false`) every camera at the source: Stop all, or Resume all
    /// (spec 3.7 V-I17). The hub has already closed or opened its gate. The default does nothing
    /// more.
    ///
    /// # Errors
    ///
    /// The source's error.
    async fn freeze(&self, frozen: bool) -> Result<()> {
        let _ = frozen;
        Ok(())
    }

    /// Where the frames a screen captures for `camera` go once they pass the hub's gate
    /// ([`Cameras::ingest`]): the frame pipeline's push side. `None`, the default, for a camera
    /// whose source reads its own frames.
    fn intake(&self, camera: &str) -> Option<Arc<dyn FrameIntake>> {
        let _ = camera;
        None
    }

    /// The camera the voice screen's camera feeds, when this source serves one: the voice view
    /// hands that camera's frames to [`Cameras::ingest`] and mirrors its switch.
    fn voice_camera(&self) -> Option<String> {
        None
    }

    /// Keeps `frame` of `camera` as evidence: everyone in it blurred first, then written once,
    /// kept [`EVIDENCE_RETENTION_DAYS`](crate::EVIDENCE_RETENTION_DAYS) (board choice C11, spec
    /// 3.7 V-I9). Returns the evidence a card and a held call carry: the kept copy's digest, its
    /// artifact and its regions.
    ///
    /// # Errors
    ///
    /// The source's error; nothing is kept then.
    async fn keep_frame(
        &self,
        camera: &CameraFacts,
        frame: &HeldFrame,
    ) -> Result<adk_core::ImageEvidence>;
}

/// The push side of a frame pipeline: where [`Cameras::ingest`] hands a screen's frames once
/// they passed the gate (spec 3.7 §7.13 `SurfaceCameraSource`).
///
/// # Example
///
/// ```rust
/// use std::sync::Mutex;
/// use adk_surface::camera::FrameIntake;
///
/// #[derive(Default)]
/// struct Count(Mutex<usize>);
///
/// impl FrameIntake for Count {
///     fn push(&self, _bytes: Vec<u8>, _at: chrono::DateTime<chrono::Utc>) -> bool {
///         *self.0.lock().unwrap() += 1;
///         true
///     }
/// }
/// ```
pub trait FrameIntake: Send + Sync {
    /// Takes one encoded frame captured `at`; `false` when the pipeline dropped it.
    fn push(&self, bytes: Vec<u8>, at: DateTime<Utc>) -> bool;
}

/// Who did something to a camera, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acted {
    /// The person's user id.
    pub by: String,
    /// When.
    pub at: DateTime<Utc>,
}

/// One camera's gate and the frames held for it.
#[derive(Debug, Default)]
struct Feed {
    on: bool,
    /// The person's last switch, when there was one.
    switched: Option<Acted>,
    /// The person stopped the stream.
    stopped: Option<Acted>,
    /// The latest frames, newest last; memory only.
    recent: VecDeque<HeldFrame>,
    /// Frames a person froze to raise a card from, with when; memory only.
    frozen: VecDeque<(HeldFrame, DateTime<Utc>)>,
    /// Frames dropped at the gate.
    dropped: u64,
    /// When a screen last handed this camera a frame that was taken.
    last_ingest: Option<DateTime<Utc>>,
}

/// What became of a frame offered to the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offered {
    /// Held in memory and shown, under this digest.
    Shown {
        /// `sha256:` of its bytes.
        digest: String,
    },
    /// Handed to the camera's frame pipeline ([`Cameras::ingest`]), which shows what it makes
    /// of it.
    Forwarded,
    /// Dropped at the gate, and why.
    Dropped(DropReason),
}

/// Why the hub dropped a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The camera is switched off: a source switched off sends nothing.
    Off,
    /// The stream was stopped.
    Stopped,
    /// Stop all paused every camera (spec 3.7 V-I17).
    Frozen,
    /// No source serves a camera of that id, or none takes its frames.
    Unknown,
    /// A screen sent it within a [`MAX_SCREEN_FPS`] interval of the last frame taken.
    TooSoon,
}

/// What the hub tells the surfaces watching a camera.
#[derive(Debug, Clone)]
pub enum CameraUpdate {
    /// A frame passed the gate.
    Frame {
        /// The camera.
        camera: String,
        /// The frame.
        frame: Arc<HeldFrame>,
    },
    /// Something other than the frame changed: the switch, a stop, a zone.
    Changed {
        /// The camera.
        camera: String,
    },
}

impl CameraUpdate {
    /// The camera it is about.
    pub fn camera(&self) -> &str {
        match self {
            Self::Frame { camera, .. } | Self::Changed { camera } => camera,
        }
    }
}

/// A camera's switch and stop as the hub keeps them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gate {
    pub(crate) on: bool,
    pub(crate) switched: Option<Acted>,
    pub(crate) stopped: Option<Acted>,
    /// Stop all paused every camera.
    pub(crate) frozen: bool,
}

/// The cameras of a home and their gate (spec 3.7 §8.2): every frame passes [`Cameras::offer`],
/// which drops what a switched-off or stopped camera sends and holds the rest in memory only.
///
/// # Example
///
/// ```rust
/// # use std::sync::Arc;
/// # use adk_surface::camera::{CameraFacts, CameraFrame, CameraSource, Cameras, DropReason, HeldFrame, Offered};
/// # struct OneCamera;
/// # #[async_trait::async_trait]
/// # impl CameraSource for OneCamera {
/// #     async fn cameras(&self) -> adk_surface::Result<Vec<CameraFacts>> {
/// #         Ok(vec![CameraFacts::new("line-2", "Line 2 camera", "inspector", "s-line")])
/// #     }
/// #     async fn keep_frame(&self, _: &CameraFacts, _: &HeldFrame) -> adk_surface::Result<adk_core::ImageEvidence> {
/// #         Err(adk_surface::SurfaceError::unsupported("keeping frames"))
/// #     }
/// # }
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let cameras = Cameras::new(Arc::new(OneCamera));
/// let frame = || CameraFrame::new(b"jpeg".to_vec(), "image/jpeg", 1920, 1080, chrono::Utc::now());
/// assert!(matches!(cameras.offer("line-2", frame()).await, Offered::Shown { .. }));
/// cameras.switch("line-2", false, "ana", chrono::Utc::now()).await.unwrap();
/// assert_eq!(cameras.offer("line-2", frame()).await, Offered::Dropped(DropReason::Off));
/// assert!(cameras.latest("line-2").await.is_none());
/// # }
/// ```
#[derive(Clone)]
pub struct Cameras {
    inner: Arc<HubInner>,
}

struct HubInner {
    source: Arc<dyn CameraSource>,
    feeds: RwLock<HashMap<String, Feed>>,
    updates: broadcast::Sender<CameraUpdate>,
    /// Stop all: every camera paused until Resume all.
    frozen: AtomicBool,
    /// The kill switch the hub follows besides Stop all, when it follows one.
    governance: std::sync::RwLock<Option<GovernanceControl>>,
}

/// A [`Cameras`] that does not keep the hub alive: what a frame pipeline holds, so the hub and
/// its source do not own each other.
#[cfg(feature = "vision-camera")]
#[derive(Clone)]
pub(crate) struct WeakCameras(std::sync::Weak<HubInner>);

#[cfg(feature = "vision-camera")]
impl WeakCameras {
    /// The hub, while anything else holds it.
    pub(crate) fn upgrade(&self) -> Option<Cameras> {
        self.0.upgrade().map(|inner| Cameras { inner })
    }
}

impl std::fmt::Debug for Cameras {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cameras").finish_non_exhaustive()
    }
}

impl Cameras {
    /// The hub over `source`. Each camera's switch starts as its facts say.
    pub fn new(source: Arc<dyn CameraSource>) -> Self {
        let (updates, _) = broadcast::channel(64);
        Self {
            inner: Arc::new(HubInner {
                source,
                feeds: RwLock::new(HashMap::new()),
                updates,
                frozen: AtomicBool::new(false),
                governance: std::sync::RwLock::new(None),
            }),
        }
    }

    /// Follows `control` as well as Stop all: while it is frozen the gate drops every frame of
    /// every camera, wherever the freeze came from (spec 3.7 V-I17).
    #[must_use]
    pub fn with_governance(self, control: GovernanceControl) -> Self {
        *self.inner.governance.write().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(control);
        self
    }

    #[cfg(feature = "vision-camera")]
    pub(crate) fn downgrade(&self) -> WeakCameras {
        WeakCameras(Arc::downgrade(&self.inner))
    }

    /// Whether every camera is paused: by Stop all, or by the kill switch the hub follows.
    pub fn is_frozen(&self) -> bool {
        self.inner.frozen.load(Ordering::SeqCst)
            || self
                .inner
                .governance
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(GovernanceControl::is_frozen)
    }

    /// Stop all (spec 3.7 V-I17): the gate drops every frame of every camera from now on, then
    /// the source is told to pause its streams. The frames held stay, so a card can still be
    /// raised from one.
    ///
    /// # Errors
    ///
    /// The source's error, after the gate closed.
    pub async fn freeze_all(&self) -> Result<()> {
        self.inner.frozen.store(true, Ordering::SeqCst);
        self.changed_all().await;
        self.inner.source.freeze(true).await
    }

    /// Resume all: the gate opens again for every camera a person has not switched off or
    /// stopped, then the source is told.
    ///
    /// # Errors
    ///
    /// The source's error, after the gate opened.
    pub async fn thaw_all(&self) -> Result<()> {
        self.inner.frozen.store(false, Ordering::SeqCst);
        self.changed_all().await;
        self.inner.source.freeze(false).await
    }

    async fn changed_all(&self) {
        let cameras: Vec<String> = self.inner.feeds.read().await.keys().cloned().collect();
        for camera in cameras {
            self.changed(&camera);
        }
    }

    /// The camera the voice screen's camera feeds, when the source serves one.
    pub fn voice_camera(&self) -> Option<String> {
        self.inner.source.voice_camera()
    }

    /// Whether `camera` takes the frames a screen captures ([`ingest`](Self::ingest)): its
    /// source has a pipeline's push side for it, as the web home's own camera does.
    pub fn takes_frames(&self, camera: &str) -> bool {
        self.inner.source.intake(camera).is_some()
    }

    /// Hands a frame a screen captured for `camera` to the camera's frame pipeline, once it
    /// passes the gate: dropped when the camera is off, stopped, paused by Stop all or unknown,
    /// when it comes within a [`MAX_SCREEN_FPS`] interval of the last frame taken, or when no
    /// pipeline takes its frames. The pipeline offers what it makes of the frame — redacted,
    /// with what it found — to [`offer`](Self::offer); nothing is held here.
    pub async fn ingest(&self, camera: &str, bytes: Vec<u8>, at: DateTime<Utc>) -> Offered {
        if let Err(reason) = self.admit(camera).await {
            return Offered::Dropped(reason);
        }
        {
            let gap = chrono::Duration::milliseconds(1_000 / i64::from(MAX_SCREEN_FPS));
            let mut feeds = self.inner.feeds.write().await;
            if let Some(feed) = feeds.get_mut(camera) {
                if feed.last_ingest.is_some_and(|last| at >= last && at - last < gap) {
                    feed.dropped += 1;
                    return Offered::Dropped(DropReason::TooSoon);
                }
                feed.last_ingest = Some(at);
            }
        }
        match self.inner.source.intake(camera) {
            Some(intake) if intake.push(bytes, at) => Offered::Forwarded,
            Some(_) | None => Offered::Dropped(DropReason::Unknown),
        }
    }

    /// Whether `camera`'s gate lets a frame through now, counting the frames it drops.
    async fn admit(&self, camera: &str) -> std::result::Result<(), DropReason> {
        if !self.inner.feeds.read().await.contains_key(camera) {
            // A camera seen for the first time: its switch starts as its facts say.
            if let Err(error) = self.facts().await {
                tracing::warn!(error = %error, camera.id = %camera, "a frame was dropped: the camera source could not be read");
                return Err(DropReason::Unknown);
            }
        }
        let frozen = self.is_frozen();
        let mut feeds = self.inner.feeds.write().await;
        let Some(feed) = feeds.get_mut(camera) else {
            return Err(DropReason::Unknown);
        };
        let refused = if feed.stopped.is_some() {
            Some(DropReason::Stopped)
        } else if !feed.on {
            Some(DropReason::Off)
        } else if frozen {
            Some(DropReason::Frozen)
        } else {
            None
        };
        match refused {
            Some(reason) => {
                feed.dropped += 1;
                Err(reason)
            }
            None => Ok(()),
        }
    }

    /// The source.
    pub fn source(&self) -> &Arc<dyn CameraSource> {
        &self.inner.source
    }

    /// Every camera's facts, each camera's gate made the first time it is seen.
    pub(crate) async fn facts(&self) -> Result<Vec<CameraFacts>> {
        let facts = self.inner.source.cameras().await?;
        let mut feeds = self.inner.feeds.write().await;
        for camera in &facts {
            feeds
                .entry(camera.id.clone())
                .or_insert_with(|| Feed { on: camera.on, ..Feed::default() });
        }
        Ok(facts)
    }

    /// Offers `frame` of `camera`: dropped when the camera is off, stopped, paused by Stop all or
    /// unknown, else held in memory as its newest frame and shown to whoever watches. Never
    /// written anywhere.
    pub async fn offer(&self, camera: &str, frame: CameraFrame) -> Offered {
        if let Err(reason) = self.admit(camera).await {
            return Offered::Dropped(reason);
        }
        let held = {
            let mut feeds = self.inner.feeds.write().await;
            let Some(feed) = feeds.get_mut(camera) else {
                return Offered::Dropped(DropReason::Unknown);
            };
            // The gate may have closed since it was read: a switch or a stop wins the race.
            if feed.stopped.is_some() || !feed.on {
                feed.dropped += 1;
                return Offered::Dropped(if feed.stopped.is_some() {
                    DropReason::Stopped
                } else {
                    DropReason::Off
                });
            }
            let held = HeldFrame { digest: crate::evidence::digest_of(&frame.bytes), frame };
            if feed.recent.len() == HELD_FRAMES {
                feed.recent.pop_front();
            }
            feed.recent.push_back(held.clone());
            held
        };
        let digest = held.digest.clone();
        let _ = self
            .inner
            .updates
            .send(CameraUpdate::Frame { camera: camera.to_string(), frame: Arc::new(held) });
        Offered::Shown { digest }
    }

    /// Turns `camera` on or off for `by` at `at`. The gate changes first, so no frame passes
    /// after an off, and off forgets every frame held; then the source is told.
    ///
    /// # Errors
    ///
    /// `surface.not_found` for an unknown camera; the source's error, after the gate changed.
    pub async fn switch(&self, camera: &str, on: bool, by: &str, at: DateTime<Utc>) -> Result<()> {
        {
            let mut feeds = self.inner.feeds.write().await;
            let feed = feeds
                .get_mut(camera)
                .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))?;
            feed.on = on;
            feed.switched = Some(Acted { by: by.to_string(), at });
            if !on {
                feed.recent.clear();
                feed.frozen.clear();
            }
        }
        self.changed(camera);
        self.inner.source.switch(camera, on).await
    }

    /// Ends `camera`'s stream for `by` at `at`: the gate closes for good and every frame held is
    /// forgotten; then the source is told.
    ///
    /// # Errors
    ///
    /// `surface.not_found` for an unknown camera; the source's error, after the gate closed.
    pub async fn stop(&self, camera: &str, by: &str, at: DateTime<Utc>) -> Result<()> {
        {
            let mut feeds = self.inner.feeds.write().await;
            let feed = feeds
                .get_mut(camera)
                .ok_or_else(|| SurfaceError::not_found(format!("camera '{camera}'")))?;
            feed.on = false;
            feed.stopped = Some(Acted { by: by.to_string(), at });
            feed.recent.clear();
            feed.frozen.clear();
        }
        self.changed(camera);
        self.inner.source.stop(camera).await
    }

    /// The pipeline started `camera`'s stream again after a stop: its gate opens.
    pub async fn started(&self, camera: &str) {
        if let Some(feed) = self.inner.feeds.write().await.get_mut(camera) {
            feed.stopped = None;
            feed.on = true;
        }
        self.changed(camera);
    }

    /// Tells whoever watches `camera` to read it again: a zone, a watch or the governor changed.
    pub fn changed(&self, camera: &str) {
        let _ = self.inner.updates.send(CameraUpdate::Changed { camera: camera.to_string() });
    }

    /// The newest frame held for `camera`.
    pub async fn latest(&self, camera: &str) -> Option<HeldFrame> {
        self.inner.feeds.read().await.get(camera)?.recent.back().cloned()
    }

    /// The frame of `camera` with `digest`, while the hub still holds it: among the latest, or
    /// frozen for a card.
    pub async fn held(&self, camera: &str, digest: &str) -> Option<HeldFrame> {
        let feeds = self.inner.feeds.read().await;
        let feed = feeds.get(camera)?;
        // Newest first: a still scene repeats its bytes, and the newest copy is the one shown.
        feed.recent
            .iter()
            .rev()
            .find(|held| held.digest == digest)
            .or_else(|| feed.frozen.iter().map(|(held, _)| held).find(|held| held.digest == digest))
            .cloned()
    }

    /// Freezes frame `digest` of `camera` at `at` for a card (board choice C11): held in memory
    /// for [`FROZEN_MINUTES`], the newest [`FROZEN_FRAMES`] of them, and forgotten when the camera
    /// is switched off. Nothing is written. Returns the frames up to it, oldest first, for its
    /// trails; `None` when the hub no longer holds it.
    pub async fn freeze(
        &self,
        camera: &str,
        digest: &str,
        at: DateTime<Utc>,
    ) -> Option<Vec<HeldFrame>> {
        let mut feeds = self.inner.feeds.write().await;
        let feed = feeds.get_mut(camera)?;
        feed.frozen.retain(|(_, frozen)| at - *frozen < chrono::Duration::minutes(FROZEN_MINUTES));
        let upto: Vec<HeldFrame> = match feed.recent.iter().rposition(|held| held.digest == digest)
        {
            Some(index) => feed.recent.iter().take(index + 1).cloned().collect(),
            None => vec![feed.frozen.iter().find(|(held, _)| held.digest == digest)?.0.clone()],
        };
        let frame = upto.last()?.clone();
        feed.frozen.retain(|(held, _)| held.digest != digest);
        if feed.frozen.len() == FROZEN_FRAMES {
            feed.frozen.pop_front();
        }
        feed.frozen.push_back((frame, at));
        Some(upto)
    }

    /// The frames held for `camera`, oldest first.
    pub(crate) async fn recent(&self, camera: &str) -> Vec<HeldFrame> {
        self.inner
            .feeds
            .read()
            .await
            .get(camera)
            .map(|feed| feed.recent.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// How many frames of `camera` the gate dropped.
    pub async fn dropped(&self, camera: &str) -> u64 {
        self.inner.feeds.read().await.get(camera).map_or(0, |feed| feed.dropped)
    }

    /// Whether `camera` is on: switched on, not stopped and not paused by Stop all.
    pub async fn is_on(&self, camera: &str) -> bool {
        !self.is_frozen()
            && self
                .inner
                .feeds
                .read()
                .await
                .get(camera)
                .is_some_and(|feed| feed.on && feed.stopped.is_none())
    }

    /// `camera`'s gate, as the views show it.
    pub(crate) async fn gate(&self, camera: &str) -> Option<Gate> {
        let frozen = self.is_frozen();
        self.inner.feeds.read().await.get(camera).map(|feed| Gate {
            on: feed.on,
            switched: feed.switched.clone(),
            stopped: feed.stopped.clone(),
            frozen,
        })
    }

    /// Every frame that passes the gate and every change, as it happens.
    pub fn subscribe(&self) -> broadcast::Receiver<CameraUpdate> {
        self.inner.updates.subscribe()
    }
}
