//! OpenAI WebRTC transport implementation.
//!
//! This module provides the WebRTC-based transport for OpenAI's Realtime API,
//! offering lower-latency audio compared to the WebSocket transport.
//!
//! The module includes:
//! - [`OpusCodec`] — Opus encoder/decoder wrapping `audiopus` for PCM16 ↔ Opus conversion.
//! - [`OpenAIWebRTCSession`] — WebRTC session implementing `RealtimeSession` via `str0m`.

use std::convert::TryFrom;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Channels, MutSignals, SampleRate};
use str0m::Rtc;
use str0m::change::SdpAnswer;
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid, Pt};
use tokio::sync::{Mutex, mpsc};

use std::sync::atomic::AtomicU64;

use crate::audio::AudioChunk;
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ConversationItem, ServerEvent, ToolResponse};
use crate::session::RealtimeSession;

/// Maximum size of an encoded Opus frame in bytes.
///
/// Opus frames are typically much smaller than this, but we allocate a
/// generous buffer to avoid truncation. 4000 bytes is well above the
/// maximum possible Opus frame size for typical audio configurations.
const MAX_OPUS_FRAME_BYTES: usize = 4000;

/// Maximum number of decoded samples per channel per frame.
///
/// At 48 kHz with 120ms frames (the maximum Opus frame duration),
/// this would be 5760 samples. We use 5760 as a safe upper bound.
const MAX_DECODED_SAMPLES_PER_CHANNEL: usize = 5760;

/// Opus audio codec for encoding PCM16 to Opus and decoding Opus to PCM16.
///
/// Wraps `audiopus` encoder and decoder, configured for a specific sample rate
/// and channel count. Used internally by `OpenAIWebRTCSession` to transcode
/// audio between the PCM16 format used by `adk-realtime` and the Opus format
/// required by WebRTC media tracks.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::openai::webrtc::OpusCodec;
///
/// let mut codec = OpusCodec::new(24000, 1)?;
///
/// // Encode PCM16 samples to Opus
/// let pcm: Vec<i16> = vec![0; 480]; // 20ms at 24kHz mono
/// let opus_data = codec.encode(&pcm)?;
///
/// // Decode Opus back to PCM16
/// let decoded = codec.decode(&opus_data)?;
/// ```
pub struct OpusCodec {
    encoder: Encoder,
    decoder: Decoder,
    sample_rate: SampleRate,
    channels: Channels,
}

impl OpusCodec {
    /// Creates a new Opus codec with the given sample rate and channel count.
    ///
    /// The encoder is configured for VoIP application mode, which is optimized
    /// for speech and low-latency communication.
    ///
    /// # Arguments
    ///
    /// * `sample_rate` — Sample rate in Hz. Must be one of: 8000, 12000, 16000, 24000, 48000.
    /// * `channels` — Number of audio channels. Must be 1 (mono) or 2 (stereo).
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError::OpusCodecError` if the sample rate or channel count
    /// is not supported by Opus, or if encoder/decoder creation fails.
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let sample_rate = SampleRate::try_from(sample_rate as i32)
            .map_err(|e| RealtimeError::opus(format!("Invalid sample rate {sample_rate}: {e}")))?;

        let channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            other => {
                return Err(RealtimeError::opus(format!(
                    "Invalid channel count {other}: must be 1 (mono) or 2 (stereo)"
                )));
            }
        };

        let encoder = Encoder::new(sample_rate, channels, Application::Voip)
            .map_err(|e| RealtimeError::opus(format!("Failed to create Opus encoder: {e}")))?;

        let decoder = Decoder::new(sample_rate, channels)
            .map_err(|e| RealtimeError::opus(format!("Failed to create Opus decoder: {e}")))?;

        Ok(Self { encoder, decoder, sample_rate, channels })
    }

    /// Encodes PCM16 audio samples to an Opus frame.
    ///
    /// The input must contain a valid number of samples for an Opus frame at the
    /// configured sample rate. For example, at 24 kHz mono, valid frame sizes are
    /// 120, 240, 480, 960, 1920, or 2880 samples (corresponding to 2.5ms, 5ms,
    /// 10ms, 20ms, 40ms, or 60ms frame durations).
    ///
    /// # Arguments
    ///
    /// * `pcm` — PCM16 audio samples. For stereo, samples must be interleaved.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError::OpusCodecError` if encoding fails (e.g., invalid
    /// frame size or internal Opus error).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let mut output = vec![0u8; MAX_OPUS_FRAME_BYTES];
        let encoded_len = self
            .encoder
            .encode(pcm, &mut output)
            .map_err(|e| RealtimeError::opus(format!("Opus encode failed: {e}")))?;
        output.truncate(encoded_len);
        Ok(output)
    }

    /// Decodes an Opus frame to PCM16 audio samples.
    ///
    /// The output contains decoded PCM16 samples at the configured sample rate.
    /// For stereo, samples are interleaved.
    ///
    /// # Arguments
    ///
    /// * `opus_data` — Encoded Opus frame data. Must not be empty.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError::OpusCodecError` if decoding fails (e.g., corrupted
    /// Opus data, empty input, or internal Opus error).
    pub fn decode(&mut self, opus_data: &[u8]) -> Result<Vec<i16>> {
        let channel_count = match self.channels {
            Channels::Mono => 1,
            Channels::Stereo => 2,
            // Channels::Auto shouldn't occur since we only construct Mono/Stereo,
            // but handle it defensively.
            _ => 1,
        };
        let max_samples = MAX_DECODED_SAMPLES_PER_CHANNEL * channel_count;
        let mut output = vec![0i16; max_samples];

        let packet = audiopus::packet::Packet::try_from(opus_data)
            .map_err(|e| RealtimeError::opus(format!("Invalid Opus packet: {e}")))?;

        let mut_signals = MutSignals::try_from(output.as_mut_slice())
            .map_err(|e| RealtimeError::opus(format!("Failed to create output buffer: {e}")))?;

        let decoded_samples = self
            .decoder
            .decode(Some(packet), mut_signals, false)
            .map_err(|e| RealtimeError::opus(format!("Opus decode failed: {e}")))?;

        // decoded_samples is per-channel; total samples = decoded_samples * channels
        let total_samples = decoded_samples * channel_count;
        output.truncate(total_samples);
        Ok(output)
    }

    /// Returns the configured sample rate.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// Returns the configured channel count.
    pub fn channels(&self) -> Channels {
        self.channels
    }
}

/// OpenAI Realtime API base URL for REST endpoints.
const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

/// Name of the WebRTC data channel used for JSON event exchange.
const DATA_CHANNEL_LABEL: &str = "oai-events";

/// OpenAI WebRTC session for the Realtime API.
///
/// Uses Sans-IO WebRTC (`str0m`) for media transport and a data channel
/// for JSON event exchange. Audio is sent/received as Opus over WebRTC
/// media tracks, while tool calls, session updates, and text responses
/// flow over the "oai-events" data channel.
///
/// # Connection Flow
///
/// 1. Create `str0m::Rtc` instance with audio track and data channel
/// 2. Generate local SDP offer
/// 3. Request ephemeral token from OpenAI `/v1/realtime/sessions`
/// 4. Exchange SDP offer with OpenAI `/v1/realtime?model=...`
/// 5. Apply SDP answer to complete the WebRTC handshake
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::openai::webrtc::OpenAIWebRTCSession;
/// use adk_realtime::config::RealtimeConfig;
///
/// let session = OpenAIWebRTCSession::connect(
///     "sk-...",
///     "gpt-4o-realtime-preview-2024-12-17",
///     RealtimeConfig::default(),
/// ).await?;
/// ```
pub struct OpenAIWebRTCSession {
    /// Unique session identifier.
    session_id: String,
    /// Whether the WebRTC connection is active.
    connected: Arc<AtomicBool>,
    /// The str0m WebRTC instance (Sans-IO, requires external I/O driving).
    rtc: Arc<Mutex<Rtc>>,
    /// Media ID for the audio track used to send/receive Opus audio.
    audio_track_id: Mid,
    /// ID of the "oai-events" data channel for JSON event exchange.
    data_channel_id: ChannelId,
    /// Opus encoder for PCM16 → Opus conversion.
    opus_encoder: Arc<Mutex<OpusCodec>>,
    /// Cached Opus payload type negotiated during SDP exchange.
    opus_pt: Pt,
    /// Clock rate (frequency) for the negotiated audio codec (typically 48 kHz for Opus).
    clock_rate: Frequency,
    /// Running RTP sample offset for audio timestamps.
    rtp_sample_offset: AtomicU64,
    /// Channel receiver for incoming server events (from data channel and audio).
    event_rx: Arc<Mutex<mpsc::UnboundedReceiver<Result<ServerEvent>>>>,
    /// Channel sender for pushing server events from background processing.
    event_tx: mpsc::UnboundedSender<Result<ServerEvent>>,
    /// Buffer for messages sent before the data channel is open.
    /// Once the channel opens, queued messages are flushed in order.
    pending_dc_messages: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Whether the data channel has been opened and is ready for writes.
    dc_open: Arc<AtomicBool>,
}

/// Response from OpenAI's ephemeral token endpoint.
#[derive(Debug, serde::Deserialize)]
struct EphemeralTokenResponse {
    /// The ephemeral client secret token.
    client_secret: ClientSecret,
}

/// Client secret within the ephemeral token response.
#[derive(Debug, serde::Deserialize)]
struct ClientSecret {
    /// The actual token value.
    value: String,
}

/// Response body from the SDP exchange endpoint.
#[derive(Debug, serde::Deserialize)]
struct SdpExchangeResponse {
    /// The SDP answer string from OpenAI.
    sdp: String,
    /// The server-assigned session type (e.g., "answer").
    #[serde(rename = "type")]
    _sdp_type: String,
}

impl OpenAIWebRTCSession {
    /// Establish a WebRTC connection to OpenAI's Realtime API.
    ///
    /// This performs the full SDP signaling flow:
    /// 1. Creates a `str0m::Rtc` instance with an audio media track and data channel
    /// 2. Generates a local SDP offer
    /// 3. Obtains an ephemeral token from OpenAI
    /// 4. Exchanges the SDP offer for an SDP answer via OpenAI's endpoint
    /// 5. Applies the SDP answer to complete the handshake
    ///
    /// # Arguments
    ///
    /// * `api_key` — OpenAI API key for authentication.
    /// * `model_id` — Model identifier (e.g., "gpt-4o-realtime-preview-2024-12-17").
    /// * `config` — Realtime session configuration.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError::ConnectionError` if SDP signaling fails.
    /// Returns `RealtimeError::AuthError` if the ephemeral token request fails.
    /// Returns `RealtimeError::WebRTCError` if the Rtc instance cannot be configured.
    pub async fn connect(api_key: &str, model_id: &str, _config: RealtimeConfig) -> Result<Self> {
        // Step 1: Create str0m Rtc instance
        let mut rtc = Rtc::new(Instant::now());

        // Step 2: Add audio track (Opus, send+recv) and data channel via SDP API
        let mut changes = rtc.sdp_api();

        // Add audio media line for bidirectional Opus audio
        let audio_track_id = changes.add_media(
            MediaKind::Audio,
            Direction::SendRecv,
            None, // stream_id
            None, // track_id
            None, // ssrc
        );

        // Add the "oai-events" data channel for JSON event exchange
        let data_channel_id = changes.add_channel(DATA_CHANNEL_LABEL.to_string());

        // Step 3: Generate local SDP offer
        let (offer, pending) = changes.apply().ok_or_else(|| {
            RealtimeError::webrtc("Failed to generate SDP offer: no changes to apply")
        })?;

        let offer_sdp = offer.to_sdp_string();

        tracing::debug!(
            audio_mid = %audio_track_id,
            channel_id = ?data_channel_id,
            "Generated local SDP offer for OpenAI WebRTC"
        );

        // Step 4: Obtain ephemeral token from OpenAI
        let http_client = reqwest::Client::new();

        let ephemeral_token =
            Self::request_ephemeral_token(&http_client, api_key, model_id).await?;

        tracing::debug!("Obtained ephemeral token for WebRTC signaling");

        // Step 5: Exchange SDP offer with OpenAI endpoint
        let answer_sdp =
            Self::exchange_sdp(&http_client, &ephemeral_token, model_id, &offer_sdp).await?;

        // Step 6: Parse and apply SDP answer
        let answer = SdpAnswer::from_sdp_string(&answer_sdp)
            .map_err(|e| RealtimeError::webrtc(format!("Failed to parse SDP answer: {e}")))?;

        rtc.sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| RealtimeError::webrtc(format!("Failed to apply SDP answer: {e}")))?;

        tracing::info!(
            audio_mid = %audio_track_id,
            "OpenAI WebRTC SDP handshake complete"
        );

        // Create Opus codec for audio encoding/decoding (24kHz mono for OpenAI)
        let opus_codec = OpusCodec::new(24000, 1)?;

        // Resolve the Opus payload type from the negotiated SDP parameters.
        // The writer for the audio track exposes the negotiated codecs; we
        // pick the first one (Opus is the only audio codec we negotiate).
        let (opus_pt, clock_rate) = {
            let writer = rtc.writer(audio_track_id).ok_or_else(|| {
                RealtimeError::webrtc("Audio track writer not available after SDP answer")
            })?;
            let params = writer.payload_params().next().ok_or_else(|| {
                RealtimeError::webrtc(
                    "No payload type negotiated for audio track — SDP answer may be invalid",
                )
            })?;
            (params.pt(), params.spec().clock_rate)
        };

        // Create event channel for delivering server events
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        let session_id = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            session_id,
            connected: Arc::new(AtomicBool::new(true)),
            rtc: Arc::new(Mutex::new(rtc)),
            audio_track_id,
            data_channel_id,
            opus_encoder: Arc::new(Mutex::new(opus_codec)),
            opus_pt,
            clock_rate,
            rtp_sample_offset: AtomicU64::new(0),
            event_rx: Arc::new(Mutex::new(event_rx)),
            event_tx,
            pending_dc_messages: Arc::new(Mutex::new(Vec::new())),
            dc_open: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Request an ephemeral token from OpenAI's session endpoint.
    ///
    /// POST /v1/realtime/sessions with the API key to get a short-lived
    /// client secret for WebRTC signaling.
    async fn request_ephemeral_token(
        client: &reqwest::Client,
        api_key: &str,
        model_id: &str,
    ) -> Result<String> {
        let url = format!("{}/realtime/sessions", OPENAI_API_BASE);

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "model": model_id,
                "voice": "alloy",
            }))
            .send()
            .await
            .map_err(|e| {
                RealtimeError::AuthError(format!("Failed to request ephemeral token: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RealtimeError::AuthError(format!(
                "Ephemeral token request failed with status {status}: {body}"
            )));
        }

        let token_response: EphemeralTokenResponse = response.json().await.map_err(|e| {
            RealtimeError::AuthError(format!("Failed to parse ephemeral token response: {e}"))
        })?;

        Ok(token_response.client_secret.value)
    }

    /// Exchange the local SDP offer with OpenAI's WebRTC endpoint.
    ///
    /// POST /v1/realtime?model=... with the SDP offer and ephemeral token,
    /// receiving an SDP answer in return.
    async fn exchange_sdp(
        client: &reqwest::Client,
        ephemeral_token: &str,
        model_id: &str,
        offer_sdp: &str,
    ) -> Result<String> {
        let url = format!("{}/realtime?model={}", OPENAI_API_BASE, model_id);

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {ephemeral_token}"))
            .header("Content-Type", "application/sdp")
            .body(offer_sdp.to_string())
            .send()
            .await
            .map_err(|e| RealtimeError::connection(format!("SDP exchange request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RealtimeError::connection(format!(
                "SDP exchange failed with status {status}: {body}"
            )));
        }

        // The response Content-Type may be application/sdp (raw SDP string)
        // or application/json with an sdp field. Handle both.
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response.text().await.map_err(|e| {
            RealtimeError::connection(format!("Failed to read SDP answer body: {e}"))
        })?;

        if content_type.contains("application/sdp") {
            Ok(body)
        } else {
            // Try parsing as JSON with an "sdp" field
            let parsed: SdpExchangeResponse = serde_json::from_str(&body).map_err(|e| {
                RealtimeError::connection(format!(
                    "Failed to parse SDP exchange response as JSON: {e}"
                ))
            })?;
            Ok(parsed.sdp)
        }
    }

    /// Returns the audio track Mid for sending/receiving media.
    pub fn audio_track_id(&self) -> Mid {
        self.audio_track_id
    }

    /// Returns the data channel ID for the "oai-events" channel.
    pub fn data_channel_id(&self) -> ChannelId {
        self.data_channel_id
    }

    /// Returns a reference to the Rtc instance.
    pub fn rtc(&self) -> &Arc<Mutex<Rtc>> {
        &self.rtc
    }

    /// Returns a clone of the event sender for pushing events from background tasks.
    pub fn event_sender(&self) -> mpsc::UnboundedSender<Result<ServerEvent>> {
        self.event_tx.clone()
    }
}

impl std::fmt::Debug for OpenAIWebRTCSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIWebRTCSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::Relaxed))
            .field("audio_track_id", &self.audio_track_id)
            .field("data_channel_id", &self.data_channel_id)
            .finish()
    }
}

use async_trait::async_trait;
use base64::Engine;
use futures::Stream;
use serde_json::Value;
use std::pin::Pin;

impl OpenAIWebRTCSession {
    /// Send a JSON-serializable value over the "oai-events" data channel.
    ///
    /// If the data channel is not yet open, the message is queued and will be
    /// flushed in order once the channel opens. This prevents message loss
    /// during the brief window between SDP handshake completion and data
    /// channel establishment.
    async fn send_data_channel_message(&self, value: &Value) -> Result<()> {
        let json_bytes = serde_json::to_vec(value)
            .map_err(|e| RealtimeError::protocol(format!("JSON serialize error: {e}")))?;

        if !self.dc_open.load(Ordering::Acquire) {
            // Channel not open yet — queue for later flush
            let mut pending = self.pending_dc_messages.lock().await;
            // Cap at 50 messages to prevent unbounded growth
            if pending.len() >= 50 {
                return Err(RealtimeError::webrtc(
                    "Data channel message queue full (50 messages). Channel may not be opening.",
                ));
            }
            pending.push(json_bytes);
            tracing::debug!(
                "Data channel not open yet, queued message ({} pending)",
                pending.len()
            );
            return Ok(());
        }

        let mut rtc = self.rtc.lock().await;
        let mut channel = rtc
            .channel(self.data_channel_id)
            .ok_or_else(|| RealtimeError::webrtc("Data channel 'oai-events' not available"))?;
        channel
            .write(true, json_bytes.as_slice())
            .map_err(|e| RealtimeError::webrtc(format!("Data channel write failed: {e}")))?;

        Ok(())
    }

    /// Mark the data channel as open and flush any queued messages.
    ///
    /// Called when the data channel `OnOpen` event is received from str0m.
    /// Drains the pending message queue in FIFO order.
    pub async fn flush_pending_dc_messages(&self) -> Result<()> {
        self.dc_open.store(true, Ordering::Release);

        let mut pending = self.pending_dc_messages.lock().await;
        if pending.is_empty() {
            return Ok(());
        }

        let count = pending.len();
        tracing::info!("Data channel opened — flushing {count} queued messages");

        let mut rtc = self.rtc.lock().await;
        let mut channel = rtc.channel(self.data_channel_id).ok_or_else(|| {
            RealtimeError::webrtc("Data channel 'oai-events' not available during flush")
        })?;

        for msg in pending.drain(..) {
            channel
                .write(true, msg.as_slice())
                .map_err(|e| RealtimeError::webrtc(format!("Data channel flush failed: {e}")))?;
        }

        Ok(())
    }

    /// Encode PCM16 samples to Opus and write to the audio media track.
    ///
    /// Takes raw i16 PCM samples, encodes them to Opus via the session's
    /// `OpusCodec`, and writes the resulting Opus frame to the str0m audio
    /// track identified by `audio_track_id`.
    async fn write_audio_to_track(&self, pcm_samples: &[i16]) -> Result<()> {
        // Opus encode
        let opus_data = {
            let mut encoder = self.opus_encoder.lock().await;
            encoder.encode(pcm_samples)?
        };

        // Advance the running RTP sample offset.
        // Opus for WebRTC uses a 48 kHz clock regardless of the actual sample rate.
        // Each frame's duration in clock ticks equals the number of input samples
        // scaled from the codec sample rate (24 kHz) to the negotiated clock rate.
        let clock_hz = self.clock_rate.get() as u64;
        let samples_at_clock = (pcm_samples.len() as u64) * clock_hz / 24000;
        let rtp_offset = self.rtp_sample_offset.fetch_add(samples_at_clock, Ordering::Relaxed);

        // Write Opus frame to the str0m audio track.
        // str0m is Sans-IO: `writer(mid).write(...)` queues the media for the
        // next `poll_output()` cycle driven by the external I/O loop.
        let mut rtc = self.rtc.lock().await;
        let writer = rtc
            .writer(self.audio_track_id)
            .ok_or_else(|| RealtimeError::webrtc("Audio track writer not available"))?;

        let now = Instant::now();
        let rtp_time = MediaTime::new(rtp_offset, self.clock_rate);
        writer
            .write(self.opus_pt, now, rtp_time, opus_data)
            .map_err(|e| RealtimeError::webrtc(format!("Audio track write failed: {e}")))?;

        Ok(())
    }
}

#[async_trait]
impl RealtimeSession for OpenAIWebRTCSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Send raw PCM16 audio over the WebRTC audio track.
    ///
    /// Converts the `AudioChunk` data (little-endian PCM16 bytes) to i16
    /// samples, Opus-encodes them, and writes the Opus frame to the media
    /// track for transmission.
    async fn send_audio(&self, audio: &AudioChunk) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        let pcm_samples = audio
            .to_i16_samples()
            .map_err(|e| RealtimeError::opus(format!("Invalid PCM16 audio data: {e}")))?;

        self.write_audio_to_track(&pcm_samples).await
    }

    /// Send base64-encoded PCM16 audio over the WebRTC audio track.
    ///
    /// Decodes the base64 string to raw bytes, interprets them as
    /// little-endian i16 PCM samples, Opus-encodes, and writes to the
    /// media track.
    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        let raw_bytes = base64::engine::general_purpose::STANDARD
            .decode(audio_base64)
            .map_err(|e| RealtimeError::audio(format!("Invalid base64 audio: {e}")))?;

        if raw_bytes.len() % 2 != 0 {
            return Err(RealtimeError::audio(format!(
                "Invalid PCM16 data length: {} (must be even)",
                raw_bytes.len()
            )));
        }

        let pcm_samples: Vec<i16> =
            raw_bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();

        self.write_audio_to_track(&pcm_samples).await
    }

    /// Send a text message via the "oai-events" data channel.
    ///
    /// Creates a `conversation.item.create` event with a user text message
    /// and sends it as JSON over the data channel.
    async fn send_text(&self, text: &str) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        let item = ConversationItem::user_text(text);
        let event = ClientEvent::ConversationItemCreate {
            item: serde_json::to_value(&item)
                .map_err(|e| RealtimeError::protocol(format!("Serialize text item: {e}")))?,
        };
        self.send_event(event).await
    }

    /// Send a tool/function response via the "oai-events" data channel.
    ///
    /// Creates a `conversation.item.create` event with a function_call_output
    /// item and sends it over the data channel, then triggers a response.
    async fn send_tool_response(&self, response: ToolResponse) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        let output = match &response.output {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        let item = ConversationItem::tool_response(response.call_id, output);
        let event = ClientEvent::ConversationItemCreate {
            item: serde_json::to_value(&item)
                .map_err(|e| RealtimeError::protocol(format!("Serialize tool response: {e}")))?,
        };
        self.send_event(event).await?;

        // Trigger response after tool output (same pattern as WebSocket session)
        self.create_response().await
    }

    /// Commit the audio input buffer via the data channel.
    ///
    /// Sends an `input_audio_buffer.commit` event for manual VAD mode.
    async fn commit_audio(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        self.send_event(ClientEvent::InputAudioBufferCommit).await
    }

    /// Clear the audio input buffer via the data channel.
    ///
    /// Sends an `input_audio_buffer.clear` event.
    async fn clear_audio(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        self.send_event(ClientEvent::InputAudioBufferClear).await
    }

    /// Trigger a response from the model via the data channel.
    ///
    /// Sends a `response.create` event.
    async fn create_response(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        self.send_event(ClientEvent::ResponseCreate { config: None }).await
    }

    /// Interrupt/cancel the current response via the data channel.
    ///
    /// Sends a `response.cancel` event.
    async fn interrupt(&self) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        self.send_event(ClientEvent::ResponseCancel).await
    }

    /// Send a raw `ClientEvent` over the "oai-events" data channel.
    ///
    /// Serializes the event to JSON and writes it to the data channel.
    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        if !self.is_connected() {
            return Err(RealtimeError::NotConnected);
        }

        let value = serde_json::to_value(&event)
            .map_err(|e| RealtimeError::protocol(format!("Serialize event: {e}")))?;
        self.send_data_channel_message(&value).await
    }

    /// Receive the next server event.
    ///
    /// Reads from the internal mpsc channel that is fed by the background
    /// I/O driving loop. Returns `None` when the session is closed and
    /// the channel is drained.
    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        let mut rx = self.event_rx.lock().await;
        rx.recv().await
    }

    /// Get a stream of server events.
    ///
    /// Wraps the mpsc receiver as an async stream for convenient iteration.
    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        let rx = self.event_rx.clone();
        Box::pin(async_stream::stream! {
            let mut rx = rx.lock().await;
            while let Some(event) = rx.recv().await {
                yield event;
            }
        })
    }

    /// Close the WebRTC session gracefully.
    ///
    /// Marks the session as disconnected and calls `rtc.disconnect()` to
    /// signal the str0m instance to tear down the peer connection.
    async fn close(&self) -> Result<()> {
        self.connected.store(false, Ordering::Relaxed);
        let mut rtc = self.rtc.lock().await;
        rtc.disconnect();
        Ok(())
    }
}
