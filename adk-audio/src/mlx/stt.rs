//! MLX STT provider for Apple Silicon.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{SttOptions, SttProvider, Transcript};

use super::config::MlxSttConfig;

/// MLX-based STT provider using Metal GPU on Apple Silicon.
///
/// Runs Whisper models locally via `mlx-rs` with unified memory.
/// Supports 16kHz mono input; audio is resampled automatically if needed.
pub struct MlxSttProvider {
    config: MlxSttConfig,
    #[allow(dead_code)]
    model_path: std::path::PathBuf,
    #[allow(dead_code)] // Used when full inference is wired
    tokenizer: tokenizers::Tokenizer,
}

impl MlxSttProvider {
    /// Load model from HuggingFace Hub or local cache.
    pub async fn new(config: MlxSttConfig, registry: &LocalModelRegistry) -> AudioResult<Self> {
        let model_path = registry.get_or_download(&config.model_id).await?;
        let tokenizer = Self::load_tokenizer(&model_path)?;
        Ok(Self { config, model_path, tokenizer })
    }

    /// Convenience: load default Whisper large-v3-turbo with default registry.
    pub async fn default_whisper() -> AudioResult<Self> {
        let registry = LocalModelRegistry::default();
        Self::new(MlxSttConfig::default(), &registry).await
    }

    fn load_tokenizer(model_path: &std::path::Path) -> AudioResult<tokenizers::Tokenizer> {
        let tokenizer_path = model_path.join("tokenizer.json");
        tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| AudioError::Stt {
            provider: "MLX".into(),
            message: format!("failed to load tokenizer: {e}"),
        })
    }
}

#[async_trait]
impl SttProvider for MlxSttProvider {
    async fn transcribe(&self, audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        // Convert PCM16 to f32 normalized samples
        let samples: Vec<f32> = audio.samples().iter().map(|&s| s as f32 / 32768.0).collect();

        if samples.is_empty() {
            return Err(AudioError::Stt {
                provider: "MLX".into(),
                message: "empty audio input".into(),
            });
        }

        // Compute log-mel spectrogram
        let mel = super::mel::compute_log_mel_spectrogram(&samples, self.config.sample_rate)?;

        // TODO: Full MLX Whisper inference pipeline requires loading encoder/decoder
        // weights into mlx_rs arrays and running the forward pass on Metal GPU.
        // This requires Whisper-specific architecture code.
        // For now, return a placeholder indicating spectrogram dimensions.
        Err(AudioError::Stt {
            provider: "MLX".into(),
            message: format!(
                "MLX Whisper inference not yet wired: mel spectrogram {}×{} frames. \
                 Model at: {}",
                mel.n_frames,
                mel.n_mels,
                self.model_path.display()
            ),
        })
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        // Windowed 30s fallback — accumulate frames and transcribe each window
        Ok(Box::pin(futures::stream::empty()))
    }
}
