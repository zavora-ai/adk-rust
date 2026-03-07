//! Native Qwen3-TTS provider using the `qwen_tts` Candle-based crate.
//!
//! Wraps [`qwen_tts::model::Model`] to implement [`TtsProvider`] for
//! high-quality multilingual text-to-speech with predefined speaker voices.
//!
//! Supports 10 languages: Chinese, English, Japanese, Korean, German, French,
//! Russian, Portuguese, Spanish, and Italian.
//!
//! Requires the `qwen3-tts` feature flag.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use candle_core::Device;
use futures::Stream;
use qwen_tts::model::Model;
use qwen_tts::model::loader::{LoaderConfig, ModelLoader};
use tokio::sync::Mutex;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::registry::LocalModelRegistry;
use crate::traits::{TtsProvider, TtsRequest, Voice};

/// Qwen3-TTS model size variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Qwen3TtsVariant {
    /// 0.6 billion parameter model — faster, lower quality.
    #[default]
    Small,
    /// 1.7 billion parameter model — slower, higher quality.
    Large,
}

impl Qwen3TtsVariant {
    /// Returns the HuggingFace model ID for this variant.
    pub fn model_id(&self) -> &str {
        match self {
            Self::Small => "Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice",
            Self::Large => "Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice",
        }
    }
}

impl std::fmt::Display for Qwen3TtsVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Small => write!(f, "0.6B"),
            Self::Large => write!(f, "1.7B"),
        }
    }
}

/// Predefined speakers available in Qwen3-TTS CustomVoice models.
const PREDEFINED_SPEAKERS: &[(&str, &str)] = &[
    ("vivian", "en"),
    ("serena", "en"),
    ("dylan", "en"),
    ("eric", "en"),
    ("ryan", "en"),
    ("aiden", "en"),
    ("uncle_fu", "zh"),
    ("ono_anna", "ja"),
    ("sohee", "ko"),
];

/// Supported language mappings: (ISO 639-1 code, qwen_tts language name).
const LANGUAGE_MAP: &[(&str, &str)] = &[
    ("en", "english"),
    ("zh", "chinese"),
    ("ja", "japanese"),
    ("ko", "korean"),
    ("de", "german"),
    ("fr", "french"),
    ("ru", "russian"),
    ("pt", "portuguese"),
    ("es", "spanish"),
    ("it", "italian"),
];

/// Native Qwen3-TTS provider — Candle-based, CPU/Metal/CUDA.
///
/// # Example
///
/// ```rust,ignore
/// use adk_audio::{Qwen3TtsNativeProvider, Qwen3TtsVariant, TtsProvider, TtsRequest};
///
/// let provider = Qwen3TtsNativeProvider::new(Qwen3TtsVariant::Small).await?;
/// let request = TtsRequest {
///     text: "Hello!".into(),
///     voice: "vivian".into(),
///     ..Default::default()
/// };
/// let frame = provider.synthesize(&request).await?;
/// ```
pub struct Qwen3TtsNativeProvider {
    model: Arc<Mutex<Model>>,
    voices: Vec<Voice>,
    sample_rate: u32,
    variant: Qwen3TtsVariant,
}

fn make_voice(name: &str, lang: &str) -> Voice {
    Voice { id: name.into(), name: name.into(), language: lang.into(), gender: None }
}

/// Resolve a voice string to (speaker_name, language).
///
/// Supports formats:
/// - `"vivian"` — speaker name, language defaults to speaker's native language
/// - `"lang:zh"` or `"lang:chinese"` — default speaker for that language
/// - `"vivian:zh"` — explicit speaker + language override
fn resolve_voice(voice: &str) -> (&str, &str) {
    if voice.is_empty() || voice == "default" {
        return ("vivian", "english");
    }

    // "lang:XX" format — pick a default speaker for that language
    if let Some(lang_code) = voice.strip_prefix("lang:") {
        let lang_name = resolve_language_name(lang_code);
        // Find a speaker whose native language matches
        let speaker = PREDEFINED_SPEAKERS
            .iter()
            .find(|(_, l)| *l == lang_code)
            .map(|(s, _)| *s)
            .unwrap_or("vivian");
        return (speaker, lang_name);
    }

    // "speaker:lang" format
    if let Some((speaker, lang)) = voice.split_once(':') {
        return (speaker, resolve_language_name(lang));
    }

    // Plain speaker name — use their native language
    let native_lang = PREDEFINED_SPEAKERS
        .iter()
        .find(|(s, _)| *s == voice)
        .map(|(_, l)| resolve_language_name(l))
        .unwrap_or("english");
    (voice, native_lang)
}

/// Map an ISO 639-1 code or language name to the qwen_tts language name.
fn resolve_language_name(input: &str) -> &str {
    let lower = input.to_lowercase();
    // Check if it's already a full language name
    for &(_, name) in LANGUAGE_MAP {
        if lower == name {
            return name;
        }
    }
    // Check ISO code
    for &(code, name) in LANGUAGE_MAP {
        if lower == code {
            return name;
        }
    }
    // Fallback
    "english"
}

impl Qwen3TtsNativeProvider {
    /// Load a Qwen3-TTS model variant, downloading from HuggingFace if needed.
    ///
    /// # Errors
    ///
    /// Returns `AudioError::Tts` if model download or loading fails.
    pub async fn new(variant: Qwen3TtsVariant) -> AudioResult<Self> {
        let registry = LocalModelRegistry::default();
        let model_dir = registry.get_or_download(variant.model_id()).await?;
        Self::from_dir(&model_dir, variant)
    }

    /// Load from a local directory (already downloaded).
    pub fn from_dir(model_dir: &std::path::Path, variant: Qwen3TtsVariant) -> AudioResult<Self> {
        let loader =
            ModelLoader::from_local_dir(model_dir.to_str().unwrap_or(".")).map_err(|e| {
                AudioError::Tts {
                    provider: "Qwen3TTS".into(),
                    message: format!("failed to create model loader: {e}"),
                }
            })?;

        let device = if cfg!(target_os = "macos") {
            Device::new_metal(0).unwrap_or(Device::Cpu)
        } else {
            Device::Cpu
        };

        let model = loader.load_tts_model(&device, &LoaderConfig::default()).map_err(|e| {
            AudioError::Tts {
                provider: "Qwen3TTS".into(),
                message: format!("failed to load model: {e}"),
            }
        })?;

        let sample_rate = model.sample_rate() as u32;
        let voices = PREDEFINED_SPEAKERS.iter().map(|(n, l)| make_voice(n, l)).collect();

        Ok(Self { model: Arc::new(Mutex::new(model)), voices, sample_rate, variant })
    }

    /// Get the model variant.
    pub fn variant(&self) -> Qwen3TtsVariant {
        self.variant
    }

    /// Get the sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[async_trait]
impl TtsProvider for Qwen3TtsNativeProvider {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let text = request.text.clone();
        let voice = request.voice.clone();
        let model = self.model.clone();
        let sample_rate = self.sample_rate;

        let pcm_bytes = tokio::task::spawn_blocking(move || -> AudioResult<Vec<u8>> {
            let model = model.blocking_lock();
            let (speaker, language) = resolve_voice(&voice);

            let result = model
                .generate_custom_voice_from_text(&text, speaker, language, None, None)
                .map_err(|e| AudioError::Tts {
                    provider: "Qwen3TTS".into(),
                    message: format!("generation failed: {e}"),
                })?;

            // Convert audio Tensor to PCM16 LE bytes
            let audio = result.audio;
            let flat: Vec<f32> = audio
                .flatten_all()
                .map_err(|e| AudioError::Tts {
                    provider: "Qwen3TTS".into(),
                    message: format!("failed to flatten audio tensor: {e}"),
                })?
                .to_vec1()
                .map_err(|e| AudioError::Tts {
                    provider: "Qwen3TTS".into(),
                    message: format!("failed to convert audio tensor to vec: {e}"),
                })?;

            let mut pcm = Vec::with_capacity(flat.len() * 2);
            for sample in &flat {
                let clamped = sample.clamp(-1.0, 1.0);
                let i16_val = (clamped * 32767.0) as i16;
                pcm.extend_from_slice(&i16_val.to_le_bytes());
            }
            Ok(pcm)
        })
        .await
        .map_err(|e| AudioError::Tts {
            provider: "Qwen3TTS".into(),
            message: format!("blocking task failed: {e}"),
        })??;

        Ok(AudioFrame::new(pcm_bytes, sample_rate, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        let full_frame = self.synthesize(request).await?;
        let chunk_bytes = (full_frame.sample_rate as usize * 100 / 1000) * 2; // 100ms of PCM16

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
