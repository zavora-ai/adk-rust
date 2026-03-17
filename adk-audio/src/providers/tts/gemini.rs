//! Gemini native audio TTS provider.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::tts::CloudTtsConfig;
use crate::traits::{TtsProvider, TtsRequest, Voice};

/// Gemini TTS provider using `generateContent` with audio response modality.
pub struct GeminiTts {
    config: CloudTtsConfig,
    client: reqwest::Client,
    model: String,
    voices: Vec<Voice>,
}

impl GeminiTts {
    /// Create from environment variable `GEMINI_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("GEMINI_API_KEY").map_err(|_| AudioError::Tts {
            provider: "gemini".into(),
            message: "GEMINI_API_KEY not set".into(),
        })?;
        Ok(Self::new(CloudTtsConfig::new(api_key)))
    }

    /// Create with explicit config.
    pub fn new(config: CloudTtsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            model: "gemini-2.5-flash-preview-tts".into(),
            voices: vec![
                Voice {
                    id: "Puck".into(),
                    name: "Puck".into(),
                    language: "en".into(),
                    gender: Some("male".into()),
                },
                Voice {
                    id: "Charon".into(),
                    name: "Charon".into(),
                    language: "en".into(),
                    gender: Some("male".into()),
                },
                Voice {
                    id: "Kore".into(),
                    name: "Kore".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
                Voice {
                    id: "Fenrir".into(),
                    name: "Fenrir".into(),
                    language: "en".into(),
                    gender: Some("male".into()),
                },
                Voice {
                    id: "Aoede".into(),
                    name: "Aoede".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
            ],
        }
    }

    fn base_url(&self) -> String {
        self.config.base_url.clone().unwrap_or_else(|| {
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                self.model
            )
        })
    }
}

#[async_trait]
impl TtsProvider for GeminiTts {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let voice = if request.voice.is_empty() { "Puck" } else { &request.voice };
        let url = self.base_url();

        let body = serde_json::json!({
            "contents": [{"parts": [{"text": request.text}]}],
            "generationConfig": {
                "response_modalities": ["AUDIO"],
                "speech_config": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {
                            "voiceName": voice
                        }
                    }
                }
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

        // Extract base64 audio from response
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
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}
