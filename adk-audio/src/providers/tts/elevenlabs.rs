//! ElevenLabs TTS provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::tts::CloudTtsConfig;
use crate::traits::{Emotion, TtsProvider, TtsRequest, Voice};

/// ElevenLabs TTS provider.
///
/// Uses the ElevenLabs v1 API for high-quality voice synthesis.
/// Configure via `ELEVENLABS_API_KEY` environment variable or builder.
pub struct ElevenLabsTts {
    config: CloudTtsConfig,
    client: reqwest::Client,
    voices: Vec<Voice>,
}

impl ElevenLabsTts {
    /// Create from environment variable `ELEVENLABS_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("ELEVENLABS_API_KEY").map_err(|_| AudioError::Tts {
            provider: "elevenlabs".into(),
            message: "ELEVENLABS_API_KEY not set".into(),
        })?;
        Ok(Self::new(CloudTtsConfig::new(api_key)))
    }

    /// Create with explicit config.
    pub fn new(config: CloudTtsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            voices: vec![
                Voice {
                    id: "21m00Tcm4TlvDq8ikWAM".into(),
                    name: "Rachel".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
                Voice {
                    id: "AZnzlk1XvdvUeBnXmlld".into(),
                    name: "Domi".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
                Voice {
                    id: "EXAVITQu4vr4xnSDxMaL".into(),
                    name: "Bella".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
                Voice {
                    id: "ErXwobaYiN019PkySvjV".into(),
                    name: "Antoni".into(),
                    language: "en".into(),
                    gender: Some("male".into()),
                },
            ],
        }
    }

    fn base_url(&self) -> &str {
        self.config.base_url.as_deref().unwrap_or("https://api.elevenlabs.io")
    }

    fn emotion_to_settings(&self, emotion: Option<&Emotion>) -> serde_json::Value {
        match emotion {
            Some(Emotion::Happy) => serde_json::json!({"stability": 0.4, "similarity_boost": 0.8}),
            Some(Emotion::Sad) => serde_json::json!({"stability": 0.7, "similarity_boost": 0.6}),
            Some(Emotion::Angry) => serde_json::json!({"stability": 0.3, "similarity_boost": 0.9}),
            Some(Emotion::Whisper) => {
                serde_json::json!({"stability": 0.9, "similarity_boost": 0.3})
            }
            Some(Emotion::Excited) => {
                serde_json::json!({"stability": 0.3, "similarity_boost": 0.8})
            }
            Some(Emotion::Calm) => serde_json::json!({"stability": 0.8, "similarity_boost": 0.5}),
            _ => serde_json::json!({"stability": 0.5, "similarity_boost": 0.75}),
        }
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsTts {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let voice_id = if request.voice.is_empty() { &self.voices[0].id } else { &request.voice };
        let url = format!(
            "{}/v1/text-to-speech/{voice_id}?output_format=pcm_24000",
            self.base_url()
        );
        let voice_settings = self.emotion_to_settings(request.emotion.as_ref());

        let body = serde_json::json!({
            "text": request.text,
            "model_id": "eleven_multilingual_v2",
            "voice_settings": voice_settings,
        });

        let resp = self
            .client
            .post(&url)
            .header("xi-api-key", &self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Tts {
                provider: "elevenlabs".into(),
                message: e.to_string(),
            })?;

        if !resp.status().is_success() {
            return Err(AudioError::Tts {
                provider: "elevenlabs".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let pcm = resp.bytes().await.map_err(|e| AudioError::Tts {
            provider: "elevenlabs".into(),
            message: e.to_string(),
        })?;

        Ok(AudioFrame::new(pcm, 24000, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        let voice_id = if request.voice.is_empty() {
            self.voices[0].id.clone()
        } else {
            request.voice.clone()
        };
        let url = format!(
            "{}/v1/text-to-speech/{voice_id}/stream?output_format=pcm_24000",
            self.base_url()
        );
        let voice_settings = self.emotion_to_settings(request.emotion.as_ref());

        let body = serde_json::json!({
            "text": request.text,
            "model_id": "eleven_multilingual_v2",
            "voice_settings": voice_settings,
        });

        let resp = self
            .client
            .post(&url)
            .header("xi-api-key", &self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Tts {
                provider: "elevenlabs".into(),
                message: e.to_string(),
            })?;

        if !resp.status().is_success() {
            return Err(AudioError::Tts {
                provider: "elevenlabs".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let stream = async_stream::stream! {
            use futures::StreamExt;
            let mut byte_stream = resp.bytes_stream();
            while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(data) => {
                        if data.len() >= 2 {
                            yield Ok(AudioFrame::new(data, 24000, 1));
                        }
                    }
                    Err(e) => {
                        yield Err(AudioError::Tts { provider: "elevenlabs".into(), message: e.to_string() });
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}
