//! [`SurfaceCameraSource`]: the frames the web home captures, pushed in through a
//! [`CameraFeed`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;

use super::queue::{Pushed, QueueSender, queue};
use super::{FrameSource, FrameStream, SOURCE_QUEUE_FRAMES, Sampler};
use crate::error::{Result, VisionError};
use crate::media::{DecodeLimits, DecodedImage, Frame, FramePixels, FrameSourceId};
use crate::traits::Sampling;

type Encoded = (Vec<u8>, DateTime<Utc>);

struct Shared {
    on: AtomicBool,
    consumer: Mutex<Option<QueueSender<Encoded>>>,
}

impl Shared {
    fn consumer(&self) -> std::sync::MutexGuard<'_, Option<QueueSender<Encoded>>> {
        self.consumer.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The push side of a [`SurfaceCameraSource`]: what the web home's frame handler calls.
///
/// The camera starts off. While it is off, or while no stream reads the source, a pushed
/// frame is dropped on the server at once (S12), and switching it off also drops the frames
/// already queued. Frames are decoded only when the stream reads them.
///
/// The voice screen's frame gate, `adk_surface::voice::VisualInputs`, decides what reaches a
/// realtime provider; a handler that also feeds the vision pipeline pushes the same frames
/// here and mirrors the camera switch with [`switch`](Self::switch).
#[derive(Clone)]
pub struct CameraFeed {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for CameraFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CameraFeed").field("on", &self.is_on()).finish_non_exhaustive()
    }
}

impl CameraFeed {
    /// Turns the camera on or off; off drops every queued frame.
    pub fn switch(&self, on: bool) {
        self.shared.on.store(on, Ordering::SeqCst);
        if !on && let Some(consumer) = self.shared.consumer().as_ref() {
            consumer.clear();
        }
    }

    /// Whether the camera is on.
    pub fn is_on(&self) -> bool {
        self.shared.on.load(Ordering::SeqCst)
    }

    /// Offers one encoded frame (JPEG, as the web home sends; PNG and WebP decode too),
    /// captured at `captured_at`. Returns whether it was queued: `false` while the camera is
    /// off or no stream reads the source.
    pub fn push_frame(&self, encoded: Vec<u8>, captured_at: DateTime<Utc>) -> bool {
        if !self.is_on() {
            return false;
        }
        let consumer = self.shared.consumer();
        match consumer.as_ref() {
            Some(sender) => sender.push((encoded, captured_at)) != Pushed::Closed,
            None => false,
        }
    }
}

/// The camera frames the web home already captures for the voice screen (spec 3.7 §7.13),
/// as a [`FrameSource`].
///
/// `adk-vision` does not depend on `adk-surface` — `adk-surface` draws regions from their
/// published schema — so frames come in through the [`CameraFeed`] this constructor returns,
/// which the surface's frame handler calls. One stream reads the source at a time; a second
/// call to [`frames`](FrameSource::frames) ends the first stream.
///
/// # Example
///
/// ```
/// use adk_vision::stream::{FrameSource, SurfaceCameraSource};
/// use adk_vision::{DecodedImage, FrameSourceId, Sampling};
/// use futures::StreamExt;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let (camera, feed) = SurfaceCameraSource::new(FrameSourceId("web-home".into()));
/// let png = DecodedImage::filled(8, 6, [200; 4]).encode_png().unwrap();
/// let mut frames = camera.frames(Sampling { fps: 10.0, max_long_edge: None }).await.unwrap();
/// assert!(!feed.push_frame(png.clone(), chrono::Utc::now()), "off: dropped");
/// feed.switch(true);
/// assert!(feed.push_frame(png, chrono::Utc::now()));
/// let frame = frames.next().await.unwrap().unwrap();
/// assert_eq!(frame.index, 0);
/// # });
/// ```
pub struct SurfaceCameraSource {
    id: FrameSourceId,
    shared: Arc<Shared>,
    limits: DecodeLimits,
}

impl std::fmt::Debug for SurfaceCameraSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceCameraSource").field("id", &self.id).finish_non_exhaustive()
    }
}

impl SurfaceCameraSource {
    /// A source and the feed that pushes frames into it, the camera off.
    pub fn new(id: FrameSourceId) -> (Self, CameraFeed) {
        let shared = Arc::new(Shared { on: AtomicBool::new(false), consumer: Mutex::new(None) });
        let feed = CameraFeed { shared: Arc::clone(&shared) };
        (Self { id, shared, limits: DecodeLimits::default() }, feed)
    }
}

#[async_trait]
impl FrameSource for SurfaceCameraSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        let sampler = Sampler::new(sampling)?;
        let (tx, rx) = queue::<Encoded>(SOURCE_QUEUE_FRAMES);
        // Replacing the sender ends the previous stream.
        *self.shared.consumer() = Some(tx);
        let id = self.id.clone();
        let limits = self.limits;
        struct State {
            sampler: Sampler,
            first: Option<DateTime<Utc>>,
            index: u64,
        }
        let state = State { sampler, first: None, index: 0 };
        let frames = futures::stream::unfold((rx, state), move |(rx, mut state)| {
            let id = id.clone();
            async move {
                loop {
                    let (encoded, captured_at) = rx.recv().await?;
                    let first = *state.first.get_or_insert(captured_at);
                    let pts = (captured_at - first).to_std().unwrap_or(Duration::ZERO);
                    let index = state.index;
                    state.index += 1;
                    if !state.sampler.admit(pts) {
                        continue;
                    }
                    let fitter = state.sampler.clone();
                    let decoded = tokio::task::spawn_blocking(move || {
                        DecodedImage::decode(&encoded, &limits)
                            .map(|(pixels, _)| fitter.fit(pixels))
                    })
                    .await
                    .unwrap_or_else(|err| {
                        Err(VisionError::Decode(format!("camera frame decode panicked: {err}")))
                    });
                    // One bad frame from a browser is skipped, not the end of the camera.
                    let pixels = match decoded {
                        Ok(pixels) => pixels,
                        Err(err) => {
                            tracing::warn!(frame.source = %id.0, frame.index = index, error = %err, "camera frame skipped");
                            continue;
                        }
                    };
                    let frame = Frame {
                        source: id.clone(),
                        index,
                        pts,
                        captured_at,
                        pixels: FramePixels::Ephemeral(Arc::new(pixels)),
                    };
                    return Some((Ok(frame), (rx, state)));
                }
            }
        });
        Ok(frames.boxed())
    }
}
