//! Deepgram Nova STT provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::stt::frame_to_wav_bytes;
use crate::traits::{Speaker, SttOptions, SttProvider, Transcript, Word};

/// Deepgram Nova STT provider.
///
/// Uses the Deepgram `/v1/listen` endpoint.
/// Configure via `DEEPGRAM_API_KEY` environment variable.
pub struct DeepgramStt {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
}

impl DeepgramStt {
    /// Create with an explicit API key (useful for testing without env vars).
    #[doc(hidden)]
    pub fn with_api_key(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.deepgram.com".into(),
        }
    }

    /// Create from environment variable `DEEPGRAM_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("DEEPGRAM_API_KEY").map_err(|_| AudioError::Stt {
            provider: "deepgram".into(),
            message: "DEEPGRAM_API_KEY not set".into(),
        })?;
        Ok(Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.deepgram.com".into(),
        })
    }
}

#[async_trait]
impl SttProvider for DeepgramStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        let wav_bytes = frame_to_wav_bytes(audio)?;

        let mut params = vec!["model=nova-2".to_string(), "smart_format=true".to_string()];
        if opts.diarize {
            params.push("diarize=true".to_string());
        }
        if opts.word_timestamps {
            params.push("utterances=true".to_string());
        }
        if let Some(ref lang) = opts.language {
            params.push(format!("language={lang}"));
        }
        if opts.smart_format {
            params.push("punctuate=true".to_string());
        }

        let url = format!("{}/v1/listen?{}", self.base_url, params.join("&"));

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", "audio/wav")
            .body(wav_bytes.to_vec())
            .send()
            .await
            .map_err(|e| AudioError::Stt { provider: "deepgram".into(), message: e.to_string() })?;

        if !resp.status().is_success() {
            return Err(AudioError::Stt {
                provider: "deepgram".into(),
                message: format!("HTTP {}", resp.status()),
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AudioError::Stt { provider: "deepgram".into(), message: e.to_string() })?;

        let channel = &json["results"]["channels"][0]["alternatives"][0];
        let text = channel["transcript"].as_str().unwrap_or_default().to_string();
        let confidence = channel["confidence"].as_f64().unwrap_or(0.0) as f32;

        let words: Vec<Word> = channel["words"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|w| Word {
                        text: w["word"].as_str().unwrap_or_default().to_string(),
                        start_ms: (w["start"].as_f64().unwrap_or(0.0) * 1000.0) as u32,
                        end_ms: (w["end"].as_f64().unwrap_or(0.0) * 1000.0) as u32,
                        confidence: w["confidence"].as_f64().unwrap_or(0.0) as f32,
                        speaker: w["speaker"].as_u64().map(|s| s as u32),
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Extract unique speakers
        let mut speaker_ids: Vec<u32> = words.iter().filter_map(|w| w.speaker).collect();
        speaker_ids.sort();
        speaker_ids.dedup();
        let speakers: Vec<Speaker> =
            speaker_ids.into_iter().map(|id| Speaker { id, label: None }).collect();

        let language_detected =
            json["results"]["channels"][0]["detected_language"].as_str().map(String::from);

        Ok(Transcript { text, words, speakers, confidence, language_detected })
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Err(AudioError::Stt {
            provider: "deepgram".into(),
            message: "streaming transcription not yet implemented".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transcribe_stream_returns_explicit_unimplemented_error() {
        let provider = DeepgramStt {
            api_key: "test-key".to_string(),
            client: reqwest::Client::new(),
            base_url: "https://api.deepgram.com".to_string(),
        };

        let result = provider
            .transcribe_stream(Box::pin(futures::stream::empty()), &SttOptions::default())
            .await;

        match result {
            Err(AudioError::Stt { provider, message }) => {
                assert_eq!(provider, "deepgram");
                assert!(message.contains("not yet implemented"));
            }
            Err(err) => panic!("unexpected audio error: {err}"),
            Ok(_) => panic!("expected explicit STT error"),
        }
    }
}
