//! The privacy step every stream frame passes before anything else sees it (spec 3.7
//! §7.13, V-I9, S18).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::cx::VisionCx;
use crate::digest::Digest;
use crate::edit::{Rgba, ops};
use crate::egress::Redactor;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, TransformChain};
use crate::media::{DecodedImage, Frame, FrameMeta, FramePixels, FrameSourceId};
use crate::redact::REDACTION_MARGIN;
use crate::sent::SentImage;

/// Redacts a frame's pixels in place before any other stage sees them: the face blur of
/// spec 3.7 S18 ([`RedactorPrivacy::blur_faces`]), any [`RedactorPrivacy`], or a hook of
/// your own.
///
/// A hook that fails fails closed: the pipeline drops the frame, so nothing downstream sees
/// or stores it.
///
/// # Example
///
/// ```
/// use adk_vision::stream::PrivacyHook;
/// use adk_vision::{BBox, DecodedImage, FrameMeta, VisionCx};
///
/// /// Blacks out a fixed window a camera can see into.
/// struct Window;
///
/// #[async_trait::async_trait]
/// impl PrivacyHook for Window {
///     fn name(&self) -> &str {
///         "window"
///     }
///
///     async fn redact(
///         &self,
///         _cx: &VisionCx,
///         _frame: &FrameMeta,
///         pixels: &mut DecodedImage,
///     ) -> adk_vision::Result<Vec<BBox>> {
///         let (w, _) = pixels.size();
///         Ok(vec![BBox::new(w as f32 / 2.0, 0.0, w as f32, 40.0)])
///     }
/// }
/// ```
#[async_trait]
pub trait PrivacyHook: Send + Sync {
    /// The hook's name, recorded with every frame it passed: `none` for [`NoRedaction`].
    fn name(&self) -> &str;

    /// Redacts `pixels` in place and returns the boxes it redacted, in the frame's pixels.
    ///
    /// # Errors
    ///
    /// Any failure: a detector that cannot load, a cancelled call. The frame is dropped.
    async fn redact(
        &self,
        cx: &VisionCx,
        frame: &FrameMeta,
        pixels: &mut DecodedImage,
    ) -> Result<Vec<BBox>>;
}

/// The explicit choice to redact nothing, for a scene that never shows a person: a
/// conveyor of parts, a gauge, a screen with no camera in it. Kept frames record `none` as
/// their privacy step.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRedaction;

#[async_trait]
impl PrivacyHook for NoRedaction {
    fn name(&self) -> &str {
        "none"
    }

    async fn redact(
        &self,
        _cx: &VisionCx,
        _frame: &FrameMeta,
        _pixels: &mut DecodedImage,
    ) -> Result<Vec<BBox>> {
        Ok(Vec::new())
    }
}

/// How [`RedactorPrivacy`] hides what its redactor finds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum RedactionMethod {
    /// Solid black: cannot be reversed.
    Fill,
    /// Gaussian blur of this standard deviation, in pixels.
    Blur {
        /// The standard deviation.
        sigma: f32,
    },
    /// Square blocks of this size, in pixels.
    Pixelate {
        /// The block size.
        block: u32,
    },
    /// Gaussian blur whose standard deviation is this share of each box's longer side, so a
    /// face near the camera is blurred as hard as a far one. At
    /// [`FACE_BLUR`](RedactionMethod::FACE_BLUR) YuNet no longer finds the face it blurred.
    ScaledBlur {
        /// The share of the box's longer side.
        fraction: f32,
    },
}

impl RedactionMethod {
    /// The face blur of spec 3.7 S18: a standard deviation of a quarter of the face box.
    pub const FACE_BLUR: Self = Self::ScaledBlur { fraction: 0.25 };
}

/// A [`PrivacyHook`] over any on-device egress [`Redactor`]: the boxes it finds, plus a
/// two-pixel margin, are filled, blurred or pixelated.
///
/// Over [`FaceRedactor`](crate::FaceRedactor) with [`RedactionMethod::FACE_BLUR`] it is the
/// face blur of spec 3.7 S18 ([`blur_faces`](Self::blur_faces)): each face blurred, each
/// person the face detector finds no face for blurred whole. Over
/// [`PersonRedactor`](crate::PersonRedactor) it covers every detected person whole.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::stream::{RedactionMethod, RedactorPrivacy};
/// use adk_vision::{BBox, RedactionClass, Redactor, SentImage, VisionCx};
///
/// struct Corner;
///
/// #[async_trait::async_trait]
/// impl Redactor for Corner {
///     fn class(&self) -> RedactionClass {
///         RedactionClass::faces()
///     }
///     async fn find(&self, _cx: &VisionCx, _image: &SentImage) -> adk_vision::Result<Vec<BBox>> {
///         Ok(vec![BBox::new(0.0, 0.0, 8.0, 8.0)])
///     }
/// }
///
/// let hook = RedactorPrivacy::new(Arc::new(Corner), RedactionMethod::Fill);
/// assert_eq!(adk_vision::stream::PrivacyHook::name(&hook), "redactor:faces");
/// ```
pub struct RedactorPrivacy {
    redactor: Arc<dyn Redactor>,
    method: RedactionMethod,
    name: String,
    hold: u64,
    /// Each source's boxes found in its latest frames, by frame index.
    held: std::sync::Mutex<std::collections::HashMap<FrameSourceId, Vec<(u64, BBox)>>>,
}

/// How much a held box grows per frame it is held, as a share of its longer side.
const HOLD_GROWTH: f32 = 0.25;

impl std::fmt::Debug for RedactorPrivacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedactorPrivacy")
            .field("name", &self.name)
            .field("method", &self.method)
            .field("hold", &self.hold)
            .finish_non_exhaustive()
    }
}

impl RedactorPrivacy {
    /// Redacts what `redactor` finds with `method`.
    pub fn new(redactor: Arc<dyn Redactor>, method: RedactionMethod) -> Self {
        let name = format!("redactor:{}", redactor.class());
        Self { redactor, method, name, hold: 0, held: std::sync::Mutex::default() }
    }

    /// Holds each box for `frames` more frames of its source: a box found in one of the
    /// last `frames` frames that nothing found in this frame overlaps is redacted again,
    /// widened by a quarter of its longer side per frame since, because a detector misses a
    /// moving face for a frame now and then. Nothing is held by default.
    pub fn with_hold(mut self, frames: u64) -> Self {
        self.hold = frames;
        self
    }

    /// Blurs faces on the device before anything is written (spec 3.7 S18, V-I9):
    /// [`FaceRedactor::on_device`](crate::FaceRedactor::on_device) — YuNet, with RF-DETR
    /// Nano covering each person it finds no face for — under
    /// [`RedactionMethod::FACE_BLUR`] (feature `onnx`).
    ///
    /// Each face is held for two more frames ([`with_hold`](Self::with_hold)), so a frame in
    /// which YuNet misses a face it found just before still has it blurred. The models load
    /// on the first frame. A frame whose redactor fails is dropped, so no later stage sees,
    /// and no keeper stores, a frame that was not redacted.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_vision::stream::{PrivacyHook, RedactorPrivacy};
    /// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher};
    ///
    /// let fetcher =
    ///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    /// let hook = RedactorPrivacy::blur_faces(fetcher);
    /// assert_eq!(hook.name(), "redactor:faces");
    /// ```
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
    pub fn blur_faces(fetcher: crate::ModelFetcher) -> Self {
        Self::new(Arc::new(crate::FaceRedactor::on_device(fetcher)), RedactionMethod::FACE_BLUR)
            .with_hold(2)
    }
}

#[async_trait]
impl PrivacyHook for RedactorPrivacy {
    fn name(&self) -> &str {
        &self.name
    }

    async fn redact(
        &self,
        cx: &VisionCx,
        frame: &FrameMeta,
        pixels: &mut DecodedImage,
    ) -> Result<Vec<BBox>> {
        let (w, h) = pixels.size();
        // The digest only labels this ephemeral view for the redactor; an ephemeral frame
        // has no encoded form to hash, and the raw pixels identify it as well.
        let view = SentImage::new(
            Digest::of(pixels.rgba()),
            Arc::new(pixels.clone()),
            TransformChain::new(w, h),
        );
        let found = self.redactor.find(cx, &view).await?;
        let mut boxes = found.clone();
        if self.hold > 0 {
            let mut held = self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let recent = held.entry(frame.source.clone()).or_default();
            // A source that starts again numbers its frames from zero: nothing carries over.
            recent.retain(|(then, _)| frame.index > *then && frame.index - then <= self.hold);
            for (then, bbox) in recent.iter() {
                let age = (frame.index - then) as f32;
                let grown = bbox
                    .expanded(HOLD_GROWTH * age * bbox.width().max(bbox.height()))
                    .clamped(w, h);
                if !found.iter().any(|f| f.intersection(&grown).is_some()) {
                    boxes.push(grown);
                }
            }
            recent.extend(found.iter().map(|bbox| (frame.index, *bbox)));
        }
        let mut redacted = Vec::with_capacity(boxes.len());
        for bbox in boxes {
            let padded = BBox::new(
                bbox.x0 - REDACTION_MARGIN,
                bbox.y0 - REDACTION_MARGIN,
                bbox.x1 + REDACTION_MARGIN,
                bbox.y1 + REDACTION_MARGIN,
            );
            match self.method {
                RedactionMethod::Fill => {
                    ops::fill(pixels, &padded, Rgba([0, 0, 0, 255]));
                }
                RedactionMethod::Blur { sigma } => ops::blur(pixels, &padded, sigma),
                RedactionMethod::Pixelate { block } => ops::pixelate(pixels, &padded, block.max(2)),
                RedactionMethod::ScaledBlur { fraction } => {
                    let sigma = fraction * padded.width().max(padded.height());
                    ops::fast_blur(pixels, &padded, sigma);
                }
            }
            redacted.push(padded);
        }
        Ok(redacted)
    }
}

/// A frame that has passed a [`PrivacyHook`]: the only form of a frame the pipeline's later
/// stages see and [`FrameKeeper`](super::FrameKeeper) stores.
///
/// [`FramePrivacy::apply`] is the only way to make one, so an unredacted frame cannot reach
/// an artifact write (V-I9):
///
/// ```compile_fail,E0451
/// use std::sync::Arc;
/// use adk_vision::stream::RedactedFrame;
/// use adk_vision::{DecodedImage, FrameMeta, FrameSourceId};
///
/// let meta = FrameMeta { source: FrameSourceId("cam".into()), index: 0, pts_ms: 0, captured_at: chrono::Utc::now() };
/// let forged = RedactedFrame { meta, pixels: Arc::new(DecodedImage::filled(1, 1, [0; 4])), hook: "none".into(), redacted: vec![] };
/// ```
#[derive(Clone, Debug)]
pub struct RedactedFrame {
    meta: FrameMeta,
    pixels: Arc<DecodedImage>,
    hook: String,
    redacted: Vec<BBox>,
}

impl RedactedFrame {
    /// The frame's identity and timing.
    pub fn meta(&self) -> &FrameMeta {
        &self.meta
    }

    /// The redacted pixels.
    pub fn pixels(&self) -> &DecodedImage {
        &self.pixels
    }

    pub(crate) fn shared_pixels(&self) -> Arc<DecodedImage> {
        Arc::clone(&self.pixels)
    }

    /// The name of the hook the frame passed.
    pub fn hook(&self) -> &str {
        &self.hook
    }

    /// The boxes the hook redacted, in the frame's pixels.
    pub fn redacted(&self) -> &[BBox] {
        &self.redacted
    }
}

/// Runs frames through a [`PrivacyHook`].
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::stream::{FramePrivacy, NoRedaction};
/// use adk_vision::{DecodedImage, Frame, FramePixels, FrameSourceId, VisionCx};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let privacy = FramePrivacy::new(Arc::new(NoRedaction));
/// let frame = Frame {
///     source: FrameSourceId("line-1".into()),
///     index: 0,
///     pts: std::time::Duration::ZERO,
///     captured_at: chrono::Utc::now(),
///     pixels: FramePixels::Ephemeral(Arc::new(DecodedImage::filled(4, 4, [0; 4]))),
/// };
/// let redacted = privacy.apply(&VisionCx::new(), frame).await.unwrap();
/// assert_eq!(redacted.hook(), "none");
/// # });
/// ```
#[derive(Clone)]
pub struct FramePrivacy {
    hook: Arc<dyn PrivacyHook>,
}

impl std::fmt::Debug for FramePrivacy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramePrivacy").field("hook", &self.hook.name()).finish()
    }
}

impl FramePrivacy {
    /// Frames through `hook`.
    pub fn new(hook: Arc<dyn PrivacyHook>) -> Self {
        Self { hook }
    }

    /// The hook's name.
    pub fn hook_name(&self) -> &str {
        self.hook.name()
    }

    /// Redacts `frame`.
    ///
    /// # Errors
    ///
    /// The hook's error, or [`VisionError::InvalidInput`] for a frame that is already stored:
    /// it passed its privacy step before its write.
    pub async fn apply(&self, cx: &VisionCx, frame: Frame) -> Result<RedactedFrame> {
        let meta = frame.meta();
        let shared = match frame.pixels {
            FramePixels::Ephemeral(pixels) => pixels,
            FramePixels::Stored(image) => {
                return Err(VisionError::InvalidInput(format!(
                    "frame {} of `{}` is already stored as {}; a stored frame was redacted before its write",
                    meta.index, meta.source.0, image.digest
                )));
            }
        };
        let mut pixels = Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone());
        let redacted = self.hook.redact(cx, &meta, &mut pixels).await?;
        Ok(RedactedFrame {
            meta,
            pixels: Arc::new(pixels),
            hook: self.hook.name().to_string(),
            redacted,
        })
    }
}
