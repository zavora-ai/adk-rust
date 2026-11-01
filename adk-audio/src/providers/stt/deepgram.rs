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
use crate::traits::{Speaker, SttOptions, SttProvider, Transcript, TranscriptEvent, Word};

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
            "model=nova-3".to_string(),
            "encoding=linear16".to_string(),
            "sample_rate=16000".to_string(),
            "channels=1".to_string(),
            "smart_format=true".to_string(),
            "interim_results=true".to_string(),
            // Speech-start and utterance-end events report turn boundaries.
            "vad_events=true".to_string(),
            "utterance_end_ms=1000".to_string(),
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
    fn billing(&self) -> Option<crate::AudioBilling> {
        Some(crate::AudioBilling::new("deepgram", "nova-3"))
    }

    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        assert!(self.base_url.starts_with("https://"), "Deepgram requires HTTPS");
        let wav_bytes = frame_to_wav_bytes(audio)?;

        let mut params = vec!["model=nova-3".to_string(), "smart_format=true".to_string()];
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
    ) -> AudioResult<crate::TranscriptStream> {
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

        // The sender lives as long as the transcript stream: dropping the stream aborts it,
        // which releases the caller's audio stream and closes the socket.
        let sender = AbortOnDrop(tokio::spawn(async move {
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
        }));

        // Return a stream that reads WebSocket messages and yields their events.
        let transcript_stream = async_stream::stream! {
            let _sender = sender;
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

                        for event in streaming_events(&json) {
                            yield Ok(event);
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

/// Aborts a spawned task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The events one Deepgram streaming message carries.
///
/// | Message | Events |
/// |---------|--------|
/// | `Results`, `is_final: false` | `Partial` |
/// | `Results`, `is_final: true` | `Final`, then `SpeechEnded` when `speech_final` |
/// | `SpeechStarted` | `SpeechStarted` |
/// | `UtteranceEnd` | `SpeechEnded` |
/// | `Metadata` | `Usage` with the audio duration |
///
/// Deepgram numbers no turns, so every `turn_id` is `None`; each `Final` is one finalized
/// segment of an utterance.
fn streaming_events(json: &serde_json::Value) -> Vec<TranscriptEvent> {
    let millis = |seconds: Option<f64>| seconds.map(|seconds| (seconds * 1000.0).round() as u64);
    match json.get("type").and_then(serde_json::Value::as_str) {
        Some("SpeechStarted") => vec![TranscriptEvent::SpeechStarted {
            turn_id: None,
            offset_ms: millis(json["timestamp"].as_f64()),
        }],
        Some("UtteranceEnd") => vec![TranscriptEvent::SpeechEnded {
            turn_id: None,
            offset_ms: millis(json["last_word_end"].as_f64()),
        }],
        Some("Metadata") => millis(json["duration"].as_f64())
            .map(|audio_ms| TranscriptEvent::Usage { audio_ms })
            .into_iter()
            .collect(),
        _ => {
            let mut events = Vec::new();
            if let Some(transcript) = parse_streaming_response(json) {
                events.push(if json["is_final"].as_bool() == Some(true) {
                    TranscriptEvent::Final { turn_id: None, transcript }
                } else {
                    TranscriptEvent::Partial { turn_id: None, text: transcript.text }
                });
            }
            if json["speech_final"].as_bool() == Some(true) {
                let end = json["start"].as_f64().zip(json["duration"].as_f64());
                events.push(TranscriptEvent::SpeechEnded {
                    turn_id: None,
                    offset_ms: millis(end.map(|(start, duration)| start + duration)),
                });
            }
            events
        }
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

    Some(Transcript { text, words, speakers, confidence, language_detected })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;

    /// A provider pointed at a scripted WebSocket server on `addr`.
    fn local_provider(addr: std::net::SocketAddr) -> DeepgramStt {
        DeepgramStt {
            api_key: "test-key".into(),
            client: reqwest::Client::new(),
            base_url: format!("ws://{addr}"),
        }
    }

    fn pcm_frame() -> AudioFrame {
        AudioFrame::new(vec![1_u8; 320], 16_000, 1)
    }

    #[tokio::test]
    async fn the_stream_forwards_audio_and_yields_the_servers_transcripts() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut socket = tokio_tungstenite::accept_async(tcp).await.expect("handshake");
            let mut received = Vec::new();
            while let Some(Ok(message)) = socket.next().await {
                let end = matches!(&message, Message::Text(_));
                received.push(message);
                if end {
                    break;
                }
            }
            let results = serde_json::json!({
                "type": "Results",
                "is_final": true,
                "channel": {"alternatives": [{"transcript": "hello world", "confidence": 0.9}]}
            });
            socket.send(Message::Text(results.to_string().into())).await.expect("send results");
            socket.close(None).await.expect("close");
            received
        });

        let stream = local_provider(addr)
            .transcribe_stream(
                Box::pin(futures::stream::iter([pcm_frame()])),
                &SttOptions::default(),
            )
            .await
            .expect("open stream");
        let texts: Vec<String> = stream
            .filter_map(|event| async move {
                match event.expect("event") {
                    TranscriptEvent::Final { transcript, .. } => Some(transcript.text),
                    other => panic!("only a final result was sent: {other:?}"),
                }
            })
            .collect::<Vec<_>>()
            .await;

        assert_eq!(texts, ["hello world"]);
        assert_eq!(
            server.await.expect("server"),
            [Message::Binary(pcm_frame().data), Message::Text(r#"{"type":"CloseStream"}"#.into()),]
        );
    }

    #[tokio::test]
    async fn dropping_the_stream_stops_the_audio_sender_and_closes_the_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let (first_frame_tx, first_frame_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut socket = tokio_tungstenite::accept_async(tcp).await.expect("handshake");
            let first = socket.next().await.expect("a message").expect("a frame");
            let _ = first_frame_tx.send(first);
            // Ends when the client drops the connection.
            while let Some(Ok(_)) = socket.next().await {}
        });

        // An audio source that never ends; its sender half is dropped with the stream.
        let (audio_dropped_tx, audio_dropped_rx) = oneshot::channel::<()>();
        let audio = async_stream::stream! {
            let _audio_dropped_tx = audio_dropped_tx;
            yield pcm_frame();
            std::future::pending::<()>().await;
        };

        let stream = local_provider(addr)
            .transcribe_stream(Box::pin(audio), &SttOptions::default())
            .await
            .expect("open stream");
        let first = tokio::time::timeout(Duration::from_secs(5), first_frame_rx)
            .await
            .expect("the sender forwards audio")
            .expect("first frame");
        assert_eq!(first, Message::Binary(pcm_frame().data));

        drop(stream);

        tokio::time::timeout(Duration::from_secs(5), audio_dropped_rx)
            .await
            .expect("dropping the stream releases the audio source")
            .expect_err("the audio source is dropped, not completed");
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("dropping the stream closes the socket")
            .expect("server");
    }

    /// The events of `messages`, by kind and text, for compact comparison.
    fn kinds(messages: &[serde_json::Value]) -> Vec<String> {
        messages
            .iter()
            .flat_map(streaming_events)
            .map(|event| match event {
                TranscriptEvent::Partial { text, .. } => format!("partial:{text}"),
                TranscriptEvent::Final { transcript, .. } => format!("final:{}", transcript.text),
                TranscriptEvent::SpeechStarted { offset_ms, .. } => {
                    format!("started:{offset_ms:?}")
                }
                TranscriptEvent::SpeechEnded { offset_ms, .. } => format!("ended:{offset_ms:?}"),
                TranscriptEvent::Usage { audio_ms } => format!("usage:{audio_ms}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn is_final_separates_partials_from_finals_and_speech_final_ends_the_turn() {
        let alternative = |text: &str| serde_json::json!({ "alternatives": [{ "transcript": text, "confidence": 0.9 }] });
        let messages = [
            serde_json::json!({ "type": "SpeechStarted", "timestamp": 0.25 }),
            serde_json::json!({ "type": "Results", "is_final": false, "channel": alternative("hel") }),
            serde_json::json!({ "type": "Results", "is_final": false, "channel": alternative("hello wor") }),
            serde_json::json!({
                "type": "Results", "is_final": true, "speech_final": true,
                "start": 0.25, "duration": 1.5, "channel": alternative("hello world")
            }),
            serde_json::json!({ "type": "Results", "is_final": true, "channel": alternative("") }),
            serde_json::json!({ "type": "UtteranceEnd", "last_word_end": 1.75 }),
            serde_json::json!({ "type": "Metadata", "duration": 2.0, "request_id": "r" }),
        ];
        assert_eq!(
            kinds(&messages),
            [
                "started:Some(250)",
                "partial:hel",
                "partial:hello wor",
                "final:hello world",
                "ended:Some(1750)",
                "ended:Some(1750)",
                "usage:2000",
            ]
        );
    }

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
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("channels=1"));
        assert!(url.contains("interim_results=true"));
        assert!(url.contains("vad_events=true"));
        assert!(url.contains("utterance_end_ms=1000"));
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
        let opts = SttOptions { model_hint: Some("custom-model".into()), ..Default::default() };
        let url = stt.build_ws_url(&opts);
        assert!(url.contains("model=custom-model"));
        // Should not contain the default model.
        assert!(!url.contains("model=nova-3"));
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
