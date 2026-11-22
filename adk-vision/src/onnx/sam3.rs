//! [`Sam3`] and [`Sam3Tracker`]: Meta's SAM 3 on ONNX Runtime, from weights a person supplies
//! and whose SAM License they accepted (feature `sam3-local`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::OnnxOptions;
use super::embedding_cache::EmbeddingCache;
use super::preprocess::{Normalize, sigmoid, upsample_bilinear};
use super::sam2::Sam2;
use super::session::{
    Input, OrtModel, Output, check_manifest, manifest_error, named_file, onnx_file,
};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::geometry::{BBox, CoordFrame, Letterbox, Mask, RawRegion, Shape};
use crate::manifest::{LicenceClass, ModelManifest, VerifiedModel, VisionTask};
use crate::sent::SentImage;
use crate::traits::{DetectQuery, Detector, SegmentPrompt, Segmenter};

/// The licence id a SAM 3 manifest names: Meta's SAM License, which covers SAM 3's weights
/// and every export of them.
pub const SAM_LICENCE_ID: &str = "LicenseRef-SAM";

/// SAM 3's square input side.
const INPUT: u32 = 1008;
/// Every concept is padded or truncated to 32 CLIP tokens, as Meta's processor does.
const TEXT_TOKENS: usize = 32;
/// The concept image encoder's feature maps, in the order the decoder takes them.
const FPN: [&str; 3] = ["fpn_hidden_state_0", "fpn_hidden_state_1", "fpn_hidden_state_2"];
/// Objects scoring at or below this are dropped, as Meta's `Sam3Processor` drops them.
const DEFAULT_THRESHOLD: f32 = 0.5;
/// Encoded concepts kept, so a concept asked of every frame of a stream is encoded once.
const TEXT_CACHE: usize = 16;

/// Refuses a manifest that does not class its weights as Meta's SAM License, use-restricted:
/// any other class would let SAM 3 weights load without a person's recorded acceptance.
fn sam_licence(manifest: &ModelManifest) -> Result<()> {
    if manifest.licence.class == LicenceClass::UseRestricted
        && manifest.licence.id == SAM_LICENCE_ID
    {
        return Ok(());
    }
    Err(manifest_error(
        manifest,
        format!(
            "SAM 3 weights are under Meta's SAM License; the manifest must name licence id \
             `{SAM_LICENCE_ID}` with class `useRestricted`, so they load only after a person \
             accepts the licence (it names `{}` as {})",
            manifest.licence.id, manifest.licence.class
        ),
    ))
}

/// SAM 3's preprocessing: stretched to 1008×1008 and mapped to `[-1, 1]` (mean and standard
/// deviation 0.5), unless the manifest's `io` says otherwise.
fn sam3_normalize(manifest: &ModelManifest) -> Normalize {
    let mut normalize = Normalize::from_io(&manifest.io, (INPUT, INPUT), Letterbox::Stretch);
    if manifest.io.mean.is_none() {
        normalize.mean = [0.5; 3];
    }
    if manifest.io.std.is_none() {
        normalize.std = [0.5; 3];
    }
    normalize
}

/// A concept's text features and attention mask, as the decoder takes them.
struct EncodedText {
    features: Output,
    mask: Vec<i64>,
}

/// One object the decoder kept: its query, its score and its box in model-input pixels.
#[derive(Clone, Debug, PartialEq)]
struct Kept {
    query: usize,
    score: f32,
    bbox: BBox,
}

/// The objects scoring above `threshold`, best first: each query's score is
/// `sigmoid(logit) × sigmoid(presence)`, as Meta's processor computes it, and its box, in
/// `0..1` corners, is scaled to `size` (width, height).
fn kept(
    boxes: &Output,
    logits: &Output,
    presence: &Output,
    threshold: f32,
    size: (u32, u32),
) -> std::result::Result<Vec<Kept>, String> {
    let queries = logits.data.len();
    if boxes.data.len() != queries * 4 {
        return Err(format!(
            "{queries} object scores but boxes of shape {:?}; the decoder must answer `pred_boxes` \
             as [1, queries, 4]",
            boxes.shape
        ));
    }
    let presence = presence
        .data
        .first()
        .map(|&p| sigmoid(p))
        .ok_or_else(|| "the decoder answered an empty `presence_logits`".to_string())?;
    let (w, h) = (size.0 as f32, size.1 as f32);
    let mut kept: Vec<Kept> = logits
        .data
        .iter()
        .enumerate()
        .filter_map(|(query, &logit)| {
            let score = sigmoid(logit) * presence;
            let b = &boxes.data[query * 4..query * 4 + 4];
            let bbox = BBox::new(
                (b[0] * w).clamp(0.0, w),
                (b[1] * h).clamp(0.0, h),
                (b[2] * w).clamp(0.0, w),
                (b[3] * h).clamp(0.0, h),
            );
            (score > threshold && bbox.x1 > bbox.x0 && bbox.y1 > bbox.y0)
                .then_some(Kept { query, score, bbox })
        })
        .collect();
    kept.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(kept)
}

/// The CLIP tokenizer in `tokenizer.json`, padded and truncated to 32 tokens with
/// `<|endoftext|>`, as SAM 3's processor pads.
fn clip_tokenizer(manifest: &ModelManifest, model: &VerifiedModel) -> Result<Tokenizer> {
    let file = named_file(model, "tokenizer.json")?;
    let mut tokenizer = Tokenizer::from_file(file.path()).map_err(|e| {
        manifest_error(manifest, format!("`tokenizer.json` could not be read: {e}"))
    })?;
    let pad_token = "<|endoftext|>";
    let pad_id = tokenizer.token_to_id(pad_token).ok_or_else(|| {
        manifest_error(
            manifest,
            format!("`tokenizer.json` has no `{pad_token}`; SAM 3 takes the CLIP tokenizer"),
        )
    })?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: TEXT_TOKENS,
            ..TruncationParams::default()
        }))
        .map_err(|e| manifest_error(manifest, format!("the tokenizer refused truncation: {e}")))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::Fixed(TEXT_TOKENS),
        pad_id,
        pad_token: pad_token.into(),
        ..PaddingParams::default()
    }));
    Ok(tokenizer)
}

/// A lowercased concept as 32 token ids and their attention mask.
fn token_ids(tokenizer: &Tokenizer, text: &str) -> std::result::Result<(Vec<i64>, Vec<i64>), String> {
    let encoding = tokenizer.encode(text.to_lowercase(), true).map_err(|e| e.to_string())?;
    let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
    let mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| i64::from(m)).collect();
    if ids.len() != TEXT_TOKENS {
        return Err(format!("{} tokens, not {TEXT_TOKENS}", ids.len()));
    }
    Ok((ids, mask))
}

/// SAM 3 (Meta, SAM License) on ONNX Runtime: open-vocabulary segmentation and detection
/// from a noun phrase, and, with [`Sam3::with_tracker`], point and box prompts.
///
/// Meta publishes SAM 3's weights behind an access request on Hugging Face and no ONNX
/// export, so `adk-vision` ships no weights and no built-in manifest entry for it: a person
/// exports the graphs from weights they obtained, writes a manifest that names Meta's SAM
/// License (`LicenseRef-SAM`, class `useRestricted`) and accepts the licence with
/// `adk-rust vision models accept <id>`. Until then verification refuses with `licence
/// acceptance required` and the licence URL, and this constructor refuses a manifest that
/// classes the weights as anything else.
///
/// The manifest pins three graphs in the Transformers `Sam3Model` layout, found by name, and
/// the CLIP `tokenizer.json`:
///
/// | File name contains | Inputs | Outputs |
/// |--------------------|--------|---------|
/// | `vision` | `pixel_values` `[1, 3, 1008, 1008]` | `fpn_hidden_state_0`, `_1`, `_2` |
/// | `text` | `input_ids`, `attention_mask` `[1, 32]` | `text_features` `[1, 32, 256]` |
/// | `decoder` | the three feature maps, `text_features`, `attention_mask` | `pred_masks` `[1, Q, 288, 288]` (logits), `pred_boxes` `[1, Q, 4]` (`0..1` corners), `pred_logits` `[1, Q]`, `presence_logits` `[1, 1]` |
///
/// The image is stretched to 1008×1008 and mapped to `[-1, 1]`, as Meta's processor does;
/// the image encoder runs once per prepared image and the text encoder once per concept, and
/// both are kept for the next calls. Each query scores `sigmoid(logit) × sigmoid(presence)`;
/// queries above the threshold (0.5, as Meta's `Sam3Processor`) become regions labelled with
/// the concept. As a [`Segmenter`] each region is the query's mask, upsampled to 1008×1008
/// and thresholded at 0; as a [`Detector`] it is the query's box, and `Classes` are asked as
/// concepts. The backend answers in `ModelInput` pixels; the runtime maps them to the source.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, Sam3};
/// use adk_vision::{FetchConfig, FileAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let project = std::path::Path::new(".");
/// // Your own manifest, naming the files you exported and the SAM License.
/// let manifests = ManifestSet::new().with_dir(&project.join(".adk/vision/models"))?;
/// let manifest = manifests.get("sam3-local").expect("written by you");
/// // The acceptance `adk-rust vision models accept sam3-local` recorded.
/// let acceptances = Arc::new(FileAcceptanceStore::in_project(project));
/// let model = ModelFetcher::new(FetchConfig::from_env(), acceptances).fetch_blocking(manifest)?;
/// let sam3 = Sam3::new(&model, &OnnxOptions::default())?.with_threshold(0.4);
/// # Ok(())
/// # }
/// ```
pub struct Sam3 {
    descriptor: BackendDescriptor,
    vision: OrtModel,
    text: OrtModel,
    decoder: OrtModel,
    tokenizer: Arc<Tokenizer>,
    normalize: Normalize,
    threshold: f32,
    images: EmbeddingCache<Vec<Output>>,
    texts: Mutex<VecDeque<(String, Arc<EncodedText>)>>,
    tracker: Option<Sam3Tracker>,
}

impl std::fmt::Debug for Sam3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sam3")
            .field("descriptor", &self.descriptor)
            .field("threshold", &self.threshold)
            .field("tracker", &self.tracker)
            .finish_non_exhaustive()
    }
}

impl Sam3 {
    /// Loads a verified SAM 3 image encoder, text encoder, decoder and tokenizer.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest does not name the SAM License as
    /// use-restricted, is not an `ort` segmentation model, or lacks one of the three graphs or
    /// `tokenizer.json`; [`VisionError::Backend`] when onnxruntime cannot load a graph.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let manifest = model.manifest();
        sam_licence(manifest)?;
        check_manifest(manifest, VisionTask::Segment)?;
        let tokenizer = clip_tokenizer(manifest, model)?;
        let vision = OrtModel::load(&manifest.id, onnx_file(model, "vision", "")?, options)?;
        let text = OrtModel::load(&manifest.id, onnx_file(model, "text", "")?, options)?;
        let decoder = OrtModel::load(&manifest.id, onnx_file(model, "decoder", "")?, options)?;
        let normalize = sam3_normalize(manifest);
        let (width, height) = normalize.size;
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::ModelInput { width, height, letterbox: normalize.letterbox },
        )
        .with_manifest(manifest.id.clone());
        Ok(Self {
            descriptor,
            vision,
            text,
            decoder,
            tokenizer: Arc::new(tokenizer),
            normalize,
            threshold: DEFAULT_THRESHOLD,
            images: EmbeddingCache::new(4),
            texts: Mutex::new(VecDeque::with_capacity(TEXT_CACHE)),
            tracker: None,
        })
    }

    /// Keeps objects scoring above `threshold` (`0.5` by default).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Answers point and box prompts with SAM 3's tracker graphs, so one segmenter takes every
    /// prompt `sam-3.1` takes.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the tracker's input differs from this model's, so
    /// their regions would not share one frame.
    pub fn with_tracker(mut self, tracker: Sam3Tracker) -> Result<Self> {
        if tracker.descriptor().frame != self.descriptor.frame {
            return Err(VisionError::InvalidInput(format!(
                "the SAM 3 tracker `{}` answers in {:?}, and `{}` in {:?}; give both manifests \
                 the same input",
                tracker.descriptor().id,
                tracker.descriptor().frame,
                self.descriptor.id,
                self.descriptor.frame
            )));
        }
        self.tracker = Some(tracker);
        Ok(self)
    }

    async fn features(&self, image: &SentImage) -> Result<Arc<Vec<Output>>> {
        if let Some(hit) = self.images.get(image) {
            return Ok(hit);
        }
        let (w, h) = self.normalize.size;
        let input = self.vision.input_named(&["pixel_values"], 0)?;
        let mut outputs = self
            .vision
            .run(
                vec![(
                    input,
                    Input::F32(
                        vec![1, 3, i64::from(h), i64::from(w)],
                        self.normalize.tensor(image.pixels()),
                    ),
                )],
                &FPN,
            )
            .await?;
        let features = FPN
            .iter()
            .map(|name| {
                outputs.remove(*name).ok_or_else(|| {
                    self.vision.error(format!("the image encoder produced no `{name}`"))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let features = Arc::new(features);
        self.images.put(image, Arc::clone(&features));
        Ok(features)
    }

    async fn concept(&self, text: &str) -> Result<Arc<EncodedText>> {
        {
            let texts = self.texts.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, hit)) = texts.iter().find(|(t, _)| t == text) {
                return Ok(Arc::clone(hit));
            }
        }
        let (ids, mask) = token_ids(&self.tokenizer, text)
            .map_err(|e| self.text.error(format!("could not tokenise `{text}`: {e}")))?;
        let shape = vec![1, TEXT_TOKENS as i64];
        let inputs = vec![
            (self.text.input_named(&["input_ids"], 0)?, Input::I64(shape.clone(), ids)),
            (self.text.input_named(&["attention_mask"], 1)?, Input::I64(shape, mask.clone())),
        ];
        let mut outputs = self.text.run(inputs, &["text_features"]).await?;
        let features = outputs
            .remove("text_features")
            .ok_or_else(|| self.text.error("the text encoder produced no `text_features`".into()))?;
        let encoded = Arc::new(EncodedText { features, mask });
        let mut texts = self.texts.lock().unwrap_or_else(|e| e.into_inner());
        while texts.len() >= TEXT_CACHE {
            texts.pop_front();
        }
        texts.push_back((text.to_string(), Arc::clone(&encoded)));
        Ok(encoded)
    }

    /// The regions of one concept: masks for segmentation, boxes for detection.
    async fn concept_regions(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        text: &str,
        masks: bool,
    ) -> Result<Vec<RawRegion>> {
        cx.check()?;
        let features = self.features(image).await?;
        cx.check()?;
        let concept = self.concept(text).await?;
        cx.check()?;
        let mut inputs: Vec<(String, Input)> = FPN
            .iter()
            .zip(features.iter())
            .map(|(name, f)| ((*name).to_string(), Input::F32(f.shape.clone(), f.data.clone())))
            .collect();
        inputs.push((
            "text_features".into(),
            Input::F32(concept.features.shape.clone(), concept.features.data.clone()),
        ));
        inputs.push((
            "attention_mask".into(),
            Input::I64(vec![1, TEXT_TOKENS as i64], concept.mask.clone()),
        ));
        let wanted: &[&str] = if masks {
            &["pred_boxes", "pred_logits", "presence_logits", "pred_masks"]
        } else {
            &["pred_boxes", "pred_logits", "presence_logits"]
        };
        let outputs = self.decoder.run(inputs, wanted).await?;
        let (iw, ih) = self.normalize.size;
        let objects = kept(
            &outputs["pred_boxes"],
            &outputs["pred_logits"],
            &outputs["presence_logits"],
            self.threshold,
            (iw, ih),
        )
        .map_err(|e| self.decoder.error(e))?;
        let mut regions = Vec::with_capacity(objects.len());
        for object in objects {
            let shape = if masks {
                let logits = &outputs["pred_masks"];
                let dims = logits.dims();
                let (mh, mw) = match dims.as_slice() {
                    [.., mh, mw] => (*mh, *mw),
                    _ => {
                        return Err(self.decoder.error(format!("unexpected mask shape {dims:?}")));
                    }
                };
                let range = object.query * mh * mw..(object.query + 1) * mh * mw;
                let Some(logits) = logits.data.get(range) else {
                    return Err(self.decoder.error(format!("unexpected mask shape {dims:?}")));
                };
                let up = upsample_bilinear(logits, (mw, mh), (iw as usize, ih as usize));
                let bits: Vec<bool> = up.iter().map(|&v| v > 0.0).collect();
                if bits.iter().any(|&b| b) {
                    Shape::Mask(Mask::encode(iw, ih, &bits))
                } else {
                    // A kept object thinner than the mask grid: its box is all there is.
                    Shape::Box(object.bbox)
                }
            } else {
                Shape::Box(object.bbox)
            };
            regions.push(RawRegion {
                shape,
                label: Some(text.to_string()),
                score: Some(object.score),
                track: None,
            });
        }
        Ok(regions)
    }
}

#[async_trait]
impl Segmenter for Sam3 {
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
        let id = self.descriptor.id.as_str();
        let mut concepts = Vec::new();
        let mut geometric = Vec::new();
        for prompt in prompts {
            match prompt {
                SegmentPrompt::Concept { text } => concepts.push(text.as_str()),
                SegmentPrompt::Point { .. } | SegmentPrompt::Box { .. } => {
                    geometric.push(prompt.clone());
                }
                SegmentPrompt::Mask { .. } => {
                    return Err(VisionError::Unsupported(format!(
                        "`{id}` takes concept, point and box prompts, not mask prompts"
                    )));
                }
            }
        }
        if concepts.is_empty() && geometric.is_empty() {
            return Err(VisionError::InvalidInput(format!("`{id}` needs at least one prompt")));
        }
        if !geometric.is_empty() && self.tracker.is_none() {
            return Err(VisionError::Unsupported(format!(
                "`{id}` has no tracker graphs for point and box prompts; add them with \
                 `Sam3::with_tracker`, or route those prompts to SAM 2.1 or EfficientSAM"
            )));
        }
        let mut regions = Vec::new();
        for text in concepts {
            regions.extend(self.concept_regions(cx, image, text, true).await?);
        }
        if let Some(tracker) = &self.tracker
            && !geometric.is_empty()
        {
            regions.extend(tracker.segment(cx, image, &geometric).await?);
        }
        Ok(regions)
    }
}

#[async_trait]
impl Detector for Sam3 {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    async fn detect(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        query: &DetectQuery,
    ) -> Result<Vec<RawRegion>> {
        let concepts = match query {
            DetectQuery::Concepts(c) | DetectQuery::Classes(c) => c,
        };
        if concepts.is_empty() {
            return Err(VisionError::InvalidInput(format!(
                "`{}` needs at least one concept to detect",
                self.descriptor.id
            )));
        }
        let mut regions = Vec::new();
        for concept in concepts {
            regions.extend(self.concept_regions(cx, image, concept, false).await?);
        }
        Ok(regions)
    }
}

/// SAM 3's tracker (Meta, SAM License): point and box segmentation on single images, from the
/// split exports Transformers.js names `Sam3TrackerModel`.
///
/// The export has SAM 2's inputs and outputs (a `vision_encoder` with `pixel_values` in and
/// `image_embeddings.0`–`.2` out, and a `prompt_encoder_mask_decoder` with points, labels,
/// boxes and the embeddings in and `iou_scores`, `pred_masks`, `object_score_logits` out), so
/// it runs on [`Sam2`](super::Sam2)'s prompt handling with SAM 3's preprocessing: stretched
/// to 1008×1008 and mapped to `[-1, 1]`. Its 288×288 masks are upsampled to the model input.
/// Like [`Sam3`], it loads only from a manifest that names the SAM License as use-restricted,
/// after a person accepted it.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, Sam3, Sam3Tracker};
/// use adk_vision::{FetchConfig, FileAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let project = std::path::Path::new(".");
/// let manifests = ManifestSet::new().with_dir(&project.join(".adk/vision/models"))?;
/// let fetcher = ModelFetcher::new(
///     FetchConfig::from_env(),
///     Arc::new(FileAcceptanceStore::in_project(project)),
/// );
/// let options = OnnxOptions::default();
/// let concepts = fetcher.fetch_blocking(manifests.get("sam3-local").expect("yours"))?;
/// let tracker = fetcher.fetch_blocking(manifests.get("sam3-tracker-local").expect("yours"))?;
/// let sam3 = Sam3::new(&concepts, &options)?.with_tracker(Sam3Tracker::new(&tracker, &options)?)?;
/// # Ok(())
/// # }
/// ```
pub struct Sam3Tracker {
    inner: Sam2,
}

impl std::fmt::Debug for Sam3Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sam3Tracker").field("descriptor", self.inner.descriptor()).finish()
    }
}

impl Sam3Tracker {
    /// Loads a verified SAM 3 tracker encoder and decoder.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest does not name the SAM License as
    /// use-restricted, or is not an `ort` segmentation model with one encoder and one decoder
    /// file; [`VisionError::Backend`] when onnxruntime cannot load them.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        sam_licence(model.manifest())?;
        Ok(Self { inner: Sam2::with_normalize(model, options, sam3_normalize(model.manifest()))? })
    }
}

#[async_trait]
impl Segmenter for Sam3Tracker {
    fn descriptor(&self) -> &BackendDescriptor {
        self.inner.descriptor()
    }

    async fn segment(
        &self,
        cx: &VisionCx,
        image: &SentImage,
        prompts: &[SegmentPrompt],
    ) -> Result<Vec<RawRegion>> {
        self.inner.segment(cx, image, prompts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;

    fn output(shape: Vec<i64>, data: Vec<f32>) -> Output {
        Output { shape, data }
    }

    fn manifest(licence: &str, class: &str) -> ModelManifest {
        ModelManifest::from_json(
            &serde_json::json!({
                "id": "sam3-local", "task": "segment",
                "source": {"kind": "local", "dir": "/models/sam3"},
                "files": [{"path": "vision_encoder.onnx", "size": 1,
                    "sha256": "sha256:0000000000000000000000000000000000000000000000000000000000000000"}],
                "licence": {"id": licence, "url": "https://example.org/sam", "class": class},
                "runtime": {"kind": "ort", "opset": 18}
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn only_a_use_restricted_sam_licence_loads() {
        assert!(sam_licence(&manifest(SAM_LICENCE_ID, "useRestricted")).is_ok());
        for (licence, class) in [
            (SAM_LICENCE_ID, "permissive"),
            ("Apache-2.0", "permissive"),
            ("Apache-2.0", "useRestricted"),
            (SAM_LICENCE_ID, "nonCommercial"),
        ] {
            let err = sam_licence(&manifest(licence, class)).unwrap_err();
            assert!(matches!(err, VisionError::Manifest { .. }), "{licence} {class}: {err}");
            assert!(err.to_string().contains("SAM License"), "{err}");
        }
    }

    #[test]
    fn preprocessing_defaults_to_meta_s_and_yields_to_the_manifest() {
        let normalize = sam3_normalize(&manifest(SAM_LICENCE_ID, "useRestricted"));
        assert_eq!(
            normalize,
            Normalize {
                size: (1008, 1008),
                letterbox: Letterbox::Stretch,
                mean: [0.5; 3],
                std: [0.5; 3]
            }
        );
        let mut own = manifest(SAM_LICENCE_ID, "useRestricted");
        own.io.mean = Some([0.0; 3]);
        assert_eq!(sam3_normalize(&own).mean, [0.0; 3]);
        assert_eq!(sam3_normalize(&own).std, [0.5; 3]);
    }

    #[test]
    fn queries_score_with_the_presence_head_and_boxes_scale_to_the_input() {
        let boxes = output(vec![1, 3, 4], vec![
            0.1, 0.2, 0.5, 0.6, // kept
            0.0, 0.0, 1.0, 1.0, // below the threshold
            0.3, 0.3, 0.3, 0.9, // no width
        ]);
        let logits = output(vec![1, 3], vec![4.0, -1.0, 4.0]);
        let presence = output(vec![1, 1], vec![3.0]);
        let found = kept(&boxes, &logits, &presence, 0.5, (1000, 500)).unwrap();
        let score = sigmoid(4.0) * sigmoid(3.0);
        assert_eq!(
            found,
            vec![Kept { query: 0, score, bbox: BBox::new(100.0, 100.0, 500.0, 300.0) }]
        );
        // An absent concept scales every query down.
        let absent = output(vec![1, 1], vec![-6.0]);
        assert!(kept(&boxes, &logits, &absent, 0.5, (1000, 500)).unwrap().is_empty());
        // Boxes out of the image are clamped to it.
        let wide = output(vec![1, 1, 4], vec![-0.2, 0.5, 1.3, 0.9]);
        let one = output(vec![1, 1], vec![4.0]);
        assert_eq!(
            kept(&wide, &one, &presence, 0.5, (100, 100)).unwrap()[0].bbox,
            BBox::new(0.0, 50.0, 100.0, 90.0)
        );
    }

    #[test]
    fn mismatched_decoder_outputs_are_refused_with_the_layout() {
        let boxes = output(vec![1, 2, 4], vec![0.0; 8]);
        let logits = output(vec![1, 3], vec![0.0; 3]);
        let presence = output(vec![1, 1], vec![0.0]);
        let err = kept(&boxes, &logits, &presence, 0.5, (8, 8)).unwrap_err();
        assert!(err.contains("[1, queries, 4]"), "{err}");
        let empty = output(vec![1, 0], vec![]);
        assert!(kept(&output(vec![1, 3, 4], vec![0.0; 12]), &logits, &empty, 0.5, (8, 8)).is_err());
    }

    #[test]
    fn concepts_are_lowercased_and_padded_to_thirty_two_tokens() {
        let vocab = [("<|startoftext|>", 0), ("<|endoftext|>", 1), ("red", 2), ("box", 3)]
            .into_iter()
            .map(|(t, i)| (t.to_string(), i))
            .collect();
        let model =
            WordLevel::builder().vocab(vocab).unk_token("<|endoftext|>".into()).build().unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace {}));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: TEXT_TOKENS,
                ..TruncationParams::default()
            }))
            .unwrap();
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::Fixed(TEXT_TOKENS),
            pad_id: 1,
            pad_token: "<|endoftext|>".into(),
            ..PaddingParams::default()
        }));
        let (ids, mask) = token_ids(&tokenizer, "Red BOX").unwrap();
        let mut want_ids = vec![2, 3];
        want_ids.resize(TEXT_TOKENS, 1);
        let mut want_mask = vec![1, 1];
        want_mask.resize(TEXT_TOKENS, 0);
        assert_eq!((ids, mask), (want_ids, want_mask));
    }
}
