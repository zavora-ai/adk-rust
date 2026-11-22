//! On-device redactors for the egress gate's third step (spec 3.7 §7.8).
//!
//! A [`Redactor`] answers in the pixels of the minimised image the gate is about to release;
//! the gate fills each box solid black. The redactors here run any on-device backend:
//!
//! | Redactor | Class | Finds |
//! |----------|-------|-------|
//! | [`FaceRedactor`] | `faces` | Each face a face detector finds, and the whole box of each person a person detector finds no face for (spec 3.7 Q3) |
//! | [`PersonRedactor`] | `faces` | The whole box of every person a [`Detector`] finds |
//! | `PersonalTextRedactor` (feature `guardrail`) | `personal_text` | Words a [`DocumentReader`](crate::DocumentReader) reads that `PiiRedactor` patterns or labelled fields match |
//!
//! A redactor refuses a remote backend: what it is shown has not been redacted yet.
//!
//! Each redactor takes any on-device backend, or builds the default one from the built-in
//! manifest: `FaceRedactor::on_device` (feature `onnx`) over YuNet with RF-DETR Nano as its
//! person fallback, `PersonRedactor::rf_detr` (feature `onnx`) over RF-DETR Nano, and
//! `PersonalTextRedactor::pp_ocr` (features `guardrail` and `ocr`) over PP-OCRv6. A default
//! backend loads on the redactor's first use, its weights verified against the manifest; a
//! load that fails — weights missing offline, a digest mismatch — fails that call, so the
//! egress gate refuses the release rather than send the image unredacted. The one exception
//! errs toward covering: a `FaceRedactor` whose face detector cannot load covers every person
//! its person detector finds as the whole person.

use std::future::Future;
use std::pin::Pin;
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

/// The built-in manifest of the person detector `PersonRedactor::rf_detr` and
/// `FaceRedactor::on_device` load: RF-DETR Nano, full precision.
pub const DEFAULT_PERSON_DETECTOR: &str = "rf-detr-n";

/// The built-in manifest of the face detector `FaceRedactor::on_device` loads: YuNet 2023mar
/// (MIT, OpenCV Zoo), 232 KB.
pub const DEFAULT_FACE_DETECTOR: &str = "yunet-2023mar";

/// What [`FaceRedactor`] adds on each side of a face box, as a share of the box's longer
/// side: YuNet boxes a face from brow to chin, and the cover takes in the hair, ears and jaw.
pub const FACE_MARGIN: f32 = 0.15;

/// One load of an on-device backend.
pub(crate) type Loading<T> = Pin<Box<dyn Future<Output = Result<Arc<T>>> + Send>>;

/// Loads an on-device backend.
pub(crate) type Load<T> = Box<dyn Fn() -> Loading<T> + Send + Sync>;

/// The backend a redactor runs: given, or loaded on first use and kept.
///
/// A failed load is not kept, so the next call tries again: weights fetched since, or a
/// network that came back, then serve.
pub(crate) struct OnDevice<T: ?Sized> {
    name: String,
    cell: tokio::sync::OnceCell<Arc<T>>,
    load: Load<T>,
}

impl<T: ?Sized + Send + Sync + 'static> OnDevice<T> {
    pub(crate) fn ready(name: impl Into<String>, backend: Arc<T>) -> Self {
        let name = name.into();
        let missing = name.clone();
        Self {
            cell: tokio::sync::OnceCell::new_with(Some(backend)),
            load: Box::new(move || {
                let missing = missing.clone();
                Box::pin(async move {
                    Err(VisionError::BackendNotFound(format!("redactor backend `{missing}`")))
                }) as Loading<T>
            }),
            name,
        }
    }

    #[cfg(any(
        feature = "onnx",
        feature = "onnx-dynamic",
        all(feature = "ocr", feature = "guardrail")
    ))]
    pub(crate) fn lazy(name: impl Into<String>, load: Load<T>) -> Self {
        Self { name: name.into(), cell: tokio::sync::OnceCell::new(), load }
    }

    /// The backend's id, or the manifest it loads from.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) async fn get(&self) -> Result<Arc<T>> {
        self.cell.get_or_try_init(|| (self.load)()).await.cloned()
    }
}

/// Redacts faces as the whole box of every detected person.
///
/// `faces` redaction with a `PersonRedactor` covers each person a [`Detector`] finds —
/// RF-DETR's COCO `person` class, or any detector given the same labels — from head to
/// foot, for a deployment with no face detector. [`FaceRedactor`] covers faces and keeps
/// this as its fallback. The detector runs on the device; regions it returns without a label
/// are covered too, since a privacy step errs toward covering. On an approval card its boxes
/// are drawn as [`RedactionClass::people`].
///
/// `PersonRedactor::rf_detr` (feature `onnx`) loads RF-DETR Nano from the built-in manifest
/// ([`DEFAULT_PERSON_DETECTOR`]) on first use.
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
    detector: OnDevice<dyn Detector>,
    labels: Vec<String>,
    min_score: f32,
}

impl std::fmt::Debug for PersonRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonRedactor")
            .field("detector", &self.detector.name())
            .field("labels", &self.labels)
            .field("min_score", &self.min_score)
            .finish()
    }
}

impl PersonRedactor {
    /// Covers every `person` the on-device `detector` finds with a score of 0.25 or more.
    pub fn new(detector: Arc<dyn Detector>) -> Self {
        let name = detector.descriptor().id.to_string();
        Self {
            detector: OnDevice::ready(name, detector),
            labels: vec!["person".to_string()],
            min_score: 0.25,
        }
    }

    /// Covers every `person` RF-DETR Nano finds with a score of 0.25 or more: the built-in
    /// [`DEFAULT_PERSON_DETECTOR`] manifest, fetched through `fetcher` and verified against
    /// its SHA-256 on the redactor's first use (feature `onnx`).
    ///
    /// Nothing loads here. When the weights cannot be had — `HF_HUB_OFFLINE` with an empty
    /// cache, a digest mismatch — `find` fails, and the egress gate refuses the release.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_vision::{
    ///     FetchConfig, InMemoryAcceptanceStore, ModelFetcher, PersonRedactor, RedactionClass,
    ///     Redactor,
    /// };
    ///
    /// let fetcher =
    ///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    /// let redactor = PersonRedactor::rf_detr(fetcher);
    /// assert_eq!(redactor.class(), RedactionClass::faces());
    /// ```
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
    pub fn rf_detr(fetcher: crate::ModelFetcher) -> Self {
        Self {
            detector: builtin_rf_detr(fetcher),
            labels: vec!["person".to_string()],
            min_score: 0.25,
        }
    }

    /// Covers these labels instead.
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
        let detector = self.detector.get().await?;
        detect_boxes(cx, detector.as_ref(), image, &self.labels, self.min_score).await
    }

    /// Every box as `people`: each covers a face as the whole person.
    async fn find_classified(
        &self,
        cx: &VisionCx,
        image: &SentImage,
    ) -> Result<Vec<(RedactionClass, BBox)>> {
        let boxes = self.find(cx, image).await?;
        Ok(boxes.into_iter().map(|bbox| (RedactionClass::people(), bbox)).collect())
    }
}

/// Covers faces: each face a face [`Detector`] finds, widened by [`FACE_MARGIN`], and — with
/// a person fallback — the whole box of each person it finds no face for (spec 3.7 Q3).
///
/// A person seen from behind, a face turned away or too large or small for the face
/// detector is still covered, as the whole person; a person whose face is found keeps the
/// rest of the picture, so a close desk frame is not blanked by one person's box. A face
/// counts as a person's when its centre lies inside the person's box.
///
/// Both detectors run on the device. A face detector that cannot load (its weights missing
/// offline, a digest mismatch) makes the redactor cover every detected person whole, with a
/// warning; with no person fallback, or a fallback that cannot load either, the call fails
/// and the egress gate refuses the release. On an approval card faces are drawn as `faces`
/// and whole persons as [`RedactionClass::people`].
///
/// `FaceRedactor::on_device` (feature `onnx`) is the default: YuNet
/// ([`DEFAULT_FACE_DETECTOR`]) with RF-DETR Nano ([`DEFAULT_PERSON_DETECTOR`]) as the person
/// fallback, each loaded on first use.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::{
///     BBox, BackendDescriptor, CoordFrame, DetectQuery, Detector, FaceRedactor, Locality,
///     RawRegion, RedactionClass, Redactor, SentImage, VisionCx,
/// };
///
/// /// Finds one region of a fixed label, in the pixels it is sent.
/// struct Fixed(BackendDescriptor, &'static str, BBox);
///
/// #[async_trait::async_trait]
/// impl Detector for Fixed {
///     fn descriptor(&self) -> &BackendDescriptor {
///         &self.0
///     }
///     async fn detect(&self, _: &VisionCx, _: &SentImage, _: &DetectQuery) -> adk_vision::Result<Vec<RawRegion>> {
///         Ok(vec![RawRegion::boxed(self.2, self.1, 0.9)])
///     }
/// }
///
/// let local = |id| BackendDescriptor::new(id, Locality::InProcess, CoordFrame::SentPixels);
/// let faces = Fixed(local("faces"), "face", BBox::new(40.0, 20.0, 60.0, 44.0));
/// let people = Fixed(local("people"), "person", BBox::new(20.0, 10.0, 80.0, 120.0));
/// let redactor = FaceRedactor::new(Arc::new(faces)).with_person_fallback(Arc::new(people));
/// assert_eq!(redactor.class(), RedactionClass::faces());
/// ```
pub struct FaceRedactor {
    faces: OnDevice<dyn Detector>,
    face_labels: Vec<String>,
    face_min_score: f32,
    people: Option<OnDevice<dyn Detector>>,
    person_labels: Vec<String>,
    person_min_score: f32,
    margin: f32,
}

impl std::fmt::Debug for FaceRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceRedactor")
            .field("faces", &self.faces.name())
            .field("face_min_score", &self.face_min_score)
            .field("people", &self.people.as_ref().map(OnDevice::name))
            .field("person_min_score", &self.person_min_score)
            .field("margin", &self.margin)
            .finish()
    }
}

impl FaceRedactor {
    /// Covers every `face` the on-device `faces` detector finds with a score of 0.5 or more,
    /// and nothing else: add [`with_person_fallback`](Self::with_person_fallback) to cover
    /// people whose faces it misses.
    pub fn new(faces: Arc<dyn Detector>) -> Self {
        let name = faces.descriptor().id.to_string();
        Self {
            faces: OnDevice::ready(name, faces),
            face_labels: vec!["face".to_string()],
            face_min_score: 0.5,
            people: None,
            person_labels: vec!["person".to_string()],
            person_min_score: 0.25,
            margin: FACE_MARGIN,
        }
    }

    /// Covers, as the whole person, each `person` the on-device `people` detector finds with
    /// a score of 0.25 or more and no face inside.
    pub fn with_person_fallback(mut self, people: Arc<dyn Detector>) -> Self {
        let name = people.descriptor().id.to_string();
        self.people = Some(OnDevice::ready(name, people));
        self
    }

    /// YuNet for faces and RF-DETR Nano for the person fallback, from the built-in
    /// [`DEFAULT_FACE_DETECTOR`] and [`DEFAULT_PERSON_DETECTOR`] manifests, fetched through
    /// `fetcher` and verified against their SHA-256 on the redactor's first use (feature
    /// `onnx`).
    ///
    /// Nothing loads here. When YuNet cannot be had the redactor covers whole persons; when
    /// RF-DETR cannot be had `find` fails, and the egress gate refuses the release.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_vision::{
    ///     FaceRedactor, FetchConfig, InMemoryAcceptanceStore, ModelFetcher, RedactionClass,
    ///     Redactor,
    /// };
    ///
    /// let fetcher =
    ///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    /// let redactor = FaceRedactor::on_device(fetcher);
    /// assert_eq!(redactor.class(), RedactionClass::faces());
    /// ```
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
    pub fn on_device(fetcher: crate::ModelFetcher) -> Self {
        let load_faces = fetcher.clone();
        let load: Load<dyn Detector> = Box::new(move || {
            let fetcher = load_faces.clone();
            Box::pin(async move {
                let model = fetch_builtin(&fetcher, DEFAULT_FACE_DETECTOR).await?;
                let detector = load_blocking(DEFAULT_FACE_DETECTOR, move || {
                    crate::onnx::YuNet::new(&model, &crate::onnx::OnnxOptions::default())
                })
                .await?
                // The redactor's own score filter decides; the detector reports more.
                .with_threshold(0.3);
                tracing::info!(vision.backend = DEFAULT_FACE_DETECTOR, "face redactor loaded");
                Ok(Arc::new(detector) as Arc<dyn Detector>)
            }) as Loading<dyn Detector>
        });
        Self {
            faces: OnDevice::lazy(DEFAULT_FACE_DETECTOR, load),
            face_labels: vec!["face".to_string()],
            face_min_score: 0.5,
            people: Some(builtin_rf_detr(fetcher)),
            person_labels: vec!["person".to_string()],
            person_min_score: 0.25,
            margin: FACE_MARGIN,
        }
    }

    /// Covers faces scoring at least `min_score` (0.5 by default).
    pub fn with_face_min_score(mut self, min_score: f32) -> Self {
        self.face_min_score = min_score;
        self
    }

    /// Falls back to people scoring at least `min_score` (0.25 by default).
    pub fn with_person_min_score(mut self, min_score: f32) -> Self {
        self.person_min_score = min_score;
        self
    }

    /// Widens each face box by `margin` of its longer side on each side ([`FACE_MARGIN`] by
    /// default).
    pub fn with_margin(mut self, margin: f32) -> Self {
        self.margin = margin.max(0.0);
        self
    }
}

#[async_trait]
impl Redactor for FaceRedactor {
    fn class(&self) -> RedactionClass {
        RedactionClass::faces()
    }

    async fn find(&self, cx: &VisionCx, image: &SentImage) -> Result<Vec<BBox>> {
        Ok(self.find_classified(cx, image).await?.into_iter().map(|(_, bbox)| bbox).collect())
    }

    /// Faces as `faces`, the whole persons it falls back to as `people`.
    async fn find_classified(
        &self,
        cx: &VisionCx,
        image: &SentImage,
    ) -> Result<Vec<(RedactionClass, BBox)>> {
        let faces = match self.faces.get().await {
            Ok(detector) => Some(
                detect_boxes(cx, detector.as_ref(), image, &self.face_labels, self.face_min_score)
                    .await?,
            ),
            Err(err) if self.people.is_some() => {
                tracing::warn!(
                    vision.backend = self.faces.name(),
                    error = %err,
                    "face detector unavailable, covering every detected person whole"
                );
                None
            }
            Err(err) => return Err(err),
        };
        let people = match &self.people {
            Some(people) => {
                let detector = people.get().await?;
                detect_boxes(
                    cx,
                    detector.as_ref(),
                    image,
                    &self.person_labels,
                    self.person_min_score,
                )
                .await?
            }
            None => Vec::new(),
        };
        let (width, height) = image.size();
        let mut found = Vec::new();
        for face in faces.iter().flatten() {
            let margin = self.margin * face.width().max(face.height());
            found.push((RedactionClass::faces(), face.expanded(margin).clamped(width, height)));
        }
        for person in people {
            let has_face = faces.iter().flatten().any(|face| person.contains(face.center()));
            if !has_face {
                found.push((RedactionClass::people(), person));
            }
        }
        Ok(found)
    }
}

/// The boxes of `labels` scoring at least `min_score` that an on-device `detector` finds in
/// `image`, in its pixels, widened by [`REDACTION_MARGIN`]. A region without a label or a
/// score is kept, since a privacy step errs toward covering.
async fn detect_boxes(
    cx: &VisionCx,
    detector: &dyn Detector,
    image: &SentImage,
    labels: &[String],
    min_score: f32,
) -> Result<Vec<BBox>> {
    let descriptor = detector.descriptor();
    let fitted = fit_on_device(descriptor, image)?;
    let raw = detector.detect(cx, &fitted, &DetectQuery::Classes(labels.to_vec())).await?;
    let wanted = |region: &RawRegion| {
        let labelled = region
            .label
            .as_ref()
            .is_none_or(|label| labels.iter().any(|wanted| wanted.eq_ignore_ascii_case(label)));
        labelled && region.score.is_none_or(|score| score >= min_score)
    };
    let raw: Vec<RawRegion> = raw.into_iter().filter(wanted).collect();
    to_image_boxes(descriptor, &fitted, image, raw)
}

/// RF-DETR Nano from the built-in manifest, loaded on first use with a threshold low enough
/// for the redactor's own score filter to decide.
#[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
fn builtin_rf_detr(fetcher: crate::ModelFetcher) -> OnDevice<dyn Detector> {
    let load: Load<dyn Detector> = Box::new(move || {
        let fetcher = fetcher.clone();
        Box::pin(async move {
            let model = fetch_builtin(&fetcher, DEFAULT_PERSON_DETECTOR).await?;
            let detector = load_blocking(DEFAULT_PERSON_DETECTOR, move || {
                crate::onnx::RfDetr::new(&model, &crate::onnx::OnnxOptions::default())
            })
            .await?
            .with_threshold(0.05);
            tracing::info!(vision.backend = DEFAULT_PERSON_DETECTOR, "person redactor loaded");
            Ok(Arc::new(detector) as Arc<dyn Detector>)
        }) as Loading<dyn Detector>
    });
    OnDevice::lazy(DEFAULT_PERSON_DETECTOR, load)
}

/// Fetches and verifies the built-in manifest `id`.
#[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
async fn fetch_builtin(fetcher: &crate::ModelFetcher, id: &str) -> Result<crate::VerifiedModel> {
    let manifest = crate::ManifestSet::builtin()?.get(id).cloned().ok_or_else(|| {
        VisionError::Manifest { id: id.into(), reason: "not in the built-in manifest".into() }
    })?;
    fetcher.fetch(&manifest).await
}

/// Builds a backend on a blocking thread, since onnxruntime's session load blocks.
#[cfg(any(feature = "onnx", feature = "onnx-dynamic"))]
async fn load_blocking<T: Send + 'static>(
    id: &str,
    build: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(build).await.map_err(|e| VisionError::Backend {
        backend: id.into(),
        message: format!("the load task failed: {e}"),
    })?
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
