//! MLX configuration types.

/// Quantization level for MLX models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlxQuantization {
    /// 4-bit quantization (~4x memory reduction).
    Q4,
    /// 8-bit quantization (~2x memory reduction).
    Q8,
}

impl MlxQuantization {
    /// Returns the weights filename suffix for this quantization level.
    pub fn weights_file(&self) -> &'static str {
        match self {
            Self::Q4 => "model-q4.safetensors",
            Self::Q8 => "model-q8.safetensors",
        }
    }
}

/// Configuration for MLX TTS inference.
#[derive(Debug, Clone)]
pub struct MlxTtsConfig {
    /// HuggingFace model identifier or local path.
    pub model_id: String,
    /// Quantization level for reduced memory usage.
    pub quantization: Option<MlxQuantization>,
    /// Maximum generation length in tokens.
    pub max_length: usize,
    /// Output sample rate in Hz.
    pub sample_rate: u32,
}

impl Default for MlxTtsConfig {
    fn default() -> Self {
        Self {
            model_id: "mlx-community/Kokoro-82M-bf16".into(),
            quantization: None,
            max_length: 4096,
            sample_rate: 24000,
        }
    }
}

/// Configuration for MLX STT inference.
#[derive(Debug, Clone)]
pub struct MlxSttConfig {
    /// HuggingFace model identifier or local path.
    pub model_id: String,
    /// Quantization level.
    pub quantization: Option<MlxQuantization>,
    /// Maximum audio duration in seconds.
    pub max_duration_secs: u32,
    /// Expected input sample rate (audio resampled if different).
    pub sample_rate: u32,
}

impl Default for MlxSttConfig {
    fn default() -> Self {
        Self {
            model_id: "mlx-community/whisper-large-v3-turbo".into(),
            quantization: None,
            max_duration_secs: 600,
            sample_rate: 16000,
        }
    }
}
