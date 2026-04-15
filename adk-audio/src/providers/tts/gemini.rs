//! Gemini native audio TTS provider.
//!
//! Supports all Gemini TTS models:
//! - `gemini-3.1-flash-tts-preview` — expressive, audio tags, multi-speaker (default)
//! - `gemini-2.5-flash-preview-tts` — fast, multi-speaker
//! - `gemini-2.5-pro-preview-tts` — high-fidelity, multi-speaker

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::tts::CloudTtsConfig;
use crate::traits::{TtsProvider, TtsRequest, Voice};

/// Available Gemini TTS model IDs.
#[allow(dead_code)]
pub mod models {
    /// Gemini 3.1 Flash TTS — expressive audio tags, multi-speaker, low-latency.
    pub const GEMINI_3_1_FLASH_TTS: &str = "gemini-3.1-flash-tts-preview";
    /// Gemini 2.5 Flash TTS — fast, multi-speaker.
    pub const GEMINI_2_5_FLASH_TTS: &str = "gemini-2.5-flash-preview-tts";
    /// Gemini 2.5 Pro TTS — high-fidelity, multi-speaker.
    pub const GEMINI_2_5_PRO_TTS: &str = "gemini-2.5-pro-preview-tts";
}

/// Speaker configuration for multi-speaker TTS.
#[derive(Debug, Clone)]
pub struct SpeakerConfig {
    /// Speaker name (must match the name used in the transcript).
    pub name: String,
    /// Voice name from the 30 available voices.
    pub voice: String,
}

impl SpeakerConfig {
    /// Create a new speaker configuration.
    pub fn new(name: impl Into<String>, voice: impl Into<String>) -> Self {
        Self { name: name.into(), voice: voice.into() }
    }
}

/// Gemini TTS provider using `generateContent` with audio response modality.
///
/// # Example
///
/// ```rust,ignore
/// use adk_audio::GeminiTts;
///
/// // Default: gemini-3.1-flash-tts-preview
/// let tts = GeminiTts::from_env()?;
///
/// // Specific model
/// let tts = GeminiTts::from_env()?.with_model("gemini-2.5-pro-preview-tts");
///
/// // Multi-speaker
/// let tts = GeminiTts::from_env()?.with_speakers(vec![
///     SpeakerConfig::new("Alice", "Kore"),
///     SpeakerConfig::new("Bob", "Puck"),
/// ]);
/// ```
pub struct GeminiTts {
    config: CloudTtsConfig,
    client: reqwest::Client,
    model: String,
    voices: Vec<Voice>,
    speakers: Option<Vec<SpeakerConfig>>,
}

impl GeminiTts {
    /// Create from environment variable `GEMINI_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| AudioError::Tts {
                provider: "gemini".into(),
                message: "GEMINI_API_KEY or GOOGLE_API_KEY not set".into(),
            })?;
        Ok(Self::new(CloudTtsConfig::new(api_key)))
    }

    /// Create with explicit config.
    pub fn new(config: CloudTtsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            model: models::GEMINI_3_1_FLASH_TTS.into(),
            voices: build_voice_catalog(),
            speakers: None,
        }
    }

    /// Set the TTS model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Configure multi-speaker synthesis.
    ///
    /// Speaker names must match the names used in the transcript text.
    /// Up to 2 speakers are supported.
    pub fn with_speakers(mut self, speakers: Vec<SpeakerConfig>) -> Self {
        self.speakers = Some(speakers);
        self
    }

    fn base_url(&self) -> String {
        self.config.base_url.clone().unwrap_or_else(|| {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                self.model
            )
        })
    }

    fn build_speech_config(&self, voice: &str) -> serde_json::Value {
        match &self.speakers {
            Some(speakers) if !speakers.is_empty() => {
                let speaker_configs: Vec<serde_json::Value> = speakers
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "speaker": s.name,
                            "voiceConfig": {
                                "prebuiltVoiceConfig": {
                                    "voiceName": s.voice
                                }
                            }
                        })
                    })
                    .collect();
                serde_json::json!({
                    "multiSpeakerVoiceConfig": {
                        "speakerVoiceConfigs": speaker_configs
                    }
                })
            }
            _ => {
                let voice_name = if voice.is_empty() { "Kore" } else { voice };
                serde_json::json!({
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {
                            "voiceName": voice_name
                        }
                    }
                })
            }
        }
    }
}

#[async_trait]
impl TtsProvider for GeminiTts {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let url = self.base_url();
        let speech_config = self.build_speech_config(&request.voice);

        let body = serde_json::json!({
            "contents": [{"parts": [{"text": request.text}]}],
            "generationConfig": {
                "response_modalities": ["AUDIO"],
                "speech_config": speech_config
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Tts { provider: "gemini".into(), message: e.to_string() })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AudioError::Tts {
                provider: "gemini".into(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AudioError::Tts { provider: "gemini".into(), message: e.to_string() })?;

        let audio_b64 = json["candidates"][0]["content"]["parts"][0]["inlineData"]["data"]
            .as_str()
            .ok_or_else(|| AudioError::Tts {
                provider: "gemini".into(),
                message: "no audio data in response".into(),
            })?;

        use base64::Engine;
        let pcm = base64::engine::general_purpose::STANDARD.decode(audio_b64).map_err(|e| {
            AudioError::Tts {
                provider: "gemini".into(),
                message: format!("base64 decode failed: {e}"),
            }
        })?;

        Ok(AudioFrame::new(Bytes::from(pcm), 24000, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        // Gemini TTS does not support streaming — return single frame
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}

/// Build the full 30-voice catalog.
fn build_voice_catalog() -> Vec<Voice> {
    let voices = [
        ("Zephyr", "Bright"),
        ("Puck", "Upbeat"),
        ("Charon", "Informative"),
        ("Kore", "Firm"),
        ("Fenrir", "Excitable"),
        ("Leda", "Youthful"),
        ("Orus", "Firm"),
        ("Aoede", "Breezy"),
        ("Callirrhoe", "Easy-going"),
        ("Autonoe", "Bright"),
        ("Enceladus", "Breathy"),
        ("Iapetus", "Clear"),
        ("Umbriel", "Easy-going"),
        ("Algieba", "Smooth"),
        ("Despina", "Smooth"),
        ("Erinome", "Clear"),
        ("Algenib", "Gravelly"),
        ("Rasalgethi", "Informative"),
        ("Laomedeia", "Upbeat"),
        ("Achernar", "Soft"),
        ("Alnilam", "Firm"),
        ("Schedar", "Even"),
        ("Gacrux", "Mature"),
        ("Pulcherrima", "Forward"),
        ("Achird", "Friendly"),
        ("Zubenelgenubi", "Casual"),
        ("Vindemiatrix", "Gentle"),
        ("Sadachbia", "Lively"),
        ("Sadaltager", "Knowledgeable"),
        ("Sulafat", "Warm"),
    ];

    voices
        .iter()
        .map(|(name, style)| Voice {
            id: name.to_string(),
            name: format!("{name} — {style}"),
            language: "multilingual".into(),
            gender: None,
        })
        .collect()
}
