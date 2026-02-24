//! MLX TTS provider for Apple Silicon.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{TtsProvider, TtsRequest, Voice};

use super::config::MlxTtsConfig;

/// MLX-based TTS provider using Metal GPU on Apple Silicon.
///
/// Loads models from HuggingFace Hub via `LocalModelRegistry` and runs
/// inference through `mlx-rs` with unified memory (zero CPU↔GPU copy).
pub struct MlxTtsProvider {
    config: MlxTtsConfig,
    #[allow(dead_code)]
    model_path: std::path::PathBuf,
    tokenizer: tokenizers::Tokenizer,
    voices: Vec<Voice>,
}

impl MlxTtsProvider {
    /// Load model from HuggingFace Hub or local cache.
    pub async fn new(config: MlxTtsConfig, registry: &LocalModelRegistry) -> AudioResult<Self> {
        let model_path = registry.get_or_download(&config.model_id).await?;
        let tokenizer = Self::load_tokenizer(&model_path)?;
        let voices = Self::discover_voices(&model_path);
        Ok(Self { config, model_path, tokenizer, voices })
    }

    /// Convenience: load default Kokoro-82M with default registry.
    pub async fn default_kokoro() -> AudioResult<Self> {
        let registry = LocalModelRegistry::default();
        Self::new(MlxTtsConfig::default(), &registry).await
    }

    fn load_tokenizer(model_path: &std::path::Path) -> AudioResult<tokenizers::Tokenizer> {
        let tokenizer_path = model_path.join("tokenizer.json");
        tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| AudioError::Tts {
            provider: "MLX".into(),
            message: format!("failed to load tokenizer: {e}"),
        })
    }

    fn discover_voices(model_path: &std::path::Path) -> Vec<Voice> {
        let voices_dir = model_path.join("voices");
        if voices_dir.is_dir() {
            std::fs::read_dir(&voices_dir)
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let name = entry.path().file_stem()?.to_str()?.to_string();
                    Some(Voice {
                        id: name.clone(),
                        name: name.clone(),
                        language: "en".into(),
                        gender: None,
                    })
                })
                .collect()
        } else {
            vec![Voice {
                id: "default".into(),
                name: "Default".into(),
                language: "en".into(),
                gender: None,
            }]
        }
    }

    /// Get the weights filename based on quantization config.
    pub fn weights_file(&self) -> &str {
        match self.config.quantization {
            Some(q) => q.weights_file(),
            None => "model.safetensors",
        }
    }
}

#[async_trait]
impl TtsProvider for MlxTtsProvider {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        // Tokenize input text
        let encoding = self.tokenizer.encode(request.text.as_str(), true).map_err(|e| {
            AudioError::Tts { provider: "MLX".into(), message: format!("tokenization failed: {e}") }
        })?;
        let token_ids = encoding.get_ids();

        if token_ids.is_empty() {
            return Err(AudioError::Tts {
                provider: "MLX".into(),
                message: "tokenization produced no tokens".into(),
            });
        }

        // TODO: Full MLX inference pipeline requires loading safetensors weights
        // into mlx_rs::Array and running the model forward pass on Metal GPU.
        // This requires model-specific architecture code (varies by model).
        // For now, return a placeholder indicating the model path and token count.
        Err(AudioError::Tts {
            provider: "MLX".into(),
            message: format!(
                "MLX inference not yet wired: {} tokens from '{}'. \
                 Model at: {}",
                token_ids.len(),
                request.text,
                self.model_path.display()
            ),
        })
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        // Synthesize full then chunk into 100ms frames
        let full_frame = self.synthesize(request).await?;
        let chunk_bytes = (self.config.sample_rate as usize * 100 / 1000) * 2; // 100ms of PCM16

        let stream = async_stream::stream! {
            let data = full_frame.data.clone();
            let mut offset = 0;
            while offset < data.len() {
                let end = (offset + chunk_bytes).min(data.len());
                let chunk = data.slice(offset..end);
                yield Ok(AudioFrame::new(chunk, full_frame.sample_rate, full_frame.channels));
                offset = end;
            }
        };
        Ok(Box::pin(stream))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}
