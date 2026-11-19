//! [`EfficientSam`]: EfficientSAM point and box segmentation.

use std::sync::Arc;

use async_trait::async_trait;

use super::OnnxOptions;
use super::embedding_cache::EmbeddingCache;
use super::preprocess::Normalize;
use super::sam::{best_mask, group_prompts};
use super::session::{Input, OrtModel, Output, check_manifest, onnx_file};
use crate::backend::{BackendDescriptor, InputLimits, Locality};
use crate::cx::VisionCx;
use crate::error::Result;
use crate::geometry::{CoordFrame, Letterbox, Mask, RawRegion, Shape};
use crate::manifest::{VerifiedModel, VisionTask};
use crate::sent::SentImage;
use crate::traits::{SegmentPrompt, Segmenter};

/// EfficientSAM (Apache-2.0) on ONNX Runtime, from its split encoder and decoder exports.
///
/// The encoder takes the sent image in `[0, 1]`, resizes it to its 1024×1024 input and
/// normalises it inside the graph; it runs once per prepared image, and its embedding is
/// kept for the next prompts on the same image. The decoder runs once per object: each box
/// prompt, refined by the points inside it, and the points outside every box together (see
/// [`Segmenter`]). Boxes go in as SAM's corner points (labels 2 and 3). Of the three masks
/// it proposes, the one with the highest predicted IoU is kept, thresholded at 0 at the
/// sent image's size, with that IoU as its score.
///
/// The backend answers in `SentPixels`. Images longer than 1024 px are downscaled to 1024 px
/// by the runtime first, and the resize is on record.
///
/// The manifest pins two `.onnx` files: the one whose name contains `encoder` and the one
/// whose name contains `decoder`.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{EfficientSam, OnnxOptions};
/// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let manifest = ManifestSet::builtin()?.get("efficientsam-ti").cloned().expect("built in");
/// let fetcher =
///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let segmenter = EfficientSam::new(&fetcher.fetch_blocking(&manifest)?, &OnnxOptions::default())?;
/// # Ok(())
/// # }
/// ```
pub struct EfficientSam {
    descriptor: BackendDescriptor,
    encoder: OrtModel,
    decoder: OrtModel,
    cache: EmbeddingCache<Output>,
}

impl std::fmt::Debug for EfficientSam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EfficientSam").field("descriptor", &self.descriptor).finish_non_exhaustive()
    }
}

impl EfficientSam {
    /// Loads a verified EfficientSAM encoder and decoder.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`](crate::VisionError::Manifest) when the manifest is not an
    /// `ort` segmentation model with one encoder and one decoder file;
    /// [`VisionError::Backend`](crate::VisionError::Backend) when onnxruntime cannot load
    /// them.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let manifest = model.manifest();
        check_manifest(manifest, VisionTask::Segment)?;
        let encoder =
            OrtModel::load(&manifest.id, onnx_file(model, "encoder", "decoder")?, options)?;
        let decoder = OrtModel::load(&manifest.id, onnx_file(model, "decoder", "")?, options)?;
        let side = manifest.io.input.map_or(1024, |i| i.width.max(i.height));
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::SentPixels,
        )
        .with_manifest(manifest.id.clone())
        .with_limits(InputLimits::long_edge(side));
        Ok(Self { descriptor, encoder, decoder, cache: EmbeddingCache::new(4) })
    }

    async fn embedding(&self, image: &SentImage) -> Result<Arc<Output>> {
        if let Some(hit) = self.cache.get(image) {
            return Ok(hit);
        }
        let (w, h) = image.size();
        let unit = Normalize {
            size: (w, h),
            letterbox: Letterbox::Stretch,
            mean: [0.0; 3],
            std: [1.0; 3],
        };
        let input = self.encoder.input_named(&["batched_images"], 0)?;
        let output = self.encoder.outputs().first().map(|(n, _)| n.clone()).unwrap_or_default();
        let mut outputs = self
            .encoder
            .run(
                vec![(
                    input,
                    Input::F32(vec![1, 3, i64::from(h), i64::from(w)], unit.tensor(image.pixels())),
                )],
                &[output.as_str()],
            )
            .await?;
        let embedding = Arc::new(
            outputs
                .remove(&output)
                .ok_or_else(|| self.encoder.error("no image embedding".into()))?,
        );
        self.cache.put(image, Arc::clone(&embedding));
        Ok(embedding)
    }
}

#[async_trait]
impl Segmenter for EfficientSam {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn segment(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
        cx.check()?;
        let groups = group_prompts(self.descriptor.id.as_str(), prompts)?;
        let embedding = self.embedding(image).await?;
        let (w, h) = image.size();
        let mut regions = Vec::with_capacity(groups.len());
        for group in groups {
            cx.check()?;
            let mut coords = Vec::new();
            let mut labels = Vec::new();
            for (at, positive) in &group.points {
                coords.extend_from_slice(at);
                labels.push(if *positive { 1.0 } else { 0.0 });
            }
            if let Some(b) = group.bbox {
                coords.extend_from_slice(&[b.x0, b.y0, b.x1, b.y1]);
                labels.extend_from_slice(&[2.0, 3.0]);
            }
            let n = labels.len() as i64;
            let outputs = self
                .decoder
                .run(
                    vec![
                        (
                            "image_embeddings".into(),
                            Input::F32(embedding.shape.clone(), embedding.data.clone()),
                        ),
                        ("batched_point_coords".into(), Input::F32(vec![1, 1, n, 2], coords)),
                        ("batched_point_labels".into(), Input::F32(vec![1, 1, n], labels)),
                        (
                            "orig_im_size".into(),
                            Input::I64(vec![2], vec![i64::from(h), i64::from(w)]),
                        ),
                    ],
                    &["output_masks", "iou_predictions"],
                )
                .await?;
            let (masks, iou) = (&outputs["output_masks"], &outputs["iou_predictions"]);
            let Some(best) = best_mask(&iou.data) else { continue };
            let size = w as usize * h as usize;
            let Some(logits) = masks.data.get(best * size..(best + 1) * size) else {
                return Err(self.decoder.error(format!(
                    "the decoder's masks {:?} do not cover a {w}x{h} image",
                    masks.shape
                )));
            };
            let bits: Vec<bool> = logits.iter().map(|&v| v >= 0.0).collect();
            regions.push(RawRegion {
                shape: Shape::Mask(Mask::encode(w, h, &bits)),
                label: None,
                score: Some(iou.data[best]),
                track: None,
            });
        }
        Ok(regions)
    }
}
