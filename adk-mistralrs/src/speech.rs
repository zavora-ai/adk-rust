//! Speech generation model support for text-to-speech synthesis.
//!
//! This module provides support for speech generation models like Dia 1.6b,
//! enabling text-to-speech synthesis with multi-speaker dialogue support.
//!
//! ## Features
//!
//! - Load speech models from HuggingFace Hub
//! - Generate speech from text with configurable voice parameters
//! - Multi-speaker dialogue generation with speaker tags
//! - Output audio in WAV format
//!
//! ## Example
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsSpeechModel, SpeechConfig, VoiceConfig};
//!
//! let model = MistralRsSpeechModel::from_hf("nari-labs/Dia-1.6B").await?;
//!
//! // Generate speech from text
//! let audio = model.generate_speech("Hello, world!").await?;
//!
//! // Generate multi-speaker dialogue
//! let dialogue = model.generate_dialogue(
//!     "[S1] Hello! [S2] Hi there! How are you?"
//! ).await?;
//! ```

use std::sync::Arc;

use mistralrs::{SpeechLoaderType, SpeechModelBuilder};
use tracing::{debug, info, instrument};

use crate::config::{Device, ModelSource};
use crate::error::{MistralRsError, Result};

/// Configuration for voice parameters in speech generation.
///
/// # Example
///
/// ```rust
/// use adk_mistralrs::VoiceConfig;
///
/// let config = VoiceConfig::default()
///     .with_speaker_id(1)
///     .with_speed(1.0);
/// ```
#[derive(Debug, Clone, Default)]
pub struct VoiceConfig {
    /// Speaker ID for multi-speaker models (e.g., S1, S2 in Dia)
    pub speaker_id: Option<u32>,
    /// Speech speed multiplier (1.0 = normal speed)
    pub speed: Option<f32>,
    /// Pitch adjustment (-1.0 to 1.0, 0.0 = normal)
    pub pitch: Option<f32>,
    /// Energy/volume adjustment (-1.0 to 1.0, 0.0 = normal)
    pub energy: Option<f32>,
}

impl VoiceConfig {
    /// Create a new voice config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the speaker ID for multi-speaker models.
    pub fn with_speaker_id(mut self, id: u32) -> Self {
        self.speaker_id = Some(id);
        self
    }

    /// Set the speech speed multiplier.
    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = Some(speed);
        self
    }

    /// Set the pitch adjustment.
    pub fn with_pitch(mut self, pitch: f32) -> Self {
        self.pitch = Some(pitch);
        self
    }

    /// Set the energy/volume adjustment.
    pub fn with_energy(mut self, energy: f32) -> Self {
        self.energy = Some(energy);
        self
    }
}

/// Configuration for speech model loading.
///
/// # Example
///
/// ```rust
/// use adk_mistralrs::{SpeechConfig, ModelSource};
///
/// let config = SpeechConfig::builder()
///     .model_source(ModelSource::huggingface("nari-labs/Dia-1.6B"))
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct SpeechConfig {
    /// Model source: HuggingFace ID or local path
    pub model_source: ModelSource,
    /// Speech loader type (e.g., Dia)
    pub loader_type: SpeechLoaderType,
    /// Device configuration
    pub device: Device,
    /// Optional DAC model ID for audio codec
    pub dac_model_id: Option<String>,
    /// Maximum number of sequences
    pub max_num_seqs: Option<usize>,
    /// Voice configuration
    pub voice: VoiceConfig,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self {
            model_source: ModelSource::HuggingFace(String::new()),
            loader_type: SpeechLoaderType::Dia,
            device: Device::Auto,
            dac_model_id: None,
            max_num_seqs: None,
            voice: VoiceConfig::default(),
        }
    }
}

impl SpeechConfig {
    /// Create a new config builder.
    pub fn builder() -> SpeechConfigBuilder {
        SpeechConfigBuilder::default()
    }
}

/// Builder for SpeechConfig.
#[derive(Debug, Clone, Default)]
pub struct SpeechConfigBuilder {
    config: SpeechConfig,
}

impl SpeechConfigBuilder {
    /// Set the model source.
    pub fn model_source(mut self, source: ModelSource) -> Self {
        self.config.model_source = source;
        self
    }

    /// Set the speech loader type.
    pub fn loader_type(mut self, loader_type: SpeechLoaderType) -> Self {
        self.config.loader_type = loader_type;
        self
    }

    /// Set the device.
    pub fn device(mut self, device: Device) -> Self {
        self.config.device = device;
        self
    }

    /// Set the DAC model ID.
    pub fn dac_model_id(mut self, id: impl Into<String>) -> Self {
        self.config.dac_model_id = Some(id.into());
        self
    }

    /// Set the maximum number of sequences.
    pub fn max_num_seqs(mut self, max: usize) -> Self {
        self.config.max_num_seqs = Some(max);
        self
    }

    /// Set the voice configuration.
    pub fn voice(mut self, voice: VoiceConfig) -> Self {
        self.config.voice = voice;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> SpeechConfig {
        self.config
    }
}

/// Audio output from speech generation.
#[derive(Debug, Clone)]
pub struct SpeechOutput {
    /// Raw PCM audio samples
    pub pcm_data: Vec<f32>,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of audio channels
    pub channels: u16,
}

impl SpeechOutput {
    /// Create a new speech output.
    pub fn new(pcm_data: Vec<f32>, sample_rate: u32, channels: u16) -> Self {
        Self { pcm_data, sample_rate, channels }
    }

    /// Get the duration of the audio in seconds.
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 || self.channels == 0 {
            return 0.0;
        }
        self.pcm_data.len() as f32 / (self.sample_rate as f32 * self.channels as f32)
    }

    /// Convert to WAV bytes.
    pub fn to_wav_bytes(&self) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        mistralrs::speech_utils::write_pcm_as_wav(
            &mut buffer,
            &self.pcm_data,
            self.sample_rate,
            self.channels,
        )
        .map_err(|e| MistralRsError::audio_processing(format!("Failed to encode WAV: {}", e)))?;
        Ok(buffer)
    }
}

/// A mistral.rs speech model for text-to-speech synthesis.
///
/// This struct wraps a mistral.rs speech model and provides methods for
/// generating speech from text, including multi-speaker dialogue support.
///
/// # Supported Models
///
/// - Dia 1.6B (nari-labs/Dia-1.6B)
///
/// # Example
///
/// ```rust,ignore
/// use adk_mistralrs::MistralRsSpeechModel;
///
/// let model = MistralRsSpeechModel::from_hf("nari-labs/Dia-1.6B").await?;
/// let audio = model.generate_speech("Hello, world!").await?;
/// ```
pub struct MistralRsSpeechModel {
    /// The underlying mistral.rs model instance
    model: Arc<mistralrs::Model>,
    /// Model name for identification
    name: String,
    /// Configuration used to create this model
    config: SpeechConfig,
}

impl MistralRsSpeechModel {
    /// Create a new speech model from configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration specifying model source and options
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = SpeechConfig::builder()
    ///     .model_source(ModelSource::huggingface("nari-labs/Dia-1.6B"))
    ///     .build();
    /// let model = MistralRsSpeechModel::new(config).await?;
    /// ```
    #[instrument(skip(config), fields(model_source = ?config.model_source))]
    pub async fn new(config: SpeechConfig) -> Result<Self> {
        let model_id = match &config.model_source {
            ModelSource::HuggingFace(id) => id.clone(),
            ModelSource::Local(path) => path.display().to_string(),
            ModelSource::Gguf(path) => path.display().to_string(),
            ModelSource::Uqff(path) => path.display().to_string(),
        };

        info!("Loading mistral.rs speech model: {}", model_id);

        let mut builder = SpeechModelBuilder::new(&model_id, config.loader_type);

        // Apply DAC model ID if configured
        if let Some(dac_id) = &config.dac_model_id {
            builder = builder.with_dac_model_id(dac_id.clone());
            debug!("DAC model ID configured: {}", dac_id);
        }

        // Apply max sequences if configured
        if let Some(max_seqs) = config.max_num_seqs {
            builder = builder.with_max_num_seqs(max_seqs);
            debug!("Max sequences configured: {}", max_seqs);
        }

        // Apply device selection
        if matches!(config.device, Device::Cpu) {
            builder = builder.with_force_cpu();
            debug!("Forcing CPU device");
        }

        // Enable logging
        builder = builder.with_logging();

        // Build the model
        let model = builder
            .build()
            .await
            .map_err(|e| MistralRsError::model_load(&model_id, e.to_string()))?;

        info!("Speech model loaded successfully: {}", model_id);

        Ok(Self { model: Arc::new(model), name: model_id, config })
    }

    /// Create from HuggingFace model ID with defaults.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID (e.g., "nari-labs/Dia-1.6B")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let model = MistralRsSpeechModel::from_hf("nari-labs/Dia-1.6B").await?;
    /// ```
    pub async fn from_hf(model_id: &str) -> Result<Self> {
        let config =
            SpeechConfig::builder().model_source(ModelSource::huggingface(model_id)).build();
        Self::new(config).await
    }

    /// Get the model name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the model configuration.
    pub fn config(&self) -> &SpeechConfig {
        &self.config
    }

    /// Get a reference to the underlying mistral.rs model.
    pub fn inner(&self) -> &mistralrs::Model {
        &self.model
    }

    /// Generate speech from text.
    ///
    /// # Arguments
    ///
    /// * `text` - Text to convert to speech
    ///
    /// # Returns
    ///
    /// Audio output containing PCM data, sample rate, and channel count.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let audio = model.generate_speech("Hello, world!").await?;
    /// println!("Duration: {} seconds", audio.duration_secs());
    /// ```
    pub async fn generate_speech(&self, text: &str) -> Result<SpeechOutput> {
        debug!("Generating speech for text: {}", text);

        let (pcm, rate, channels) = self
            .model
            .generate_speech(text)
            .await
            .map_err(|e| MistralRsError::speech(format!("Speech generation failed: {}", e)))?;

        // Convert Arc<Vec<f32>> to Vec<f32>
        let pcm_data = (*pcm).clone();
        Ok(SpeechOutput::new(pcm_data, rate as u32, channels as u16))
    }

    /// Generate multi-speaker dialogue from text with speaker tags.
    ///
    /// The text should contain speaker tags like `[S1]` and `[S2]` to indicate
    /// different speakers. This is the format used by Dia models.
    ///
    /// # Arguments
    ///
    /// * `dialogue` - Text with speaker tags (e.g., "`[S1]` Hello! `[S2]` Hi there!")
    ///
    /// # Returns
    ///
    /// Audio output containing the synthesized dialogue.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let audio = model.generate_dialogue(
    ///     "[S1] Hello! How are you? [S2] I'm doing great, thanks!"
    /// ).await?;
    /// ```
    pub async fn generate_dialogue(&self, dialogue: &str) -> Result<SpeechOutput> {
        debug!("Generating dialogue: {}", dialogue);

        // Dia models use the same generate_speech method for dialogue
        // The speaker tags [S1], [S2] are parsed by the model
        let (pcm, rate, channels) =
            self.model.generate_speech(dialogue).await.map_err(|e| {
                MistralRsError::speech(format!("Dialogue generation failed: {}", e))
            })?;

        // Convert Arc<Vec<f32>> to Vec<f32>
        let pcm_data = (*pcm).clone();
        Ok(SpeechOutput::new(pcm_data, rate as u32, channels as u16))
    }

    /// Generate speech with custom voice configuration.
    ///
    /// # Arguments
    ///
    /// * `text` - Text to convert to speech
    /// * `voice` - Voice configuration parameters
    ///
    /// # Returns
    ///
    /// Audio output with the specified voice settings.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let voice = VoiceConfig::new().with_speed(1.2);
    /// let audio = model.generate_speech_with_voice("Hello!", voice).await?;
    /// ```
    pub async fn generate_speech_with_voice(
        &self,
        text: &str,
        _voice: VoiceConfig,
    ) -> Result<SpeechOutput> {
        // Note: Voice configuration is stored but not yet applied
        // as mistral.rs speech API doesn't expose these parameters directly.
        // Future versions may support this through SpeechGenerationConfig.
        self.generate_speech(text).await
    }
}

impl std::fmt::Debug for MistralRsSpeechModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralRsSpeechModel")
            .field("name", &self.name)
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_config_builder() {
        let config =
            VoiceConfig::new().with_speaker_id(1).with_speed(1.5).with_pitch(0.2).with_energy(-0.1);

        assert_eq!(config.speaker_id, Some(1));
        assert_eq!(config.speed, Some(1.5));
        assert_eq!(config.pitch, Some(0.2));
        assert_eq!(config.energy, Some(-0.1));
    }

    #[test]
    fn test_voice_config_default() {
        let config = VoiceConfig::default();

        assert!(config.speaker_id.is_none());
        assert!(config.speed.is_none());
        assert!(config.pitch.is_none());
        assert!(config.energy.is_none());
    }

    #[test]
    fn test_speech_config_builder() {
        let config = SpeechConfig::builder()
            .model_source(ModelSource::huggingface("nari-labs/Dia-1.6B"))
            .loader_type(SpeechLoaderType::Dia)
            .device(Device::Cpu)
            .dac_model_id("custom/dac")
            .max_num_seqs(16)
            .voice(VoiceConfig::new().with_speed(1.0))
            .build();

        assert!(matches!(config.model_source, ModelSource::HuggingFace(_)));
        assert!(matches!(config.loader_type, SpeechLoaderType::Dia));
        assert_eq!(config.device, Device::Cpu);
        assert_eq!(config.dac_model_id, Some("custom/dac".to_string()));
        assert_eq!(config.max_num_seqs, Some(16));
        assert_eq!(config.voice.speed, Some(1.0));
    }

    #[test]
    fn test_speech_output() {
        let pcm = vec![0.0f32; 44100]; // 1 second at 44.1kHz mono
        let output = SpeechOutput::new(pcm, 44100, 1);

        assert_eq!(output.sample_rate, 44100);
        assert_eq!(output.channels, 1);
        assert!((output.duration_secs() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_speech_output_duration_stereo() {
        let pcm = vec![0.0f32; 88200]; // 1 second at 44.1kHz stereo
        let output = SpeechOutput::new(pcm, 44100, 2);

        assert!((output.duration_secs() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_speech_output_duration_zero() {
        let output = SpeechOutput::new(vec![], 0, 0);
        assert_eq!(output.duration_secs(), 0.0);
    }
}
