//! ONNX Runtime TTS provider.

use std::path::PathBuf;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{TtsProvider, TtsRequest, Voice};

use super::execution_provider::OnnxExecutionProvider;
use ort::session::Session;
use ort::value::Value;

/// Configuration for ONNX TTS inference.
#[derive(Debug, Clone)]
pub struct OnnxModelConfig {
    /// HuggingFace model identifier or local path.
    pub model_id: String,
    /// Execution provider for hardware acceleration.
    pub execution_provider: OnnxExecutionProvider,
    /// Number of intra-op threads for CPU execution.
    pub num_threads: Option<usize>,
    /// Maximum generation length in tokens.
    pub max_length: usize,
    /// Output sample rate in Hz.
    pub sample_rate: u32,
}

impl Default for OnnxModelConfig {
    fn default() -> Self {
        Self {
            model_id: "kokoro-onnx/Kokoro-82M".into(),
            execution_provider: OnnxExecutionProvider::auto_detect(),
            num_threads: None,
            max_length: 4096,
            sample_rate: 24000,
        }
    }
}

/// ONNX Runtime TTS provider with cross-platform hardware acceleration.
pub struct OnnxTtsProvider {
    config: OnnxModelConfig,
    #[allow(dead_code)]
    model_path: PathBuf,
    session: tokio::sync::Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    voices: Vec<Voice>,
}

impl OnnxTtsProvider {
    /// Load model from HuggingFace Hub or local cache.
    pub async fn new(config: OnnxModelConfig, registry: &LocalModelRegistry) -> AudioResult<Self> {
        let model_path = registry.get_or_download(&config.model_id).await?;
        let session = Self::create_session(&model_path, &config)?;
        let tokenizer = Self::load_tokenizer(&model_path)?;
        let voices = super::voice::discover_voices(&model_path);
        Ok(Self {
            config,
            model_path,
            session: tokio::sync::Mutex::new(session),
            tokenizer,
            voices,
        })
    }

    /// Convenience: load default Kokoro-82M ONNX with auto-detected provider.
    pub async fn default_kokoro() -> AudioResult<Self> {
        let registry = LocalModelRegistry::default();
        Self::new(OnnxModelConfig::default(), &registry).await
    }

    fn create_session(
        model_path: &std::path::Path,
        config: &OnnxModelConfig,
    ) -> AudioResult<Session> {
        let onnx_path = model_path.join("model.onnx");
        if !onnx_path.exists() {
            return Err(AudioError::Tts {
                provider: "ONNX".into(),
                message: format!(
                    "model.onnx not found at {}. Ensure the model repository contains an ONNX file.",
                    onnx_path.display()
                ),
            });
        }

        let mut builder = Session::builder().map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("failed to create session builder: {e}"),
        })?;

        if let Some(threads) = config.num_threads {
            builder = builder.with_intra_threads(threads).map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("failed to set thread count: {e}"),
            })?;
        }

        builder = match config.execution_provider {
            OnnxExecutionProvider::Cuda => builder
                .with_execution_providers([
                    ort::execution_providers::CUDAExecutionProvider::default().build(),
                ])
                .map_err(|e| AudioError::Tts {
                    provider: "ONNX".into(),
                    message: format!(
                        "CUDA execution provider failed: {e}. Ensure CUDA toolkit is installed."
                    ),
                })?,
            OnnxExecutionProvider::CoreMl => builder
                .with_execution_providers([
                    ort::execution_providers::CoreMLExecutionProvider::default().build(),
                ])
                .map_err(|e| AudioError::Tts {
                    provider: "ONNX".into(),
                    message: format!("CoreML execution provider failed: {e}."),
                })?,
            OnnxExecutionProvider::DirectMl => {
                tracing::warn!("DirectML not available on this platform, falling back to CPU");
                builder
            }
            OnnxExecutionProvider::Cpu => builder,
        };

        builder.commit_from_file(&onnx_path).map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("failed to load ONNX model: {e}"),
        })
    }

    fn load_tokenizer(model_path: &std::path::Path) -> AudioResult<tokenizers::Tokenizer> {
        let tokenizer_path = model_path.join("tokenizer.json");
        tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("failed to load tokenizer: {e}"),
        })
    }
}

#[async_trait]
impl TtsProvider for OnnxTtsProvider {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let encoding =
            self.tokenizer.encode(request.text.as_str(), true).map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("tokenization failed: {e}"),
            })?;
        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let seq_len = token_ids.len();

        if seq_len == 0 {
            return Err(AudioError::Tts {
                provider: "ONNX".into(),
                message: "tokenization produced no tokens".into(),
            });
        }

        // Create input tensor using (shape, data) tuple — ort v2 API
        let input_ids = Value::from_array(([1i64, seq_len as i64], token_ids)).map_err(|e| {
            AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("ort value creation failed: {e}"),
            }
        })?;

        let inputs = ort::inputs!["input_ids" => input_ids];

        let mut session = self.session.lock().await;
        let outputs = session.run(inputs).map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("inference failed on {}: {e}", self.config.execution_provider),
        })?;

        // Extract output — ort v2 returns (&Shape, &[T])
        let output_value = &outputs[0];
        let (_shape, audio_slice) =
            output_value.try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("failed to extract output tensor: {e}"),
            })?;

        // Convert f32 [-1.0, 1.0] to PCM16
        let sample_bytes: Vec<u8> = audio_slice
            .iter()
            .flat_map(|s| {
                let clamped = s.clamp(-1.0, 1.0);
                let pcm = (clamped * 32767.0) as i16;
                pcm.to_le_bytes()
            })
            .collect();

        Ok(AudioFrame::new(sample_bytes, self.config.sample_rate, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        let full_frame = self.synthesize(request).await?;
        let chunk_bytes = (self.config.sample_rate as usize * 100 / 1000) * 2;

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
