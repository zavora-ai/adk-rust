//! Live transcription over the Meta Model API's realtime WebSocket
//! (`wss://api.meta.ai/v1/asr/realtime`, `muse-voice-transcribe-1.0`).
//!
//! | Step | Client | Server |
//! |------|--------|--------|
//! | Handshake | One JSON text frame carrying the key in `authorization.accessToken`, the encoding, model, mode and hints | `{"sessionId": …}`, within 10 s |
//! | Audio | Raw PCM16LE mono binary frames, at most the lead window ahead of the audio processed | `speechStart`, `transcript`, `speechEnd`, `speechComplete`, `speaker`, `audioProgress` |
//! | End | `{"type":"endStream"}` when the audio ends | Trailing events, then close `1000` |
//!
//! The key travels only in the handshake frame: never in the URL, a header or a log. No
//! audio is sent before the server acknowledges the handshake. The client never reconnects:
//! Meta offers no resume token, a new session would lose the turn state and bill the audio
//! twice.
//!
//! | Close | Error |
//! |-------|-------|
//! | `1008` (invalid request, policy, backlog) | `InvalidInput`, `model.muse.realtime_rejected` |
//! | `1011` (internal) | `Unavailable`, `model.muse.realtime_failed`; `model.muse.session_limit` when the 60-minute session limit closed it |
//! | `1013` (rate limited) | `RateLimited`, `model.muse.realtime_rate_limited` |
//! | none (the connection dropped) | `Unavailable`, `model.muse.realtime_incomplete` |
//!
//! A session is metered against the current run ([`adk_core::meter_usage`]'s scope): the
//! spend hold covers a full 60-minute session at $0.18 per hour before the socket opens,
//! and the audio processed is committed, in whole seconds rounded down, when the session
//! ends or the stream is dropped. A session that ends before any transcript is released, as
//! Meta does not bill it.

use std::pin::Pin;
use std::time::Duration;

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, MeteredUsage, ModelCallScope, UsageCall, UsageHold,
    UsageUnit, UsageVector,
};
use futures::{SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Bytes, Message};

use super::asr::MUSE_VOICE_MODEL;
use super::config::MuseConfig;
use crate::pricing::PricingCatalog;

/// Longest realtime session Meta accepts: "Sessions run for up to 60 minutes".
const MAX_SESSION_MS: u64 = 60 * 60 * 1_000;

/// When a session's turn boundaries are reported, fixed at the handshake.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AsrMode {
    /// One turn: a `transcript` with `final: true` once the audio ends.
    PushToTalk,
    /// Turns detected from silence: `speechStart`, `transcript`s, `speechEnd`, then
    /// `speechComplete` per turn.
    #[default]
    Endpointing,
    /// [`Endpointing`](Self::Endpointing) with a `speaker` event per speaker change.
    Diarization,
}

impl AsrMode {
    /// The mode's wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PushToTalk => "PUSH_TO_TALK",
            Self::Endpointing => "ENDPOINTING",
            Self::Diarization => "DIARIZATION",
        }
    }
}

/// The encoding of the audio frames: raw 16-bit little-endian mono PCM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AsrEncoding {
    /// 16 kHz.
    #[default]
    Pcm16Khz,
    /// 24 kHz.
    Pcm24Khz,
}

impl AsrEncoding {
    /// The encoding's wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pcm16Khz => "PCM_16KHZ",
            Self::Pcm24Khz => "PCM_24KHZ",
        }
    }

    /// Bytes of audio per millisecond.
    pub const fn bytes_per_ms(self) -> u64 {
        match self {
            Self::Pcm16Khz => 32,
            Self::Pcm24Khz => 48,
        }
    }
}

/// Options for one realtime transcription session.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_model::muse::{AsrEncoding, AsrMode, RealtimeAsrOptions};
///
/// let options = RealtimeAsrOptions::default()
///     .with_mode(AsrMode::Diarization)
///     .with_encoding(AsrEncoding::Pcm24Khz)
///     .with_keywords(["ADK-Rust"])
///     .with_lead_window(Duration::from_secs(1));
/// assert_eq!(options.mode, AsrMode::Diarization);
/// assert_eq!(RealtimeAsrOptions::default().lead_window, Duration::from_secs(2));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RealtimeAsrOptions {
    /// The audio encoding.
    pub encoding: AsrEncoding,
    /// When turn boundaries are reported.
    pub mode: AsrMode,
    /// BCP-47 languages the audio is likely in.
    pub language_bias: Vec<String>,
    /// Domain vocabulary to favour.
    pub keywords: Vec<String>,
    /// The most audio sent ahead of what the server reports processed. Meta refuses a
    /// backlog over 5 seconds.
    pub lead_window: Duration,
    /// How long the server has to acknowledge the handshake.
    pub handshake_timeout: Duration,
    /// A caller-chosen session id, sent as the `sessionId` query parameter.
    pub session_id: Option<String>,
}

impl Default for RealtimeAsrOptions {
    fn default() -> Self {
        Self {
            encoding: AsrEncoding::default(),
            mode: AsrMode::default(),
            language_bias: Vec::new(),
            keywords: Vec::new(),
            lead_window: Duration::from_secs(2),
            handshake_timeout: Duration::from_secs(10),
            session_id: None,
        }
    }
}

impl RealtimeAsrOptions {
    /// Sets the audio encoding.
    #[must_use]
    pub fn with_encoding(mut self, encoding: AsrEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Sets when turn boundaries are reported.
    #[must_use]
    pub fn with_mode(mut self, mode: AsrMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the languages the audio is likely in.
    #[must_use]
    pub fn with_language_bias(
        mut self,
        languages: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.language_bias = languages.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the keyword hints.
    #[must_use]
    pub fn with_keywords(mut self, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keywords = keywords.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the most audio sent ahead of what the server processed.
    #[must_use]
    pub fn with_lead_window(mut self, lead_window: Duration) -> Self {
        self.lead_window = lead_window;
        self
    }

    /// Sets how long the server has to acknowledge the handshake.
    #[must_use]
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Sets the session id sent as the `sessionId` query parameter.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// One event of a realtime transcription session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AsrEvent {
    /// The server acknowledged the handshake.
    Started {
        /// The server's session id.
        session_id: String,
    },
    /// Speech began a turn.
    SpeechStart {
        /// The turn.
        turn_id: u32,
        /// Audio processed when it began, in milliseconds.
        audio_processed_ms: Option<u64>,
    },
    /// A transcript of the current turn: cumulative partial text, or the turn's final text in
    /// [`AsrMode::PushToTalk`].
    Transcript {
        /// The turn of the latest `SpeechStart`, `None` before one.
        turn_id: Option<u32>,
        /// The text.
        text: String,
        /// Whether the server will not revise it.
        is_final: bool,
        /// Audio processed, in milliseconds.
        audio_processed_ms: Option<u64>,
    },
    /// Speech ended a turn.
    SpeechEnd {
        /// The turn.
        turn_id: u32,
    },
    /// A turn's final text ([`AsrMode::Endpointing`] and [`AsrMode::Diarization`]).
    SpeechComplete {
        /// The turn.
        turn_id: u32,
        /// The turn's final text.
        text: String,
    },
    /// The speaker changed ([`AsrMode::Diarization`]).
    Speaker {
        /// The server's label for the speaker.
        label: String,
    },
    /// Audio the server has processed, in milliseconds.
    AudioProgress {
        /// Milliseconds processed.
        audio_processed_ms: u64,
    },
    /// The audio the session is billed for, sent once the session closed normally.
    Usage {
        /// Milliseconds processed.
        audio_ms: u64,
    },
}

/// The events of a realtime transcription session.
pub type AsrEventStream = Pin<Box<dyn Stream<Item = Result<AsrEvent, AdkError>> + Send>>;

/// Client for live transcription over the realtime WebSocket.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseConfig, MuseRealtimeAsrClient, RealtimeAsrOptions};
///
/// let asr = MuseRealtimeAsrClient::new(MuseConfig::new(api_key, "muse-voice-transcribe-1.0"))?
///     .with_options(RealtimeAsrOptions::default());
/// let mut events = asr.transcribe(microphone_chunks).await?;
/// while let Some(event) = events.next().await {
///     println!("{:?}", event?);
/// }
/// ```
#[derive(Debug, Clone)]
pub struct MuseRealtimeAsrClient {
    config: MuseConfig,
    options: RealtimeAsrOptions,
}

impl MuseRealtimeAsrClient {
    /// A client for `config`; the model defaults to `muse-voice-transcribe-1.0`.
    ///
    /// # Errors
    ///
    /// Returns `model.muse.invalid_config` when the base URL is not an `http` or `https`
    /// URL.
    pub fn new(mut config: MuseConfig) -> Result<Self, AdkError> {
        if config.model.is_empty() || config.model.starts_with("muse-spark") {
            config.model = MUSE_VOICE_MODEL.to_string();
        }
        let client = Self { config, options: RealtimeAsrOptions::default() };
        client.url()?;
        Ok(client)
    }

    /// Sets the session options.
    #[must_use]
    pub fn with_options(mut self, options: RealtimeAsrOptions) -> Self {
        self.options = options;
        self
    }

    /// The session options.
    pub fn options(&self) -> &RealtimeAsrOptions {
        &self.options
    }

    /// The WebSocket URL: the base URL with `ws`/`wss` for `http`/`https`, the realtime path,
    /// and the session id when one is set. It never carries the key.
    fn url(&self) -> Result<String, AdkError> {
        let base = self.config.effective_base_url().trim_end_matches('/');
        let socket = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.muse.invalid_config",
                "the Muse base URL must be an http or https URL",
            )
            .with_provider("muse"));
        };
        let url = format!("{socket}/asr/realtime");
        let parsed = match &self.options.session_id {
            Some(session) => reqwest::Url::parse_with_params(&url, [("sessionId", session)]),
            None => reqwest::Url::parse(&url),
        };
        parsed.map(String::from).map_err(|error| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.muse.invalid_config",
                format!("the Muse realtime URL is invalid: {error}"),
            )
            .with_provider("muse")
        })
    }

    /// Transcribes `audio` live: opens a session, waits for the handshake to be
    /// acknowledged, then streams `audio` and yields the server's events until the session
    /// closes.
    ///
    /// `audio` is raw PCM16LE mono at the configured encoding's rate, in chunks of any size.
    /// When it ends the client sends `endStream` and reads the trailing events until the
    /// server closes the session. Dropping the returned stream stops sending, closes the
    /// socket and releases `audio`.
    ///
    /// # Errors
    ///
    /// Returns the current run's refusal (spend limit, budget) before anything is sent, a
    /// connection or handshake error, or the error the server closed the handshake with.
    pub async fn transcribe<S, B>(&self, audio: S) -> Result<AsrEventStream, AdkError>
    where
        S: Stream<Item = B> + Send + 'static,
        B: Into<Bytes> + Send + 'static,
    {
        let model = self.config.model.clone();
        let hold = match ModelCallScope::current() {
            Some(scope) => {
                let longest = UsageVector::new().with(UsageUnit::AudioMillis, MAX_SESSION_MS);
                let mut call = UsageCall::new("muse", model.clone());
                if let Some(bound) =
                    PricingCatalog::standard().unit_cost_micro_usd(Some("muse"), &model, &longest)
                {
                    call = call.with_max_cost_micro_usd(bound);
                }
                Some(scope.begin_usage(&call).await?)
            }
            None => None,
        };
        let meter =
            SessionMeter { hold, model: model.clone(), processed_ms: 0, transcribed: false };
        let span = tracing::info_span!(
            "muse.asr_realtime",
            gen_ai.system = "muse",
            gen_ai.request.model = %model,
            asr.mode = self.options.mode.as_str(),
            asr.session_id = tracing::field::Empty,
            asr.audio_sent_ms = tracing::field::Empty,
            asr.audio_processed_ms = tracing::field::Empty,
            asr.close_code = tracing::field::Empty,
        );
        let (socket, _response) = tokio_tungstenite::connect_async(self.url()?)
            .await
            .map_err(|error| connect_error(&error))?;
        let (mut sink, mut source) = socket.split();

        let handshake = json!({
            "authorization": { "accessToken": format!("Bearer {}", self.config.api_key) },
            "audioEncoding": self.options.encoding.as_str(),
            "model": model,
            "mode": self.options.mode.as_str(),
            "partialMode": "CUMULATIVE",
            "emitAudioProgress": true,
            "languageBias": self.options.language_bias,
            "keywords": self.options.keywords,
        });
        sink.send(Message::Text(handshake.to_string().into()))
            .await
            .map_err(|error| connect_error(&error))?;
        let session_id = tokio::time::timeout(self.options.handshake_timeout, async {
            while let Some(frame) = source.next().await {
                match frame.map_err(|error| read_error(&error))? {
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        if value.get("type").and_then(Value::as_str) == Some("error") {
                            return Err(server_error(&value));
                        }
                        if value.get("type").is_none()
                            && let Some(session) = value.get("sessionId").and_then(Value::as_str)
                        {
                            return Ok(session.to_string());
                        }
                    }
                    Message::Close(frame) => return Err(close_error(frame.as_ref())),
                    Message::Binary(_)
                    | Message::Ping(_)
                    | Message::Pong(_)
                    | Message::Frame(_) => {}
                }
            }
            Err(incomplete())
        })
        .await
        .map_err(|_| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Timeout,
                "model.muse.realtime_handshake",
                format!(
                    "the Muse realtime session did not acknowledge the handshake within {:?}",
                    self.options.handshake_timeout
                ),
            )
            .with_provider("muse")
        })??;
        span.record("asr.session_id", session_id.as_str());

        let (processed_tx, processed_rx) = watch::channel(0_u64);
        let lead_ms = u64::try_from(self.options.lead_window.as_millis()).unwrap_or(u64::MAX);
        let bytes_per_ms = self.options.encoding.bytes_per_ms();
        let sender_span = span.clone();
        // The sender lives as long as the event stream: dropping the stream aborts it, which
        // releases `audio` and closes the socket.
        let sender = AbortOnDrop(tokio::spawn(async move {
            let mut processed = processed_rx;
            let mut audio = std::pin::pin!(audio);
            let mut sent_ms = 0_u64;
            while let Some(chunk) = audio.next().await {
                // Hold the chunk until the server is within the lead window of the audio sent.
                while sent_ms > processed.borrow().saturating_add(lead_ms) {
                    if processed.changed().await.is_err() {
                        return;
                    }
                }
                let chunk: Bytes = chunk.into();
                sent_ms += (chunk.len() as u64).div_ceil(bytes_per_ms);
                if sink.send(Message::Binary(chunk)).await.is_err() {
                    return;
                }
            }
            sender_span.record("asr.audio_sent_ms", sent_ms);
            let end = json!({ "type": "endStream" }).to_string();
            let _ = sink.send(Message::Text(end.into())).await;
        }));

        let events = async_stream::stream! {
            let _sender = sender;
            let mut meter = meter;
            let mut turn: Option<u32> = None;
            yield Ok(AsrEvent::Started { session_id });
            loop {
                let Some(frame) = source.next().await else {
                    yield Err(incomplete());
                    break;
                };
                let text = match frame {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(frame)) => {
                        let code = frame.as_ref().map(|frame| u16::from(frame.code));
                        if let Some(code) = code {
                            span.record("asr.close_code", code);
                        }
                        span.record("asr.audio_processed_ms", meter.processed_ms);
                        if code.is_none_or(|code| code == 1000) {
                            yield Ok(AsrEvent::Usage { audio_ms: meter.processed_ms });
                        } else {
                            yield Err(close_error(frame.as_ref()));
                        }
                        break;
                    }
                    Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                    Err(error) => {
                        yield Err(read_error(&error));
                        break;
                    }
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                let processed = value.get("audioProcessedMs").and_then(Value::as_u64);
                if let Some(processed) = processed {
                    meter.processed_ms = meter.processed_ms.max(processed);
                    let _ = processed_tx.send(meter.processed_ms);
                }
                let turn_of = |value: &Value| {
                    value.get("turnId").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok())
                };
                let text_of = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                let event = match value.get("type").and_then(Value::as_str) {
                    Some("speechStart") => {
                        let Some(id) = turn_of(&value) else { continue };
                        turn = Some(id);
                        AsrEvent::SpeechStart { turn_id: id, audio_processed_ms: processed }
                    }
                    Some("transcript") => {
                        meter.transcribed = true;
                        AsrEvent::Transcript {
                            turn_id: turn,
                            text: text_of("transcript"),
                            is_final: value.get("final").and_then(Value::as_bool).unwrap_or(false),
                            audio_processed_ms: processed,
                        }
                    }
                    Some("speechEnd") => {
                        let Some(id) = turn_of(&value) else { continue };
                        AsrEvent::SpeechEnd { turn_id: id }
                    }
                    Some("speechComplete") => {
                        let Some(id) = turn_of(&value) else { continue };
                        meter.transcribed = true;
                        AsrEvent::SpeechComplete { turn_id: id, text: text_of("transcript") }
                    }
                    Some("speaker") => AsrEvent::Speaker { label: text_of("label") },
                    Some("audioProgress") => match processed {
                        Some(processed) => AsrEvent::AudioProgress { audio_processed_ms: processed },
                        None => continue,
                    },
                    Some("error") => {
                        yield Err(server_error(&value));
                        break;
                    }
                    // Event types added later are ignored.
                    _ => continue,
                };
                yield Ok(event);
            }
            meter.settle().await;
        };
        Ok(Box::pin(events))
    }
}

/// The spend hold of one session, settled with the audio processed. Dropped unsettled — the
/// stream dropped mid-session — it settles on the runtime.
struct SessionMeter {
    hold: Option<UsageHold>,
    model: String,
    processed_ms: u64,
    transcribed: bool,
}

impl SessionMeter {
    async fn settle(&mut self) {
        let Some(hold) = self.hold.take() else { return };
        // Meta bills no session that failed before a transcript.
        if !self.transcribed {
            hold.release().await;
            return;
        }
        let units = UsageVector::new().with(UsageUnit::AudioMillis, self.processed_ms);
        let cost =
            PricingCatalog::standard().unit_cost_micro_usd(Some("muse"), &self.model, &units);
        let usage = MeteredUsage::new(units);
        hold.commit(match cost {
            Some(cost) => usage.with_cost_micro_usd(cost),
            None => usage,
        })
        .await;
    }
}

impl Drop for SessionMeter {
    fn drop(&mut self) {
        if self.hold.is_none() {
            return;
        }
        let mut meter = SessionMeter {
            hold: self.hold.take(),
            model: std::mem::take(&mut self.model),
            processed_ms: self.processed_ms,
            transcribed: self.transcribed,
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { meter.settle().await });
        }
    }
}

/// Aborts a spawned task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn incomplete() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.muse.realtime_incomplete",
        "the Muse realtime session ended without a close frame",
    )
    .with_provider("muse")
}

/// The error a close frame carries.
fn close_error(frame: Option<&tungstenite::protocol::CloseFrame>) -> AdkError {
    let code = frame.map(|frame| frame.code);
    let reason = frame.map(|frame| frame.reason.to_string()).unwrap_or_default();
    let (category, error_code) = match code {
        Some(CloseCode::Policy) => (ErrorCategory::InvalidInput, "model.muse.realtime_rejected"),
        Some(CloseCode::Error) if reason.contains("Max session duration") => {
            (ErrorCategory::Unavailable, "model.muse.session_limit")
        }
        Some(CloseCode::Error) => (ErrorCategory::Unavailable, "model.muse.realtime_failed"),
        Some(CloseCode::Again) => (ErrorCategory::RateLimited, "model.muse.realtime_rate_limited"),
        Some(_) | None => (ErrorCategory::Unavailable, "model.muse.realtime_failed"),
    };
    let number = code.map(u16::from).map_or_else(|| "none".to_string(), |code| code.to_string());
    AdkError::new(
        ErrorComponent::Model,
        category,
        error_code,
        format!(
            "the Muse realtime session closed with code {number}: {}",
            adk_core::upstream_excerpt(&reason)
        ),
    )
    .with_provider("muse")
}

/// The error an `error` event reports.
fn server_error(value: &Value) -> AdkError {
    let kind = value.get("errorType").or_else(|| value.get("error")).and_then(Value::as_str);
    let message = value.get("message").and_then(Value::as_str).unwrap_or("no message");
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.muse.realtime_error",
        format!(
            "the Muse realtime session reported an error{}: {}",
            kind.map(|kind| format!(" ({kind})")).unwrap_or_default(),
            adk_core::upstream_excerpt(message)
        ),
    )
    .with_provider("muse")
}

fn connect_error(error: &tungstenite::Error) -> AdkError {
    let (category, status) = match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            (crate::retry::category_for_status_code(status), Some(status))
        }
        tungstenite::Error::Url(_) | tungstenite::Error::HttpFormat(_) => {
            (ErrorCategory::InvalidInput, None)
        }
        _ => (ErrorCategory::Unavailable, None),
    };
    let error = AdkError::new(
        ErrorComponent::Model,
        category,
        "model.muse.realtime_connect",
        format!("could not open the Muse realtime session: {error}"),
    )
    .with_provider("muse");
    match status {
        Some(status) => error.with_upstream_status(status),
        None => error,
    }
}

fn read_error(error: &tungstenite::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.muse.realtime_read",
        format!("the Muse realtime session failed: {error}"),
    )
    .with_provider("muse")
}
