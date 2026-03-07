//! Configuration types and builder for ONNX STT inference.
//!
//! Provides [`OnnxSttConfig`] with a fluent builder API for selecting the
//! speech-to-text backend (Whisper, Distil-Whisper, Moonshine), model variant,
//! decoding parameters, and hardware execution provider.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_audio::onnx::stt::{OnnxSttConfig, SttBackend, WhisperModelSize};
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

use crate::error::{AudioError, AudioResult};
use crate::onnx::execution_provider::OnnxExecutionProvider;

/// Speech-to-text backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttBackend {
    /// OpenAI Whisper (tiny through large-v3-turbo).
    #[default]
    Whisper,
    /// HuggingFace Distil-Whisper (faster, near-Whisper accuracy).
    DistilWhisper,
    /// Useful Sensors Moonshine (ultra-lightweight, edge-optimized).
    Moonshine,
}

/// Whisper model size variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhisperModelSize {
    /// Whisper tiny (~39M parameters).
    Tiny,
    /// Whisper base (~74M parameters).
    #[default]
    Base,
    /// Whisper small (~244M parameters).
    Small,
    /// Whisper medium (~769M parameters).
    Medium,
    /// Whisper large-v3-turbo (~809M parameters).
    LargeV3Turbo,
}

/// Distil-Whisper model variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistilWhisperVariant {
    /// distil-small.en — English-only, fastest.
    #[default]
    DistilSmallEn,
    /// distil-medium.en — English-only, balanced.
    DistilMediumEn,
    /// distil-large-v3 — Multilingual, highest quality.
    DistilLargeV3,
}

/// Moonshine model variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MoonshineVariant {
    /// Moonshine tiny (~27M parameters).
    #[default]
    Tiny,
    /// Moonshine base (~61M parameters).
    Base,
}

/// Configuration for ONNX STT inference.
///
/// Use [`OnnxSttConfig::builder()`] for a fluent construction API with
/// sensible defaults and validation.
///
/// # Defaults
///
/// - Backend: [`SttBackend::Whisper`]
/// - Model size: [`WhisperModelSize::Base`]
/// - Beam size: 1 (greedy decoding)
/// - Temperature: 0.0 (deterministic)
/// - Execution provider: auto-detected
/// - Language: `None` (auto-detect)
#[derive(Debug, Clone)]
pub struct OnnxSttConfig {
    /// Which STT backend to use.
    pub backend: SttBackend,
    /// Whisper model size (only used when `backend == Whisper`).
    pub whisper_size: WhisperModelSize,
    /// Distil-Whisper variant (only used when `backend == DistilWhisper`).
    pub distil_variant: DistilWhisperVariant,
    /// Moonshine variant (only used when `backend == Moonshine`).
    pub moonshine_variant: MoonshineVariant,
    /// Beam search width. 1 = greedy decoding. Must be 1..=10.
    pub beam_size: u8,
    /// Sampling temperature. 0.0 = deterministic.
    pub temperature: f32,
    /// Hardware execution provider for ONNX Runtime.
    pub execution_provider: OnnxExecutionProvider,
    /// Language hint for transcription (ISO 639-1 code). `None` = auto-detect.
    pub language: Option<String>,
}

impl OnnxSttConfig {
    /// Create a new builder with sensible defaults.
    pub fn builder() -> OnnxSttConfigBuilder {
        OnnxSttConfigBuilder::default()
    }

    /// Returns the HuggingFace model ID for the configured backend and variant.
    pub fn model_id(&self) -> &str {
        match self.backend {
            SttBackend::Whisper => match self.whisper_size {
                WhisperModelSize::Tiny => "onnx-community/whisper-tiny",
                WhisperModelSize::Base => "onnx-community/whisper-base",
                WhisperModelSize::Small => "onnx-community/whisper-small",
                WhisperModelSize::Medium => "onnx-community/whisper-medium",
                WhisperModelSize::LargeV3Turbo => "onnx-community/whisper-large-v3-turbo",
            },
            SttBackend::DistilWhisper => match self.distil_variant {
                DistilWhisperVariant::DistilSmallEn => "distil-whisper/distil-small.en",
                DistilWhisperVariant::DistilMediumEn => "distil-whisper/distil-medium.en",
                DistilWhisperVariant::DistilLargeV3 => "distil-whisper/distil-large-v3",
            },
            SttBackend::Moonshine => match self.moonshine_variant {
                MoonshineVariant::Tiny => "usefulsensors/moonshine-tiny-onnx",
                MoonshineVariant::Base => "usefulsensors/moonshine-base-onnx",
            },
        }
    }
}

/// Builder for [`OnnxSttConfig`].
///
/// Provides a fluent API for constructing STT configuration with validation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_audio::onnx::stt::{OnnxSttConfig, SttBackend};
///
/// let config = OnnxSttConfig::builder()
///     .stt_backend(SttBackend::DistilWhisper)
///     .beam_size(5)
///     .build()?;
/// ```
#[derive(Debug, Clone)]
pub struct OnnxSttConfigBuilder {
    backend: SttBackend,
    whisper_size: WhisperModelSize,
    distil_variant: DistilWhisperVariant,
    moonshine_variant: MoonshineVariant,
    beam_size: u8,
    temperature: f32,
    execution_provider: OnnxExecutionProvider,
    language: Option<String>,
}

impl Default for OnnxSttConfigBuilder {
    fn default() -> Self {
        Self {
            backend: SttBackend::default(),
            whisper_size: WhisperModelSize::default(),
            distil_variant: DistilWhisperVariant::default(),
            moonshine_variant: MoonshineVariant::default(),
            beam_size: 1,
            temperature: 0.0,
            execution_provider: OnnxExecutionProvider::auto_detect(),
            language: None,
        }
    }
}

impl OnnxSttConfigBuilder {
    /// Set the STT backend (Whisper, DistilWhisper, or Moonshine).
    pub fn stt_backend(mut self, backend: SttBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Set the Whisper model size (used when backend is Whisper).
    pub fn model_size(mut self, size: WhisperModelSize) -> Self {
        self.whisper_size = size;
        self
    }

    /// Set the Distil-Whisper variant (used when backend is DistilWhisper).
    pub fn distil_variant(mut self, variant: DistilWhisperVariant) -> Self {
        self.distil_variant = variant;
        self
    }

    /// Set the Moonshine variant (used when backend is Moonshine).
    pub fn moonshine_variant(mut self, variant: MoonshineVariant) -> Self {
        self.moonshine_variant = variant;
        self
    }

    /// Set the beam search width. Must be 1..=10.
    ///
    /// A value of 1 uses greedy decoding (fastest). Higher values improve
    /// accuracy at the cost of speed.
    pub fn beam_size(mut self, size: u8) -> Self {
        self.beam_size = size;
        self
    }

    /// Set the sampling temperature. 0.0 = deterministic output.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.temperature = temp;
        self
    }

    /// Set the hardware execution provider for ONNX Runtime.
    pub fn execution_provider(mut self, ep: OnnxExecutionProvider) -> Self {
        self.execution_provider = ep;
        self
    }

    /// Set the language hint (ISO 639-1 code, e.g. `"en"`, `"fr"`).
    ///
    /// When set, constrains transcription to the specified language.
    /// When omitted, the model auto-detects the spoken language.
    pub fn language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    /// Build the configuration, validating all parameters.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Stt`] if `beam_size` is not in the range 1..=10.
    pub fn build(self) -> AudioResult<OnnxSttConfig> {
        if !(1..=10).contains(&self.beam_size) {
            return Err(AudioError::Stt {
                provider: "ONNX".into(),
                message: format!("beam_size must be 1..=10, got {}", self.beam_size),
            });
        }

        Ok(OnnxSttConfig {
            backend: self.backend,
            whisper_size: self.whisper_size,
            distil_variant: self.distil_variant,
            moonshine_variant: self.moonshine_variant,
            beam_size: self.beam_size,
            temperature: self.temperature,
            execution_provider: self.execution_provider,
            language: self.language,
        })
    }
}
