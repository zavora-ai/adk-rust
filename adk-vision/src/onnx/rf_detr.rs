//! [`RfDetr`]: RF-DETR object detection, with instance masks from an RF-DETR-Seg export.

use async_trait::async_trait;

use super::OnnxOptions;
use super::preprocess::{Normalize, sigmoid, upsample_bilinear};
use super::session::{Input, OrtModel, check_manifest, manifest_error, onnx_file};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Mask, RawRegion, Shape};
use crate::manifest::{VerifiedModel, VisionTask};
use crate::sent::SentImage;
use crate::traits::{DetectQuery, Detector};

/// RF-DETR (Apache-2.0 for the N, S and M checkpoints) on ONNX Runtime.
///
/// The image is fitted into the manifest's input (`io.input`; RF-DETR stretches to a
/// square of 384, 512 or 576 px for N, S and M), rescaled to `[0, 1]` and normalised with
/// `io.mean` and `io.std` when the manifest gives them. The graph answers 300 queries with
/// class logits and normalised `(cx, cy, w, h)` boxes; every query-class pair whose sigmoid
/// score reaches the threshold (0.5 by default) is a detection, best first, as RF-DETR's
/// reference post-processing does. A set predictor needs no NMS. `io.labels` names each
/// logit column; a column with an empty label is never reported.
///
/// An RF-DETR-Seg export has a third output of per-query mask logits; each detection then
/// carries its instance mask, upsampled to the model input and thresholded at 0, in place of
/// its box.
///
/// The backend answers in `ModelInput` pixels; the runtime undoes the letterbox and the
/// transform chain, so every region reaches the caller in source pixels.
///
/// | Graph | Names accepted |
/// |-------|----------------|
/// | input | `pixel_values`, `input`, `images` (else the first input) |
/// | boxes | `pred_boxes`, `dets`, `boxes` (else the output whose last dimension is 4) |
/// | logits | `logits`, `labels`, `scores` |
/// | masks | `pred_masks`, `masks` (RF-DETR-Seg) |
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, RfDetr};
/// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let manifest = ManifestSet::builtin()?.get("rf-detr-n").cloned().expect("built in");
/// let fetcher =
///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let detector = RfDetr::new(&fetcher.fetch_blocking(&manifest)?, &OnnxOptions::default())?
///     .with_threshold(0.4);
/// # Ok(())
/// # }
/// ```
pub struct RfDetr {
    descriptor: BackendDescriptor,
    model: OrtModel,
    input: String,
    boxes: String,
    logits: String,
    masks: Option<String>,
    normalize: Normalize,
    labels: Vec<String>,
    threshold: f32,
    max_detections: usize,
}

impl std::fmt::Debug for RfDetr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RfDetr")
            .field("descriptor", &self.descriptor)
            .field("threshold", &self.threshold)
            .field("masks", &self.masks.is_some())
            .finish_non_exhaustive()
    }
}

impl RfDetr {
    /// Loads a verified RF-DETR or RF-DETR-Seg export.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest is not an `ort` detection model with an
    /// `io.input` and `io.labels` and exactly one `.onnx` file, or the graph lacks a boxes or
    /// logits output; [`VisionError::Backend`] when onnxruntime cannot load it.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let manifest = model.manifest();
        check_manifest(manifest, VisionTask::Detect)?;
        let Some(input) = manifest.io.input else {
            return Err(manifest_error(
                manifest,
                "RF-DETR needs `io.input`, the square it was trained at".into(),
            ));
        };
        if manifest.io.labels.iter().all(String::is_empty) {
            return Err(manifest_error(
                manifest,
                "RF-DETR needs `io.labels`, one per logit column".into(),
            ));
        }
        let session = OrtModel::load(&manifest.id, onnx_file(model, "", "")?, options)?;
        let outputs = session.outputs();
        let named = |names: &[&str]| {
            outputs.iter().find(|(n, _)| names.contains(&n.as_str())).map(|(n, _)| n.clone())
        };
        let boxes = named(&["pred_boxes", "dets", "boxes"]).or_else(|| {
            outputs.iter().find(|(_, s)| s.len() == 3 && s[2] == 4).map(|(n, _)| n.clone())
        });
        let logits = named(&["logits", "labels", "scores"]);
        let (Some(boxes), Some(logits)) = (boxes, logits) else {
            return Err(manifest_error(
                manifest,
                format!(
                    "the graph's outputs {:?} hold no boxes and logits pair",
                    outputs.iter().map(|(n, _)| n).collect::<Vec<_>>()
                ),
            ));
        };
        let masks = named(&["pred_masks", "masks"]);
        let input_name = session.input_named(&["pixel_values", "input", "images"], 0)?;
        let normalize =
            Normalize::from_io(&manifest.io, (input.width, input.height), input.letterbox);
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::ModelInput {
                width: input.width,
                height: input.height,
                letterbox: input.letterbox,
            },
        )
        .with_manifest(manifest.id.clone());
        Ok(Self {
            descriptor,
            model: session,
            input: input_name,
            boxes,
            logits,
            masks,
            normalize,
            labels: manifest.io.labels.clone(),
            threshold: 0.5,
            max_detections: 300,
        })
    }

    /// Reports detections scoring at least `threshold` (0.5 by default).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Reports at most `max` detections (300 by default).
    pub fn with_max_detections(mut self, max: usize) -> Self {
        self.max_detections = max;
        self
    }

    /// `true` when the graph answers with instance masks (RF-DETR-Seg).
    pub fn segments(&self) -> bool {
        self.masks.is_some()
    }

    /// The logit columns a query selects.
    fn columns(&self, query: &DetectQuery) -> Result<Vec<bool>> {
        let known = |c: usize| !self.labels[c].is_empty();
        match query {
            DetectQuery::Classes(names) if names.is_empty() => {
                Ok((0..self.labels.len()).map(known).collect())
            }
            DetectQuery::Classes(names) => {
                let mut wanted = vec![false; self.labels.len()];
                for name in names {
                    let column = self
                        .labels
                        .iter()
                        .position(|l| !l.is_empty() && l.eq_ignore_ascii_case(name.trim()))
                        .ok_or_else(|| {
                            VisionError::InvalidInput(format!(
                                "`{}` detects {} classes and `{name}` is not one of them, such as {}",
                                self.descriptor.id,
                                self.labels.iter().filter(|l| !l.is_empty()).count(),
                                self.labels
                                    .iter()
                                    .filter(|l| !l.is_empty())
                                    .take(5)
                                    .map(String::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        })?;
                    wanted[column] = true;
                }
                Ok(wanted)
            }
            DetectQuery::Concepts(_) => Err(VisionError::Unsupported(format!(
                "`{}` detects the closed set of classes its manifest lists; ask for \
                 `DetectQuery::Classes`, or route open-vocabulary concepts to a concept detector such as `sam-3.1`",
                self.descriptor.id
            ))),
        }
    }
}

/// One detection in model-input pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Detection {
    pub query: usize,
    pub class: usize,
    pub score: f32,
    pub bbox: BBox,
}

/// RF-DETR's post-processing: every `(query, class)` pair the columns allow whose sigmoid
/// score reaches `threshold`, best first, at most `max`, with its normalised
/// `(cx, cy, w, h)` box scaled to the `input` size.
pub(crate) fn decode(
    logits: &[f32],
    boxes: &[f32],
    classes: usize,
    columns: &[bool],
    input: (u32, u32),
    threshold: f32,
    max: usize,
) -> Vec<Detection> {
    let queries = boxes.len() / 4;
    let (w, h) = (input.0 as f32, input.1 as f32);
    let mut found: Vec<Detection> = Vec::new();
    for query in 0..queries.min(logits.len() / classes.max(1)) {
        for (class, &allowed) in columns.iter().enumerate().take(classes) {
            if !allowed {
                continue;
            }
            let score = sigmoid(logits[query * classes + class]);
            if score < threshold {
                continue;
            }
            let b = &boxes[query * 4..query * 4 + 4];
            let (cx, cy, bw, bh) = (b[0] * w, b[1] * h, b[2] * w, b[3] * h);
            let bbox = BBox::new(cx - bw / 2.0, cy - bh / 2.0, cx + bw / 2.0, cy + bh / 2.0);
            found.push(Detection { query, class, score, bbox });
        }
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    found.truncate(max);
    found
}

/// A query's mask logits, upsampled to the model input and thresholded at 0.
pub(crate) fn instance_mask(logits: &[f32], raster: (usize, usize), input: (u32, u32)) -> Mask {
    let (w, h) = (input.0 as usize, input.1 as usize);
    let up = upsample_bilinear(logits, raster, (w, h));
    let bits: Vec<bool> = up.iter().map(|&v| v > 0.0).collect();
    Mask::encode(input.0, input.1, &bits)
}

#[async_trait]
impl Detector for RfDetr {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        cx.check()?;
        let columns = self.columns(query)?;
        let (iw, ih) = self.normalize.size;
        let tensor = self.normalize.tensor(image.pixels());
        let mut wanted = vec![self.boxes.as_str(), self.logits.as_str()];
        if let Some(masks) = &self.masks {
            wanted.push(masks);
        }
        let outputs = self
            .model
            .run(
                vec![(
                    self.input.clone(),
                    Input::F32(vec![1, 3, i64::from(ih), i64::from(iw)], tensor),
                )],
                &wanted,
            )
            .await?;
        let (boxes, logits) = (&outputs[&self.boxes], &outputs[&self.logits]);
        let classes = logits.dims().last().copied().unwrap_or(0);
        if classes != self.labels.len() {
            return Err(self.model.error(format!(
                "the graph answers {classes} classes but the manifest labels {}",
                self.labels.len()
            )));
        }
        let detections = decode(
            &logits.data,
            &boxes.data,
            classes,
            &columns,
            (iw, ih),
            self.threshold,
            self.max_detections,
        );
        let masks = self.masks.as_ref().map(|name| &outputs[name]);
        let regions = detections
            .into_iter()
            .map(|d| {
                let shape = masks
                    .and_then(|m| {
                        let dims = m.dims();
                        let [_, _, mh, mw] = dims.as_slice() else { return None };
                        let size = mh * mw;
                        let logits = m.data.get(d.query * size..(d.query + 1) * size)?;
                        let mask = instance_mask(logits, (*mw, *mh), (iw, ih));
                        (mask.area() > 0).then_some(Shape::Mask(mask))
                    })
                    .unwrap_or(Shape::Box(d.bbox));
                RawRegion {
                    shape,
                    label: Some(self.labels[d.class].clone()),
                    score: Some(d.score),
                    track: None,
                }
            })
            .collect::<Vec<_>>();
        tracing::debug!(vision.backend = %self.descriptor.id, vision.regions = regions.len(), "rf-detr detected");
        Ok(regions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logit(p: f32) -> f32 {
        (p / (1.0 - p)).ln()
    }

    #[test]
    fn pairs_over_the_threshold_are_detections_best_first_in_input_pixels() {
        // Three queries over three columns; column 0 is unlabelled.
        let logits = [
            logit(0.9),
            logit(0.2),
            logit(0.7), // query 0: class 2 at 0.7
            logit(0.1),
            logit(0.95),
            logit(0.6), // query 1: class 1 at 0.95, class 2 at 0.6
            logit(0.3),
            logit(0.3),
            logit(0.3), // query 2: nothing
        ];
        let boxes = [
            0.5, 0.5, 0.5, 0.25, // query 0: centred, half wide, a quarter high
            0.25, 0.75, 0.1, 0.1, //
            0.5, 0.5, 1.0, 1.0,
        ];
        let columns = [false, true, true];
        let found = decode(&logits, &boxes, 3, &columns, (384, 384), 0.5, 300);
        assert_eq!(
            found.iter().map(|d| (d.query, d.class)).collect::<Vec<_>>(),
            vec![(1, 1), (0, 2), (1, 2)]
        );
        assert!((found[0].score - 0.95).abs() < 1e-5);
        let b = found[1].bbox;
        assert!(
            (b.x0 - 96.0).abs() < 1e-3
                && (b.y0 - 144.0).abs() < 1e-3
                && (b.x1 - 288.0).abs() < 1e-3
                && (b.y1 - 240.0).abs() < 1e-3,
            "{b:?}"
        );
        assert_eq!(decode(&logits, &boxes, 3, &columns, (384, 384), 0.5, 1).len(), 1);
        // Column 0 scores 0.9 but has no label, so it is never reported.
        assert!(found.iter().all(|d| d.class != 0));
    }

    #[test]
    fn instance_masks_are_upsampled_to_the_model_input() {
        // A 2x2 logit raster with the top-left cell set, into a 4x4 input.
        let mask = instance_mask(&[5.0, -5.0, -5.0, -5.0], (2, 2), (4, 4));
        assert_eq!((mask.width, mask.height), (4, 4));
        assert_eq!(mask.bbox(), Some(BBox::new(0.0, 0.0, 2.0, 2.0)));
    }
}
