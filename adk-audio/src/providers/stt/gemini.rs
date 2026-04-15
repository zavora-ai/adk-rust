//! Gemini audio understanding STT provider.
//!
//! Uses the `generateContent` API with audio input to transcribe speech.
//! Gemini models can process audio inline and return text transcriptions.
//!
//! This is a batch-mode provider — audio is sent as a single request and
//! the full transcript is returned. For real-time streaming transcription,
//! use the Gemini Live API via `adk-realtime`.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;
use crate::providers::stt::frame_to_wav_bytes;
use crate::traits::{SttOptions, SttProvider, Transcript};

/// Default model for Gemini STT (audio understanding).
const DEFAULT_MODEL: &str = "gemini-3-flash-preview";

/// Gemini STT provider using `generateContent` with audio input.
///
/// Sends audio as inline data to the Gemini API and receives a text
/// transcription. Supports language detection and optional prompting
/// for specialized transcription tasks.
///
/// # Example
///
/// ```rust,ignore
/// use adk_audio::{GeminiStt, SttProvider, SttOptions, AudioFrame};
///
/// let stt = GeminiStt::from_env()?;
/// let transcript = stt.transcribe(&audio_frame, &SttOptions::default()).await?;
/// println!("Transcribed: {}", transcript.text);
/// ```
pub struct GeminiStt {
    api_key: String,
    client: reqwest::Client,
    model: String,
    /// Optional custom prompt for transcription.
    prompt: String,
}

impl GeminiStt {
    /// Create from environment variable `GEMINI_API_KEY` or `GOOGLE_API_KEY`.
    pub fn from_env() -> AudioResult<Self> {
        let api_key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| AudioError::Stt {
                provider: "gemini".into(),
                message: "GEMINI_API_KEY or GOOGLE_API_KEY not set".into(),
            })?;
        Ok(Self {
            api_key,
            client: reqwest::Client::new(),
            model: DEFAULT_MODEL.into(),
            prompt: "Transcribe this audio accurately. Return only the transcription text, no commentary.".into(),
        })
    }

    /// Create with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            client: reqwest::Client::new(),
            model: DEFAULT_MODEL.into(),
            prompt: "Transcribe this audio accurately. Return only the transcription text, no commentary.".into(),
        }
    }

    /// Set the model to use for transcription.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set a custom transcription prompt.
    ///
    /// The prompt is sent alongside the audio to guide the model's output.
    /// For example: "Transcribe this audio in English with punctuation."
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = prompt.into();
        self
    }

    fn url(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model
        )
    }
}

#[async_trait]
impl SttProvider for GeminiStt {
    async fn transcribe(&self, audio: &AudioFrame, opts: &SttOptions) -> AudioResult<Transcript> {
        let wav_bytes = frame_to_wav_bytes(audio)?;

        use base64::Engine;
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav_bytes);

        // Build the prompt with optional language hint
        let prompt = if let Some(ref lang) = opts.language {
            format!("{} The audio is in {lang}.", self.prompt)
        } else {
            self.prompt.clone()
        };

        let body = serde_json::json!({
            "contents": [{
                "parts": [
                    {"text": prompt},
                    {
                        "inlineData": {
                            "mimeType": "audio/wav",
                            "data": audio_b64
                        }
                    }
                ]
            }]
        });

        let resp = self
            .client
            .post(&self.url())
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AudioError::Stt { provider: "gemini".into(), message: e.to_string() })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AudioError::Stt {
                provider: "gemini".into(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AudioError::Stt { provider: "gemini".into(), message: e.to_string() })?;

        let text = json["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();

        Ok(Transcript {
            text,
            words: vec![],
            speakers: vec![],
            confidence: 1.0,
            language_detected: opts.language.clone(),
        })
    }

    async fn transcribe_stream(
        &self,
        _audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>> {
        // Gemini generateContent doesn't support streaming STT.
        // For real-time streaming, use adk-realtime with Gemini Live.
        Ok(Box::pin(futures::stream::empty()))
    }
}
