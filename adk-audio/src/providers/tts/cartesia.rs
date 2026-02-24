//! Cartesia Sonic TTS provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::tts::CloudTtsConfig;
use crate::traits::{TtsProvider, TtsRequest, Voice};

/// Cartesia Sonic TTS provider.
///
/// Uses the Cartesia API for ultra-low-latency voice synthesis.
/// Configure via `CARTESIA_API_KEY` environment variable or builder.
pub struct CartesiaTts {
    config: CloudTtsConfig,
    client: reqwest::Client,
    model: String,
    voices: Vec<Voice>,
}

impl CartesiaTts {
    /// Create from environment variable `CARTESIA_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("CARTESIA_API_KEY").map_err(|_| AudioError::Tts {
            provider: "cartesia".into(),
            message: "CARTESIA_API_KEY not set".into(),
        })?;
        Ok(Self::new(CloudTtsConfig::new(api_key)))
    }

    /// Create with explicit config.
    pub fn new(config: CloudTtsConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            model: "sonic-2".into(),
            voices: vec![
                Voice {
                    id: "a0e99841-438c-4a64-b679-ae501e7d6091".into(),
                    name: "Barbershop Man".into(),
                    language: "en".into(),
                    gender: Some("male".into()),
                },
                Voice {
                    id: "156fb8d2-335b-4950-9cb3-a2d33befec77".into(),
                    name: "Friendly Woman".into(),
                    language: "en".into(),
                    gender: Some("female".into()),
                },
            ],
        }
    }

    fn base_url(&self) -> &str {
        self.config.base_url.as_deref().unwrap_or("https://api.cartesia.ai")
    }
}

#[async_trait]
impl TtsProvider for CartesiaTts {
    async fn synthesize(&self, request: &TtsRequest) -> AudioResult<AudioFrame> {
        let voice_id = if request.voice.is_empty() { &self.voices[0].id } else { &request.voice };
        let url = format!("{}/tts/bytes", self.base_url());

        let body = serde_json::json!({
            "model_id": self.model,
            "transcript": request.text,
            "voice": {"mode": "id", "id": voice_id},
            "output_format": {
                "container": "raw",
                "encoding": "pcm_s16le",
                "sample_rate": 24000
            },
            "language": request.language.as_deref().unwrap_or("en"),
        });

        let resp = self
            .client
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Cartesia-Version", "2024-06-10")
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Tts { provider: "cartesia".into(), message: e.to_string() })?;

        if !resp.status().is_success() {
            return Err(AudioError::Tts {
                provider: "cartesia".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let pcm = resp
            .bytes()
            .await
            .map_err(|e| AudioError::Tts { provider: "cartesia".into(), message: e.to_string() })?;

        Ok(AudioFrame::new(pcm, 24000, 1))
    }

    async fn synthesize_stream(
        &self,
        request: &TtsRequest,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<AudioFrame>> + Send>>> {
        // Cartesia supports WebSocket streaming, but for now use batch
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[Voice] {
        &self.voices
    }
}
