//! AssemblyAI Universal STT provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::stt::frame_to_wav_bytes;
use crate::traits::{Speaker, SttOptions, SttProvider, Transcript, Word};

/// AssemblyAI Universal STT provider.
///
/// Uses the AssemblyAI async transcription API (upload → create → poll).
/// Configure via `ASSEMBLYAI_API_KEY` environment variable.
pub struct AssemblyAiStt {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
}

impl AssemblyAiStt {
    /// Create with an explicit API key (useful for testing without env vars).
    #[doc(hidden)]
    pub fn with_api_key(api_key: String) -> Self {
        Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.assemblyai.com".into(),
        }
    }

    /// Create from environment variable `ASSEMBLYAI_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("ASSEMBLYAI_API_KEY").map_err(|_| AudioError::Stt {
            provider: "assemblyai".into(),
            message: "ASSEMBLYAI_API_KEY not set".into(),
        })?;
        Ok(Self {
            api_key,
            client: reqwest::Client::new(),
            base_url: "https://api.assemblyai.com".into(),
        })
    }
}

#[async_trait]
impl SttProvider for AssemblyAiStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        let wav_bytes = frame_to_wav_bytes(audio)?;

        // Step 1: Upload audio (base_url is always HTTPS — enforced at construction)
        assert!(self.base_url.starts_with("https://"), "AssemblyAI requires HTTPS");
        let upload_url = format!("{}/v2/upload", self.base_url);
        let upload_resp = self
            .client
            .post(&upload_url)
            .header("authorization", &self.api_key)
            .header("content-type", "application/octet-stream")
            .body(wav_bytes.to_vec())
            .send()
            .await
            .map_err(|e| AudioError::Stt {
                provider: "assemblyai".into(),
                message: e.to_string(),
            })?;

        if !upload_resp.status().is_success() {
            return Err(AudioError::Stt {
                provider: "assemblyai".into(),
                message: format!("upload HTTP {}", upload_resp.status()),
            });
        }

        let upload_json: serde_json::Value = upload_resp.json().await.map_err(|e| {
            AudioError::Stt { provider: "assemblyai".into(), message: e.to_string() }
        })?;
        let audio_url = upload_json["upload_url"].as_str().ok_or_else(|| AudioError::Stt {
            provider: "assemblyai".into(),
            message: "no upload_url in response".into(),
        })?;

        // Step 2: Create transcription job
        let create_url = format!("{}/v2/transcript", self.base_url);
        let mut body = serde_json::json!({
            "audio_url": audio_url,
            "language_detection": true,
        });
        if opts.diarize {
            body["speaker_labels"] = serde_json::json!(true);
        }
        if let Some(ref lang) = opts.language {
            body["language_code"] = serde_json::json!(lang);
            body["language_detection"] = serde_json::json!(false);
        }

        let create_resp = self
            .client
            .post(&create_url)
            .header("authorization", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Stt {
                provider: "assemblyai".into(),
                message: e.to_string(),
            })?;

        if !create_resp.status().is_success() {
            return Err(AudioError::Stt {
                provider: "assemblyai".into(),
                message: format!("create HTTP {}", create_resp.status()),
            });
        }

        let create_json: serde_json::Value = create_resp.json().await.map_err(|e| {
            AudioError::Stt { provider: "assemblyai".into(), message: e.to_string() }
        })?;
        let transcript_id = create_json["id"].as_str().ok_or_else(|| AudioError::Stt {
            provider: "assemblyai".into(),
            message: "no id in response".into(),
        })?;

        // Step 3: Poll for completion
        let poll_url = format!("{}/v2/transcript/{transcript_id}", self.base_url);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let poll_resp = self
                .client
                .get(&poll_url)
                .header("authorization", &self.api_key)
                .send()
                .await
                .map_err(|e| AudioError::Stt {
                    provider: "assemblyai".into(),
                    message: e.to_string(),
                })?;

            let poll_json: serde_json::Value = poll_resp.json().await.map_err(|e| {
                AudioError::Stt { provider: "assemblyai".into(), message: e.to_string() }
            })?;

            let status = poll_json["status"].as_str().unwrap_or("unknown");
            match status {
                "completed" => {
                    return parse_assemblyai_response(&poll_json);
                }
                "error" => {
                    let error_msg = poll_json["error"].as_str().unwrap_or("unknown error");
                    return Err(AudioError::Stt {
                        provider: "assemblyai".into(),
                        message: error_msg.to_string(),
                    });
                }
                _ => continue,
            }
        }
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        Err(AudioError::Stt {
            provider: "assemblyai".into(),
            message: "streaming transcription not yet implemented".into(),
        })
    }
}

fn parse_assemblyai_response(json: &serde_json::Value) -> AudioResult<Transcript> {
    let text = json["text"].as_str().unwrap_or_default().to_string();
    let confidence = json["confidence"].as_f64().unwrap_or(0.0) as f32;
    let language_detected = json["language_code"].as_str().map(String::from);

    let words: Vec<Word> = json["words"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|w| Word {
                    text: w["text"].as_str().unwrap_or_default().to_string(),
                    start_ms: w["start"].as_u64().unwrap_or(0) as u32,
                    end_ms: w["end"].as_u64().unwrap_or(0) as u32,
                    confidence: w["confidence"].as_f64().unwrap_or(0.0) as f32,
                    speaker: w["speaker"]
                        .as_str()
                        .and_then(|s| s.strip_prefix("speaker_").and_then(|n| n.parse().ok())),
                })
                .collect()
        })
        .unwrap_or_default();

    let mut speaker_ids: Vec<u32> = words.iter().filter_map(|w| w.speaker).collect();
    speaker_ids.sort();
    speaker_ids.dedup();
    let speakers: Vec<Speaker> =
        speaker_ids.into_iter().map(|id| Speaker { id, label: None }).collect();

    Ok(Transcript { text, words, speakers, confidence, language_detected })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transcribe_stream_returns_explicit_unimplemented_error() {
        let provider = AssemblyAiStt {
            api_key: "test-key".to_string(),
            client: reqwest::Client::new(),
            base_url: "https://api.assemblyai.com".to_string(),
        };

        let result = provider
            .transcribe_stream(Box::pin(futures::stream::empty()), &SttOptions::default())
            .await;

        match result {
            Err(AudioError::Stt { provider, message }) => {
                assert_eq!(provider, "assemblyai");
                assert!(message.contains("not yet implemented"));
            }
            Err(err) => panic!("unexpected audio error: {err}"),
            Ok(_) => panic!("expected explicit STT error"),
        }
    }
}
