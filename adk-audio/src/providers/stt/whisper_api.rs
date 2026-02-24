//! OpenAI Whisper API STT provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::stt::frame_to_wav_bytes;
use crate::traits::{SttOptions, SttProvider, Transcript, Word};

/// OpenAI Whisper API STT provider.
///
/// Uses the `/v1/audio/transcriptions` endpoint.
/// Configure via `OPENAI_API_KEY` environment variable.
pub struct WhisperApiStt {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl WhisperApiStt {
    /// Create from environment variable `OPENAI_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| AudioError::Stt {
            provider: "whisper".into(),
            message: "OPENAI_API_KEY not set".into(),
        })?;
        Ok(Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.openai.com".into(),
            model: "whisper-1".into(),
        })
    }
}

#[async_trait]
impl SttProvider for WhisperApiStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        let wav_bytes = frame_to_wav_bytes(audio)?;
        let url = format!("{}/v1/audio/transcriptions", self.base_url);

        let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| AudioError::Stt { provider: "whisper".into(), message: e.to_string() })?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "verbose_json")
            .part("file", part);

        if let Some(ref lang) = opts.language {
            form = form.text("language", lang.clone());
        }
        if opts.word_timestamps {
            form = form.text("timestamp_granularities[]", "word");
        }

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| AudioError::Stt {
            provider: "whisper".into(),
            message: e.to_string(),
        })?;

        if !resp.status().is_success() {
            return Err(AudioError::Stt {
                provider: "whisper".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AudioError::Stt { provider: "whisper".into(), message: e.to_string() })?;

        let text = json["text"].as_str().unwrap_or_default().to_string();
        let language_detected = json["language"].as_str().map(String::from);

        let words = json["words"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|w| Word {
                        text: w["word"].as_str().unwrap_or_default().to_string(),
                        start_ms: (w["start"].as_f64().unwrap_or(0.0) * 1000.0) as u32,
                        end_ms: (w["end"].as_f64().unwrap_or(0.0) * 1000.0) as u32,
                        confidence: 1.0,
                        speaker: None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Transcript { text, words, speakers: vec![], confidence: 1.0, language_detected })
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        // Whisper API doesn't support native streaming; use windowed fallback
        Ok(Box::pin(futures::stream::empty()))
    }
}
