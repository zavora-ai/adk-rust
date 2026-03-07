//! Chatterbox TTS provider — multi-model ONNX pipeline for voice cloning.
//!
//! Chatterbox is a 4-model pipeline:
//! 1. `speech_encoder.onnx` — encodes reference voice WAV into speaker embeddings
//! 2. `embed_tokens.onnx` — converts text token IDs + position IDs + exaggeration into embeddings
//! 3. `language_model.onnx` — autoregressive LLM generating speech tokens (with KV-cache)
//! 4. `conditional_decoder.onnx` — converts speech tokens + speaker info to audio waveform
//!
//! All models live in the `onnx/` subfolder of the HuggingFace repo. Each `.onnx`
//! file has a corresponding `.onnx_data` file containing the actual weights.
//! Variants (fp16, q4, q4f16) only affect the language model filename.
//!
//! Supports voice cloning from a reference WAV file. Models auto-download
//! from HuggingFace Hub on first use (~1.4GB for fp32).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_audio::onnx::chatterbox::{ChatterboxTtsProvider, ChatterboxConfig};
//!
//! let config = ChatterboxConfig::default();
//! let provider = ChatterboxTtsProvider::load(config).await?;
//! let frame = provider.synthesize(&request).await?;
//! ```

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::traits::{TtsProvider, TtsRequest, Voice};

use super::execution_provider::OnnxExecutionProvider;
use ort::memory::{Allocator, MemoryInfo};
use ort::session::Session;
use ort::session::SessionInputValue;
use ort::value::{Tensor, Value};

// ---------------------------------------------------------------------------
// Constants (from official reference implementation on HuggingFace model card)
// ---------------------------------------------------------------------------

const S3GEN_SR: u32 = 24000;
const START_SPEECH_TOKEN: i64 = 6561;
const STOP_SPEECH_TOKEN: i64 = 6562;
const NUM_LAYERS: usize = 30;
const NUM_KV_HEADS: usize = 16;
const HEAD_DIM: usize = 64;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Model variant / quantization level for Chatterbox ONNX models.
///
/// All variants share the same `speech_encoder`, `embed_tokens`, and
/// `conditional_decoder` models. Only the `language_model` filename differs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatterboxVariant {
    /// Full precision (fp32) — best quality, largest size (~1.4GB total).
    #[default]
    Fp32,
    /// Half precision (fp16) — good quality, smaller language model.
    Fp16,
    /// 4-bit quantized — smallest language model, fastest.
    Q4,
    /// 4-bit quantized with fp16 — mixed precision.
    Q4F16,
}

impl ChatterboxVariant {
    /// Returns the language model filename for this variant.
    fn language_model_filename(&self) -> &str {
        match self {
            Self::Fp32 => "language_model.onnx",
            Self::Fp16 => "language_model_fp16.onnx",
            Self::Q4 => "language_model_q4.onnx",
            Self::Q4F16 => "language_model_q4f16.onnx",
        }
    }

    /// Returns the language model data filename for this variant.
    fn language_model_data_filename(&self) -> &str {
        match self {
            Self::Fp32 => "language_model.onnx_data",
            Self::Fp16 => "language_model_fp16.onnx_data",
            Self::Q4 => "language_model_q4.onnx_data",
            Self::Q4F16 => "language_model_q4f16.onnx_data",
        }
    }
}

/// Configuration for the Chatterbox TTS provider.
#[derive(Debug, Clone)]
pub struct ChatterboxConfig {
    /// HuggingFace repo ID (default: `"onnx-community/chatterbox-ONNX"`).
    pub repo_id: String,
    /// Model variant / quantization level.
    pub variant: ChatterboxVariant,
    /// ONNX execution provider for hardware acceleration.
    pub execution_provider: OnnxExecutionProvider,
    /// Maximum new tokens to generate (default: 2000).
    pub max_new_tokens: usize,
    /// Repetition penalty for the language model (default: 1.2).
    pub repetition_penalty: f32,
    /// Exaggeration control (0.0–1.0, default: 0.5). Higher = more expressive.
    pub exaggeration: f32,
    /// Path to a reference WAV file for voice cloning (optional).
    /// If not set, falls back to `default_voice.wav` from the model repo.
    pub reference_wav: Option<PathBuf>,
}

impl Default for ChatterboxConfig {
    fn default() -> Self {
        Self {
            repo_id: "onnx-community/chatterbox-ONNX".into(),
            variant: ChatterboxVariant::default(),
            execution_provider: OnnxExecutionProvider::Cpu,
            max_new_tokens: 2000,
            repetition_penalty: 1.2,
            exaggeration: 0.5,
            reference_wav: None,
        }
    }
}

// ---------------------------------------------------------------------------
// KV-Cache types
// ---------------------------------------------------------------------------

struct KvEntry {
    shape: Vec<i64>,
    data: Vec<f32>,
}

struct KvCache {
    keys: Vec<KvEntry>,
    values: Vec<KvEntry>,
}

struct LmStepOutput {
    logits: Vec<f32>,
    kv_cache: KvCache,
}

/// Outputs from the speech encoder — 4 tensors used by different pipeline stages.
struct SpeechEncoderOutput {
    /// Conditioning embedding for the LM prefill (concatenated with text embeds).
    cond_emb: (Vec<i64>, Vec<f32>),
    /// Prompt tokens prepended to generated speech tokens before decoding.
    prompt_token: (Vec<i64>, Vec<i64>),
    /// Speaker x-vector passed to the conditional decoder.
    ref_x_vector: (Vec<i64>, Vec<f32>),
    /// Speaker features passed to the conditional decoder.
    prompt_feat: (Vec<i64>, Vec<f32>),
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Chatterbox TTS provider — 4-model ONNX pipeline with voice cloning.
pub struct ChatterboxTtsProvider {
    config: ChatterboxConfig,
    speech_encoder: tokio::sync::Mutex<Session>,
    embed_tokens: tokio::sync::Mutex<Session>,
    language_model: tokio::sync::Mutex<Session>,
    conditional_decoder: tokio::sync::Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    /// Cached speech encoder output for the reference voice.
    cached_encoder_output: Option<SpeechEncoderOutput>,
    /// Path to the default_voice.wav from the model repo (fallback reference).
    default_voice_path: Option<PathBuf>,
}

impl ChatterboxTtsProvider {
    /// Load all 4 ONNX models from HuggingFace Hub (auto-downloads on first use).
    pub async fn load(config: ChatterboxConfig) -> AudioResult<Self> {
        let (model_dir, default_voice_path) = Self::ensure_models(&config).await?;
        let onnx_dir = model_dir.join("onnx");

        tracing::info!(
            repo = %config.repo_id,
            variant = ?config.variant,
            ep = %config.execution_provider,
            "loading chatterbox models"
        );

        eprintln!("[chatterbox] loading speech_encoder.onnx ...");
        let mut speech_encoder =
            Self::create_session(&onnx_dir.join("speech_encoder.onnx"), &config)?;
        eprintln!("[chatterbox] speech_encoder loaded OK");

        eprintln!("[chatterbox] loading embed_tokens.onnx ...");
        let embed_tokens = Self::create_session(&onnx_dir.join("embed_tokens.onnx"), &config)?;
        eprintln!("[chatterbox] embed_tokens loaded OK");

        eprintln!("[chatterbox] loading conditional_decoder.onnx ...");
        let conditional_decoder =
            Self::create_session(&onnx_dir.join("conditional_decoder.onnx"), &config)?;
        eprintln!("[chatterbox] conditional_decoder loaded OK");

        eprintln!("[chatterbox] loading {} ...", config.variant.language_model_filename());
        let language_model = Self::create_session(
            &onnx_dir.join(config.variant.language_model_filename()),
            &config,
        )?;
        eprintln!("[chatterbox] language_model loaded OK");

        let tokenizer_path = model_dir.join("tokenizer.json");
        eprintln!("[chatterbox] loading tokenizer from {} ...", tokenizer_path.display());
        let tokenizer =
            tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to load tokenizer from {}: {e}", tokenizer_path.display()),
            })?;
        eprintln!("[chatterbox] tokenizer loaded OK");

        // Pre-encode reference voice
        eprintln!("[chatterbox] pre-encoding reference voice ...");
        let cached_encoder_output = if let Some(ref wav_path) = config.reference_wav {
            eprintln!("[chatterbox] running speech encoder on {} ...", wav_path.display());
            Some(Self::run_speech_encoder(&mut speech_encoder, wav_path)?)
        } else if let Some(ref default_wav) = default_voice_path {
            tracing::info!(path = %default_wav.display(), "using default_voice.wav as reference");
            eprintln!("[chatterbox] running speech encoder on default_voice.wav ...");
            Some(Self::run_speech_encoder(&mut speech_encoder, default_wav)?)
        } else {
            eprintln!("[chatterbox] no reference voice, skipping pre-encode");
            None
        };
        eprintln!("[chatterbox] reference voice encoding done");

        Ok(Self {
            config,
            speech_encoder: tokio::sync::Mutex::new(speech_encoder),
            embed_tokens: tokio::sync::Mutex::new(embed_tokens),
            language_model: tokio::sync::Mutex::new(language_model),
            conditional_decoder: tokio::sync::Mutex::new(conditional_decoder),
            tokenizer,
            cached_encoder_output,
            default_voice_path,
        })
    }

    /// Download all required model files from HuggingFace Hub.
    async fn ensure_models(config: &ChatterboxConfig) -> AudioResult<(PathBuf, Option<PathBuf>)> {
        let api = hf_hub::api::sync::Api::new().map_err(|e| AudioError::ModelDownload {
            model_id: config.repo_id.clone(),
            message: format!("failed to create HuggingFace API client: {e}"),
        })?;
        let repo = api.model(config.repo_id.clone());

        let shared_files = [
            "onnx/speech_encoder.onnx",
            "onnx/speech_encoder.onnx_data",
            "onnx/embed_tokens.onnx",
            "onnx/embed_tokens.onnx_data",
            "onnx/conditional_decoder.onnx",
            "onnx/conditional_decoder.onnx_data",
        ];

        for path in &shared_files {
            tracing::debug!(file = %path, "ensuring model file");
            repo.get(path).map_err(|e| AudioError::ModelDownload {
                model_id: config.repo_id.clone(),
                message: format!("failed to download {path}: {e}"),
            })?;
        }

        let lm_graph = format!("onnx/{}", config.variant.language_model_filename());
        let lm_data = format!("onnx/{}", config.variant.language_model_data_filename());
        for path in [&lm_graph, &lm_data] {
            tracing::debug!(file = %path, "ensuring language model file");
            repo.get(path).map_err(|e| AudioError::ModelDownload {
                model_id: config.repo_id.clone(),
                message: format!("failed to download {path}: {e}"),
            })?;
        }

        let tokenizer_file = repo.get("tokenizer.json").map_err(|e| AudioError::ModelDownload {
            model_id: config.repo_id.clone(),
            message: format!("failed to download tokenizer.json: {e}"),
        })?;

        let default_voice_path = match repo.get("default_voice.wav") {
            Ok(path) => {
                tracing::debug!("downloaded default_voice.wav");
                Some(path)
            }
            Err(e) => {
                tracing::warn!("could not download default_voice.wav (non-fatal): {e}");
                None
            }
        };

        let model_dir = tokenizer_file
            .parent()
            .ok_or_else(|| AudioError::ModelDownload {
                model_id: config.repo_id.clone(),
                message: "could not determine model directory".into(),
            })?
            .to_path_buf();

        tracing::info!(dir = %model_dir.display(), "chatterbox models ready");
        Ok((model_dir, default_voice_path))
    }

    fn create_session(onnx_path: &Path, config: &ChatterboxConfig) -> AudioResult<Session> {
        if !onnx_path.exists() {
            return Err(AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("model file not found: {}", onnx_path.display()),
            });
        }

        let mut builder = Session::builder().map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("failed to create session builder: {e}"),
        })?;

        builder = match config.execution_provider {
            OnnxExecutionProvider::Cuda => builder
                .with_execution_providers([
                    ort::execution_providers::CUDAExecutionProvider::default().build(),
                ])
                .map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("CUDA execution provider failed: {e}"),
                })?,
            OnnxExecutionProvider::CoreMl => builder
                .with_execution_providers([
                    ort::execution_providers::CoreMLExecutionProvider::default().build(),
                ])
                .map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("CoreML execution provider failed: {e}"),
                })?,
            OnnxExecutionProvider::DirectMl => {
                tracing::warn!("DirectML not available, falling back to CPU");
                builder
            }
            OnnxExecutionProvider::Cpu => builder,
        };

        builder.commit_from_file(onnx_path).map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("failed to load ONNX model {}: {e}", onnx_path.display()),
        })
    }

    // -----------------------------------------------------------------------
    // Speech Encoder — returns 4 outputs: cond_emb, prompt_token,
    //                  ref_x_vector, prompt_feat
    // -----------------------------------------------------------------------

    fn run_speech_encoder(
        session: &mut Session,
        wav_path: &Path,
    ) -> AudioResult<SpeechEncoderOutput> {
        let wav_data = std::fs::read(wav_path).map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("failed to read reference WAV {}: {e}", wav_path.display()),
        })?;
        let frame = crate::codec::decode(&wav_data, crate::codec::AudioFormat::Wav)?;

        // Convert to f32 normalized [-1, 1]
        let samples: Vec<f32> = frame.samples().iter().map(|&s| s as f32 / 32768.0).collect();

        // Resample to 24kHz if needed
        let samples = if frame.sample_rate != S3GEN_SR {
            simple_resample(&samples, frame.sample_rate, S3GEN_SR)
        } else {
            samples
        };

        // Reference Python: audio_values shape is [1, audio_length] (2D)
        let num_samples = samples.len();

        // Try 2D shape first [1, audio_length] (reference Python uses this),
        // fall back to 3D [1, 1, audio_length] (some ONNX exports use this).
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "audio_values".into());

        let input_tensor = {
            // Speech encoder typically expects 2D [batch, samples] input.
            // Try 2D first; if the model needs 3D [batch, channels, samples],
            // callers can adjust.
            Value::from_array(([1i64, num_samples as i64], samples))
        }
        .map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("failed to create speech encoder input tensor: {e}"),
        })?;

        let outputs =
            session.run(ort::inputs![input_name.as_str() => input_tensor]).map_err(|e| {
                AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("speech encoder inference failed: {e}"),
                }
            })?;

        let num_outputs = outputs.len();
        tracing::debug!(num_outputs, "speech encoder produced outputs");

        if num_outputs >= 4 {
            // Reference model: 4 outputs — cond_emb, prompt_token, ref_x_vector, prompt_feat
            let (cond_shape, cond_data) =
                outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to extract cond_emb: {e}"),
                })?;
            let (pt_shape, pt_data) =
                outputs[1].try_extract_tensor::<i64>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to extract prompt_token: {e}"),
                })?;
            let (xv_shape, xv_data) =
                outputs[2].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to extract ref_x_vector: {e}"),
                })?;
            let (pf_shape, pf_data) =
                outputs[3].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to extract prompt_feat: {e}"),
                })?;

            tracing::debug!(
                cond_shape = ?cond_shape.iter().collect::<Vec<_>>(),
                prompt_token_shape = ?pt_shape.iter().collect::<Vec<_>>(),
                xvector_shape = ?xv_shape.iter().collect::<Vec<_>>(),
                prompt_feat_shape = ?pf_shape.iter().collect::<Vec<_>>(),
                "speech encoder output shapes"
            );

            Ok(SpeechEncoderOutput {
                cond_emb: (cond_shape.iter().copied().collect(), cond_data.to_vec()),
                prompt_token: (pt_shape.iter().copied().collect(), pt_data.to_vec()),
                ref_x_vector: (xv_shape.iter().copied().collect(), xv_data.to_vec()),
                prompt_feat: (pf_shape.iter().copied().collect(), pf_data.to_vec()),
            })
        } else {
            // Fallback: single output model (last_hidden_state) — older export
            let (shape, data) =
                outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to extract speech encoder output: {e}"),
                })?;
            let shape_vec: Vec<i64> = shape.iter().copied().collect();

            tracing::warn!(
                num_outputs,
                shape = ?shape_vec,
                "speech encoder has fewer than 4 outputs — using fallback single-output mode"
            );

            // Use the single output as cond_emb, synthesize empty placeholders for the rest
            Ok(SpeechEncoderOutput {
                cond_emb: (shape_vec, data.to_vec()),
                prompt_token: (vec![1, 0], vec![]),
                ref_x_vector: (vec![1, 1, 1], vec![0.0]),
                prompt_feat: (vec![1, 1, 1], vec![0.0]),
            })
        }
    }

    async fn get_encoder_output(
        &self,
        voice_wav: Option<&Path>,
    ) -> AudioResult<SpeechEncoderOutput> {
        if voice_wav.is_none() {
            if let Some(ref out) = self.cached_encoder_output {
                return Ok(SpeechEncoderOutput {
                    cond_emb: out.cond_emb.clone(),
                    prompt_token: out.prompt_token.clone(),
                    ref_x_vector: out.ref_x_vector.clone(),
                    prompt_feat: out.prompt_feat.clone(),
                });
            }
        }

        let wav_path = voice_wav
            .or(self.config.reference_wav.as_deref())
            .or(self.default_voice_path.as_deref())
            .ok_or_else(|| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: "no reference WAV provided for voice cloning. \
                    Set `reference_wav` in ChatterboxConfig or pass a voice WAV path."
                    .into(),
            })?;

        let mut session = self.speech_encoder.lock().await;
        Self::run_speech_encoder(&mut session, wav_path)
    }

    // -----------------------------------------------------------------------
    // Embed Tokens — takes input_ids, position_ids, exaggeration
    // -----------------------------------------------------------------------

    fn run_embed_tokens(
        session: &mut Session,
        token_ids: &[i64],
        position_ids: &[i64],
        exaggeration: f32,
    ) -> AudioResult<(Vec<i64>, Vec<f32>)> {
        let seq_len = token_ids.len() as i64;

        let ids_tensor = Value::from_array(([1i64, seq_len], token_ids.to_vec())).map_err(|e| {
            AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to create input_ids tensor: {e}"),
            }
        })?;

        let pos_tensor =
            Value::from_array(([1i64, seq_len], position_ids.to_vec())).map_err(|e| {
                AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to create position_ids tensor: {e}"),
                }
            })?;

        let exag_tensor =
            Value::from_array(([1i64], vec![exaggeration])).map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to create exaggeration tensor: {e}"),
            })?;

        // Check which inputs the model actually expects
        let input_names: Vec<String> =
            session.inputs().iter().map(|i| i.name().to_string()).collect();

        let outputs = if input_names.contains(&"exaggeration".to_string())
            && input_names.contains(&"position_ids".to_string())
        {
            session.run(ort::inputs![
                "input_ids" => ids_tensor,
                "position_ids" => pos_tensor,
                "exaggeration" => exag_tensor
            ])
        } else if input_names.contains(&"position_ids".to_string()) {
            session.run(ort::inputs![
                "input_ids" => ids_tensor,
                "position_ids" => pos_tensor
            ])
        } else {
            session.run(ort::inputs!["input_ids" => ids_tensor])
        }
        .map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("embed_tokens inference failed: {e}"),
        })?;

        let (shape, data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to extract token embeddings: {e}"),
            })?;

        Ok((shape.iter().copied().collect(), data.to_vec()))
    }

    // -----------------------------------------------------------------------
    // Autoregressive generation — follows reference Python implementation
    // -----------------------------------------------------------------------

    fn generate_speech_tokens(
        &self,
        lm_session: &mut Session,
        embed_session: &mut Session,
        encoder_output: &SpeechEncoderOutput,
        text: &str,
    ) -> AudioResult<Vec<i64>> {
        // Tokenize text
        let encoding = self.tokenizer.encode(text, false).map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("tokenization failed: {e}"),
        })?;
        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();

        if input_ids.is_empty() {
            return Err(AudioError::Tts {
                provider: "Chatterbox".into(),
                message: "tokenization produced no tokens".into(),
            });
        }

        // Compute position_ids: for speech tokens (>= START_SPEECH_TOKEN) use 0,
        // otherwise use sequential positions starting from -1 (matching reference Python:
        // np.arange(seq_len) - 1). This offset is critical for correct LM behavior.
        let position_ids: Vec<i64> = input_ids
            .iter()
            .enumerate()
            .map(|(i, &id)| if id >= START_SPEECH_TOKEN { 0 } else { i as i64 - 1 })
            .collect();

        // Initial token: START_SPEECH_TOKEN
        let mut generate_tokens: Vec<i64> = vec![START_SPEECH_TOKEN];

        // First iteration: embed the START_SPEECH_TOKEN with the text tokens' context
        let (_, first_embeds) = Self::run_embed_tokens(
            embed_session,
            &[START_SPEECH_TOKEN],
            &position_ids[..1], // position 0 for the start token
            self.config.exaggeration,
        )?;

        // Wait — the reference Python does it differently. Let me re-read:
        // It embeds input_ids (text tokens) first, then on i==0 concatenates
        // cond_emb with the text embeddings. Let me follow that exactly.

        // Actually, re-reading the reference more carefully:
        // - ort_embed_tokens_inputs starts with the full text input_ids
        // - On i==0: embeds text, prepends cond_emb, runs LM with full prefill
        // - On i>0: embeds just the new token, runs LM with KV-cache
        // Let me redo this properly.

        drop(first_embeds); // discard, we'll redo

        // Step 1: Embed the full text input_ids
        let (_, text_embeds) = Self::run_embed_tokens(
            embed_session,
            &input_ids,
            &position_ids,
            self.config.exaggeration,
        )?;

        // Step 2: Concatenate cond_emb (from speech encoder) with text embeddings
        let cond_emb = &encoder_output.cond_emb.1;
        let cond_shape = &encoder_output.cond_emb.0;

        // cond_emb shape: [1, cond_seq_len, hidden_dim]
        // text_embeds shape: [1, text_seq_len, hidden_dim]
        let hidden_dim = if cond_shape.len() == 3 {
            cond_shape[2] as usize
        } else {
            // Infer from text embeds
            text_embeds.len() / input_ids.len()
        };

        let cond_seq_len = cond_emb.len() / hidden_dim;
        let text_seq_len = input_ids.len();
        let total_seq_len = cond_seq_len + text_seq_len;

        eprintln!(
            "[chatterbox] input_ids ({} tokens): {:?}",
            input_ids.len(),
            &input_ids[..input_ids.len().min(20)]
        );
        eprintln!("[chatterbox] position_ids: {:?}", &position_ids[..position_ids.len().min(20)]);
        eprintln!(
            "[chatterbox] cond_emb shape: {:?}, hidden_dim={hidden_dim}, cond_seq_len={cond_seq_len}",
            cond_shape
        );
        eprintln!(
            "[chatterbox] total_seq_len={total_seq_len} (cond={cond_seq_len} + text={text_seq_len})"
        );

        let mut prefill_embeds = Vec::with_capacity(total_seq_len * hidden_dim);
        prefill_embeds.extend_from_slice(cond_emb);
        prefill_embeds.extend_from_slice(&text_embeds);

        tracing::debug!(
            cond_seq_len,
            text_seq_len,
            total_seq_len,
            hidden_dim,
            "prefill dimensions"
        );

        // Step 3: Prefill — run LM with concatenated embeddings
        let first_output = self.run_lm_step(
            lm_session,
            &prefill_embeds,
            total_seq_len,
            hidden_dim,
            None,          // no KV-cache yet
            total_seq_len, // attention_mask length
        )?;

        eprintln!("[chatterbox] prefill logits len={}, top5: {:?}", first_output.logits.len(), {
            let mut indexed: Vec<(usize, f32)> =
                first_output.logits.iter().copied().enumerate().collect();
            indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            indexed.iter().take(5).map(|(i, v)| (*i, *v)).collect::<Vec<_>>()
        });

        let mut next_token = self.sample_token(&first_output.logits, &generate_tokens);
        eprintln!("[chatterbox] prefill → next_token={next_token} (STOP={})", STOP_SPEECH_TOKEN);
        eprintln!(
            "[chatterbox] prefill KV cache: layer0 key shape={:?}, value shape={:?}",
            first_output.kv_cache.keys[0].shape, first_output.kv_cache.values[0].shape,
        );
        generate_tokens.push(next_token);
        let mut kv_cache = first_output.kv_cache;
        let mut attn_len = total_seq_len; // tracks attention_mask length

        // Step 4: Autoregressive decode loop
        for step in 1..self.config.max_new_tokens {
            if next_token == STOP_SPEECH_TOKEN {
                tracing::debug!(step, "stopping: STOP_SPEECH_TOKEN reached");
                break;
            }

            // Embed the new token
            let step_position_ids = vec![step as i64];
            let (_, token_emb) = Self::run_embed_tokens(
                embed_session,
                &[next_token],
                &step_position_ids,
                self.config.exaggeration,
            )?;

            attn_len += 1;

            let output =
                self.run_lm_step(lm_session, &token_emb, 1, hidden_dim, Some(&kv_cache), attn_len)?;

            next_token = self.sample_token(&output.logits, &generate_tokens);
            if step <= 10 {
                eprintln!(
                    "[chatterbox] step {step} → next_token={next_token}, kv_shape={:?}, attn_len={attn_len}",
                    output.kv_cache.keys[0].shape
                );
            }
            generate_tokens.push(next_token);
            kv_cache = output.kv_cache;

            if step % 100 == 0 {
                tracing::debug!(step, tokens = generate_tokens.len(), "generating...");
            }
        }

        tracing::info!(tokens = generate_tokens.len(), "speech token generation complete");

        // Remove START and STOP tokens, prepend prompt_token from speech encoder
        let mut speech_tokens: Vec<i64> = generate_tokens
            .iter()
            .copied()
            .filter(|&t| t != START_SPEECH_TOKEN && t != STOP_SPEECH_TOKEN)
            .collect();

        // Prepend prompt_token from speech encoder (reference: speech_tokens = concat(prompt_token, speech_tokens))
        let prompt_tokens = &encoder_output.prompt_token.1;
        if !prompt_tokens.is_empty() {
            let mut combined = prompt_tokens.clone();
            combined.extend_from_slice(&speech_tokens);
            speech_tokens = combined;
        }

        Ok(speech_tokens)
    }

    fn sample_token(&self, logits: &[f32], generated: &[i64]) -> i64 {
        let mut logits = logits.to_vec();

        // Apply repetition penalty (reference Python implementation)
        if self.config.repetition_penalty != 1.0 {
            for &token in generated {
                let idx = token as usize;
                if idx < logits.len() {
                    if logits[idx] < 0.0 {
                        logits[idx] *= self.config.repetition_penalty;
                    } else {
                        logits[idx] /= self.config.repetition_penalty;
                    }
                }
            }
        }

        // Greedy argmax
        logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(idx, _)| idx as i64)
            .unwrap_or(STOP_SPEECH_TOKEN)
    }

    // -----------------------------------------------------------------------
    // Language model step — with attention_mask
    // -----------------------------------------------------------------------

    fn run_lm_step(
        &self,
        session: &mut Session,
        inputs_embeds: &[f32],
        seq_len: usize,
        hidden_dim: usize,
        kv_cache: Option<&KvCache>,
        attn_mask_len: usize,
    ) -> AudioResult<LmStepOutput> {
        let embed_tensor =
            Value::from_array(([1i64, seq_len as i64, hidden_dim as i64], inputs_embeds.to_vec()))
                .map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to create inputs_embeds tensor: {e}"),
                })?;

        // attention_mask: all ones, length = total sequence seen so far
        let attn_mask: Vec<i64> = vec![1i64; attn_mask_len];
        let attn_tensor =
            Value::from_array(([1i64, attn_mask_len as i64], attn_mask)).map_err(|e| {
                AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to create attention_mask tensor: {e}"),
                }
            })?;

        let mut input_values: Vec<(String, Value)> = vec![
            ("inputs_embeds".into(), embed_tensor.into()),
            ("attention_mask".into(), attn_tensor.into()),
        ];

        // KV-cache: GroupQueryAttention requires past_key_values inputs even
        // on the first prefill step. When no cache exists yet, provide empty
        // tensors with shape [1, NUM_KV_HEADS, 0, HEAD_DIM].
        // NOTE: ort's Value::from_array rejects zero-length dimensions, so we
        // use Tensor::new (allocator-based) for the empty initial cache.
        let allocator =
            Allocator::new(session, MemoryInfo::default()).map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to create allocator: {e}"),
            })?;

        for layer in 0..NUM_LAYERS {
            let key_name = format!("past_key_values.{layer}.key");
            let value_name = format!("past_key_values.{layer}.value");

            if let Some(cache) = kv_cache {
                let ke = &cache.keys[layer];
                let ve = &cache.values[layer];

                let kt = Value::from_array((ke.shape.clone(), ke.data.clone())).map_err(|e| {
                    AudioError::Tts {
                        provider: "Chatterbox".into(),
                        message: format!("KV key tensor layer {layer}: {e}"),
                    }
                })?;
                let vt = Value::from_array((ve.shape.clone(), ve.data.clone())).map_err(|e| {
                    AudioError::Tts {
                        provider: "Chatterbox".into(),
                        message: format!("KV value tensor layer {layer}: {e}"),
                    }
                })?;
                input_values.push((key_name, kt.into()));
                input_values.push((value_name, vt.into()));
            } else {
                // Empty KV cache: seq_len dimension is 0
                let empty_shape = [1usize, NUM_KV_HEADS, 0, HEAD_DIM];
                let kt =
                    Tensor::<f32>::new(&allocator, empty_shape).map_err(|e| AudioError::Tts {
                        provider: "Chatterbox".into(),
                        message: format!("KV key tensor layer {layer}: {e}"),
                    })?;
                let vt =
                    Tensor::<f32>::new(&allocator, empty_shape).map_err(|e| AudioError::Tts {
                        provider: "Chatterbox".into(),
                        message: format!("KV value tensor layer {layer}: {e}"),
                    })?;
                input_values.push((key_name, kt.into_dyn()));
                input_values.push((value_name, vt.into_dyn()));
            }
        }

        let outputs = session.run(input_values).map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("language model inference failed: {e}"),
        })?;

        // Extract logits — [1, seq_len, vocab_size]
        let (_, logits_data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to extract logits: {e}"),
            })?;

        let vocab_size = logits_data.len() / seq_len;
        let last_logits = if seq_len > 1 {
            logits_data[(seq_len - 1) * vocab_size..].to_vec()
        } else {
            logits_data.to_vec()
        };

        // Extract KV-cache from present.N.key/value outputs (indices 1..=2*NUM_LAYERS)
        let mut keys = Vec::with_capacity(NUM_LAYERS);
        let mut values = Vec::with_capacity(NUM_LAYERS);
        for layer in 0..NUM_LAYERS {
            let key_idx = 1 + layer * 2;
            let val_idx = 2 + layer * 2;

            let (ks, kd) =
                outputs[key_idx].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("KV key extract layer {layer}: {e}"),
                })?;
            let (vs, vd) =
                outputs[val_idx].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("KV value extract layer {layer}: {e}"),
                })?;

            keys.push(KvEntry { shape: ks.iter().copied().collect(), data: kd.to_vec() });
            values.push(KvEntry { shape: vs.iter().copied().collect(), data: vd.to_vec() });
        }

        Ok(LmStepOutput { logits: last_logits, kv_cache: KvCache { keys, values } })
    }

    // -----------------------------------------------------------------------
    // Conditional Decoder — speech tokens + speaker info → audio waveform
    // Reference: 3 inputs — speech_tokens, speaker_embeddings, speaker_features
    // -----------------------------------------------------------------------

    fn run_conditional_decoder(
        session: &mut Session,
        speech_tokens: &[i64],
        encoder_output: &SpeechEncoderOutput,
    ) -> AudioResult<Vec<f32>> {
        let seq_len = speech_tokens.len() as i64;
        let tokens_tensor =
            Value::from_array(([1i64, seq_len], speech_tokens.to_vec())).map_err(|e| {
                AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to create decoder tokens tensor: {e}"),
                }
            })?;

        // Discover actual input names from the model
        let input_names: Vec<String> =
            session.inputs().iter().map(|i| i.name().to_string()).collect();

        tracing::debug!(input_names = ?input_names, "conditional decoder input names");

        // Build inputs based on what the model expects
        let ref_xv = &encoder_output.ref_x_vector;
        let prompt_feat = &encoder_output.prompt_feat;

        let xv_tensor = Value::from_array((ref_xv.0.clone(), ref_xv.1.clone())).map_err(|e| {
            AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to create speaker_embeddings tensor: {e}"),
            }
        })?;

        let pf_tensor =
            Value::from_array((prompt_feat.0.clone(), prompt_feat.1.clone())).map_err(|e| {
                AudioError::Tts {
                    provider: "Chatterbox".into(),
                    message: format!("failed to create speaker_features tensor: {e}"),
                }
            })?;

        // The reference Python uses: speech_tokens, speaker_embeddings, speaker_features
        // But the ONNX model may use different names (input_ids, encoder_hidden_states, etc.)
        // We match by position if names don't match exactly.
        let outputs = if input_names.len() >= 3 {
            // Use actual model input names
            let token_name = &input_names[0];
            let xv_name = &input_names[1];
            let pf_name = &input_names[2];

            tracing::debug!(
                token_name,
                xv_name,
                pf_name,
                "using model's actual input names for conditional decoder"
            );

            session.run(vec![
                (token_name.clone(), SessionInputValue::from(tokens_tensor)),
                (xv_name.clone(), SessionInputValue::from(xv_tensor)),
                (pf_name.clone(), SessionInputValue::from(pf_tensor)),
            ])
        } else if input_names.len() == 2 {
            // Fallback: 2-input model (older export with just tokens + encoder_hidden_states)
            let token_name = &input_names[0];
            let enc_name = &input_names[1];

            // Use cond_emb as the encoder hidden states
            let cond = &encoder_output.cond_emb;
            let cond_tensor =
                Value::from_array((cond.0.clone(), cond.1.clone())).map_err(|e| {
                    AudioError::Tts {
                        provider: "Chatterbox".into(),
                        message: format!("failed to create encoder_hidden_states tensor: {e}"),
                    }
                })?;

            session.run(vec![
                (token_name.clone(), SessionInputValue::from(tokens_tensor)),
                (enc_name.clone(), SessionInputValue::from(cond_tensor)),
            ])
        } else {
            return Err(AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!(
                    "conditional decoder has unexpected number of inputs: {}, expected 2 or 3. Names: {input_names:?}",
                    input_names.len()
                ),
            });
        }
        .map_err(|e| AudioError::Tts {
            provider: "Chatterbox".into(),
            message: format!("conditional decoder inference failed: {e}"),
        })?;

        let (shape, audio_data) =
            outputs[0].try_extract_tensor::<f32>().map_err(|e| AudioError::Tts {
                provider: "Chatterbox".into(),
                message: format!("failed to extract decoder audio output: {e}"),
            })?;

        tracing::debug!(
            shape = ?shape.iter().collect::<Vec<_>>(),
            samples = audio_data.len(),
            "conditional decoder output"
        );

        Ok(audio_data.to_vec())
    }
}

// ---------------------------------------------------------------------------
// TtsProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl TtsProvider for ChatterboxTtsProvider {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let voice_wav = if !request.voice.is_empty() && Path::new(&request.voice).exists() {
            Some(PathBuf::from(&request.voice))
        } else {
            None
        };

        let encoder_output = self.get_encoder_output(voice_wav.as_deref()).await?;

        eprintln!(
            "[chatterbox] encoder output: cond_emb shape={:?} len={}, prompt_token shape={:?} len={}, xvec shape={:?}, pfeat shape={:?}",
            encoder_output.cond_emb.0,
            encoder_output.cond_emb.1.len(),
            encoder_output.prompt_token.0,
            encoder_output.prompt_token.1.len(),
            encoder_output.ref_x_vector.0,
            encoder_output.prompt_feat.0,
        );

        tracing::info!(text_len = request.text.len(), "starting chatterbox synthesis");

        let mut lm_session = self.language_model.lock().await;
        let mut embed_session = self.embed_tokens.lock().await;

        let speech_tokens = self.generate_speech_tokens(
            &mut lm_session,
            &mut embed_session,
            &encoder_output,
            &request.text,
        )?;

        drop(lm_session);
        drop(embed_session);

        tracing::info!(speech_tokens = speech_tokens.len(), "decoding speech tokens to audio");

        let mut decoder_session = self.conditional_decoder.lock().await;
        let audio_f32 =
            Self::run_conditional_decoder(&mut decoder_session, &speech_tokens, &encoder_output)?;

        tracing::info!(audio_samples = audio_f32.len(), "audio decoded");

        let sample_bytes: Vec<u8> = audio_f32
            .iter()
            .flat_map(|s| {
                let clamped = s.clamp(-1.0, 1.0);
                let pcm = (clamped * 32767.0) as i16;
                pcm.to_le_bytes()
            })
            .collect();

        Ok(AudioFrame::new(sample_bytes, S3GEN_SR, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        let full_frame = self.synthesize(request).await?;
        let chunk_bytes = (S3GEN_SR as usize * 100 / 1000) * 2; // 100ms chunks

        let stream = async_stream::stream! {
            let data = full_frame.data.clone();
            let mut offset = 0;
            while offset < data.len() {
                let end = (offset + chunk_bytes).min(data.len());
                let chunk = data.slice(offset..end);
                yield Ok(AudioFrame::new(chunk, S3GEN_SR, 1));
                offset = end;
            }
        };
        Ok(Box::pin(stream))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// Utility
// ---------------------------------------------------------------------------

fn simple_resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (samples.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f64 * ratio;
        let idx0 = src_idx as usize;
        let frac = (src_idx - idx0 as f64) as f32;
        let s0 = samples.get(idx0).copied().unwrap_or(0.0);
        let s1 = samples.get(idx0 + 1).copied().unwrap_or(s0);
        output.push(s0 + frac * (s1 - s0));
    }
    output
}
