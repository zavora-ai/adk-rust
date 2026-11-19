//! On-device redactors for the egress gate's third step (spec 3.7 §7.8).
//!
//! A [`Redactor`] answers in the pixels of the minimised image the gate is about to release;
//! the gate fills each box solid black. The redactors here run any on-device backend:
//!
//! | Redactor | Class | Finds |
//! |----------|-------|-------|
//! | [`PersonRedactor`] | `faces` | The whole box of every person a [`Detector`] finds (spec 3.7 Q3) |
//! | `PersonalTextRedactor` (feature `guardrail`) | `personal_text` | Words a [`DocumentReader`](crate::DocumentReader) reads that `PiiRedactor` patterns or labelled fields match |
//!
//! A redactor refuses a remote backend: what it is shown has not been redacted yet.

use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::{BackendDescriptor, Oversize};
use crate::cx::VisionCx;
use crate::edit::ops;
use crate::egress::{RedactionClass, Redactor};
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, RawRegion, RegionNormalizer, Transform};
use crate::sent::SentImage;
use crate::traits::{DetectQuery, Detector};

/// Pixels added around each box a redactor finds, so anti-aliased edges are covered too.
pub const REDACTION_MARGIN: f32 = 2.0;

/// Redacts faces as the whole box of every detected person (spec 3.7 Q3).
///
/// Until a permissively licensed face detector is chosen, `faces` redaction covers each
/// person a [`Detector`] finds — RF-DETR's COCO `person` class, or any detector given the
/// same labels — from head to foot. The detector runs on the device; regions it returns
/// without a label are covered too, since a privacy step errs toward covering.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::{
///     BBox, BackendDescriptor, CoordFrame, DetectQuery, Detector, Locality, PersonRedactor,
///     RawRegion, RedactionClass, Redactor, SentImage, VisionCx,
/// };
///
/// /// Finds one person at a fixed place, in the pixels it is sent.
/// struct OnePerson(BackendDescriptor);
///
/// #[async_trait::async_trait]
/// impl Detector for OnePerson {
///     fn descriptor(&self) -> &BackendDescriptor {
///         &self.0
///     }
///     async fn detect(&self, _: &VisionCx, _: &SentImage, _: &DetectQuery) -> adk_vision::Result<Vec<RawRegion>> {
///         Ok(vec![RawRegion::boxed(BBox::new(10.0, 10.0, 30.0, 60.0), "person", 0.9)])
///     }
/// }
///
/// let detector = OnePerson(BackendDescriptor::new("people", Locality::InProcess, CoordFrame::SentPixels));
/// let redactor = PersonRedactor::new(Arc::new(detector));
/// assert_eq!(redactor.class(), RedactionClass::faces());
/// ```
pub struct PersonRedactor {
    detector: Arc<dyn Detector>,
    labels: Vec<String>,
    min_score: f32,
}

impl std::fmt::Debug for PersonRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonRedactor")
            .field("detector", &self.detector.descriptor().id)
            .field("labels", &self.labels)
            .field("min_score", &self.min_score)
            .finish()
    }
}

impl PersonRedactor {
    /// Covers every `person` the on-device `detector` finds with a score of 0.25 or more.
    pub fn new(detector: Arc<dyn Detector>) -> Self {
        Self { detector, labels: vec!["person".to_string()], min_score: 0.25 }
    }

    /// Covers these labels instead, such as `face` once a face detector is installed.
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// Covers detections scoring at least `min_score`.
    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = min_score;
        self
    }
}

#[async_trait]
impl Redactor for PersonRedactor {
    fn class(&self) -> RedactionClass {
        RedactionClass::faces()
    }

    async fn find(&self, cx: &VisionCx, image: &SentImage) -> Result<Vec<BBox>> {
        let descriptor = self.detector.descriptor();
        let fitted = fit_on_device(descriptor, image)?;
        let raw =
            self.detector.detect(cx, &fitted, &DetectQuery::Classes(self.labels.clone())).await?;
        let wanted = |region: &RawRegion| {
            let labelled = region.label.as_ref().is_none_or(|label| {
                self.labels.iter().any(|wanted| wanted.eq_ignore_ascii_case(label))
            });
            labelled && region.score.is_none_or(|score| score >= self.min_score)
        };
        let raw: Vec<RawRegion> = raw.into_iter().filter(wanted).collect();
        to_image_boxes(descriptor, &fitted, image, raw)
    }
}

/// `image` fitted to an on-device backend's input limits, keeping the transform chain from
/// the source, so the backend's answer maps back.
pub(crate) fn fit_on_device(
    descriptor: &BackendDescriptor,
    image: &SentImage,
) -> Result<SentImage> {
    if descriptor.locality.is_remote() {
        return Err(VisionError::InvalidInput(format!(
            "backend `{}` is remote; a redactor runs on the device, before anything is released",
            descriptor.id
        )));
    }
    let Some((width, height)) = descriptor.limits.fit(image.size()) else {
        return Ok(image.clone());
    };
    if descriptor.limits.oversize == Oversize::Refuse {
        let (w, h) = image.size();
        return Err(VisionError::InputTooLarge {
            backend: descriptor.id.to_string(),
            limit: descriptor.limits.describe(),
            actual: format!("{w}x{h}"),
        });
    }
    let mut chain = image.chain().clone();
    chain.push(Transform::Resize { width, height })?;
    let pixels = ops::resize(image.pixels(), width, height);
    Ok(SentImage::new(image.source(), Arc::new(pixels), chain))
}

/// Maps a backend's raw regions on `fitted` to boxes in `image`'s pixels, widened by
/// [`REDACTION_MARGIN`].
pub(crate) fn to_image_boxes(
    descriptor: &BackendDescriptor,
    fitted: &SentImage,
    image: &SentImage,
    raw: Vec<RawRegion>,
) -> Result<Vec<BBox>> {
    let normalizer = RegionNormalizer::new(
        fitted.source(),
        fitted.chain().clone(),
        descriptor.frame,
        descriptor.id.clone(),
    );
    let (width, height) = image.size();
    let mut boxes = Vec::new();
    for region in normalizer.regions(raw)? {
        if let Some(bbox) = region.shape.bbox() {
            let target = image.chain().bbox_to_target(&bbox);
            boxes.push(target.expanded(REDACTION_MARGIN).clamped(width, height));
        }
    }
    Ok(boxes)
}
