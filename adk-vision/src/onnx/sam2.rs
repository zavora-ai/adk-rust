//! [`Sam2`]: SAM 2.1 point and box segmentation on single images.

use std::sync::Arc;

use async_trait::async_trait;

use super::OnnxOptions;
use super::embedding_cache::EmbeddingCache;
use super::preprocess::{Normalize, upsample_bilinear};
use super::sam::{best_mask, group_prompts};
use super::session::{Input, OrtModel, Output, check_manifest, onnx_file};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::Result;
use crate::geometry::{CoordFrame, Letterbox, Mask, RawRegion, Shape};
use crate::manifest::{VerifiedModel, VisionTask};
use crate::sent::SentImage;
use crate::traits::{SegmentPrompt, Segmenter};

/// The image encoder's three feature maps, in the order the decoder takes them.
const EMBEDDINGS: [&str; 3] = ["image_embeddings.0", "image_embeddings.1", "image_embeddings.2"];

/// SAM 2.1 (Apache-2.0) on ONNX Runtime, from the split vision-encoder and
/// prompt-encoder-and-mask-decoder exports Transformers.js uses.
///
/// The image is stretched to the manifest's input (1024×1024) and normalised with
/// `io.mean` and `io.std`, as SAM 2's processor does; the encoder runs once per prepared
/// image and its features are kept for the next prompts on the same image. The decoder runs
/// once per object (see [`Segmenter`]): a box with the points inside it, or the points
/// outside every box. Of the three masks it proposes, the one with the highest predicted IoU
/// is upsampled from 256×256 to the model input and thresholded at 0, with that IoU as its
/// score. An object the decoder scores as absent yields no region.
///
/// The backend answers in `ModelInput` pixels; the runtime maps the prompts in and the masks
/// back out.
///
/// The manifest pins the `.onnx` file whose name contains `encoder` but not `decoder`, the
/// one whose name contains `decoder`, and their `.onnx_data` external weights beside them.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, Sam2};
/// use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let manifest = ManifestSet::builtin()?.get("sam2.1-hiera-tiny-q4").cloned().expect("built in");
/// let fetcher =
///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let segmenter = Sam2::new(&fetcher.fetch_blocking(&manifest)?, &OnnxOptions::default())?;
/// # Ok(())
/// # }
/// ```
pub struct Sam2 {
    descriptor: BackendDescriptor,
    encoder: OrtModel,
    decoder: OrtModel,
    normalize: Normalize,
    cache: EmbeddingCache<Vec<Output>>,
}

impl std::fmt::Debug for Sam2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sam2").field("descriptor", &self.descriptor).finish_non_exhaustive()
    }
}

impl Sam2 {
    /// Loads a verified SAM 2.1 encoder and decoder.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`](crate::VisionError::Manifest) when the manifest is not an
    /// `ort` segmentation model with one encoder and one decoder file;
    /// [`VisionError::Backend`](crate::VisionError::Backend) when onnxruntime cannot load
    /// them.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let normalize = Normalize::from_io(&model.manifest().io, (1024, 1024), Letterbox::Stretch);
        Self::with_normalize(model, options, normalize)
    }

    /// Loads the same split graphs with a family's own input size and normalisation, such
    /// as SAM 3's tracker, whose export shares SAM 2's inputs and outputs.
    pub(super) fn with_normalize(
        model: &VerifiedModel,
        options: &OnnxOptions,
        normalize: Normalize,
    ) -> Result<Self> {
        let manifest = model.manifest();
        check_manifest(manifest, VisionTask::Segment)?;
        let encoder =
            OrtModel::load(&manifest.id, onnx_file(model, "encoder", "decoder")?, options)?;
        let decoder = OrtModel::load(&manifest.id, onnx_file(model, "decoder", "")?, options)?;
        let (width, height) = normalize.size;
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::ModelInput { width, height, letterbox: normalize.letterbox },
        )
        .with_manifest(manifest.id.clone());
        Ok(Self { descriptor, encoder, decoder, normalize, cache: EmbeddingCache::new(4) })
    }

    async fn embeddings(&self, image: &SentImage) -> Result<Arc<Vec<Output>>> {
        if let Some(hit) = self.cache.get(image) {
            return Ok(hit);
        }
        let (w, h) = self.normalize.size;
        let input = self.encoder.input_named(&["pixel_values"], 0)?;
        let mut outputs = self
            .encoder
            .run(
                vec![(
                    input,
                    Input::F32(
                        vec![1, 3, i64::from(h), i64::from(w)],
                        self.normalize.tensor(image.pixels()),
                    ),
                )],
                &EMBEDDINGS,
            )
            .await?;
        let embeddings = EMBEDDINGS
            .iter()
            .map(|name| {
                outputs
                    .remove(*name)
                    .ok_or_else(|| self.encoder.error(format!("the encoder produced no `{name}`")))
            })
            .collect::<Result<Vec<_>>>()?;
        let embeddings = Arc::new(embeddings);
        self.cache.put(image, Arc::clone(&embeddings));
        Ok(embeddings)
    }
}

#[async_trait]
impl Segmenter for Sam2 {
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
        let embeddings = self.embeddings(image).await?;
        let (iw, ih) = self.normalize.size;
        let mut regions = Vec::with_capacity(groups.len());
        for group in groups {
            cx.check()?;
            let mut points = Vec::new();
            let mut labels = Vec::new();
            for (at, positive) in &group.points {
                points.extend_from_slice(at);
                labels.push(i64::from(*positive));
            }
            let boxes = match group.bbox {
                Some(b) => {
                    if labels.is_empty() {
                        // No point: one placeholder the prompt encoder embeds as zero.
                        points.extend_from_slice(&[0.0, 0.0]);
                        labels.push(-10);
                    }
                    vec![b.x0, b.y0, b.x1, b.y1]
                }
                None => {
                    // Points alone end with SAM's padding point, as the reference does
                    // when no box follows them.
                    points.extend_from_slice(&[0.0, 0.0]);
                    labels.push(-1);
                    Vec::new()
                }
            };
            let n = labels.len() as i64;
            let nb = (boxes.len() / 4) as i64;
            let mut inputs = vec![
                ("input_points".to_string(), Input::F32(vec![1, 1, n, 2], points)),
                ("input_labels".to_string(), Input::I64(vec![1, 1, n], labels)),
                ("input_boxes".to_string(), Input::F32(vec![1, nb, 4], boxes)),
            ];
            for (name, embedding) in EMBEDDINGS.iter().zip(embeddings.iter()) {
                inputs.push((
                    (*name).to_string(),
                    Input::F32(embedding.shape.clone(), embedding.data.clone()),
                ));
            }
            let outputs = self
                .decoder
                .run(inputs, &["iou_scores", "pred_masks", "object_score_logits"])
                .await?;
            let (iou, masks) = (&outputs["iou_scores"], &outputs["pred_masks"]);
            if outputs["object_score_logits"].data.first().is_some_and(|&s| s < 0.0) {
                continue;
            }
            let Some(best) = best_mask(&iou.data) else { continue };
            let dims = masks.dims();
            let (mh, mw) = match dims.as_slice() {
                [.., mh, mw] => (*mh, *mw),
                _ => return Err(self.decoder.error(format!("unexpected mask shape {dims:?}"))),
            };
            let Some(logits) = masks.data.get(best * mh * mw..(best + 1) * mh * mw) else {
                return Err(self.decoder.error(format!("unexpected mask shape {dims:?}")));
            };
            let up = upsample_bilinear(logits, (mw, mh), (iw as usize, ih as usize));
            let bits: Vec<bool> = up.iter().map(|&v| v > 0.0).collect();
            regions.push(RawRegion {
                shape: Shape::Mask(Mask::encode(iw, ih, &bits)),
                label: None,
                score: Some(iou.data[best]),
                track: None,
            });
        }
        Ok(regions)
    }
}
