//! OpenAI TTS provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::tts::CloudTtsConfig;
use crate::traits::{TtsProvider, TtsRequest, Voice};

/// OpenAI TTS provider using the `/v1/audio/speech` endpoint.
pub struct OpenAiTts {
    config: CloudTtsConfig,
    client: reqwest::Client,
    model: String,
    voices: Vec<Voice>,
}

impl OpenAiTts {
    /// Create from environment variable `OPENAI_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| AudioError::Tts {
            provider: "openai".into(),
            message: "OPENAI_API_KEY not set".into(),
        })?;
        Ok(Self::new(CloudTtsConfig::new(api_key)))
    }

    /// Create with explicit config.
    pub fn new(config: CloudTtsConfig) -> Self {
        let voices = vec![
            Voice { id: "alloy".into(), name: "Alloy".into(), language: "en".into(), gender: None },
            Voice {
                id: "echo".into(),
                name: "Echo".into(),
                language: "en".into(),
                gender: Some("male".into()),
            },
            Voice { id: "fable".into(), name: "Fable".into(), language: "en".into(), gender: None },
            Voice {
                id: "onyx".into(),
                name: "Onyx".into(),
                language: "en".into(),
                gender: Some("male".into()),
            },
            Voice {
                id: "nova".into(),
                name: "Nova".into(),
                language: "en".into(),
                gender: Some("female".into()),
            },
            Voice {
                id: "shimmer".into(),
                name: "Shimmer".into(),
                language: "en".into(),
                gender: Some("female".into()),
            },
        ];
        Self { config, client: reqwest::Client::new(), model: "tts-1".into(), voices }
    }

    /// Use the HD model (tts-1-hd) for higher quality.
    pub fn hd(mut self) -> Self {
        self.model = "tts-1-hd".into();
        self
    }

    fn base_url(&self) -> &str {
        self.config.base_url.as_deref().unwrap_or("https://api.openai.com")
    }
}

#[async_trait]
impl TtsProvider for OpenAiTts {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let voice = if request.voice.is_empty() { "alloy" } else { &request.voice };
        let url = format!("{}/v1/audio/speech", self.base_url());

        let body = serde_json::json!({
            "model": self.model,
            "input": request.text,
            "voice": voice,
            "response_format": "pcm",
            "speed": request.speed,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Tts { provider: "openai".into(), message: e.to_string() })?;

        if !resp.status().is_success() {
            return Err(AudioError::Tts {
                provider: "openai".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let pcm = resp
            .bytes()
            .await
            .map_err(|e| AudioError::Tts { provider: "openai".into(), message: e.to_string() })?;

        Ok(AudioFrame::new(pcm, 24000, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        // OpenAI TTS doesn't have a native streaming endpoint for PCM,
        // so we fetch the full response and yield it as a single frame.
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}
