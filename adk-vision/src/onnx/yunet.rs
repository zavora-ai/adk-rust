//! [`YuNet`]: the face detector of the privacy redactors (spec 3.7 Q3).

use async_trait::async_trait;
use image::imageops::{self, FilterType};

use super::OnnxOptions;
use super::session::{Input, OrtModel, check_manifest, manifest_error, onnx_file};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Letterbox, RawRegion};
use crate::manifest::{VerifiedModel, VisionTask};
use crate::media::DecodedImage;
use crate::sent::SentImage;
use crate::traits::{DetectQuery, Detector};

/// The strides of YuNet's three detection heads, in model-input pixels.
const STRIDES: [u32; 3] = [8, 16, 32];

/// YuNet (MIT, OpenCV Zoo `face_detection_yunet_2023mar`) on ONNX Runtime: faces, for the
/// privacy redactors only (V-I8). It detects; it never recognises or compares faces.
///
/// YuNet finds faces of about 10 to 300 pixels in its 640×640 input. The image is fitted
/// into the input with the manifest's letterbox (top-left for the built-in `yunet-2023mar`)
/// as BGR values from 0 to 255, once at the full input and once fitted into half of it, so a
/// face close to the camera that fills most of the frame is found too. Each head's anchor
/// scores `sqrt(cls × obj)`; anchors scoring at least the threshold (0.5 by default) are
/// detections, merged across the two passes by non-maximum suppression at IoU 0.3.
///
/// Regions are labelled with the manifest's one label (`face`) and answered in the pixels
/// of the image the backend received ([`CoordFrame::SentPixels`]).
///
/// | Graph | Names |
/// |-------|-------|
/// | input | `input` (else the first input), `1×3×640×640` |
/// | per stride 8, 16, 32 | `cls_<s>`, `obj_<s>` (`1×N×1`), `bbox_<s>` (`1×N×4`) |
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, YuNet};
/// use adk_vision::{
///     DEFAULT_FACE_DETECTOR, FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher,
/// };
///
/// # fn demo() -> adk_vision::Result<()> {
/// let manifest = ManifestSet::builtin()?.get(DEFAULT_FACE_DETECTOR).cloned().expect("built in");
/// let fetcher =
///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let faces = YuNet::new(&fetcher.fetch_blocking(&manifest)?, &OnnxOptions::default())?
///     .with_threshold(0.6);
/// # Ok(())
/// # }
/// ```
pub struct YuNet {
    descriptor: BackendDescriptor,
    model: OrtModel,
    input: String,
    size: (u32, u32),
    letterbox: Letterbox,
    label: String,
    threshold: f32,
    nms_iou: f32,
    max_detections: usize,
}

impl std::fmt::Debug for YuNet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YuNet")
            .field("descriptor", &self.descriptor)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

impl YuNet {
    /// Loads a verified YuNet export.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest is not an `ort` detection model with an
    /// `io.input`, exactly one label and exactly one `.onnx` file, or the graph lacks a
    /// `cls_<s>`, `obj_<s>` or `bbox_<s>` output for a stride; [`VisionError::Backend`] when
    /// onnxruntime cannot load it.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let manifest = model.manifest();
        check_manifest(manifest, VisionTask::Detect)?;
        let Some(input) = manifest.io.input else {
            return Err(manifest_error(
                manifest,
                "YuNet needs `io.input`, the size its graph takes".into(),
            ));
        };
        let label = match manifest.io.labels.as_slice() {
            [label] if !label.is_empty() => label.clone(),
            _ => {
                return Err(manifest_error(
                    manifest,
                    "YuNet needs exactly one label in `io.labels`, such as `face`".into(),
                ));
            }
        };
        let model_session = OrtModel::load(&manifest.id, onnx_file(model, "", "")?, options)?;
        let missing: Vec<String> = STRIDES
            .iter()
            .flat_map(|s| [format!("cls_{s}"), format!("obj_{s}"), format!("bbox_{s}")])
            .filter(|name| !model_session.outputs().iter().any(|(n, _)| n == name))
            .collect();
        if !missing.is_empty() {
            return Err(manifest_error(
                manifest,
                format!("the graph has no output {missing:?}; is it a YuNet 2023mar export?"),
            ));
        }
        let input_name = model_session.input_named(&["input"], 0)?;
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::SentPixels,
        )
        .with_manifest(manifest.id.clone());
        Ok(Self {
            descriptor,
            model: model_session,
            input: input_name,
            size: (input.width, input.height),
            letterbox: input.letterbox,
            label,
            threshold: 0.5,
            nms_iou: 0.3,
            max_detections: 300,
        })
    }

    /// Reports faces scoring at least `threshold` (0.5 by default; OpenCV's demo uses 0.9,
    /// and a privacy step errs toward covering).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Reports at most `max` faces (300 by default).
    pub fn with_max_detections(mut self, max: usize) -> Self {
        self.max_detections = max;
        self
    }

    fn check_query(&self, query: &DetectQuery) -> Result<()> {
        match query {
            DetectQuery::Classes(names)
                if names.iter().all(|n| n.trim().eq_ignore_ascii_case(&self.label)) =>
            {
                Ok(())
            }
            DetectQuery::Classes(names) => Err(VisionError::InvalidInput(format!(
                "`{}` detects `{}` only, not {:?}",
                self.descriptor.id, self.label, names
            ))),
            DetectQuery::Concepts(_) => Err(VisionError::Unsupported(format!(
                "`{}` detects `{}` only; ask for `DetectQuery::Classes`",
                self.descriptor.id, self.label
            ))),
        }
    }

    /// One pass: `pixels` fitted into `fit` (at most the input size) and placed at the
    /// input's top-left, the rest zero. Answers detections in `pixels`' own coordinates.
    async fn pass(&self, pixels: &DecodedImage, fit: (u32, u32)) -> Result<Vec<Detection>> {
        let (iw, ih) = self.size;
        let geometry = self.letterbox.geometry(pixels.size(), fit);
        let (rw, rh) = geometry.resized;
        let resized;
        let source = if (rw, rh) == pixels.size() {
            pixels.inner()
        } else {
            resized = imageops::resize(pixels.inner(), rw, rh, FilterType::Triangle);
            &resized
        };
        let (w, h) = (iw as usize, ih as usize);
        let mut tensor = vec![0.0f32; 3 * w * h];
        let (left, top) = (geometry.pad_left as usize, geometry.pad_top as usize);
        for (x, y, pixel) in source.enumerate_pixels() {
            let (tx, ty) = (x as usize + left, y as usize + top);
            if tx >= w || ty >= h {
                continue;
            }
            // BGR, as OpenCV hands YuNet its frames.
            for (plane, channel) in [2usize, 1, 0].into_iter().enumerate() {
                tensor[plane * w * h + ty * w + tx] = f32::from(pixel.0[channel]);
            }
        }
        let names: Vec<String> = STRIDES
            .iter()
            .flat_map(|s| [format!("cls_{s}"), format!("obj_{s}"), format!("bbox_{s}")])
            .collect();
        let wanted: Vec<&str> = names.iter().map(String::as_str).collect();
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
        let mut found = Vec::new();
        for stride in STRIDES {
            let head = Head {
                cls: &outputs[&format!("cls_{stride}")].data,
                obj: &outputs[&format!("obj_{stride}")].data,
                bbox: &outputs[&format!("bbox_{stride}")].data,
            };
            found.extend(decode(&head, stride, (iw, ih), self.threshold));
        }
        let (sx, sy) = (geometry.scale_x as f32, geometry.scale_y as f32);
        let (px, py) = (geometry.pad_left as f32, geometry.pad_top as f32);
        let (pw, ph) = pixels.size();
        Ok(found
            .into_iter()
            .map(|d| Detection {
                score: d.score,
                bbox: BBox::new(
                    (d.bbox.x0 - px) / sx,
                    (d.bbox.y0 - py) / sy,
                    (d.bbox.x1 - px) / sx,
                    (d.bbox.y1 - py) / sy,
                )
                .clamped(pw, ph),
            })
            .filter(|d| d.bbox.width() > 0.0 && d.bbox.height() > 0.0)
            .collect())
    }
}

/// One head's outputs for one stride.
pub(crate) struct Head<'a> {
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
}

/// One face in some image's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Detection {
    pub score: f32,
    pub bbox: BBox,
}

/// YuNet's anchor-free decoding, as OpenCV's `FaceDetectorYN` does it: the anchor in row `r`
/// and column `c` of a stride-`s` head scores `sqrt(clamp(cls) × clamp(obj))`, centres at
/// `((c + dx) s, (r + dy) s)` and spans `(e^dw s, e^dh s)`. Boxes are in input pixels.
pub(crate) fn decode(
    head: &Head<'_>,
    stride: u32,
    input: (u32, u32),
    threshold: f32,
) -> Vec<Detection> {
    let cols = (input.0 / stride) as usize;
    let rows = (input.1 / stride) as usize;
    let anchors = (cols * rows).min(head.cls.len()).min(head.obj.len()).min(head.bbox.len() / 4);
    let s = stride as f32;
    let mut found = Vec::new();
    for i in 0..anchors {
        let score = (head.cls[i].clamp(0.0, 1.0) * head.obj[i].clamp(0.0, 1.0)).sqrt();
        if score < threshold {
            continue;
        }
        let (r, c) = ((i / cols) as f32, (i % cols) as f32);
        let b = &head.bbox[i * 4..i * 4 + 4];
        let (cx, cy) = ((c + b[0]) * s, (r + b[1]) * s);
        let (w, h) = (b[2].exp() * s, b[3].exp() * s);
        found.push(Detection {
            score,
            bbox: BBox::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0),
        });
    }
    found
}

/// Greedy non-maximum suppression: best first, dropping any box overlapping a kept one at
/// `iou` or more.
pub(crate) fn suppress(mut found: Vec<Detection>, iou: f32, max: usize) -> Vec<Detection> {
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in found {
        if kept.len() >= max {
            break;
        }
        if kept.iter().all(|k| k.bbox.iou(&d.bbox) < iou) {
            kept.push(d);
        }
    }
    kept
}

#[async_trait]
impl Detector for YuNet {
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
        self.check_query(query)?;
        let (iw, ih) = self.size;
        let mut found = self.pass(image.pixels(), (iw, ih)).await?;
        // A face nearer than about half the frame outgrows the 300 px YuNet was trained on.
        cx.check()?;
        found.extend(self.pass(image.pixels(), ((iw / 2).max(1), (ih / 2).max(1))).await?);
        let faces = suppress(found, self.nms_iou, self.max_detections);
        tracing::debug!(vision.backend = %self.descriptor.id, vision.regions = faces.len(), "yunet detected");
        Ok(faces
            .into_iter()
            .map(|d| RawRegion::boxed(d.bbox, self.label.clone(), d.score))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_decode_as_opencv_does() {
        // A 32x16 input at stride 16: two columns, one row. Anchor 1 scores
        // sqrt(0.81 * 1.0) = 0.9 and centres at ((1 + 0.5) * 16, (0 + 0.5) * 16) = (24, 8),
        // e^0 * 16 = 16 px square; anchor 0 scores sqrt(0.25 * 0.25) = 0.25.
        let head = Head {
            cls: &[0.25, 0.81],
            obj: &[0.25, 1.4],
            bbox: &[0.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0],
        };
        let found = decode(&head, 16, (32, 16), 0.5);
        assert_eq!(found.len(), 1);
        assert!((found[0].score - 0.9).abs() < 1e-6, "{found:?}");
        assert_eq!(found[0].bbox, BBox::new(16.0, 0.0, 32.0, 16.0));
        assert_eq!(decode(&head, 16, (32, 16), 0.2).len(), 2);
    }

    #[test]
    fn suppression_keeps_the_best_of_overlapping_boxes() {
        let d = |score, x0| Detection { score, bbox: BBox::new(x0, 0.0, x0 + 10.0, 10.0) };
        let kept = suppress(vec![d(0.6, 1.0), d(0.9, 0.0), d(0.7, 50.0)], 0.3, 10);
        assert_eq!(kept, vec![d(0.9, 0.0), d(0.7, 50.0)]);
        assert_eq!(suppress(vec![d(0.6, 1.0), d(0.9, 0.0), d(0.7, 50.0)], 0.3, 1).len(), 1);
    }
}
