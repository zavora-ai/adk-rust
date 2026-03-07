//! ONNX Runtime TTS provider.
//!
//! Generic provider that can run any ONNX TTS model by pairing it with
//! the appropriate [`Preprocessor`]. Ships with two built-in preprocessors:
//!
//! - [`TokenizerPreprocessor`] — for models with a `tokenizer.json` (default)
//! - [`KokoroPreprocessor`] — for Kokoro-82M using espeak-ng phonemization (requires `kokoro` feature)

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{TtsProvider, TtsRequest, Voice};

use super::execution_provider::OnnxExecutionProvider;
use super::preprocessor::{Preprocessor, PreprocessorOutput, TokenizerPreprocessor};
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
    /// Name of the ONNX file inside the model directory (default: `"model.onnx"`).
    pub onnx_filename: String,
}

impl Default for OnnxModelConfig {
    fn default() -> Self {
        Self {
            model_id: "kokoro-onnx/Kokoro-82M".into(),
            execution_provider: OnnxExecutionProvider::auto_detect(),
            num_threads: None,
            max_length: 4096,
            sample_rate: 24000,
            onnx_filename: "model.onnx".into(),
        }
    }
}

/// ONNX Runtime TTS provider with cross-platform hardware acceleration.
///
/// Uses a [`Preprocessor`] to convert text into model-ready inputs,
/// making it compatible with different ONNX TTS architectures.
///
/// # Example (Kokoro)
///
/// ```rust,ignore
/// use adk_audio::onnx::{OnnxTtsProvider, OnnxModelConfig, KokoroPreprocessor};
///
/// let preprocessor = KokoroPreprocessor::new(voices_path, "en-us")?;
/// let provider = OnnxTtsProvider::with_preprocessor(config, model_dir, preprocessor)?;
/// let frame = provider.synthesize(&request).await?;
/// ```
pub struct OnnxTtsProvider {
    config: OnnxModelConfig,
    model_dir: PathBuf,
    session: tokio::sync::Mutex<Session>,
    preprocessor: Box<dyn Preprocessor>,
    voices: Vec<Voice>,
}

impl OnnxTtsProvider {
    /// Create a provider with a custom preprocessor and a local model directory.
    ///
    /// Use this when you already have the model files on disk (e.g., Kokoro
    /// downloaded to `~/.cache/kokoros/`).
    pub fn with_preprocessor(
        config: OnnxModelConfig,
        model_dir: impl Into<PathBuf>,
        preprocessor: impl Preprocessor + 'static,
    ) -> AudioResult<Self> {
        let model_dir = model_dir.into();
        let session = Self::create_session(&model_dir, &config)?;
        let voices = super::voice::discover_voices(&model_dir);
        Ok(Self {
            config,
            model_dir,
            session: tokio::sync::Mutex::new(session),
            preprocessor: Box::new(preprocessor),
            voices,
        })
    }

    /// Load model from HuggingFace Hub or local cache using the default
    /// [`TokenizerPreprocessor`].
    pub async fn new(config: OnnxModelConfig, registry: &LocalModelRegistry) -> AudioResult<Self> {
        let model_path = registry.get_or_download(&config.model_id).await?;
        let session = Self::create_session(&model_path, &config)?;
        let tokenizer = TokenizerPreprocessor::from_model_dir(&model_path)?;
        let voices = super::voice::discover_voices(&model_path);
        Ok(Self {
            config,
            model_dir: model_path,
            session: tokio::sync::Mutex::new(session),
            preprocessor: Box::new(tokenizer),
            voices,
        })
    }

    /// Convenience: load default Kokoro-82M ONNX with auto-detected provider.
    pub async fn default_kokoro() -> AudioResult<Self> {
        let registry = LocalModelRegistry::default();
        Self::new(OnnxModelConfig::default(), &registry).await
    }

    /// Override the voice catalog (useful when the preprocessor knows the voices).
    pub fn set_voices(&mut self, voices: Vec<Voice>) {
        self.voices = voices;
    }

    fn create_session(model_dir: &Path, config: &OnnxModelConfig) -> AudioResult<Session> {
        let onnx_path = model_dir.join(&config.onnx_filename);
        if !onnx_path.exists() {
            return Err(AudioError::Tts {
                provider: "ONNX".into(),
                message: format!(
                    "{} not found at {}. Ensure the model directory contains the ONNX file.",
                    config.onnx_filename,
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

    /// Run ONNX inference with preprocessor output.
    ///
    /// Handles two model architectures:
    /// - Single-input models: just `input_ids` / `tokens`
    /// - Kokoro-style 3-input models: `tokens` + `style` + `speed`
    fn run_inference(
        &self,
        session: &mut Session,
        output: &PreprocessorOutput,
    ) -> AudioResult<Vec<f32>> {
        let seq_len = output.token_ids.len();

        // Build the tokens tensor
        let tokens_tensor = Value::from_array(([1i64, seq_len as i64], output.token_ids.clone()))
            .map_err(|e| AudioError::Tts {
            provider: "ONNX".into(),
            message: format!("failed to create tokens tensor: {e}"),
        })?;

        // Check what inputs the model expects by inspecting session inputs
        let input_names: Vec<String> =
            session.inputs().iter().map(|i| i.name().to_string()).collect();

        let outputs = if input_names.len() >= 3
            && input_names.iter().any(|n| n == "style")
            && input_names.iter().any(|n| n == "speed")
        {
            // Kokoro-style 3-input model: tokens + style + speed
            let style_data = output.style_embedding.as_ref().ok_or_else(|| AudioError::Tts {
                provider: "ONNX".into(),
                message: "model requires style embedding but preprocessor did not provide one"
                    .into(),
            })?;
            let style_len = style_data.len();
            let style_tensor = Value::from_array(([1i64, style_len as i64], style_data.clone()))
                .map_err(|e| AudioError::Tts {
                    provider: "ONNX".into(),
                    message: format!("failed to create style tensor: {e}"),
                })?;

            let speed_val = output.speed.unwrap_or(1.0);
            let speed_tensor =
                Value::from_array(([1i64], vec![speed_val])).map_err(|e| AudioError::Tts {
                    provider: "ONNX".into(),
                    message: format!("failed to create speed tensor: {e}"),
                })?;

            // Determine the tokens input name (Kokoro v1.0 uses "tokens", timestamped uses "input_ids")
            let tokens_name =
                if input_names.contains(&"tokens".to_string()) { "tokens" } else { "input_ids" };

            let inputs = ort::inputs![tokens_name => tokens_tensor, "style" => style_tensor, "speed" => speed_tensor];
            session.run(inputs).map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("inference failed on {}: {e}", self.config.execution_provider),
            })?
        } else {
            // Generic single-input model
            let input_name = input_names.first().map_or("input_ids", |n: &String| n.as_str());
            // We need to own the string for the ort macro
            let inputs = ort::inputs![input_name.to_string() => tokens_tensor];
            session.run(inputs).map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("inference failed on {}: {e}", self.config.execution_provider),
            })?
        };

        // Extract audio output — try common output names
        let output_value = &outputs[0];
        let (_shape, audio_slice) =
            output_value.try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                provider: "ONNX".into(),
                message: format!("failed to extract output tensor: {e}"),
            })?;

        Ok(audio_slice.to_vec())
    }
}

#[async_trait]
impl TtsProvider for OnnxTtsProvider {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        // Run preprocessor
        let preprocessed = self.preprocessor.preprocess(
            &request.text,
            &request.voice,
            request.speed,
            &self.model_dir,
        )?;

        tracing::debug!(
            preprocessor = self.preprocessor.name(),
            tokens = preprocessed.token_ids.len(),
            has_style = preprocessed.style_embedding.is_some(),
            "preprocessed text for ONNX inference"
        );

        // Run inference
        let mut session = self.session.lock().await;
        let audio_f32 = self.run_inference(&mut session, &preprocessed)?;

        // Convert f32 [-1.0, 1.0] to PCM16
        let sample_bytes: Vec<u8> = audio_f32
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
