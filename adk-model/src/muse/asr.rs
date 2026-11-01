//! Speech-to-text via the Meta Model API (`muse-voice-transcribe-1.0`).
//!
//! File transcription: `POST /v1/asr/transcribe` (multipart upload). Audio in,
//! transcript text out — 25 languages with code-switching, diarization, and
//! turn-level timestamps. File audio: WAV mono PCM16 at 16/24kHz.
//!
//! Live transcription over the realtime WebSocket is `MuseRealtimeAsrClient` (feature
//! `muse-realtime`); this client covers the file path.
//!
//! Every transcription is metered against the current run ([`adk_core::meter_usage`]): the
//! spend hold covers the WAV file's duration (or the 10-minute file limit when the header
//! cannot be read), and the reported audio duration is committed at $0.18 per hour, in
//! whole seconds rounded down. A call refused by the run's budget or spend ledger is not
//! sent.

use super::config::MuseConfig;
use crate::pricing::PricingCatalog;
use adk_core::{AdkError, ErrorComponent, MeteredUsage, UsageCall, UsageUnit, UsageVector};
use reqwest::Client;
use serde::Deserialize;

/// Model served by the ASR endpoints.
pub const MUSE_VOICE_MODEL: &str = "muse-voice-transcribe-1.0";

/// Longest audio `/asr/transcribe` accepts: "Maximum 10 minutes (longer returns `400`)".
const MAX_FILE_AUDIO_MS: u64 = 10 * 60 * 1_000;

/// One speech turn (turn-level timestamps; word-level is not available).
#[derive(Debug, Clone, Deserialize)]
pub struct TranscriptSegment {
    /// Turn id.
    #[serde(default, rename = "turnId")]
    pub turn_id: Option<u32>,
    /// Turn text.
    #[serde(default, rename = "transcript")]
    pub text: String,
    /// Speaker label (`DIARIZATION` only).
    #[serde(default)]
    pub speaker: Option<String>,
    /// Start offset in milliseconds.
    #[serde(default, rename = "startMs")]
    pub start_ms: Option<u64>,
    /// End offset in milliseconds.
    #[serde(default, rename = "endMs")]
    pub end_ms: Option<u64>,
}

/// Transcription result.
#[derive(Debug, Clone, Deserialize)]
pub struct Transcript {
    /// Session id for the transcription.
    #[serde(default, rename = "sessionId")]
    pub session_id: Option<String>,
    /// Full transcript text (present in every mode).
    #[serde(default, rename = "transcript")]
    pub text: String,
    /// Audio duration in milliseconds.
    #[serde(default, rename = "audioDurationMs")]
    pub duration_ms: Option<u64>,
    /// Detected speech turns (empty in `PUSH_TO_TALK` mode).
    #[serde(default, rename = "turns")]
    pub segments: Vec<TranscriptSegment>,
}

/// Options for one transcription.
#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// Session mode fixed at start (`PUSH_TO_TALK`, `ENDPOINTING`, `DIARIZATION`).
    pub mode: Option<String>,
    /// Language bias hints (BCP-47 codes).
    pub language_bias: Vec<String>,
    /// Keyword hints for domain vocabulary.
    pub keywords: Vec<String>,
}

/// ASR client for `muse-voice-transcribe-1.0`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseAsrClient, MuseConfig};
///
/// let asr = MuseAsrClient::new(MuseConfig::new(api_key, "muse-voice-transcribe-1.0"))?;
/// let transcript = asr.transcribe(wav_bytes, "memo.wav", "audio/wav", Default::default()).await?;
/// ```
pub struct MuseAsrClient {
    client: Client,
    config: MuseConfig,
}

impl MuseAsrClient {
    /// Create an ASR client. The config model defaults to `muse-voice-transcribe-1.0`.
    pub fn new(mut config: MuseConfig) -> Result<Self, AdkError> {
        if config.model.is_empty() || config.model.starts_with("muse-spark") {
            config.model = MUSE_VOICE_MODEL.to_string();
        }
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        Ok(Self { client, config })
    }

    /// Transcribe an audio file (multipart upload).
    ///
    /// # Errors
    ///
    /// Returns the API error (HTTP status, code `model.muse.api_error`) on rejection, or
    /// the current run's refusal (spend limit, budget) before anything is sent.
    pub async fn transcribe(
        &self,
        audio: Vec<u8>,
        filename: impl Into<String>,
        content_type: &str,
        options: TranscribeOptions,
    ) -> Result<Transcript, AdkError> {
        let catalog = PricingCatalog::standard();
        let price = |audio_ms: u64| {
            let units = UsageVector::new().with(UsageUnit::AudioMillis, audio_ms);
            catalog.unit_cost_micro_usd(Some("muse"), &self.config.model, &units)
        };
        let longest = wav_duration_ms(&audio).unwrap_or(MAX_FILE_AUDIO_MS).min(MAX_FILE_AUDIO_MS);
        let mut call = UsageCall::new("muse", self.config.model.clone());
        if let Some(bound) = price(longest) {
            call = call.with_max_cost_micro_usd(bound);
        }
        adk_core::meter_usage(call, async {
            let transcript = self.send(audio, filename.into(), content_type, &options).await?;
            let audio_ms = transcript.duration_ms.unwrap_or(longest);
            let usage =
                MeteredUsage::new(UsageVector::new().with(UsageUnit::AudioMillis, audio_ms));
            let usage = match price(audio_ms) {
                Some(cost) => usage.with_cost_micro_usd(cost),
                None => usage,
            };
            Ok((transcript, usage))
        })
        .await
    }

    async fn send(
        &self,
        audio: Vec<u8>,
        filename: String,
        content_type: &str,
        options: &TranscribeOptions,
    ) -> Result<Transcript, AdkError> {
        let mut settings = serde_json::json!({
            "model": self.config.model,
            "audioEncoding": "WAV",
        });
        if let Some(mode) = &options.mode {
            settings["mode"] = mode.clone().into();
        }
        if !options.language_bias.is_empty() {
            settings["languageBias"] = options.language_bias.clone().into();
        }
        if !options.keywords.is_empty() {
            settings["keywords"] = options.keywords.clone().into();
        }
        let request_part = reqwest::multipart::Part::text(settings.to_string())
            .mime_str("application/json")
            .map_err(|e| AdkError::model(format!("invalid request part: {e}")))?;
        let audio_part = reqwest::multipart::Part::bytes(audio)
            .file_name(filename)
            .mime_str(content_type)
            .map_err(|e| AdkError::model(format!("invalid audio content type: {e}")))?;
        let form =
            reqwest::multipart::Form::new().part("request", request_part).part("audio", audio_part);
        let response = self
            .client
            .post(format!("{}/asr/transcribe", self.base()))
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Model,
                    crate::http_timeouts::transport_category(&e),
                    "model.muse.request",
                    format!("Muse ASR request failed: {e}"),
                )
                .with_provider("muse")
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let status_code = status.as_u16();
            let error_text = response.text().await.unwrap_or_default();
            return Err(AdkError::new(
                ErrorComponent::Model,
                crate::retry::category_for_status_code(status_code),
                "model.muse.api_error",
                format!(
                    "Muse ASR API error (HTTP {status}): {}",
                    adk_core::upstream_excerpt(&error_text)
                ),
            )
            .with_upstream_status(status_code)
            .with_provider("muse"));
        }
        let text = response
            .text()
            .await
            .map_err(|e| AdkError::model(format!("Failed to read ASR response: {e}")))?;
        serde_json::from_str(&text).map_err(|e| {
            AdkError::model(format!(
                "Failed to parse ASR response: {}",
                adk_core::json_error_summary(&e)
            ))
        })
    }

    fn base(&self) -> String {
        self.config.effective_base_url().trim_end_matches('/').to_string()
    }
}

/// Duration of a RIFF WAV file from its `fmt ` byte rate and `data` length, rounded up to
/// a whole millisecond. `None` when the header cannot be read.
fn wav_duration_ms(wav: &[u8]) -> Option<u64> {
    if wav.get(0..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut offset = 12usize;
    let mut byte_rate = None;
    while let Some(header) = wav.get(offset..offset + 8) {
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let body = offset + 8;
        match &header[0..4] {
            b"fmt " => {
                byte_rate =
                    Some(u32::from_le_bytes(wav.get(body + 8..body + 12)?.try_into().ok()?));
            }
            b"data" => {
                let rate = u64::from(byte_rate.filter(|rate| *rate > 0)?);
                // A streamed WAV can declare a placeholder length; the bytes present count.
                let length = size.min(wav.len() - body) as u64;
                return Some((length * 1_000).div_ceil(rate));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        offset = body.checked_add(size)?.checked_add(size % 2)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(sample_rate: u32, samples: usize) -> Vec<u8> {
        let data = samples * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data as u32).to_le_bytes());
        wav.resize(44 + data, 0);
        wav
    }

    #[test]
    fn the_wav_header_gives_the_duration() {
        assert_eq!(wav_duration_ms(&wav(16_000, 16_000 * 90)), Some(90_000));
        assert_eq!(wav_duration_ms(&wav(24_000, 12)), Some(1));
        assert_eq!(wav_duration_ms(b"not a wav file"), None);
        let mut truncated = wav(16_000, 16_000);
        truncated.truncate(44 + 16_000);
        assert_eq!(wav_duration_ms(&truncated), Some(500));
    }

    #[test]
    fn transcript_parses_text_and_segments() {
        let parsed: Transcript = serde_json::from_str(
            r#"{"sessionId":"s1","transcript":"hello world","audioDurationMs":1200,"turns":[{"turnId":1,"startMs":0,"endMs":500,"transcript":"hello","speaker":"A"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.text, "hello world");
        assert_eq!(parsed.duration_ms, Some(1200));
        assert_eq!(parsed.segments.len(), 1);
        assert_eq!(parsed.segments[0].turn_id, Some(1));
        assert_eq!(parsed.segments[0].speaker.as_deref(), Some("A"));
    }
}
