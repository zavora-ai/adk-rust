//! ONNX-based speech-to-text (STT) inference module.
//!
//! Provides [`OnnxSttProvider`] implementing the [`SttProvider`](crate::traits::SttProvider)
//! trait with three ASR backends:
//!
//! - **Whisper** — OpenAI's Whisper (tiny through large-v3-turbo)
//! - **Distil-Whisper** — HuggingFace distilled variants (2–6× faster)
//! - **Moonshine** — Useful Sensors' lightweight edge-optimized ASR
//!
//! All inference runs on-device via ONNX Runtime with no API keys required.
//! Models auto-download from HuggingFace Hub on first use via
//! [`LocalModelRegistry`](crate::registry::LocalModelRegistry).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_audio::{OnnxSttConfig, SttBackend, WhisperModelSize};
//!
//! let config = OnnxSttConfig::builder()
//!     .stt_backend(SttBackend::Whisper)
//!     .model_size(WhisperModelSize::Small)
//!     .beam_size(3)
//!     .language("en")
//!     .build()?;
//!
//! assert_eq!(config.model_id(), "onnx-community/whisper-small");
//! ```

mod config;

#[cfg(any(feature = "whisper-onnx", feature = "distil-whisper"))]
#[allow(unused)]
pub(crate) mod mel;

#[cfg(any(feature = "whisper-onnx", feature = "distil-whisper"))]
#[allow(unused)]
pub(crate) mod whisper;

#[cfg(feature = "moonshine")]
#[allow(unused)]
pub(crate) mod moonshine;

pub use config::{
    DistilWhisperVariant, MoonshineVariant, OnnxSttConfig, OnnxSttConfigBuilder, SttBackend,
    WhisperModelSize,
};

// ── OnnxSttProvider ────────────────────────────────────────────────────

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use ort::session::Session;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{SttOptions, SttProvider, Transcript};

use super::execution_provider::OnnxExecutionProvider;

/// ONNX-based speech-to-text provider supporting Whisper, Distil-Whisper,
/// and Moonshine.
///
/// Holds encoder and decoder ONNX sessions, a tokenizer for decoding token
/// IDs back to text, and the model configuration. Sessions are wrapped in
/// [`tokio::sync::Mutex`] for safe concurrent access.
///
/// # Example
///
/// ```rust,ignore
/// use adk_audio::{OnnxSttProvider, OnnxSttConfig, SttBackend, WhisperModelSize};
///
/// let config = OnnxSttConfig::builder()
///     .stt_backend(SttBackend::Whisper)
///     .model_size(WhisperModelSize::Base)
///     .build()?;
///
/// let provider = OnnxSttProvider::new(config, &LocalModelRegistry::default()).await?;
/// ```
#[allow(dead_code)] // Fields used by SttProvider impl in task 8.2
pub struct OnnxSttProvider {
    /// STT configuration (backend, model variant, beam size, etc.).
    config: OnnxSttConfig,
    /// ONNX encoder session (mel → hidden states for Whisper, PCM → hidden states for Moonshine).
    encoder_session: tokio::sync::Mutex<Session>,
    /// ONNX decoder session (hidden states + token IDs → logits).
    decoder_session: tokio::sync::Mutex<Session>,
    /// HuggingFace tokenizer for decoding token IDs to text.
    tokenizer: tokenizers::Tokenizer,
    /// Local directory containing the model files.
    model_dir: PathBuf,
}

#[allow(dead_code)] // Methods used by SttProvider impl in task 8.2
impl OnnxSttProvider {
    /// Load an ONNX STT provider from configuration, auto-downloading the
    /// model from HuggingFace Hub if not already cached.
    ///
    /// # Arguments
    ///
    /// * `config` — STT configuration specifying backend, model variant, and EP
    /// * `registry` — Model registry for downloading and caching model weights
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::ModelDownload`] if the model cannot be downloaded,
    /// [`AudioError::Stt`] if ONNX sessions or the tokenizer fail to load.
    pub async fn new(config: OnnxSttConfig, registry: &LocalModelRegistry) -> AudioResult<Self> {
        let model_id = config.model_id();
        let model_dir = registry.get_or_download(model_id).await?;

        let (encoder_path, decoder_path) = Self::resolve_model_paths(&model_dir, &config)?;

        let encoder_session =
            Self::create_session(&encoder_path, &config.execution_provider, "encoder")?;
        let decoder_session =
            Self::create_session(&decoder_path, &config.execution_provider, "decoder")?;

        let tokenizer = Self::load_tokenizer(&model_dir)?;

        Ok(Self {
            config,
            encoder_session: tokio::sync::Mutex::new(encoder_session),
            decoder_session: tokio::sync::Mutex::new(decoder_session),
            tokenizer,
            model_dir,
        })
    }

    /// Convenience constructor: load default Whisper base model with
    /// auto-detected execution provider.
    ///
    /// Equivalent to:
    /// ```rust,ignore
    /// let config = OnnxSttConfig::builder()
    ///     .stt_backend(SttBackend::Whisper)
    ///     .model_size(WhisperModelSize::Base)
    ///     .build()?;
    /// OnnxSttProvider::new(config, &LocalModelRegistry::default()).await
    /// ```
    pub async fn default_whisper() -> AudioResult<Self> {
        let config = OnnxSttConfig::builder().stt_backend(SttBackend::Whisper).build()?;
        let registry = LocalModelRegistry::default();
        Self::new(config, &registry).await
    }

    /// Handle a zero-length [`AudioFrame`] by returning an empty transcript
    /// with confidence 0.0, without running any inference.
    pub(crate) fn empty_transcript() -> Transcript {
        Transcript {
            text: String::new(),
            confidence: 0.0,
            language_detected: None,
            words: Vec::new(),
            ..Default::default()
        }
    }

    /// Get a reference to the provider's configuration.
    pub fn config(&self) -> &OnnxSttConfig {
        &self.config
    }

    /// Get a reference to the model directory path.
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// Get a reference to the tokenizer.
    pub fn tokenizer(&self) -> &tokenizers::Tokenizer {
        &self.tokenizer
    }

    /// Lock and return a mutable reference to the encoder session.
    pub(crate) async fn encoder_session(&self) -> tokio::sync::MutexGuard<'_, Session> {
        self.encoder_session.lock().await
    }

    /// Lock and return a mutable reference to the decoder session.
    pub(crate) async fn decoder_session(&self) -> tokio::sync::MutexGuard<'_, Session> {
        self.decoder_session.lock().await
    }

    /// Check if the input audio frame is zero-length.
    pub(crate) fn is_empty_audio(audio: &AudioFrame) -> bool {
        audio.data.is_empty()
    }

    // ── Private helpers ────────────────────────────────────────────────

    /// Resolve encoder and decoder ONNX file paths based on the backend.
    ///
    /// - Whisper / Distil-Whisper: `encoder_model.onnx` + `decoder_model_merged.onnx`
    /// - Moonshine: `encoder_model.onnx` + `decoder_model.onnx`
    ///
    /// Many HuggingFace ONNX repos (e.g. `onnx-community/whisper-base`) store
    /// model files inside an `onnx/` subdirectory and only ship quantized
    /// variants (fp16, int8, q4, etc.) instead of a plain `encoder_model.onnx`.
    /// This method searches both the root and the `onnx/` subdirectory, trying
    /// the unquantized filename first, then falling back through quantized
    /// variants in decreasing precision order.
    fn resolve_model_paths(
        model_dir: &Path,
        config: &OnnxSttConfig,
    ) -> AudioResult<(PathBuf, PathBuf)> {
        // Encoder candidates in preference order (highest precision first)
        let encoder_candidates = [
            "encoder_model.onnx",
            "encoder_model_fp16.onnx",
            "encoder_model_int8.onnx",
            "encoder_model_quantized.onnx",
            "encoder_model_uint8.onnx",
            "encoder_model_q4.onnx",
        ];

        // Decoder candidates depend on backend
        let decoder_candidates: &[&str] = match config.backend {
            SttBackend::Whisper | SttBackend::DistilWhisper => &[
                "decoder_model_merged.onnx",
                "decoder_model_merged_fp16.onnx",
                "decoder_model_merged_int8.onnx",
                "decoder_model_merged_quantized.onnx",
                "decoder_model_merged_uint8.onnx",
                "decoder_model_merged_q4.onnx",
                // Fall back to non-merged decoder if merged not available
                "decoder_model.onnx",
                "decoder_model_fp16.onnx",
                "decoder_model_int8.onnx",
                "decoder_model_quantized.onnx",
                "decoder_model_uint8.onnx",
                "decoder_model_q4.onnx",
            ],
            SttBackend::Moonshine => &[
                "decoder_model.onnx",
                "decoder_model_fp16.onnx",
                "decoder_model_int8.onnx",
                "decoder_model_quantized.onnx",
                "decoder_model_uint8.onnx",
                "decoder_model_q4.onnx",
            ],
        };

        // Search directories: model_dir root, then onnx/ subdirectory
        let search_dirs: Vec<PathBuf> = {
            let mut dirs = vec![model_dir.to_path_buf()];
            let onnx_sub = model_dir.join("onnx");
            if onnx_sub.is_dir() {
                dirs.push(onnx_sub);
            }
            dirs
        };

        let encoder_path = Self::find_first_existing(&search_dirs, &encoder_candidates);
        let decoder_path = Self::find_first_existing(&search_dirs, decoder_candidates);

        let backend_name = Self::backend_name(&config.backend);

        let encoder_path = encoder_path.ok_or_else(|| AudioError::Stt {
            provider: format!("ONNX/{backend_name}"),
            message: format!(
                "encoder model not found in {} (also checked onnx/ subdirectory). \
                 Looked for: {}. Ensure the model directory is complete.",
                model_dir.display(),
                encoder_candidates.join(", "),
            ),
        })?;

        let decoder_path = decoder_path.ok_or_else(|| AudioError::Stt {
            provider: format!("ONNX/{backend_name}"),
            message: format!(
                "decoder model not found in {} (also checked onnx/ subdirectory). \
                 Looked for: {}. Ensure the model directory is complete.",
                model_dir.display(),
                decoder_candidates.join(", "),
            ),
        })?;

        tracing::info!(
            encoder = %encoder_path.display(),
            decoder = %decoder_path.display(),
            "resolved ONNX model paths"
        );

        Ok((encoder_path, decoder_path))
    }

    /// Search through directories and candidate filenames, returning the first
    /// existing file path found.
    fn find_first_existing(dirs: &[PathBuf], candidates: &[&str]) -> Option<PathBuf> {
        for dir in dirs {
            for candidate in candidates {
                let path = dir.join(candidate);
                if path.exists() {
                    return Some(path);
                }
            }
        }
        None
    }

    /// Create an ONNX session with execution provider fallback.
    ///
    /// Tries the requested execution provider first. If it fails, logs a
    /// warning via `tracing::warn` and falls back to CPU.
    fn create_session(
        onnx_path: &Path,
        ep: &OnnxExecutionProvider,
        session_name: &str,
    ) -> AudioResult<Session> {
        let builder = Session::builder().map_err(|e| AudioError::Stt {
            provider: "ONNX".into(),
            message: format!("failed to create {session_name} session builder: {e}"),
        })?;

        let builder = Self::apply_execution_provider(builder, ep, session_name);

        builder.commit_from_file(onnx_path).map_err(|e| AudioError::Stt {
            provider: "ONNX".into(),
            message: format!(
                "failed to load {session_name} ONNX model at {}: {e}",
                onnx_path.display()
            ),
        })
    }

    /// Apply the requested execution provider to a session builder.
    ///
    /// On failure, logs a warning and returns the builder unchanged (CPU fallback).
    fn apply_execution_provider(
        builder: ort::session::builder::SessionBuilder,
        ep: &OnnxExecutionProvider,
        session_name: &str,
    ) -> ort::session::builder::SessionBuilder {
        match ep {
            OnnxExecutionProvider::Cuda => {
                match builder.with_execution_providers([
                    ort::execution_providers::CUDAExecutionProvider::default().build(),
                ]) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "CUDA not available for {session_name}, falling back to CPU: {e}"
                        );
                        Session::builder().unwrap_or_else(|_| unreachable!())
                    }
                }
            }
            OnnxExecutionProvider::CoreMl => {
                match builder.with_execution_providers([
                    ort::execution_providers::CoreMLExecutionProvider::default().build(),
                ]) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "CoreML not available for {session_name}, falling back to CPU: {e}"
                        );
                        Session::builder().unwrap_or_else(|_| unreachable!())
                    }
                }
            }
            OnnxExecutionProvider::DirectMl => {
                tracing::warn!("DirectML not available for {session_name}, falling back to CPU");
                builder
            }
            OnnxExecutionProvider::Cpu => builder,
        }
    }

    /// Load the HuggingFace tokenizer from `tokenizer.json` in the model directory.
    ///
    /// If `tokenizer.json` is not found directly in `model_dir`, also checks
    /// the parent directory (handles the case where `model_dir` points to an
    /// `onnx/` subdirectory within the HuggingFace cache snapshot).
    fn load_tokenizer(model_dir: &Path) -> AudioResult<tokenizers::Tokenizer> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        let tokenizer_path = if tokenizer_path.exists() {
            tokenizer_path
        } else if let Some(parent) = model_dir.parent() {
            let parent_path = parent.join("tokenizer.json");
            if parent_path.exists() {
                tracing::debug!(
                    "tokenizer.json not in {}, found in parent {}",
                    model_dir.display(),
                    parent.display()
                );
                parent_path
            } else {
                return Err(AudioError::Stt {
                    provider: "ONNX".into(),
                    message: format!(
                        "tokenizer.json not found at {} or parent directory. \
                         Ensure the model directory is complete.",
                        model_dir.display()
                    ),
                });
            }
        } else {
            return Err(AudioError::Stt {
                provider: "ONNX".into(),
                message: format!(
                    "tokenizer.json not found at {}. Ensure the model directory is complete.",
                    tokenizer_path.display()
                ),
            });
        };

        tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| AudioError::Stt {
            provider: "ONNX".into(),
            message: format!("failed to load tokenizer from {}: {e}", tokenizer_path.display()),
        })
    }

    /// Human-readable name for the backend (used in error messages).
    fn backend_name(backend: &SttBackend) -> &'static str {
        match backend {
            SttBackend::Whisper => "Whisper",
            SttBackend::DistilWhisper => "DistilWhisper",
            SttBackend::Moonshine => "Moonshine",
        }
    }
}

// ── SttProvider trait implementation ───────────────────────────────────

/// Whisper/Distil-Whisper segment duration in seconds.
const WHISPER_SEGMENT_SECS: u32 = 30;

/// Target sample rate for all STT backends.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Number of PCM16 samples in one 30-second Whisper segment at 16 kHz mono.
const WHISPER_SEGMENT_SAMPLES: usize = WHISPER_SEGMENT_SECS as usize * TARGET_SAMPLE_RATE as usize;

/// Preprocess an [`AudioFrame`] into 16 kHz mono f32 samples.
///
/// Downmixes stereo to mono and resamples to 16 kHz if needed.
fn preprocess_to_f32_16khz(audio: &AudioFrame) -> Vec<f32> {
    let raw = audio.samples();
    // Downmix to mono
    let mono = if audio.channels > 1 {
        #[cfg(any(feature = "whisper-onnx", feature = "distil-whisper"))]
        {
            mel::downmix_to_mono(raw, audio.channels)
        }
        #[cfg(not(any(feature = "whisper-onnx", feature = "distil-whisper")))]
        {
            // Inline downmix for moonshine-only builds
            let ch = audio.channels as usize;
            let n_frames = raw.len() / ch;
            let mut out = Vec::with_capacity(n_frames);
            for i in 0..n_frames {
                let mut sum: i32 = 0;
                for c in 0..ch {
                    sum += raw[i * ch + c] as i32;
                }
                out.push((sum / ch as i32) as i16);
            }
            out
        }
    } else {
        raw.to_vec()
    };

    // Convert i16 → f32
    let f32_samples: Vec<f32> = mono.iter().map(|&s| s as f32 / 32768.0).collect();

    // Resample to 16 kHz
    if audio.sample_rate == TARGET_SAMPLE_RATE {
        f32_samples
    } else {
        #[cfg(any(feature = "whisper-onnx", feature = "distil-whisper"))]
        {
            mel::resample_to_16khz(&f32_samples, audio.sample_rate)
        }
        #[cfg(not(any(feature = "whisper-onnx", feature = "distil-whisper")))]
        {
            // Inline linear-interpolation resample for moonshine-only builds
            if audio.sample_rate == 0 || f32_samples.is_empty() {
                return Vec::new();
            }
            let ratio = audio.sample_rate as f64 / TARGET_SAMPLE_RATE as f64;
            let output_len = ((f32_samples.len() as f64) / ratio).round() as usize;
            if output_len == 0 {
                return Vec::new();
            }
            let mut output = Vec::with_capacity(output_len);
            for i in 0..output_len {
                let src_pos = i as f64 * ratio;
                let idx = src_pos as usize;
                let frac = (src_pos - idx as f64) as f32;
                let sample = if idx + 1 < f32_samples.len() {
                    f32_samples[idx] * (1.0 - frac) + f32_samples[idx + 1] * frac
                } else if idx < f32_samples.len() {
                    f32_samples[idx]
                } else {
                    0.0
                };
                output.push(sample);
            }
            output
        }
    }
}

#[async_trait]
impl SttProvider for OnnxSttProvider {
    /// Transcribe a single audio frame.
    ///
    /// Dispatches to [`WhisperDecoder`] or [`MoonshineDecoder`] based on the
    /// configured [`SttBackend`]. Returns an empty transcript with confidence
    /// 0.0 for zero-length input.
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        if Self::is_empty_audio(audio) {
            return Ok(Self::empty_transcript());
        }

        match self.config.backend {
            #[cfg(any(feature = "whisper-onnx", feature = "distil-whisper"))]
            SttBackend::Whisper | SttBackend::DistilWhisper => {
                let mel_data = mel::compute_whisper_mel(audio)?;
                let decoder =
                    whisper::WhisperDecoder::new(self.config.clone(), self.tokenizer.clone());
                let mut enc = self.encoder_session().await;
                let mut dec = self.decoder_session().await;
                decoder.transcribe(&mut enc, &mut dec, &mel_data, opts)
            }

            #[cfg(not(any(feature = "whisper-onnx", feature = "distil-whisper")))]
            SttBackend::Whisper | SttBackend::DistilWhisper => Err(AudioError::Stt {
                provider: "ONNX".into(),
                message: format!(
                    "backend {:?} requires the `whisper-onnx` or `distil-whisper` feature",
                    self.config.backend
                ),
            }),

            #[cfg(feature = "moonshine")]
            SttBackend::Moonshine => {
                let samples = preprocess_to_f32_16khz(audio);
                if samples.is_empty() {
                    return Ok(Self::empty_transcript());
                }
                let decoder = moonshine::MoonshineDecoder::new(self.tokenizer.clone());
                let mut enc = self.encoder_session().await;
                let mut dec = self.decoder_session().await;
                decoder.transcribe(&mut enc, &mut dec, &samples, opts)
            }

            #[cfg(not(feature = "moonshine"))]
            SttBackend::Moonshine => Err(AudioError::Stt {
                provider: "ONNX".into(),
                message: "Moonshine backend requires the `moonshine` feature".into(),
            }),
        }
    }

    /// Transcribe a stream of audio frames.
    ///
    /// For Whisper/Distil-Whisper: buffers audio into 30-second segments,
    /// pads the final segment with silence, and yields one [`Transcript`]
    /// per segment.
    ///
    /// For Moonshine: buffers audio into variable-length chunks (also using
    /// 30-second segments for consistency) without silence padding, yielding
    /// one [`Transcript`] per chunk.
    async fn transcribe_stream(
        &self,
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        let config = self.config.clone();
        let tokenizer = self.tokenizer.clone();
        let opts = opts.clone();

        // Collect all frames first, then segment and transcribe.
        // We need the ONNX sessions which are behind Mutex, so we process
        // segments sequentially using the provider's sessions.
        let mut audio_stream = audio;
        let mut all_samples: Vec<f32> = Vec::new();

        while let Some(frame) = audio_stream.next().await {
            if !Self::is_empty_audio(&frame) {
                let samples = preprocess_to_f32_16khz(&frame);
                all_samples.extend_from_slice(&samples);
            }
        }

        // If no audio was received, return an empty stream
        if all_samples.is_empty() {
            return Ok(Box::pin(futures::stream::empty()));
        }

        // Segment the audio and transcribe each segment
        let is_whisper = matches!(config.backend, SttBackend::Whisper | SttBackend::DistilWhisper);
        let mut transcripts: Vec<AudioResult<Transcript>> = Vec::new();

        if is_whisper {
            // Whisper/Distil-Whisper: fixed 30-second segments with silence padding
            let total_samples = all_samples.len();
            let mut offset = 0;

            while offset < total_samples {
                let end = (offset + WHISPER_SEGMENT_SAMPLES).min(total_samples);
                let mut segment = all_samples[offset..end].to_vec();

                // Pad final segment with silence to 30 seconds
                if segment.len() < WHISPER_SEGMENT_SAMPLES {
                    segment.resize(WHISPER_SEGMENT_SAMPLES, 0.0);
                }

                // Build an AudioFrame from the f32 segment for mel computation
                let pcm16: Vec<i16> = segment
                    .iter()
                    .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
                    .collect();
                let bytes: Vec<u8> = pcm16.iter().flat_map(|s| s.to_le_bytes()).collect();
                let frame = AudioFrame::new(bytes::Bytes::from(bytes), TARGET_SAMPLE_RATE, 1);

                let result = self.transcribe(&frame, &opts).await;
                transcripts.push(result);

                offset += WHISPER_SEGMENT_SAMPLES;
            }
        } else {
            // Moonshine: variable-length chunks (use 30s segments for consistency)
            let total_samples = all_samples.len();
            let mut offset = 0;

            while offset < total_samples {
                let end = (offset + WHISPER_SEGMENT_SAMPLES).min(total_samples);
                let segment = &all_samples[offset..end];
                // No silence padding for Moonshine — variable-length input

                #[cfg(feature = "moonshine")]
                {
                    let decoder = moonshine::MoonshineDecoder::new(tokenizer.clone());
                    let mut enc = self.encoder_session().await;
                    let mut dec = self.decoder_session().await;
                    let result = decoder.transcribe(&mut enc, &mut dec, segment, &opts);
                    transcripts.push(result);
                }

                #[cfg(not(feature = "moonshine"))]
                {
                    let _ = (segment, &tokenizer);
                    transcripts.push(Err(AudioError::Stt {
                        provider: "ONNX".into(),
                        message: "Moonshine backend requires the `moonshine` feature".into(),
                    }));
                }

                offset += WHISPER_SEGMENT_SAMPLES;
            }
        }

        Ok(Box::pin(futures::stream::iter(transcripts)))
    }
}
