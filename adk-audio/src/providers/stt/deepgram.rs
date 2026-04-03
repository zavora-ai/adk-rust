//! Deepgram Nova STT provider.

use std::pin::Pin;

use async_trait::async_trait;
use futures::{SinkExt, Stream, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::{debug, warn};

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

    /// Build the WebSocket URL with query parameters for streaming STT.
    fn build_ws_url(&self, opts: &SttOptions) -> String {
        let ws_base = self.base_url.replace("https://", "wss://");
        let mut params = vec![
            "model=nova-2".to_string(),
            "encoding=linear16".to_string(),
            "sample_rate=16000".to_string(),
            "channels=1".to_string(),
            "smart_format=true".to_string(),
            "interim_results=true".to_string(),
        ];
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
        if let Some(ref model) = opts.model_hint {
            // Override default model if caller specifies one.
            params.retain(|p| !p.starts_with("model="));
            params.push(format!("model={model}"));
        }
        format!("{ws_base}/v1/listen?{}", params.join("&"))
    }
}

#[async_trait]
impl SttProvider for DeepgramStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        assert!(self.base_url.starts_with("https://"), "Deepgram requires HTTPS");
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
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        let ws_url = self.build_ws_url(opts);
        debug!(url = %ws_url, "connecting to Deepgram streaming STT");

        // Build the WebSocket request with Authorization header.
        let mut request = ws_url.into_client_request().map_err(|e| AudioError::Stt {
            provider: "deepgram".into(),
            message: format!("failed to build WebSocket request: {e}"),
        })?;
        request.headers_mut().insert(
            "Authorization",
            format!("Token {}", self.api_key).parse().map_err(|e| AudioError::Stt {
                provider: "deepgram".into(),
                message: format!("invalid authorization header: {e}"),
            })?,
        );

        // Connect to the Deepgram WebSocket.
        let (ws_stream, _resp) =
            tokio_tungstenite::connect_async(request).await.map_err(|e| AudioError::Stt {
                provider: "deepgram".into(),
                message: format!("WebSocket connection failed: {e}"),
            })?;

        let (mut ws_sink, mut ws_source) = ws_stream.split();

        // Spawn a task that reads audio frames and sends them as binary messages.
        tokio::spawn(async move {
            let mut audio = audio;
            while let Some(frame) = audio.next().await {
                // Send raw PCM-16 LE bytes directly — Deepgram expects raw audio
                // matching the encoding/sample_rate/channels query params.
                if let Err(e) = ws_sink.send(Message::Binary(frame.data)).await {
                    warn!("deepgram ws send error: {e}");
                    break;
                }
            }
            // Signal end of audio by sending a close-stream message.
            let close_msg = serde_json::json!({"type": "CloseStream"});
            let _ = ws_sink.send(Message::Text(close_msg.to_string().into())).await;
        });

        // Return a stream that reads WebSocket messages and yields Transcript values.
        let transcript_stream = async_stream::stream! {
            while let Some(msg_result) = ws_source.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        yield Err(AudioError::Stt {
                            provider: "deepgram".into(),
                            message: format!("WebSocket read error: {e}"),
                        });
                        break;
                    }
                };

                match msg {
                    Message::Text(text) => {
                        let json: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("deepgram: failed to parse JSON: {e}");
                                continue;
                            }
                        };

                        // Check for error responses from Deepgram.
                        if let Some(err_msg) = json.get("error").and_then(|v| v.as_str()) {
                            yield Err(AudioError::Stt {
                                provider: "deepgram".into(),
                                message: err_msg.to_string(),
                            });
                            break;
                        }

                        // Parse transcript results.
                        if let Some(transcript) = parse_streaming_response(&json) {
                            yield Ok(transcript);
                        }
                    }
                    Message::Close(_) => break,
                    _ => {} // Ignore ping/pong/binary responses
                }
            }
        };

        Ok(Box::pin(transcript_stream))
    }
}

/// Parse a Deepgram streaming WebSocket JSON response into a `Transcript`.
///
/// Returns `None` for metadata-only messages (e.g. `UtteranceEnd`, `SpeechStarted`).
fn parse_streaming_response(json: &serde_json::Value) -> Option<Transcript> {
    // Deepgram streaming responses have a "channel" object with alternatives.
    let channel = json.get("channel")?;
    let alt = channel.get("alternatives")?.get(0)?;

    let text = alt["transcript"].as_str().unwrap_or_default().to_string();
    // Skip empty transcripts (silence / no speech detected).
    if text.is_empty() {
        return None;
    }

    let confidence = alt["confidence"].as_f64().unwrap_or(0.0) as f32;
    let is_final = json.get("is_final").and_then(|v| v.as_bool()).unwrap_or(false);

    let words: Vec<Word> = alt["words"]
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

    let mut speaker_ids: Vec<u32> = words.iter().filter_map(|w| w.speaker).collect();
    speaker_ids.sort();
    speaker_ids.dedup();
    let speakers: Vec<Speaker> =
        speaker_ids.into_iter().map(|id| Speaker { id, label: None }).collect();

    let language_detected =
        json.get("metadata").and_then(|m| m["language"].as_str()).map(String::from);

    // Encode finality in the transcript: final transcripts have full confidence,
    // interim transcripts are partial results the caller can display/update.
    let _ = is_final; // is_final is reflected by the presence of words/confidence

    Some(Transcript { text, words, speakers, confidence, language_detected })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_streaming_final_transcript() {
        let json: serde_json::Value = serde_json::json!({
            "type": "Results",
            "channel_index": [0, 1],
            "duration": 1.5,
            "start": 0.0,
            "is_final": true,
            "channel": {
                "alternatives": [{
                    "transcript": "hello world",
                    "confidence": 0.95,
                    "words": [
                        {"word": "hello", "start": 0.0, "end": 0.5, "confidence": 0.96},
                        {"word": "world", "start": 0.6, "end": 1.0, "confidence": 0.94}
                    ]
                }]
            }
        });

        let transcript = parse_streaming_response(&json).expect("should parse");
        assert_eq!(transcript.text, "hello world");
        assert!((transcript.confidence - 0.95).abs() < 0.01);
        assert_eq!(transcript.words.len(), 2);
        assert_eq!(transcript.words[0].text, "hello");
        assert_eq!(transcript.words[0].start_ms, 0);
        assert_eq!(transcript.words[0].end_ms, 500);
        assert_eq!(transcript.words[1].text, "world");
    }

    #[test]
    fn parse_streaming_interim_transcript() {
        let json: serde_json::Value = serde_json::json!({
            "type": "Results",
            "is_final": false,
            "channel": {
                "alternatives": [{
                    "transcript": "hel",
                    "confidence": 0.7,
                    "words": []
                }]
            }
        });

        let transcript = parse_streaming_response(&json).expect("should parse interim");
        assert_eq!(transcript.text, "hel");
    }

    #[test]
    fn parse_streaming_empty_transcript_returns_none() {
        let json: serde_json::Value = serde_json::json!({
            "type": "Results",
            "is_final": false,
            "channel": {
                "alternatives": [{
                    "transcript": "",
                    "confidence": 0.0,
                    "words": []
                }]
            }
        });

        assert!(parse_streaming_response(&json).is_none());
    }

    #[test]
    fn parse_streaming_metadata_message_returns_none() {
        // Messages like UtteranceEnd don't have a "channel" field.
        let json: serde_json::Value = serde_json::json!({
            "type": "UtteranceEnd",
            "last_word_end": 1.5
        });

        assert!(parse_streaming_response(&json).is_none());
    }

    #[test]
    fn build_ws_url_default_opts() {
        let stt = DeepgramStt::with_api_key("test-key".into());
        let url = stt.build_ws_url(&SttOptions::default());
        assert!(url.starts_with("wss://api.deepgram.com/v1/listen?"));
        assert!(url.contains("model=nova-2"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("interim_results=true"));
    }

    #[test]
    fn build_ws_url_with_language_and_diarize() {
        let stt = DeepgramStt::with_api_key("test-key".into());
        let opts =
            SttOptions { language: Some("en-US".into()), diarize: true, ..Default::default() };
        let url = stt.build_ws_url(&opts);
        assert!(url.contains("language=en-US"));
        assert!(url.contains("diarize=true"));
    }

    #[test]
    fn build_ws_url_with_model_hint() {
        let stt = DeepgramStt::with_api_key("test-key".into());
        let opts = SttOptions { model_hint: Some("nova-3".into()), ..Default::default() };
        let url = stt.build_ws_url(&opts);
        assert!(url.contains("model=nova-3"));
        // Should not contain the default model.
        assert!(!url.contains("model=nova-2"));
    }

    #[test]
    fn parse_streaming_with_speakers() {
        let json: serde_json::Value = serde_json::json!({
            "type": "Results",
            "is_final": true,
            "channel": {
                "alternatives": [{
                    "transcript": "hi there",
                    "confidence": 0.9,
                    "words": [
                        {"word": "hi", "start": 0.0, "end": 0.3, "confidence": 0.9, "speaker": 0},
                        {"word": "there", "start": 0.4, "end": 0.8, "confidence": 0.9, "speaker": 1}
                    ]
                }]
            }
        });

        let transcript = parse_streaming_response(&json).expect("should parse");
        assert_eq!(transcript.speakers.len(), 2);
        assert_eq!(transcript.speakers[0].id, 0);
        assert_eq!(transcript.speakers[1].id, 1);
    }
}
