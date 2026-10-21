//! In-process sentence embeddings on the ONNX runtime (feature `onnx`; spec 3.1 §7.15.2,
//! §10 R1).
//!
//! [`OnnxEmbeddingProvider`] runs a sentence-transformer ONNX model — `BAAI/bge-small-en-v1.5`
//! by default, 384 dimensions — on the `ort` runtime `adk-audio` already pins. The model
//! and its tokenizer are fetched once into the Hugging Face hub cache (`HF_HOME`, else
//! `~/.cache/huggingface`) and read from there afterwards, so embedding needs no network
//! and no provider key. It is the recall embedder of the fully local harness.
//!
//! | Step | Default |
//! |---|---|
//! | Tokenize | the model's `tokenizer.json`, truncated to `max_tokens` (512), padded to the longest text of the batch |
//! | Run | `input_ids`, `attention_mask` and, when the model declares it, `token_type_ids` |
//! | Pool | mean of the last hidden states over the attention mask ([`OnnxPooling::Mean`]); [`OnnxPooling::Cls`] takes the first token |
//! | Normalize | L2, so dot products are cosine similarities |
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_rag::{EmbeddingProvider, OnnxEmbeddingConfig, OnnxEmbeddingProvider};
//!
//! # async fn run() -> adk_rag::Result<()> {
//! let provider = OnnxEmbeddingProvider::new(OnnxEmbeddingConfig::default())?;
//! let vector = provider.embed("the tests need --features sqlite").await?;
//! assert_eq!(vector.len(), 384);
//! # Ok(())
//! # }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Value;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};

/// The default model: `BAAI/bge-small-en-v1.5`, 384 dimensions.
pub const DEFAULT_MODEL_ID: &str = "BAAI/bge-small-en-v1.5";
/// The default model's dimensionality.
pub const DEFAULT_DIMENSIONS: usize = 384;
/// The ONNX file inside the model repository.
pub const DEFAULT_MODEL_FILE: &str = "onnx/model.onnx";
/// The tokenizer file inside the model repository.
pub const DEFAULT_TOKENIZER_FILE: &str = "tokenizer.json";

const PROVIDER: &str = "ONNX";

fn error(message: impl Into<String>) -> RagError {
    RagError::EmbeddingError { provider: PROVIDER.into(), message: message.into() }
}

/// How the token states become one vector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnnxPooling {
    /// The mean of the last hidden states over the attention mask.
    #[default]
    Mean,
    /// The first (`[CLS]`) token's state, which BGE models are trained on.
    Cls,
}

/// Configuration of an [`OnnxEmbeddingProvider`].
///
/// # Example
///
/// ```rust
/// use adk_rag::{OnnxEmbeddingConfig, OnnxPooling};
///
/// let config = OnnxEmbeddingConfig::default();
/// assert_eq!(config.model_id, "BAAI/bge-small-en-v1.5");
/// assert_eq!((config.dimensions, config.pooling, config.normalize), (384, OnnxPooling::Mean, true));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnnxEmbeddingConfig {
    /// The Hugging Face repository of the model.
    pub model_id: String,
    /// The ONNX file inside the repository (or `local_dir`).
    pub model_file: String,
    /// The tokenizer file inside the repository (or `local_dir`).
    pub tokenizer_file: String,
    /// A directory holding the two files, instead of the hub cache: nothing is fetched.
    pub local_dir: Option<PathBuf>,
    /// The vectors' dimensionality; the model's hidden size must match.
    pub dimensions: usize,
    /// How token states become one vector.
    pub pooling: OnnxPooling,
    /// Whether vectors are L2-normalised.
    pub normalize: bool,
    /// The most tokens of one text; longer texts are truncated.
    pub max_tokens: usize,
    /// Threads the runtime uses within one run; `None` is the runtime's default.
    pub threads: Option<usize>,
}

impl Default for OnnxEmbeddingConfig {
    fn default() -> Self {
        Self {
            model_id: DEFAULT_MODEL_ID.into(),
            model_file: DEFAULT_MODEL_FILE.into(),
            tokenizer_file: DEFAULT_TOKENIZER_FILE.into(),
            local_dir: None,
            dimensions: DEFAULT_DIMENSIONS,
            pooling: OnnxPooling::Mean,
            normalize: true,
            max_tokens: 512,
            threads: None,
        }
    }
}

impl OnnxEmbeddingConfig {
    /// The default files of the hub repository `model_id`, with `dimensions`.
    pub fn new(model_id: impl Into<String>, dimensions: usize) -> Self {
        Self { model_id: model_id.into(), dimensions, ..Self::default() }
    }

    /// Reads the model and tokenizer files from `dir` instead of the hub cache.
    #[must_use]
    pub fn with_local_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.local_dir = Some(dir.into());
        self
    }

    /// Sets the pooling.
    #[must_use]
    pub fn with_pooling(mut self, pooling: OnnxPooling) -> Self {
        self.pooling = pooling;
        self
    }

    /// Sets the thread count of one run.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = Some(threads);
        self
    }
}

/// An [`EmbeddingProvider`] running a sentence-transformer ONNX model in process.
///
/// See the [module documentation](self).
pub struct OnnxEmbeddingProvider {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    config: OnnxEmbeddingConfig,
    input_names: Vec<String>,
    model_path: PathBuf,
}

impl std::fmt::Debug for OnnxEmbeddingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxEmbeddingProvider")
            .field("model_id", &self.config.model_id)
            .field("model_path", &self.model_path)
            .field("dimensions", &self.config.dimensions)
            .field("pooling", &self.config.pooling)
            .finish()
    }
}

impl OnnxEmbeddingProvider {
    /// Loads the model and tokenizer of `config`, fetching them into the hub cache on the
    /// first use unless `local_dir` is set.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::EmbeddingError`] when a file cannot be fetched (no network and
    /// not cached) or loaded.
    pub fn new(config: OnnxEmbeddingConfig) -> Result<Self> {
        let (model_path, tokenizer_path) = match &config.local_dir {
            Some(dir) => (dir.join(&config.model_file), dir.join(&config.tokenizer_file)),
            None => fetch(&config)?,
        };
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            error(format!("failed to load the tokenizer {}: {e}", tokenizer_path.display()))
        })?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: config.max_tokens,
                ..TruncationParams::default()
            }))
            .map_err(|e| error(format!("failed to configure truncation: {e}")))?;
        tokenizer.with_padding(Some(PaddingParams::default()));

        let mut builder = Session::builder()
            .and_then(|builder| builder.with_optimization_level(GraphOptimizationLevel::Level3))
            .map_err(|e| error(format!("failed to create the session builder: {e}")))?;
        if let Some(threads) = config.threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| error(format!("failed to set the thread count: {e}")))?;
        }
        let session = builder.commit_from_file(&model_path).map_err(|e| {
            error(format!("failed to load the ONNX model {}: {e}", model_path.display()))
        })?;
        let input_names = session.inputs().iter().map(|input| input.name().to_string()).collect();
        tracing::info!(
            model.id = %config.model_id,
            model.path = %model_path.display(),
            dimensions = config.dimensions,
            "loaded the ONNX embedding model"
        );
        Ok(Self { session: Mutex::new(session), tokenizer, config, input_names, model_path })
    }

    /// The configuration.
    pub fn config(&self) -> &OnnxEmbeddingConfig {
        &self.config
    }

    /// Where the model was loaded from.
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    fn embed_sync(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| error(format!("tokenization failed: {e}")))?;
        let batch = encodings.len();
        let seq = encodings.iter().map(|encoding| encoding.get_ids().len()).max().unwrap_or(0);
        let mut ids = Vec::with_capacity(batch * seq);
        let mut mask = Vec::with_capacity(batch * seq);
        let mut types = Vec::with_capacity(batch * seq);
        for encoding in &encodings {
            let pad = seq - encoding.get_ids().len();
            ids.extend(encoding.get_ids().iter().map(|id| i64::from(*id)));
            ids.extend(std::iter::repeat_n(0, pad));
            mask.extend(encoding.get_attention_mask().iter().map(|m| i64::from(*m)));
            mask.extend(std::iter::repeat_n(0, pad));
            types.extend(encoding.get_type_ids().iter().map(|t| i64::from(*t)));
            types.extend(std::iter::repeat_n(0, pad));
        }
        let shape = [batch as i64, seq as i64];
        let tensor = |data: Vec<i64>| {
            Value::from_array((shape, data))
                .map(Value::into_dyn)
                .map_err(|e| error(format!("failed to create an input tensor: {e}")))
        };
        let mut inputs = vec![
            ("input_ids".to_string(), tensor(ids)?),
            ("attention_mask".to_string(), tensor(mask.clone())?),
        ];
        if self.input_names.iter().any(|name| name == "token_type_ids") {
            inputs.push(("token_type_ids".to_string(), tensor(types)?));
        }
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let outputs = session.run(inputs).map_err(|e| error(format!("inference failed: {e}")))?;
        let (out_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| error(format!("failed to read the hidden states: {e}")))?;
        let out_shape: Vec<i64> = out_shape.iter().copied().collect();
        pool(
            data,
            &out_shape,
            &mask,
            self.config.pooling,
            self.config.normalize,
            self.config.dimensions,
        )
    }
}

/// Pools `data` of shape `[batch, seq, hidden]` over `mask` (`[batch, seq]`, 1 for real
/// tokens) into one vector per text.
fn pool(
    data: &[f32],
    shape: &[i64],
    mask: &[i64],
    pooling: OnnxPooling,
    normalize: bool,
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    let [batch, seq, hidden] = shape else {
        return Err(error(format!(
            "expected hidden states of shape [batch, seq, hidden], got {shape:?}"
        )));
    };
    let (batch, seq, hidden) = (*batch as usize, *seq as usize, *hidden as usize);
    if hidden != dimensions {
        return Err(error(format!(
            "the model produces {hidden}-dimensional states but the configuration says {dimensions}"
        )));
    }
    if data.len() != batch * seq * hidden || mask.len() != batch * seq {
        return Err(error("the hidden states do not match the batch"));
    }
    let mut vectors = Vec::with_capacity(batch);
    for b in 0..batch {
        let mut vector = vec![0.0f32; hidden];
        match pooling {
            OnnxPooling::Mean => {
                let mut count = 0.0f32;
                for t in 0..seq {
                    if mask[b * seq + t] == 0 {
                        continue;
                    }
                    count += 1.0;
                    let row = &data[(b * seq + t) * hidden..(b * seq + t + 1) * hidden];
                    for (sum, value) in vector.iter_mut().zip(row) {
                        *sum += *value;
                    }
                }
                if count > 0.0 {
                    for value in &mut vector {
                        *value /= count;
                    }
                }
            }
            OnnxPooling::Cls => {
                vector.copy_from_slice(&data[b * seq * hidden..b * seq * hidden + hidden]);
            }
        }
        if normalize {
            let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 0.0 {
                for value in &mut vector {
                    *value /= norm;
                }
            }
        }
        vectors.push(vector);
    }
    Ok(vectors)
}

/// Fetches the model files into the hub cache, or reads them from it.
fn fetch(config: &OnnxEmbeddingConfig) -> Result<(PathBuf, PathBuf)> {
    // `from_env` honours `HF_HOME` (and `HF_ENDPOINT`); `new` always uses the home cache.
    let api = hf_hub::api::sync::ApiBuilder::from_env()
        .with_progress(false)
        .build()
        .map_err(|e| error(format!("the Hugging Face hub client failed: {e}")))?;
    let repo = api.model(config.model_id.clone());
    let get = |file: &str| {
        repo.get(file).map_err(|e| {
            error(format!(
                "could not fetch {file} of {} into the Hugging Face hub cache: {e}; the first \
                 use needs network, or point `local_dir` at a directory holding the files",
                config.model_id
            ))
        })
    };
    Ok((get(&config.model_file)?, get(&config.tokenizer_file)?))
}

#[async_trait]
impl EmbeddingProvider for OnnxEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_sync(&[text])?.into_iter().next().ok_or_else(|| error("no vector produced"))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_sync(texts)
    }

    fn dimensions(&self) -> usize {
        self.config.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pooling_averages_real_tokens_and_normalizes() {
        // batch 1, seq 3, hidden 2; the third token is padding.
        let data = [1.0, 0.0, 3.0, 4.0, 100.0, 100.0];
        let vectors = pool(&data, &[1, 3, 2], &[1, 1, 0], OnnxPooling::Mean, false, 2).unwrap();
        assert_eq!(vectors, vec![vec![2.0, 2.0]]);
        let normalized = pool(&data, &[1, 3, 2], &[1, 1, 0], OnnxPooling::Mean, true, 2).unwrap();
        let norm: f32 = normalized[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        let cls = pool(&data, &[1, 3, 2], &[1, 1, 0], OnnxPooling::Cls, false, 2).unwrap();
        assert_eq!(cls, vec![vec![1.0, 0.0]]);
    }

    #[test]
    fn a_dimension_mismatch_is_an_error() {
        let error = pool(&[0.0; 4], &[1, 2, 2], &[1, 1], OnnxPooling::Mean, true, 384).unwrap_err();
        assert!(error.to_string().contains("384"), "{error}");
    }

    /// Needs the model in the hub cache or network to fetch it: run with `--ignored`.
    #[tokio::test]
    #[ignore = "fetches BAAI/bge-small-en-v1.5 (133 MB) into the Hugging Face hub cache"]
    async fn the_default_model_embeds_related_texts_closer_than_unrelated_ones() {
        let provider = OnnxEmbeddingProvider::new(OnnxEmbeddingConfig::default()).unwrap();
        let vectors = provider
            .embed_batch(&[
                "the tests need --features sqlite",
                "run the test suite with the sqlite feature",
                "deploy on Fridays only",
            ])
            .await
            .unwrap();
        assert_eq!(vectors.len(), 3);
        assert!(vectors.iter().all(|v| v.len() == DEFAULT_DIMENSIONS));
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(dot(&vectors[0], &vectors[1]) > dot(&vectors[0], &vectors[2]));
    }
}
