//! [`SigLip2`]: SigLIP 2 image and text embeddings in one space.

use std::sync::Arc;

use async_trait::async_trait;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::OnnxOptions;
use super::preprocess::{Normalize, l2_normalize};
use super::session::{Input, OrtModel, check_manifest, manifest_error, named_file, onnx_file};
use crate::backend::{BackendDescriptor, Locality};
use crate::cx::VisionCx;
use crate::error::{Result, VisionError};
use crate::geometry::{CoordFrame, Letterbox};
use crate::manifest::{ModelSource, VerifiedModel, VisionTask};
use crate::sent::SentImage;
use crate::traits::{Embedder, EmbeddingSpace, Metric};

/// SigLIP 2's text length: every text is padded or truncated to 64 tokens, and the text
/// tower pools the last position, so the padding is part of the model.
const TEXT_TOKENS: usize = 64;

/// SigLIP 2 (Apache-2.0) on ONNX Runtime: image and text embeddings in one space, for
/// retrieval and zero-shot matching. The base model (Q14's default) gives 768 dimensions;
/// so400m (opt-in) gives 1,152.
///
/// Images are stretched to the manifest's input (256×256 for the patch16-256 models) and
/// normalised with `io.mean` and `io.std`; texts are lowercased, as SigLIP 2 was trained,
/// tokenised with the Gemma tokenizer, ended with `<eos>` and padded to 64 tokens. Both
/// towers' pooled outputs are scaled to unit length, so the space's [`Metric::Cosine`] is
/// their dot product.
///
/// The manifest pins the `.onnx` file whose name contains `vision`, the one whose name
/// contains `text`, and `tokenizer.json`.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_vision::onnx::{OnnxOptions, SigLip2};
/// use adk_vision::{Embedder, FetchConfig, InMemoryAcceptanceStore, ManifestSet, ModelFetcher};
///
/// # fn demo() -> adk_vision::Result<()> {
/// let manifest =
///     ManifestSet::builtin()?.get("siglip2-base-patch16-256-quantized").cloned().expect("built in");
/// let fetcher =
///     ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
/// let embedder = SigLip2::new(&fetcher.fetch_blocking(&manifest)?, &OnnxOptions::default())?;
/// assert_eq!(embedder.space().dimensions, 768);
/// # Ok(())
/// # }
/// ```
pub struct SigLip2 {
    descriptor: BackendDescriptor,
    space: EmbeddingSpace,
    vision: OrtModel,
    text: OrtModel,
    tokenizer: Arc<Tokenizer>,
    normalize: Normalize,
}

impl std::fmt::Debug for SigLip2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigLip2")
            .field("descriptor", &self.descriptor)
            .field("space", &self.space)
            .finish_non_exhaustive()
    }
}

impl SigLip2 {
    /// Loads a verified SigLIP 2 vision tower, text tower and tokenizer.
    ///
    /// # Errors
    ///
    /// [`VisionError::Manifest`] when the manifest is not an `ort` embedding model with
    /// those three files, or the towers disagree on the dimensions;
    /// [`VisionError::Backend`] when a file cannot be loaded.
    pub fn new(model: &VerifiedModel, options: &OnnxOptions) -> Result<Self> {
        let manifest = model.manifest();
        check_manifest(manifest, VisionTask::Embed)?;
        let vision = OrtModel::load(&manifest.id, onnx_file(model, "vision", "")?, options)?;
        let text = OrtModel::load(&manifest.id, onnx_file(model, "text", "")?, options)?;
        let tokenizer_file = named_file(model, "tokenizer.json")?;
        let mut tokenizer = Tokenizer::from_file(tokenizer_file.path()).map_err(|e| {
            manifest_error(manifest, format!("`tokenizer.json` could not be read: {e}"))
        })?;
        let pad_id = tokenizer.token_to_id("<pad>").unwrap_or(0);
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: TEXT_TOKENS,
                ..TruncationParams::default()
            }))
            .map_err(|e| {
                manifest_error(manifest, format!("the tokenizer refused truncation: {e}"))
            })?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::Fixed(TEXT_TOKENS),
            pad_id,
            pad_token: "<pad>".into(),
            ..PaddingParams::default()
        }));
        let pooled = |m: &OrtModel| {
            m.outputs()
                .iter()
                .find(|(n, _)| n == "pooler_output")
                .and_then(|(_, s)| s.last().copied())
                .and_then(|d| usize::try_from(d).ok())
        };
        let dimensions = match (pooled(&vision), pooled(&text)) {
            (Some(v), Some(t)) if v == t => v,
            (v, t) => {
                return Err(manifest_error(
                    manifest,
                    format!(
                        "the towers must both pool to one fixed size; vision gives {v:?}, text {t:?}"
                    ),
                ));
            }
        };
        let revision = match &manifest.source {
            ModelSource::HuggingFace { revision, .. }
            | ModelSource::ModelScope { revision, .. } => revision.clone(),
            ModelSource::Url { .. } | ModelSource::Local { .. } => "local".to_string(),
        };
        let space = EmbeddingSpace {
            model: manifest.id.clone(),
            revision,
            dimensions,
            metric: Metric::Cosine,
        };
        let descriptor = BackendDescriptor::new(
            manifest.id.clone(),
            Locality::InProcess,
            CoordFrame::SentPixels,
        )
        .with_manifest(manifest.id.clone());
        Ok(Self {
            descriptor,
            space,
            vision,
            text,
            tokenizer: Arc::new(tokenizer),
            normalize: Normalize::from_io(&manifest.io, (256, 256), Letterbox::Stretch),
        })
    }

    /// Splits a `[n, dimensions]` output into unit-length rows.
    fn rows(&self, data: &[f32], n: usize) -> Result<Vec<Vec<f32>>> {
        let d = self.space.dimensions;
        if data.len() != n * d {
            return Err(VisionError::Parse {
                backend: self.descriptor.id.to_string(),
                message: format!("{} values for {n} vectors of {d}", data.len()),
            });
        }
        Ok(data
            .chunks_exact(d)
            .map(|row| {
                let mut v = row.to_vec();
                l2_normalize(&mut v);
                v
            })
            .collect())
    }
}

/// Lowercases and tokenises `texts` to `[n, 64]` ids.
pub(crate) fn token_ids(
    tokenizer: &Tokenizer,
    texts: &[&str],
) -> std::result::Result<Vec<i64>, String> {
    let lowered: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
    let encodings = tokenizer.encode_batch(lowered, true).map_err(|e| e.to_string())?;
    let mut ids = Vec::with_capacity(texts.len() * TEXT_TOKENS);
    for encoding in &encodings {
        let row = encoding.get_ids();
        if row.len() != TEXT_TOKENS {
            return Err(format!("a text encoded to {} tokens, not {TEXT_TOKENS}", row.len()));
        }
        ids.extend(row.iter().map(|&id| i64::from(id)));
    }
    Ok(ids)
}

#[async_trait]
impl Embedder for SigLip2 {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    async fn embed_images(&self, cx: &VisionCx, images: &[SentImage]) -> Result<Vec<Vec<f32>>> {
        cx.check()?;
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let (w, h) = self.normalize.size;
        let mut batch = Vec::with_capacity(images.len() * 3 * w as usize * h as usize);
        for image in images {
            batch.extend(self.normalize.tensor(image.pixels()));
        }
        let n = images.len();
        let input = self.vision.input_named(&["pixel_values"], 0)?;
        let outputs = self
            .vision
            .run(
                vec![(input, Input::F32(vec![n as i64, 3, i64::from(h), i64::from(w)], batch))],
                &["pooler_output"],
            )
            .await?;
        self.rows(&outputs["pooler_output"].data, n)
    }

    async fn embed_texts(&self, cx: &VisionCx, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        cx.check()?;
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let ids = token_ids(&self.tokenizer, texts)
            .map_err(|e| self.text.error(format!("tokenisation failed: {e}")))?;
        let n = texts.len();
        let input = self.text.input_named(&["input_ids"], 0)?;
        let outputs = self
            .text
            .run(
                vec![(input, Input::I64(vec![n as i64, TEXT_TOKENS as i64], ids))],
                &["pooler_output"],
            )
            .await?;
        self.rows(&outputs["pooler_output"].data, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::models::wordlevel::WordLevel;
    use tokenizers::pre_tokenizers::whitespace::Whitespace;

    #[test]
    fn texts_are_lowercased_and_padded_to_sixty_four_tokens() {
        let vocab = [("<pad>", 0), ("<eos>", 1), ("<unk>", 3), ("a", 10), ("cat", 11)]
            .into_iter()
            .map(|(t, i)| (t.to_string(), i))
            .collect();
        let model = WordLevel::builder().vocab(vocab).unk_token("<unk>".into()).build().unwrap();
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
            pad_id: 0,
            pad_token: "<pad>".into(),
            ..PaddingParams::default()
        }));
        let ids = token_ids(&tokenizer, &["A Cat", "a"]).unwrap();
        assert_eq!(ids.len(), 2 * TEXT_TOKENS);
        assert_eq!(&ids[..3], &[10, 11, 0]);
        assert_eq!(&ids[TEXT_TOKENS..TEXT_TOKENS + 2], &[10, 0]);
        assert!(ids[2..TEXT_TOKENS].iter().all(|&i| i == 0));
    }
}
