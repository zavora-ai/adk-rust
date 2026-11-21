//! Frame ingestion: sources, sampling, the frame governor, retention and the stream pipeline
//! (spec 3.7 §7.9, §7.13).
//!
//! A [`FrameSource`] turns a camera, a file, a screen, a browser or the web home's camera
//! into a [`FrameStream`] of decoded frames at the rate and size a [`Sampling`] asks for. A
//! source never buffers without bound: when the consumer falls behind, the source drops the
//! oldest frames it holds and keeps the newest.
//!
//! | Source | Feature | Ingest | Decode |
//! |--------|---------|--------|--------|
//! | `RtspSource` | `rtsp` | RTSP through `retina`: H.264, H.265, MJPEG | MJPEG in process; H.264 and H.265 through `ffmpeg` |
//! | `FileSource` | `ffmpeg` | MP4 and MOV files | a user-installed `ffmpeg` binary |
//! | `ScreenSource` | `screen` | a `ComputerActuator` screenshot or a computer-use-mcp observation, at an interval | PNG, JPEG |
//! | `BrowserSource` | `browser` | the `browser_screenshot` tool, at an interval | PNG |
//! | `SurfaceCameraSource` | `surface-camera` | frames the web home pushes through a `CameraFeed` | JPEG |
//!
//! [`StreamPipeline`] runs one source through the fixed stages of §7.13:
//!
//! ```text
//! source → sampler → frame governor → privacy hook → detector → tracker → observation sink
//! ```
//!
//! - **Sampling.** [`Sampler`] keeps frames at most `fps` apart by presentation time and
//!   downscales them to `max_long_edge`.
//! - **The frame governor.** [`FrameGovernor`] holds a stream to its [`FrameBudget`]: a local
//!   rate, escalations per minute and frame quotas per period. Under pressure it steps down
//!   in a fixed order — lower the local rate, stop escalating, pause the source — writes a
//!   `vision.governor.step` audit record at each step, and resumes when the next period
//!   starts.
//! - **Privacy.** Every frame passes a [`PrivacyHook`] before any other stage sees it. The
//!   only way to a [`RedactedFrame`] is [`FramePrivacy::apply`], and [`FrameKeeper`] stores
//!   nothing else, so a frame is blurred before any artifact write (V-I9, S18).
//! - **Retention.** Frames stay ephemeral unless they become a wake payload, evidence on a
//!   card, or a sample a person kept; [`FrameKeeper`] stores those for 30 days by default
//!   (Q13), purges them after, and writes `vision.evidence.kept` and `.purged` records.
//! - **Detection and tracking.** The detector runs through [`VisionRuntime`], so each frame
//!   is metered like any other call and its regions come back in source pixels; the
//!   [`Tracker`] gives them anonymous track identifiers that reset with the source session.
//! - **Observations.** Each processed frame reaches an [`ObservationSink`]: watch
//!   conditions, wakes and the live camera surface consume the stream there.
//!
//! With the `conformance` feature, [`check_frame_source`] checks any source against the
//! [`FrameSource`] contract: refused bad rates, rising indices and times, the sampling rate,
//! the size cap, and ephemeral frames.
//!
//! The kill switch pauses a stream at its next frame (V-I17): while the run's governance
//! control is frozen, frames are dropped unprocessed, and the pipeline records
//! `vision.source.paused` and, after the thaw, `vision.source.resumed`.
//!
//! [`VisionRuntime`]: crate::VisionRuntime
//! [`Tracker`]: crate::Tracker
//! [`Sampling`]: crate::Sampling

mod governor;
mod pipeline;
mod privacy;
#[cfg(any(
    feature = "rtsp",
    feature = "ffmpeg",
    feature = "screen",
    feature = "browser",
    feature = "surface-camera"
))]
mod queue;
mod records;
mod retention;
mod sampler;

#[cfg(feature = "browser")]
mod browser;
#[cfg(feature = "surface-camera")]
mod camera;
#[cfg(any(feature = "screen", feature = "browser"))]
mod capture;
#[cfg(feature = "conformance")]
mod conformance;
#[cfg(feature = "ffmpeg")]
mod ffmpeg;
#[cfg(feature = "stream-fixtures")]
pub mod fixtures;
#[cfg(feature = "rtsp")]
mod rtsp;
#[cfg(feature = "screen")]
mod screen;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::Result;
use crate::media::{Frame, FrameSourceId};
use crate::traits::Sampling;

#[cfg(feature = "browser")]
pub use browser::BrowserSource;
#[cfg(feature = "surface-camera")]
pub use camera::{CameraFeed, SurfaceCameraSource};
#[cfg(any(feature = "screen", feature = "browser"))]
pub use capture::observation_image;
#[cfg(feature = "conformance")]
pub use conformance::{FrameSourceReport, check_frame_source};
#[cfg(feature = "ffmpeg")]
pub use ffmpeg::{FFMPEG_ENV, FfmpegBinary, FileSource};
pub use governor::{
    Admission, EscalationDecision, EscalationRefusal, EscalationVerdict, FrameBudget,
    FrameGovernor, FrameVerdict, GovernorChange, GovernorState, GovernorStep, StopReason,
};
pub use pipeline::{
    GovernorHandle, Observation, ObservationSink, SourceEvent, StopCause, StreamPipeline,
    StreamPipelineBuilder, StreamSummary,
};
pub use privacy::{
    FramePrivacy, NoRedaction, PrivacyHook, RedactedFrame, RedactionMethod, RedactorPrivacy,
};
pub use records::{
    VISION_EVIDENCE_KEPT, VISION_EVIDENCE_PURGED, VISION_GOVERNOR_STEP, VISION_SOURCE_PAUSED,
    VISION_SOURCE_RESUMED, VISION_SOURCE_STARTED, VISION_SOURCE_STOPPED,
};
pub use retention::{
    EVIDENCE_RETENTION, FrameKeeper, KeepReason, PurgeCause, PurgedFrame, RetentionEntry,
    RetentionPolicy,
};
#[cfg(feature = "rtsp")]
pub use rtsp::{RtspSource, RtspTransport};
pub use sampler::Sampler;
#[cfg(feature = "screen")]
pub use screen::{ScreenSource, VisualObserver};

/// How many decoded frames a source holds for a consumer that has fallen behind, before it
/// drops the oldest.
pub const SOURCE_QUEUE_FRAMES: usize = 4;

/// Decoded frames from a [`FrameSource`], in presentation order. The stream ends when the
/// source does; dropping it stops the source and any process it spawned.
pub type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame>> + Send>>;

/// A source of decoded frames: a camera, a file, a screen, a browser or the web home's
/// camera (spec 3.7 §7.13).
///
/// # Example
///
/// ```
/// use adk_vision::stream::{FrameSource, FrameStream};
/// use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, Sampling};
/// use futures::StreamExt;
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// /// Ten grey frames, 100 ms apart, sampled at the rate asked for.
/// struct Grey(FrameSourceId);
///
/// #[async_trait::async_trait]
/// impl FrameSource for Grey {
///     fn id(&self) -> &FrameSourceId {
///         &self.0
///     }
///
///     async fn frames(&self, sampling: Sampling) -> adk_vision::Result<FrameStream> {
///         let mut sampler = adk_vision::stream::Sampler::new(sampling)?;
///         let source = self.0.clone();
///         let frames = (0..10u64).filter_map(move |index| {
///             let pts = Duration::from_millis(index * 100);
///             sampler.admit(pts).then(|| {
///                 Ok(Frame {
///                     source: source.clone(),
///                     index,
///                     pts,
///                     captured_at: chrono::Utc::now(),
///                     pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(4, 4, [128; 4]))),
///                 })
///             })
///         });
///         Ok(futures::stream::iter(frames.collect::<Vec<_>>()).boxed())
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let source = Grey(FrameSourceId("grey".into()));
/// let frames: Vec<_> = source
///     .frames(Sampling { fps: 5.0, max_long_edge: None })
///     .await
///     .unwrap()
///     .collect()
///     .await;
/// // 10 frames a second sampled at 5 a second.
/// assert_eq!(frames.len(), 5);
/// # });
/// ```
#[async_trait]
pub trait FrameSource: Send + Sync {
    /// The source's identifier, which names it in frames, records and wakes. It never holds
    /// a credential.
    fn id(&self) -> &FrameSourceId;

    /// Decoded frames at the requested rate and size; the source drops what it cannot keep
    /// up with.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`](crate::VisionError::InvalidInput) for a sampling rate
    /// that is not a positive number, or the source's own refusal to open: an unreachable
    /// camera, a missing `ffmpeg`, a file that is not a video.
    async fn frames(&self, sampling: Sampling) -> Result<FrameStream>;
}
