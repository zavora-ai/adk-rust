//! Gemini Live session implementation.
//!
//! Manages a WebSocket connection to Google's Gemini Live API with support
//! for both AI Studio (API key) and Vertex AI (OAuth/ADC) backends.

use crate::audio::{AudioChunk, AudioFormat};
use crate::config::{RealtimeConfig, ToolDefinition, VadMode};
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::{ContextMutationOutcome, RealtimeSession};
use async_trait::async_trait;
use base64::Engine;
use bytes::{BufMut, Bytes, BytesMut};
use futures::stream::Stream;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex as ParkingMutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSource = futures::stream::SplitStream<WsStream>;
const WRITER_CHANNEL_CAPACITY: usize = 64;
const AUDIO_FLUSH_TARGET_MS: usize = 40;

/// Backend configuration for Gemini Live connections.
///
/// Determines how to authenticate and which endpoint to connect to.
#[derive(Debug, Clone)]
pub enum GeminiLiveBackend {
    /// AI Studio with API key authentication.
    Studio { api_key: String },

    /// Vertex AI with OAuth2/ADC authentication.
    #[cfg(feature = "vertex-live")]
    Vertex {
        /// Google Cloud credentials for OAuth2 token generation.
        credentials: google_cloud_auth::credentials::Credentials,
        /// Google Cloud region (e.g., "us-central1").
        region: String,
        /// Google Cloud project ID.
        project_id: String,
    },
}

impl GeminiLiveBackend {
    /// Create a Studio backend with API key authentication.
    pub fn studio(api_key: impl Into<String>) -> Self {
        Self::Studio { api_key: api_key.into() }
    }

    /// Create a Vertex AI backend using Application Default Credentials (ADC).
    ///
    /// This is the most ergonomic way to connect to Vertex AI Live. It
    /// automatically discovers credentials from the environment using
    /// `google-cloud-auth`'s default credential chain (environment variables,
    /// `gcloud auth application-default login`, service account files, etc.).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let backend = GeminiLiveBackend::vertex_adc("my-project", "us-central1")?;
    /// let model = GeminiRealtimeModel::new(backend, "models/gemini-3.1-flash-live-preview");
    /// ```
    #[cfg(feature = "vertex-live")]
    pub fn vertex_adc(project_id: impl Into<String>, region: impl Into<String>) -> Result<Self> {
        let credentials =
            google_cloud_auth::credentials::Builder::default().build().map_err(|e| {
                RealtimeError::AuthError(format!(
                    "Failed to discover Application Default Credentials: {e}"
                ))
            })?;
        Ok(Self::Vertex { credentials, region: region.into(), project_id: project_id.into() })
    }
}

// ── Wire format types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiClientMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<GeminiSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input: Option<GeminiRealtimeInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_response: Option<GeminiToolResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_content: Option<GeminiClientContent>,
}

/// Configuration for Gemini 2.5 Live session resumption.
///
/// See the official documentation for details:
/// https://ai.google.dev/gemini-api/docs/live-api/session-management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResumptionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSetup {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_resumption: Option<SessionResumptionConfig>,
    /// Enable transcription of the user's input audio (empty object = on).
    #[serde(skip_serializing_if = "Option::is_none")]
    input_audio_transcription: Option<Value>,
    /// Enable transcription of the model's spoken output (empty object = on).
    #[serde(skip_serializing_if = "Option::is_none")]
    output_audio_transcription: Option<Value>,
    /// Only sent when the caller asked for manual activity detection. Left
    /// absent otherwise so the server keeps its own default, which is what
    /// every existing server-VAD session relies on.
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input_config: Option<GeminiRealtimeInputConfig>,
}

/// `setup.realtimeInputConfig`.
///
/// Only the automatic-detection switch is modelled. `activityHandling` is
/// deliberately left off the wire: its default,
/// `START_OF_ACTIVITY_INTERRUPTS`, is the behaviour
/// [`ActivitySignaller::start`] depends on, and sending the field would mean
/// offering callers a way to turn barge-in off without any of the plumbing
/// that would make that coherent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRealtimeInputConfig {
    automatic_activity_detection: GeminiAutomaticActivityDetection,
}

/// `setup.realtimeInputConfig.automaticActivityDetection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiAutomaticActivityDetection {
    disabled: bool,
}

/// `activityStart` / `activityEnd`.
///
/// Both are documented as having no fields, so they serialize as `{}`. The
/// marker exists only so the `Option` in [`GeminiRealtimeInput`] can be `Some`
/// without inventing a field Google does not define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct GeminiActivityMarker {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<GeminiInlineData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRealtimeInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<GeminiMediaChunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_chunks: Option<Vec<GeminiMediaChunk>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// The user has started speaking. Only legal while automatic activity
    /// detection is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_start: Option<GeminiActivityMarker>,
    /// The user has stopped speaking. Only legal while automatic activity
    /// detection is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_end: Option<GeminiActivityMarker>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiMediaChunk {
    mime_type: String,
    data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolResponse {
    function_responses: Vec<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionResponse {
    id: String,
    response: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiClientContent {
    turns: Vec<GeminiTurn>,
    turn_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTurn {
    role: String,
    parts: Vec<GeminiPart>,
}

// ── Activity detection ──────────────────────────────────────────────────

/// Which side of the connection decides when the user is speaking.
///
/// Gemini Live treats this as a single choice, not two independent switches:
/// `activityStart` and `activityEnd` "can only be sent if automatic (i.e.
/// server-side) activity detection is disabled". Enabling client signalling is
/// therefore not an addition to server VAD, it is a transfer of ownership, and
/// this enum is the record of who holds it for the life of a session.
///
/// The mode is fixed at connect time because it is carried in the `setup`
/// frame, which Gemini Live accepts exactly once per connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityDetection {
    /// Gemini's own VAD segments turns. This is the API default and what every
    /// session gets unless it explicitly asks otherwise.
    ///
    /// Client activity frames are rejected by the server in this mode, so
    /// [`GeminiRealtimeSession::activity_signaller`] yields nothing here.
    #[default]
    Automatic,
    /// `setup.realtimeInputConfig.automaticActivityDetection.disabled = true`.
    ///
    /// The server performs no turn detection at all; the client must send
    /// `activityStart` and `activityEnd` itself. Selecting this without
    /// actually sending those frames leaves the session with no turn detection
    /// on either side.
    Manual,
}

impl ActivityDetection {
    /// Resolve the mode a config asks for.
    ///
    /// [`VadMode::None`] is the only way to reach [`Manual`](Self::Manual):
    /// it is already the crate's provider-agnostic word for "no automatic turn
    /// detection", and honouring it here is what stops it being a request that
    /// silently does nothing on Gemini.
    fn from_config(config: &RealtimeConfig) -> Self {
        match config.turn_detection.as_ref().map(|vad| vad.mode) {
            Some(VadMode::None) => Self::Manual,
            // Absent config, `ServerVad` and `SemanticVad` all leave detection
            // with the server. `SemanticVad` has no Gemini equivalent, and
            // downgrading it to the server default is strictly closer to its
            // intent than disabling detection entirely.
            _ => Self::Automatic,
        }
    }

    fn realtime_input_config(self) -> Option<GeminiRealtimeInputConfig> {
        match self {
            // Say nothing, so the server applies its own default. A session
            // that does not ask for manual detection must serialize exactly as
            // it did before this field existed.
            Self::Automatic => None,
            Self::Manual => Some(GeminiRealtimeInputConfig {
                automatic_activity_detection: GeminiAutomaticActivityDetection { disabled: true },
            }),
        }
    }
}

/// What an activity signal actually did.
///
/// Returned rather than swallowed because "the frame went out" and "the
/// session was already in that state" are different facts, and a caller that
/// records one as the other is claiming to have told the server something it
/// never said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ActivitySignalOutcome {
    /// The frame was written to the connection.
    Sent,
    /// Nothing was sent: the session was already in the requested state, so
    /// this was a duplicate report of one activity transition.
    Redundant,
}

/// Permission to send `activityStart` / `activityEnd` on a session that
/// negotiated [`ActivityDetection::Manual`].
///
/// This type exists so the protocol's exclusivity rule is enforced by
/// construction rather than by remembering to check. It is reachable only
/// through [`GeminiRealtimeSession::activity_signaller`], which returns `None`
/// for an automatic-detection session, so there is no way to hold one for a
/// session whose server would reject the frames.
///
/// # One transition, one frame
///
/// `start` and `end` are edge-triggered against a per-session activity state,
/// so calling `start` twice without an intervening `end` sends one frame and
/// reports [`ActivitySignalOutcome::Redundant`] for the second.
///
/// This is deliberate. A caller that fuses several detectors, or that reacts
/// both to its own detector and to inbound `serverContent.interrupted`, has
/// more signals than there are transitions. RFC 6787 §6.2.4 exists because a
/// duplicate cancel for a *single* barge-in cancels the *next* prompt, and the
/// cheapest place to stop that is where the frames are written.
///
/// The de-duplication is per state transition on this session, which is only
/// the outbound half. It cannot tell whether an inbound interruption report was
/// caused by a frame sent here — Gemini's `activityStart` carries no fields and
/// `serverContent.interrupted` carries no correlation id, so nothing in the
/// protocol supports that inference and this type does not fake one.
///
/// # Example
///
/// ```rust,ignore
/// // `None` on a server-VAD session — the frames would be rejected.
/// if let Some(activity) = session.activity_signaller() {
///     activity.start().await?;
///     session.send_audio(&chunk).await?;
///     activity.end().await?;
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ActivitySignaller<'a> {
    session: &'a GeminiRealtimeSession,
}

impl ActivitySignaller<'_> {
    /// Report that the user has started speaking.
    ///
    /// Google documents `activityStart` as interrupting the model's response
    /// under the default `activityHandling` of `START_OF_ACTIVITY_INTERRUPTS`.
    /// What this method guarantees is narrower and is all it will claim: the
    /// frame was written to the connection. Whether the server then stopped
    /// generating, and whether it reports that back as
    /// `serverContent.interrupted`, is the server's behaviour and is not
    /// observed here.
    ///
    /// Audio that arrives before this frame is not attributed to the turn, so
    /// send a little leading context after it rather than clipping to the
    /// exact speech onset.
    pub async fn start(&self) -> Result<ActivitySignalOutcome> {
        self.session.send_activity(ActivitySignal::Start).await
    }

    /// Report that the user has stopped speaking, ending the turn.
    pub async fn end(&self) -> Result<ActivitySignalOutcome> {
        self.session.send_activity(ActivitySignal::End).await
    }
}

/// Which of the two activity frames to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivitySignal {
    Start,
    End,
}

impl ActivitySignal {
    fn into_realtime_input(self) -> GeminiRealtimeInput {
        let marker = Some(GeminiActivityMarker {});
        match self {
            Self::Start => GeminiRealtimeInput { activity_start: marker, ..Default::default() },
            Self::End => GeminiRealtimeInput { activity_end: marker, ..Default::default() },
        }
    }

    /// The activity state this signal moves the session into.
    fn target_state(self) -> bool {
        matches!(self, Self::Start)
    }
}

// ── Session implementation ──────────────────────────────────────────────

/// Gemini Live session.
///
/// Manages a WebSocket connection to Google's Gemini Live API.
pub struct GeminiRealtimeSession {
    session_id: String,
    connected: Arc<AtomicBool>,
    outbound_tx: mpsc::Sender<Message>,
    writer_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    receiver: Arc<Mutex<WsSource>>,
    audio_buffer: Arc<ParkingMutex<BytesMut>>,
    event_queue: Arc<Mutex<std::collections::VecDeque<ServerEvent>>>,
    /// The close frame the server sent, if it sent one.
    ///
    /// Recorded because the event stream reports a deliberate server-side
    /// close and a dead socket as the same `None`, so a polling caller cannot
    /// tell an aborted session from a broken one without it.
    last_disconnect: Arc<ParkingMutex<Option<crate::session::DisconnectReason>>>,
    /// Who owns turn detection on this connection.
    ///
    /// Derived from the same config that produced the `setup` frame, so it
    /// cannot disagree with what the server was told.
    activity_detection: ActivityDetection,
    /// Whether an `activityStart` is currently outstanding (manual mode only).
    ///
    /// Makes the outbound signal edge-triggered: several observers of one
    /// barge-in collapse to a single frame. See [`ActivitySignaller`].
    activity_open: Arc<AtomicBool>,
    /// Which function-declaration field tool schemas are posted under.
    ///
    /// Held on the session because the setup frame is built from `&self`, and
    /// because it must match the dialect the caller reduced its schemas to —
    /// posting a schema that kept `additionalProperties` under the legacy
    /// `parameters` field closes the socket with WS 1007.
    schema_dialect: adk_gemini::GeminiSchemaDialect,
}

impl GeminiRealtimeSession {
    fn flush_threshold_bytes(format: &AudioFormat) -> usize {
        let bytes_per_second = format.bytes_per_second() as usize;
        // Compute target bytes for a 40ms chunk and round up so we don't under-buffer.
        // `max(1)` keeps the threshold valid even for pathological/invalid formats.
        bytes_per_second.saturating_mul(AUDIO_FLUSH_TARGET_MS).div_ceil(1000).max(1)
    }

    /// Connect to Gemini Live API using the specified backend.
    pub async fn connect(
        backend: GeminiLiveBackend,
        model: &str,
        config: RealtimeConfig,
    ) -> Result<Self> {
        Self::connect_with_dialect(backend, model, config, Default::default()).await
    }

    /// Connect, declaring which schema dialect the caller reduced its tool
    /// schemas to.
    ///
    /// Use this when tool schemas were produced by
    /// [`GeminiSchemaAdapter::json_schema()`](adk_gemini::GeminiSchemaAdapter::json_schema):
    /// their constraints survive only if they are posted under
    /// `parametersJsonSchema`, and this is what selects that field.
    /// [`connect`](Self::connect) keeps the legacy `parameters` field, so
    /// existing callers are unaffected.
    pub async fn connect_with_dialect(
        backend: GeminiLiveBackend,
        model: &str,
        config: RealtimeConfig,
        schema_dialect: adk_gemini::GeminiSchemaDialect,
    ) -> Result<Self> {
        let ws_stream = match &backend {
            GeminiLiveBackend::Studio { api_key } => {
                let url = format!(
                    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContent?key={}",
                    api_key
                );
                let request = url.into_client_request().map_err(|e| {
                    RealtimeError::connection(format!("Failed to create request: {}", e))
                })?;
                let (ws, response) = connect_async(request).await.map_err(|e| {
                    RealtimeError::connection(format!("WebSocket connect error: {}", e))
                })?;
                tracing::info!(status = ?response.status(), "Gemini WebSocket handshake successful");
                ws
            }
            #[cfg(feature = "vertex-live")]
            GeminiLiveBackend::Vertex { credentials, region, project_id } => {
                let url = build_vertex_live_url(region, project_id)?;

                // Obtain OAuth2 bearer token from ADC credentials
                let header_map =
                    match credentials.headers(Default::default()).await.map_err(|e| {
                        RealtimeError::AuthError(format!(
                            "Failed to obtain OAuth2 token from ADC credentials: {e}"
                        ))
                    })? {
                        google_cloud_auth::credentials::CacheableResource::New { data, .. } => data,
                        google_cloud_auth::credentials::CacheableResource::NotModified => {
                            return Err(RealtimeError::AuthError(
                            "ADC credentials returned NotModified with no cached token available"
                                .to_string(),
                        ));
                        }
                    };

                // Extract the Authorization header value
                let auth_value = header_map
                    .get("authorization")
                    .ok_or_else(|| {
                        RealtimeError::AuthError(
                            "ADC credentials did not produce an Authorization header".to_string(),
                        )
                    })?
                    .to_str()
                    .map_err(|e| {
                        RealtimeError::AuthError(format!(
                            "Authorization header contains non-ASCII characters: {e}"
                        ))
                    })?
                    .to_string();

                // Build a WebSocket request with the Authorization header
                let mut request = url.into_client_request().map_err(|e| {
                    RealtimeError::connection(format!("Failed to create request: {e}"))
                })?;
                request.headers_mut().insert(
                    "Authorization",
                    auth_value.parse().map_err(
                        |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                            RealtimeError::AuthError(format!(
                                "Failed to parse Authorization header value: {e}"
                            ))
                        },
                    )?,
                );

                let (ws, _) = connect_async(request).await.map_err(|e| {
                    RealtimeError::connection(format!(
                        "Vertex AI Live WebSocket connect error: {e}"
                    ))
                })?;
                ws
            }
        };

        let (mut sink, source) = ws_stream.split();
        let connected = Arc::new(AtomicBool::new(true));
        let (outbound_tx, mut outbound_rx) = mpsc::channel(WRITER_CHANNEL_CAPACITY);
        let writer_connected = Arc::clone(&connected);
        let writer_task = tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                // Close frames are terminal for the writer lifecycle: send once then stop.
                let should_close = matches!(message, Message::Close(_));
                if let Err(error) = sink.send(message).await {
                    writer_connected.store(false, Ordering::SeqCst);
                    tracing::warn!(error = %error, "gemini websocket writer send failed");
                    break;
                }
                if should_close {
                    break;
                }
            }
            // Mark disconnected on *any* writer exit path (error, close, channel shutdown).
            writer_connected.store(false, Ordering::SeqCst);
        });

        let session_id = uuid::Uuid::new_v4().to_string();
        let activity_detection = ActivityDetection::from_config(&config);

        let session = Self {
            session_id,
            connected,
            outbound_tx,
            writer_task: Arc::new(Mutex::new(Some(writer_task))),
            receiver: Arc::new(Mutex::new(source)),
            audio_buffer: Arc::new(ParkingMutex::new(BytesMut::new())),
            last_disconnect: Arc::new(ParkingMutex::new(None)),
            event_queue: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            activity_detection,
            activity_open: Arc::new(AtomicBool::new(false)),
            schema_dialect,
        };

        session.send_setup(model, config).await?;
        Ok(session)
    }

    /// Who owns turn detection on this session.
    pub fn activity_detection(&self) -> ActivityDetection {
        self.activity_detection
    }

    /// Permission to send `activityStart` / `activityEnd`, or `None` if the
    /// server is doing its own activity detection and would reject them.
    ///
    /// See [`ActivitySignaller`].
    pub fn activity_signaller(&self) -> Option<ActivitySignaller<'_>> {
        match self.activity_detection {
            ActivityDetection::Manual => Some(ActivitySignaller { session: self }),
            ActivityDetection::Automatic => None,
        }
    }

    /// Emit one activity frame, if it is a real state transition.
    ///
    /// The mode check is redundant for callers coming through
    /// [`ActivitySignaller`], which cannot exist in automatic mode. It is here
    /// because the alternative to a loud error on the in-crate paths — such as
    /// [`interrupt`](RealtimeSession::interrupt) — is a frame the server
    /// rejects, arriving as an unexplained protocol error later in the stream.
    async fn send_activity(&self, signal: ActivitySignal) -> Result<ActivitySignalOutcome> {
        if self.activity_detection != ActivityDetection::Manual {
            return Err(RealtimeError::config(format!(
                "cannot send {signal:?} activity signal: Gemini Live accepts activityStart/activityEnd \
                 only while automatic activity detection is disabled. Connect with \
                 `RealtimeConfig::without_vad()` to take ownership of turn detection."
            )));
        }

        // Claim the transition before sending, so two tasks reporting the same
        // barge-in produce one frame rather than racing to write two.
        let target = signal.target_state();
        if self
            .activity_open
            .compare_exchange(!target, target, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!(
                session_id = %self.session_id,
                signal = ?signal,
                "activity signal suppressed: session is already in that state"
            );
            return Ok(ActivitySignalOutcome::Redundant);
        }

        let msg = GeminiClientMessage {
            realtime_input: Some(signal.into_realtime_input()),
            ..Default::default()
        };

        match self.send_raw(&msg).await {
            Ok(()) => Ok(ActivitySignalOutcome::Sent),
            Err(e) => {
                // The frame never reached the connection, so the session did
                // not make the transition. Leaving the flag claimed would
                // silently drop the caller's next, genuine signal.
                self.activity_open.store(!target, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// Flush any buffered audio to the server.
    async fn flush_audio(&self) -> Result<()> {
        let data = {
            let mut buffer = self.audio_buffer.lock();
            if !buffer.is_empty() { Some(std::mem::take(&mut *buffer).freeze()) } else { None }
        };

        if let Some(data) = data {
            self.send_audio_bytes(data).await?;
        }
        Ok(())
    }

    /// Send a raw PCM audio payload by encoding it to base64 for Gemini wire format.
    async fn send_audio_bytes(&self, audio_bytes: Bytes) -> Result<()> {
        let audio_base64 = base64::engine::general_purpose::STANDARD.encode(audio_bytes);
        self.send_audio_base64(&audio_base64).await
    }

    /// Send initial setup message.
    async fn send_setup(&self, model: &str, config: RealtimeConfig) -> Result<()> {
        let setup = Self::build_setup_message(model, config, self.schema_dialect);

        if self.activity_detection == ActivityDetection::Manual {
            // Loud, because this is the configuration in which the server does
            // nothing on its own: a caller that disables automatic detection
            // and never signals gets a session that never takes a turn, and
            // the symptom (a model that simply does not answer) points nowhere
            // near the cause.
            tracing::warn!(
                session_id = %self.session_id,
                "Gemini server-side activity detection disabled; this session will not detect \
                 turns until the client sends activityStart/activityEnd via `activity_signaller()`"
            );
        }

        tracing::info!(model_id = %model, "Sending Gemini Live setup");
        self.send_raw(&setup).await
    }

    /// Build the `setup` frame for a config.
    ///
    /// Split out from [`send_setup`](Self::send_setup) so the serialized shape
    /// can be asserted without a socket — the setup frame is the only place a
    /// session states who owns turn detection, and getting it wrong is not
    /// visible until a live call misbehaves.
    fn build_setup_message(
        model: &str,
        config: RealtimeConfig,
        dialect: adk_gemini::GeminiSchemaDialect,
    ) -> GeminiClientMessage {
        let realtime_input_config = ActivityDetection::from_config(&config).realtime_input_config();

        let mut generation_config = json!({
            "responseModalities": config.modalities.unwrap_or_else(|| vec!["AUDIO".to_string()]),
        });

        if let Some(voice) = &config.voice {
            generation_config["speechConfig"] = json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice
                    }
                }
            });
        }

        // Spoken language (`RealtimeConfig::with_language`). Without it the model detects the
        // language per turn, and short or accented utterances are transcribed in the wrong one.
        if let Some(code) = config
            .extra
            .as_ref()
            .and_then(|ext| ext.get("language_code"))
            .and_then(|val| val.as_str())
            .filter(|code| !code.is_empty())
        {
            generation_config["speechConfig"]["languageCode"] = json!(code);
        }

        if let Some(temp) = config.temperature {
            generation_config["temperature"] = json!(temp);
        }

        // Emotion-aware ("affective") dialog — a generationConfig field, honored
        // by native-audio models on the v1alpha endpoint.
        if config.affective_dialog == Some(true) {
            generation_config["enableAffectiveDialog"] = json!(true);
        }

        if let Some(extra) = &config.extra
            && let Some(thinking_level) = extra.get("thinking_level")
            && let Some(obj) = generation_config.as_object_mut()
        {
            obj.insert("thinkingConfig".to_string(), json!({ "thinkingLevel": thinking_level }));
        }

        let system_instruction = config.instruction.map(|text| GeminiContent {
            parts: vec![GeminiPart { text: Some(text), inline_data: None }],
        });

        let tools = convert_tools(config.tools, dialect);

        // Functionally extract the token if it exists in the prior state map
        let handle = config
            .extra
            .as_ref()
            .and_then(|ext| ext.get("resumeToken"))
            .and_then(|val| val.as_str())
            .map(|s| s.to_string());

        let session_resumption = Some(SessionResumptionConfig { handle });

        // When transcription is requested, enable both input (user speech) and
        // output (model speech) transcription so consumers get clean text for
        // native-audio turns. An empty object turns the feature on.
        let transcription = config.input_audio_transcription.as_ref().map(|_| json!({}));

        GeminiClientMessage {
            setup: Some(GeminiSetup {
                model: model.to_string(),
                system_instruction,
                generation_config: Some(generation_config),
                tools,
                cached_content: config.cached_content,
                session_resumption,
                input_audio_transcription: transcription.clone(),
                output_audio_transcription: transcription,
                realtime_input_config,
            }),
            ..Default::default()
        }
    }

    /// Send a raw message.
    async fn send_raw<T: Serialize>(&self, value: &T) -> Result<()> {
        let msg = serde_json::to_string(value)
            .map_err(|e| RealtimeError::protocol(format!("JSON serialize error: {}", e)))?;

        self.outbound_tx
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| RealtimeError::connection(format!("Send queue error: {e}")))?;

        Ok(())
    }

    /// Receive and parse the next message.
    async fn receive_raw(&self) -> Option<Result<ServerEvent>> {
        // First check if there's anything in the queue
        {
            let mut queue = self.event_queue.lock().await;
            if let Some(event) = queue.pop_front() {
                return Some(Ok(event));
            }
        }

        let mut receiver = self.receiver.lock().await;

        match receiver.next().await {
            Some(Ok(Message::Text(text))) => match self.translate_gemini_event(&text) {
                Ok(events) => {
                    let mut queue = self.event_queue.lock().await;
                    let mut iter = events.into_iter();
                    let first = iter.next();
                    for event in iter {
                        queue.push_back(event);
                    }
                    first.map(Ok)
                }
                Err(e) => Some(Err(e)),
            },
            Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => match self.translate_gemini_event(&text) {
                    Ok(events) => {
                        let mut queue = self.event_queue.lock().await;
                        let mut iter = events.into_iter();
                        let first = iter.next();
                        for event in iter {
                            queue.push_back(event);
                        }
                        first.map(Ok)
                    }
                    Err(e) => Some(Err(e)),
                },
                Err(e) => Some(Err(RealtimeError::protocol(format!(
                    "Invalid UTF-8 in binary message: {}",
                    e
                )))),
            },
            Some(Ok(Message::Close(close_frame))) => {
                // Structured, because a server-initiated close and a dead
                // transport both surface downstream as the same `None` and get
                // the same terminal reason. The code and reason are the only
                // thing distinguishing "the provider aborted an idle session"
                // from "the network dropped", and `{:?}` on the whole frame
                // buries both inside a Debug string nothing can filter on.
                tracing::error!(
                    close_code =
                        close_frame.as_ref().map(|frame| u16::from(frame.code)).unwrap_or_default(),
                    close_reason =
                        close_frame.as_ref().map(|frame| frame.reason.as_ref()).unwrap_or(""),
                    "WebSocket closed by server"
                );
                *self.last_disconnect.lock() = Some(crate::session::DisconnectReason {
                    code: close_frame.as_ref().map(|frame| u16::from(frame.code)),
                    reason: close_frame
                        .as_ref()
                        .map(|frame| frame.reason.to_string())
                        .unwrap_or_default(),
                });
                self.connected.store(false, Ordering::SeqCst);
                None
            }
            Some(Ok(msg)) => {
                tracing::warn!("Received unhandled tungstenite message: {:?}", msg);
                Some(Ok(ServerEvent::Unknown))
            }
            Some(Err(e)) => {
                self.connected.store(false, Ordering::SeqCst);
                Some(Err(RealtimeError::connection(format!("Receive error: {}", e))))
            }
            None => {
                self.connected.store(false, Ordering::SeqCst);
                None
            }
        }
    }

    /// Translate Gemini-specific events to unified format.
    fn translate_gemini_event(&self, raw: &str) -> Result<Vec<ServerEvent>> {
        tracing::debug!(%raw, "Translating Gemini event");
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| RealtimeError::protocol(format!("Parse error: {}, raw: {}", e, raw)))?;

        // Check for setup completion
        if let Some(_setup_complete) = value.get("setupComplete") {
            return Ok(vec![ServerEvent::SessionCreated {
                event_id: uuid::Uuid::new_v4().to_string(),
                session: value.clone(),
            }]);
        }

        // Check for server content (audio/text)
        if let Some(content) = value.get("serverContent") {
            let mut events = Vec::new();

            if let Some(parts) = content.get("modelTurn").and_then(|t| t.get("parts"))
                && let Some(parts_arr) = parts.as_array()
            {
                for part in parts_arr {
                    // Audio output — decode base64 to raw bytes
                    if let Some(inline_data) = part.get("inlineData")
                        && let Some(data) = inline_data.get("data").and_then(|d| d.as_str())
                    {
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .unwrap_or_default();
                        events.push(ServerEvent::AudioDelta {
                            event_id: uuid::Uuid::new_v4().to_string(),
                            response_id: String::new(),
                            item_id: String::new(),
                            output_index: 0,
                            content_index: 0,
                            delta: decoded,
                        });
                    }
                    // Text output
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        events.push(ServerEvent::TextDelta {
                            event_id: uuid::Uuid::new_v4().to_string(),
                            response_id: String::new(),
                            item_id: String::new(),
                            output_index: 0,
                            content_index: 0,
                            delta: text.to_string(),
                        });
                    }
                }
            }

            // Output transcription: the model's spoken words as text (enabled via
            // outputAudioTranscription). Surfaced as a transcript delta so it
            // reads the same as OpenAI's audio transcript.
            if let Some(text) = content
                .get("outputTranscription")
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str())
                && !text.is_empty()
            {
                events.push(ServerEvent::TranscriptDelta {
                    event_id: uuid::Uuid::new_v4().to_string(),
                    response_id: String::new(),
                    item_id: String::new(),
                    output_index: 0,
                    content_index: 0,
                    delta: text.to_string(),
                });
            }

            // Input transcription: the user's speech as text (streamed in chunks).
            if let Some(text) = content
                .get("inputTranscription")
                .and_then(|o| o.get("text"))
                .and_then(|t| t.as_str())
                && !text.is_empty()
            {
                events.push(ServerEvent::InputTranscriptDelta {
                    item_id: String::new(),
                    content_index: 0,
                    delta: text.to_string(),
                });
            }

            if let Some(turn_complete) = content.get("turnComplete")
                && turn_complete.as_bool().unwrap_or(false)
            {
                events.push(ServerEvent::ResponseDone {
                    event_id: uuid::Uuid::new_v4().to_string(),
                    response: value.clone(),
                });
            }

            if !events.is_empty() {
                return Ok(events);
            }
        }

        // Catch the Server Update for sessionResumptionUpdate
        // Note the intentional protocol asymmetry here: the client sends the parameter as handle,
        // but the server transmits the parameter back as resumptionToken.
        // Reference: https://ai.google.dev/gemini-api/docs/live-api/session-management
        if let Some(resumption_update) = value.get("sessionResumptionUpdate")
            && let Some(token) = resumption_update.get("resumptionToken").and_then(|t| t.as_str())
        {
            tracing::debug!("Received new Gemini 2.5 Native resumption token");
            return Ok(vec![ServerEvent::SessionUpdated {
                event_id: uuid::Uuid::new_v4().to_string(),
                session: json!({ "resumeToken": token }),
            }]);
        }

        // Check for tool calls
        if let Some(tool_call) = value.get("toolCall")
            && let Some(calls) = tool_call.get("functionCalls").and_then(|c| c.as_array())
            && !calls.is_empty()
        {
            // Gemini batches parallel function calls in one frame; emit one
            // event per call — dropping any leaves the model waiting forever
            // for the missing function response.
            return Ok(calls
                .iter()
                .enumerate()
                .map(|(idx, call)| {
                    let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let args = call.get("args").cloned().unwrap_or(json!({}));
                    ServerEvent::FunctionCallDone {
                        event_id: uuid::Uuid::new_v4().to_string(),
                        response_id: String::new(),
                        item_id: String::new(),
                        output_index: idx as u32,
                        call_id: id.to_string(),
                        name: name.to_string(),
                        arguments: serde_json::to_string(&args).unwrap_or_default(),
                    }
                })
                .collect());
        }

        Ok(vec![ServerEvent::Unknown])
    }
}

#[async_trait]
impl RealtimeSession for GeminiRealtimeSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn disconnect_reason(&self) -> Option<crate::session::DisconnectReason> {
        self.last_disconnect.lock().clone()
    }

    async fn send_audio(&self, audio: &AudioChunk) -> Result<()> {
        // Format-aware threshold (sample rate/channels/bit depth), avoids hardcoded 16k assumptions.
        let flush_threshold_bytes = Self::flush_threshold_bytes(&audio.format);

        // Smart Audio Buffering: buffer small chunks to avoid overhead
        let data = {
            let mut buffer = self.audio_buffer.lock();
            buffer.put_slice(&audio.data);

            if buffer.len() >= flush_threshold_bytes {
                Some(std::mem::take(&mut *buffer).freeze())
            } else {
                None
            }
        };

        if let Some(data) = data {
            self.send_audio_bytes(data).await?;
        }
        Ok(())
    }

    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        let msg = GeminiClientMessage {
            realtime_input: Some(GeminiRealtimeInput {
                audio: Some(GeminiMediaChunk {
                    // Gemini Live requires raw 16-bit PCM little-endian at 16 kHz;
                    // declaring the rate removes any server-side ambiguity.
                    mime_type: "audio/pcm;rate=16000".to_string(),
                    data: audio_base64.to_string(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.send_raw(&msg).await
    }

    async fn send_video_frame(&self, mime_type: &str, data_base64: &str) -> Result<()> {
        // Gemini Live accepts image frames as realtimeInput media chunks.
        let msg = GeminiClientMessage {
            realtime_input: Some(GeminiRealtimeInput {
                media_chunks: Some(vec![GeminiMediaChunk {
                    mime_type: mime_type.to_string(),
                    data: data_base64.to_string(),
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.send_raw(&msg).await
    }

    async fn send_text(&self, text: &str) -> Result<()> {
        // Use client_content with turns (correct Gemini Live API format)
        let msg = GeminiClientMessage {
            client_content: Some(GeminiClientContent {
                turns: vec![GeminiTurn {
                    role: "user".to_string(),
                    parts: vec![GeminiPart { text: Some(text.to_string()), inline_data: None }],
                }],
                turn_complete: true,
            }),
            ..Default::default()
        };
        self.send_raw(&msg).await
    }

    async fn send_tool_response(&self, response: ToolResponse) -> Result<()> {
        let output = match &response.output {
            Value::String(s) => json!({ "result": s }),
            other => other.clone(),
        };

        let msg = GeminiClientMessage {
            tool_response: Some(GeminiToolResponse {
                function_responses: vec![GeminiFunctionResponse {
                    id: response.call_id,
                    response: output,
                }],
            }),
            ..Default::default()
        };
        self.send_raw(&msg).await
    }

    async fn commit_audio(&self) -> Result<()> {
        self.flush_audio().await
    }

    async fn clear_audio(&self) -> Result<()> {
        let mut buffer = self.audio_buffer.lock();
        buffer.clear();
        Ok(())
    }

    async fn create_response(&self) -> Result<()> {
        Ok(()) // Gemini auto-generates responses
    }

    async fn interrupt(&self) -> Result<()> {
        // Either way, drop audio we buffered but never framed: it belongs to
        // the turn being abandoned.
        self.clear_audio().await?;

        match self.activity_detection {
            ActivityDetection::Manual => {
                // With automatic detection disabled the server has no way to
                // learn the user has taken the turn, so `activityStart` is the
                // only thing that can carry a barge-in. Google documents it as
                // interrupting the model's response under the default
                // `activityHandling`; what is asserted here is that the frame
                // was sent, not what the server did with it.
                let outcome = self.send_activity(ActivitySignal::Start).await?;
                tracing::debug!(
                    session_id = %self.session_id,
                    ?outcome,
                    "interrupt sent activityStart"
                );
                Ok(())
            }
            ActivityDetection::Automatic => {
                // Deliberately not an error: this is the default configuration
                // and an interruption really is performed here, by the
                // server's own VAD on speech onset. What this call cannot do
                // is *initiate* one — Gemini Live has no client-side response
                // cancel, and `activityStart` is rejected while server-side
                // detection is enabled. So nothing goes to the server, and the
                // log says so rather than the return value implying otherwise.
                tracing::debug!(
                    session_id = %self.session_id,
                    "interrupt sent nothing to the server: Gemini's server-side activity detection \
                     owns barge-in in this configuration, and client activity frames are only legal \
                     with it disabled. Only unsent local audio was discarded."
                );
                Ok(())
            }
        }
    }

    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        match event {
            // Intercept standard messages from the orchestrator
            ClientEvent::Message { role, parts } => {
                let msg = translate_client_message(&role, parts);
                tracing::info!(role = ?role, "Injecting mid-flight context via native adk-rust types");
                self.send_raw(&msg).await
            }

            // Explicitly handle all other unified ClientEvent variants.
            // Returning Ok(()) silently for unsupported features is an anti-pattern.
            // We log a clear warning that Gemini does not natively support this specific control event.
            ClientEvent::AudioDelta { .. } => {
                tracing::warn!(
                    "AudioDelta is explicitly handled via `send_audio`, not `send_event`. Dropping event."
                );
                Ok(())
            }
            ClientEvent::InputAudioBufferCommit => {
                tracing::warn!(
                    "Gemini Live API does not support manual audio buffer commits. Dropping event."
                );
                Ok(())
            }
            ClientEvent::InputAudioBufferClear => {
                tracing::warn!(
                    "Gemini Live API does not support manual audio buffer clears via wire events. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ConversationItemCreate { .. } => {
                tracing::warn!(
                    "Raw ConversationItemCreate is an OpenAI construct. Use ClientEvent::Message instead for Gemini. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ResponseCreate { .. } => {
                tracing::warn!(
                    "Gemini Live API automatically generates responses based on VAD/turns. Manual ResponseCreate is unsupported. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ResponseCancel => match self.activity_detection {
                // Actionable here: with server-side detection off, `activityStart`
                // is the cancel, so honour the event instead of dropping it.
                ActivityDetection::Manual => self.interrupt().await,
                ActivityDetection::Automatic => {
                    tracing::warn!(
                        "Gemini Live API has no client-side response cancel while server-side \
                         activity detection is enabled; it interrupts on its own VAD. Connect with \
                         `RealtimeConfig::without_vad()` to drive interruption from the client. \
                         Dropping event."
                    );
                    Ok(())
                }
            },
            ClientEvent::SessionUpdate { .. } => {
                tracing::warn!(
                    "Raw SessionUpdate is an OpenAI construct. Use RealtimeRunner's `update_session` for provider-agnostic Context Mutation. Dropping event."
                );
                Ok(())
            }
            ClientEvent::UpdateSession { .. } => {
                tracing::error!(
                    "Internal UpdateSession intent leaked to the Gemini transport socket. This should have been intercepted by the RealtimeRunner."
                );
                Err(RealtimeError::ProviderError("Internal intent leaked to transport".to_string()))
            }
        }
    }

    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        self.receive_raw().await
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(async_stream::stream! {
            while self.is_connected() {
                match self.receive_raw().await {
                    Some(Ok(event)) => yield Ok(event),
                    Some(Err(e)) => yield Err(e),
                    None => break,
                }
            }
        })
    }

    async fn close(&self) -> Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        // Route close through the same channel as normal writes so ordering is preserved.
        let _ = self.outbound_tx.send(Message::Close(None)).await;

        let mut writer_task = self.writer_task.lock().await;
        if let Some(handle) = writer_task.take() {
            // Ensure deterministic teardown: don't return until the writer released the sink.
            let _ = handle.await;
        }
        Ok(())
    }

    async fn mutate_context(
        &self,
        config: crate::config::RealtimeConfig,
    ) -> Result<ContextMutationOutcome> {
        tracing::info!(
            "Gemini API does not support native mid-flight context swaps; signalling resumption needed."
        );
        Ok(ContextMutationOutcome::RequiresResumption(Box::new(config)))
    }
}

impl std::fmt::Debug for GeminiRealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiRealtimeSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .finish()
    }
}

/// Construct the Vertex AI Live WebSocket URL from region and project ID.
///
/// Returns `RealtimeError::ConfigError` if region or project_id is empty.
#[cfg(feature = "vertex-live")]
pub fn build_vertex_live_url(region: &str, project_id: &str) -> Result<String> {
    if region.is_empty() {
        return Err(RealtimeError::config("Vertex AI Live requires a non-empty region"));
    }
    if project_id.is_empty() {
        return Err(RealtimeError::config("Vertex AI Live requires a non-empty project_id"));
    }
    Ok(format!(
        "wss://{region}-aiplatform.googleapis.com/ws/\
         google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent\
         ?project_id={project_id}",
    ))
}

/// Pure translation function for converting a standard `adk_core` message into
/// Gemini Live API's native `clientContent` payload.
pub(crate) fn translate_client_message(
    role: &str,
    parts: Vec<adk_core::types::Part>,
) -> GeminiClientMessage {
    // 1. Translate the polymorphic `adk_core::types::Part` elements into strictly-typed `GeminiPart` structures.
    let mut gemini_parts: Vec<GeminiPart> = Vec::new();
    for p in parts {
        match p {
            adk_core::types::Part::Text { text } => {
                gemini_parts.push(GeminiPart { text: Some(text), inline_data: None });
            }
            adk_core::types::Part::InlineData { mime_type, data, .. } => {
                // Gemini natively encodes binary artifacts (images/audio) via a base64 payload envelope.
                let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                gemini_parts.push(GeminiPart {
                    text: None,
                    inline_data: Some(GeminiInlineData { mime_type, data: encoded }),
                });
            }

            // 2. Gracefully skip semantic elements that Google's Live API `clientContent` turn does not support
            // using `tracing::warn!`, avoiding "silent data loss" or injecting empty string placeholders.
            adk_core::types::Part::FileData { file_uri, .. } => {
                tracing::warn!(
                    "Dropping unsupported FileData part in Gemini session: {}",
                    file_uri
                );
            }
            adk_core::types::Part::Thinking { .. } => {
                tracing::warn!("Dropping unsupported Thinking part in Gemini session");
            }
            adk_core::types::Part::FunctionCall { name, .. } => {
                tracing::warn!(
                    "Dropping unsupported FunctionCall part in Gemini session: {}",
                    name
                );
            }
            adk_core::types::Part::FunctionResponse { .. } => {
                tracing::warn!("Dropping unsupported FunctionResponse part in Gemini session");
            }
            adk_core::types::Part::ServerToolCall { .. } => {
                tracing::warn!("Dropping unsupported ServerToolCall part in Gemini session");
            }
            adk_core::types::Part::ServerToolResponse { .. } => {
                tracing::warn!("Dropping unsupported ServerToolResponse part in Gemini session");
            }
            adk_core::types::Part::EmbeddedResource { resource } => {
                tracing::warn!(
                    "Dropping unsupported EmbeddedResource part in Gemini session: {}",
                    resource.uri()
                );
            }
        }
    }

    // 3. Coerce the `Role`.
    // Gemini Live's bidirectional socket strongly rejects `system` or `developer` roles
    // inside mid-flight `clientContent` turns. To support Cognitive Handoffs, we intercept
    // the system instruction and safely masquerade it as a high-priority "user" turn.
    let (gemini_role, final_parts) = match role {
        "system" | "developer" => {
            let mut modified_parts = gemini_parts;
            let mut text_injected = false;

            // Iterate to find the first actual text element in the user's prompt (avoiding images/audio arrays)
            // to safely inject the system prefix.
            for part in modified_parts.iter_mut() {
                if let Some(ref mut text) = part.text {
                    *text = format!("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\n{}", text);
                    text_injected = true;
                    break;
                }
            }

            // If the orchestrator sent a `system` role containing exclusively non-text data (e.g., just an image),
            // construct a synthetic text part to carry the directive.
            if !text_injected {
                modified_parts.insert(
                    0,
                    GeminiPart {
                        text: Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]".to_string()),
                        inline_data: None,
                    },
                );
            }

            ("user".to_string(), modified_parts)
        }
        "user" => ("user".to_string(), gemini_parts),
        "model" | "assistant" => ("model".to_string(), gemini_parts),
        _ => ("user".to_string(), gemini_parts), // Default fallback for custom orchestrator roles
    };

    // 4. Construct the native `GeminiClientContent` wire envelope.
    GeminiClientMessage {
        client_content: Some(GeminiClientContent {
            turns: vec![GeminiTurn { role: gemini_role, parts: final_parts }],
            turn_complete: true,
        }),
        ..Default::default()
    }
}

/// Convert ADK tool definitions to Gemini format.
///
/// `dialect` decides the field the schema is posted under. It is a parameter
/// rather than a constant because Gemini's two schema fields are mutually
/// exclusive and express different things: `parameters` takes an OpenAPI
/// subset, `parametersJsonSchema` takes standard JSON Schema. Hardcoding the
/// legacy field silently discards every constraint the subset cannot carry —
/// `additionalProperties`, `allOf`, `if`/`then`, `minLength`, the numeric
/// bounds — leaving the model to guess at rules the caller still enforces.
fn convert_tools(
    tools: Option<Vec<ToolDefinition>>,
    dialect: adk_gemini::GeminiSchemaDialect,
) -> Option<Vec<Value>> {
    let field = dialect.parameters_field();
    tools.map(|tools| {
        vec![json!({
            "functionDeclarations": tools.iter().map(|t| {
                let mut decl = json!({ "name": t.name });
                if let Some(desc) = &t.description {
                    decl["description"] = json!(desc);
                }
                if let Some(params) = &t.parameters {
                    decl[field] = params.clone();
                }
                decl
            }).collect::<Vec<_>>()
        })]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::types::Part;

    #[test]
    fn test_gemini_translate_text_only() {
        let parts = vec![Part::Text { text: "Hello".to_string() }];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        assert_eq!(content.turns.len(), 1);
        assert_eq!(content.turns[0].role, "user");

        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 1);
        assert_eq!(gemini_parts[0].text.as_deref(), Some("Hello"));
        assert!(gemini_parts[0].inline_data.is_none());
    }

    #[test]
    fn test_gemini_translate_text_and_inline_data() {
        let parts = vec![
            Part::Text { text: "Look:".to_string() },
            Part::inline_data("image/png", vec![0x1, 0x2]),
        ];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        assert_eq!(gemini_parts[0].text.as_deref(), Some("Look:"));

        let inline = gemini_parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline.mime_type, "image/png");
        assert_eq!(inline.data, "AQI="); // base64 encoded [1,2]
    }

    #[test]
    fn test_gemini_system_override_text_first() {
        let parts = vec![Part::Text { text: "Be helpful".to_string() }];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        assert_eq!(content.turns[0].role, "user"); // coerced

        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 1);
        assert_eq!(
            gemini_parts[0].text.as_deref(),
            Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\nBe helpful")
        );
    }

    #[test]
    fn test_gemini_system_override_non_text_first() {
        let parts = vec![
            Part::inline_data("image/png", vec![0x1]),
            Part::Text { text: "Analyze this".to_string() },
        ];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        // Ensure the directive was applied to the FIRST text part, despite being index 1
        assert!(gemini_parts[0].inline_data.is_some());
        assert_eq!(
            gemini_parts[1].text.as_deref(),
            Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\nAnalyze this")
        );
    }

    #[test]
    fn test_gemini_system_override_no_text() {
        let parts = vec![Part::inline_data("image/png", vec![0x1])];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        // Ensure a new text part was inserted at the beginning
        assert_eq!(gemini_parts[0].text.as_deref(), Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]"));
        assert!(gemini_parts[1].inline_data.is_some());
    }

    #[test]
    fn test_gemini_skips_unsupported_parts() {
        let parts = vec![
            Part::Text { text: "First".to_string() },
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "http://example.com/img".to_string(),
                annotations: None,
            }, // Should be skipped
            Part::Thinking { thinking: "Hmm".to_string(), signature: None }, // Should be skipped
            Part::Text { text: "Last".to_string() },
        ];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        assert_eq!(gemini_parts[0].text.as_deref(), Some("First"));
        assert_eq!(gemini_parts[1].text.as_deref(), Some("Last"));
    }
    #[test]
    fn test_gemini_setup_serialization_includes_model() {
        let setup = GeminiSetup {
            model: "models/gemini-2.5-flash-native-audio-latest".to_string(),
            system_instruction: None,
            generation_config: None,
            tools: None,
            cached_content: None,
            session_resumption: None,
            input_audio_transcription: None,
            output_audio_transcription: None,
            realtime_input_config: None,
        };
        let wrapper = GeminiClientMessage { setup: Some(setup), ..Default::default() };
        let js = serde_json::to_value(&wrapper).unwrap();
        let setup_json = js.get("setup").expect("setup missing").as_object().unwrap();
        assert_eq!(
            setup_json.get("model").expect("model missing from setup payload").as_str().unwrap(),
            "models/gemini-2.5-flash-native-audio-latest"
        );
    }

    #[test]
    fn test_language_code_lands_in_speech_config_next_to_the_voice() {
        let js = setup_json(RealtimeConfig::default().with_voice("Aoede").with_language("en-US"));
        let speech = &js["setup"]["generationConfig"]["speechConfig"];
        assert_eq!(speech["languageCode"], json!("en-US"));
        assert_eq!(speech["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"], json!("Aoede"));
    }

    #[test]
    fn test_language_code_without_a_voice_still_builds_speech_config() {
        let js = setup_json(RealtimeConfig::default().with_language("en-GB"));
        assert_eq!(js["setup"]["generationConfig"]["speechConfig"]["languageCode"], json!("en-GB"));
        assert!(
            setup_json(RealtimeConfig::default())["setup"]["generationConfig"]
                .get("speechConfig")
                .is_none()
        );
        assert!(
            setup_json(RealtimeConfig::default().with_language(""))["setup"]["generationConfig"]
                .get("speechConfig")
                .is_none()
        );
    }

    #[test]
    fn test_language_code_keeps_other_extras_and_replaces_a_non_object_extra() {
        let config =
            RealtimeConfig { extra: Some(json!({"thinking_level": "low"})), ..Default::default() };
        let extra = config.with_language("en-US").extra.unwrap();
        assert_eq!(extra, json!({"thinking_level": "low", "language_code": "en-US"}));

        let config =
            RealtimeConfig { extra: Some(json!(["not", "an", "object"])), ..Default::default() };
        let extra = config.with_language("fr-FR").extra.unwrap();
        assert_eq!(extra, json!({"language_code": "fr-FR"}));
    }

    #[test]
    fn test_affective_dialog_builder_sets_config() {
        let c = RealtimeConfig::default().with_affective_dialog(true);
        assert_eq!(c.affective_dialog, Some(true));
        assert_eq!(RealtimeConfig::default().affective_dialog, None);
    }

    #[test]
    fn test_flush_threshold_bytes_pcm16_16khz_40ms() {
        let threshold = GeminiRealtimeSession::flush_threshold_bytes(&AudioFormat::pcm16_16khz());
        assert_eq!(threshold, 1280);
    }

    // ── Activity signalling ─────────────────────────────────────────────

    use crate::config::{VadConfig, VadMode};

    fn setup_json(config: RealtimeConfig) -> Value {
        let msg = GeminiRealtimeSession::build_setup_message(
            "models/test",
            config,
            adk_gemini::GeminiSchemaDialect::default(),
        );
        serde_json::to_value(&msg).expect("setup serializes")
    }

    /// The frame shape is the contract with Google. Asserting the whole
    /// serialized value, rather than probing one key, is what catches a field
    /// that silently moved out of `realtimeInput` or lost its casing.
    #[test]
    fn activity_start_serializes_as_documented() {
        let msg = GeminiClientMessage {
            realtime_input: Some(ActivitySignal::Start.into_realtime_input()),
            ..Default::default()
        };

        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            json!({ "realtimeInput": { "activityStart": {} } })
        );
    }

    #[test]
    fn activity_end_serializes_as_documented() {
        let msg = GeminiClientMessage {
            realtime_input: Some(ActivitySignal::End.into_realtime_input()),
            ..Default::default()
        };

        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            json!({ "realtimeInput": { "activityEnd": {} } })
        );
    }

    /// The two frames are separate transitions; emitting both in one message
    /// would describe a turn that started and ended simultaneously.
    #[test]
    fn an_activity_frame_carries_exactly_one_marker() {
        for signal in [ActivitySignal::Start, ActivitySignal::End] {
            let input = signal.into_realtime_input();
            assert_eq!(
                input.activity_start.is_some() as u8 + input.activity_end.is_some() as u8,
                1,
                "{signal:?} should set exactly one marker"
            );
            assert!(input.audio.is_none() && input.media_chunks.is_none() && input.text.is_none());
        }
    }

    #[test]
    fn only_vad_mode_none_asks_for_manual_activity_detection() {
        let cases = [
            (None, ActivityDetection::Automatic),
            (Some(VadMode::ServerVad), ActivityDetection::Automatic),
            (Some(VadMode::SemanticVad), ActivityDetection::Automatic),
            (Some(VadMode::None), ActivityDetection::Manual),
        ];

        for (mode, expected) in cases {
            let config = match mode {
                None => RealtimeConfig::default(),
                Some(mode) => {
                    RealtimeConfig::default().with_vad(VadConfig { mode, ..VadConfig::default() })
                }
            };
            assert_eq!(ActivityDetection::from_config(&config), expected, "mode {mode:?}");
        }
    }

    /// `VadMode::None` used to be a request Gemini never heard about. This is
    /// the assertion that it now reaches the wire.
    #[test]
    fn manual_mode_disables_automatic_activity_detection_in_setup() {
        let js = setup_json(RealtimeConfig::default().without_vad());

        assert_eq!(
            js["setup"]["realtimeInputConfig"],
            json!({ "automaticActivityDetection": { "disabled": true } })
        );
    }

    /// The default path is the one that must not move. Every configuration
    /// that leaves detection with the server has to serialize exactly as it
    /// did before `realtimeInputConfig` existed, which means the key must be
    /// absent rather than present-and-false.
    #[test]
    fn server_side_detection_setups_are_byte_identical_to_before() {
        let baseline = setup_json(RealtimeConfig::default());

        for config in [
            RealtimeConfig::default(),
            RealtimeConfig::default().with_server_vad(),
            RealtimeConfig::default().with_vad(VadConfig::semantic_vad()),
        ] {
            let js = setup_json(config);
            assert!(
                js["setup"].get("realtimeInputConfig").is_none(),
                "server-side detection must not send realtimeInputConfig, got {js}"
            );
            assert_eq!(js, baseline);
        }
    }

    // ── Session-level behaviour, over a real WebSocket ──────────────────

    use tokio_tungstenite::MaybeTlsStream;
    use tokio_tungstenite::tungstenite::protocol::Role;

    type ServerEnd = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A session wired to a loopback WebSocket, so tests observe the bytes the
    /// session actually writes rather than a stand-in for them.
    async fn session_on_loopback(
        activity_detection: ActivityDetection,
    ) -> (GeminiRealtimeSession, ServerEnd) {
        session_on_loopback_with_dialect(
            activity_detection,
            adk_gemini::GeminiSchemaDialect::default(),
        )
        .await
    }

    /// As [`session_on_loopback`], with an explicit schema dialect, so a test
    /// can prove the session's own dialect is what reaches the wire.
    async fn session_on_loopback_with_dialect(
        activity_detection: ActivityDetection,
        schema_dialect: adk_gemini::GeminiSchemaDialect,
    ) -> (GeminiRealtimeSession, ServerEnd) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (client_io, accepted) =
            tokio::join!(tokio::net::TcpStream::connect(addr), listener.accept());
        let client_io = client_io.unwrap();
        let (server_io, _) = accepted.unwrap();

        let client_ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            MaybeTlsStream::Plain(client_io),
            Role::Client,
            None,
        )
        .await;
        let server_ws =
            tokio_tungstenite::WebSocketStream::from_raw_socket(server_io, Role::Server, None)
                .await;

        let (mut sink, source) = client_ws.split();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(WRITER_CHANNEL_CAPACITY);
        let writer_task = tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        });

        let session = GeminiRealtimeSession {
            session_id: "test-session".to_string(),
            connected: Arc::new(AtomicBool::new(true)),
            outbound_tx,
            writer_task: Arc::new(Mutex::new(Some(writer_task))),
            receiver: Arc::new(Mutex::new(source)),
            audio_buffer: Arc::new(ParkingMutex::new(BytesMut::new())),
            event_queue: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            last_disconnect: Arc::new(ParkingMutex::new(None)),
            activity_detection,
            activity_open: Arc::new(AtomicBool::new(false)),
            schema_dialect,
        };

        (session, server_ws)
    }

    /// The next frame the server sees, as JSON.
    async fn next_frame(server: &mut ServerEnd) -> Value {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), server.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("stream ended")
            .expect("websocket error");
        match msg {
            Message::Text(text) => serde_json::from_str(&text).expect("frame is JSON"),
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_server_vad_session_offers_no_signaller() {
        let (session, _server) = session_on_loopback(ActivityDetection::Automatic).await;

        assert!(session.activity_signaller().is_none());
        assert_eq!(session.activity_detection(), ActivityDetection::Automatic);
    }

    /// The exclusivity rule is enforced by construction via
    /// `activity_signaller`, but the in-crate path still has to fail loudly
    /// rather than emit a frame the server would reject.
    #[tokio::test]
    async fn activity_frames_are_refused_while_server_detection_is_on() {
        let (session, _server) = session_on_loopback(ActivityDetection::Automatic).await;

        let err = session.send_activity(ActivitySignal::Start).await.unwrap_err();
        assert!(
            matches!(err, RealtimeError::ConfigError(_)),
            "expected a config error, got {err:?}"
        );
        assert!(err.to_string().contains("automatic activity detection is disabled"));
    }

    /// `interrupt()` on a server-VAD session must stay a local-only operation:
    /// this is the default configuration and sending anything would be a
    /// protocol violation. The sentinel proves nothing was written, rather
    /// than a timeout only suggesting it.
    #[tokio::test]
    async fn interrupt_writes_nothing_when_the_server_owns_detection() {
        let (session, mut server) = session_on_loopback(ActivityDetection::Automatic).await;

        session.interrupt().await.expect("interrupt is not an error on the default path");
        session.send_text("sentinel").await.unwrap();

        let frame = next_frame(&mut server).await;
        assert!(
            frame.get("clientContent").is_some(),
            "an activity frame preceded the sentinel: {frame}"
        );
    }

    /// The truthfulness fix: in manual mode `interrupt()` is no longer a
    /// local no-op that returns `Ok`.
    #[tokio::test]
    async fn interrupt_emits_activity_start_in_manual_mode() {
        let (session, mut server) = session_on_loopback(ActivityDetection::Manual).await;

        session.interrupt().await.unwrap();

        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityStart": {} } })
        );
    }

    /// One barge-in reported by several observers is still one transition.
    /// A second `activityStart` would be the duplicate-cancel failure RFC 6787
    /// §6.2.4 describes.
    #[tokio::test]
    async fn one_barge_in_produces_one_activity_start() {
        let (session, mut server) = session_on_loopback(ActivityDetection::Manual).await;
        let activity = session.activity_signaller().expect("manual mode offers a signaller");

        assert_eq!(activity.start().await.unwrap(), ActivitySignalOutcome::Sent);
        assert_eq!(activity.start().await.unwrap(), ActivitySignalOutcome::Redundant);
        // `interrupt` shares the state machine, so it does not re-signal either.
        session.interrupt().await.unwrap();

        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityStart": {} } })
        );

        // Only the sentinel should follow the single start frame.
        session.send_text("sentinel").await.unwrap();
        let frame = next_frame(&mut server).await;
        assert!(
            frame.get("clientContent").is_some(),
            "a duplicate activity frame was sent: {frame}"
        );
    }

    /// Ending the turn has to re-arm the next one, or a session signals once
    /// and then goes permanently deaf.
    #[tokio::test]
    async fn ending_activity_re_arms_the_next_start() {
        let (session, mut server) = session_on_loopback(ActivityDetection::Manual).await;
        let activity = session.activity_signaller().unwrap();

        assert_eq!(activity.start().await.unwrap(), ActivitySignalOutcome::Sent);
        assert_eq!(activity.end().await.unwrap(), ActivitySignalOutcome::Sent);
        assert_eq!(activity.end().await.unwrap(), ActivitySignalOutcome::Redundant);
        assert_eq!(activity.start().await.unwrap(), ActivitySignalOutcome::Sent);

        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityStart": {} } })
        );
        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityEnd": {} } })
        );
        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityStart": {} } })
        );
    }

    /// `ResponseCancel` is actionable once the client owns detection, so it
    /// should stop being dropped with a warning.
    #[tokio::test]
    async fn response_cancel_becomes_a_real_cancel_in_manual_mode() {
        let (session, mut server) = session_on_loopback(ActivityDetection::Manual).await;

        session.send_event(ClientEvent::ResponseCancel).await.unwrap();

        assert_eq!(
            next_frame(&mut server).await,
            json!({ "realtimeInput": { "activityStart": {} } })
        );
    }

    fn tool_with_constrained_schema() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "submit".to_string(),
            description: Some("a tool".to_string()),
            parameters: Some(json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {"n": {"type": "string", "minLength": 7}}
            })),
        }]
    }

    /// The default stays on the legacy field, so nothing changes for callers
    /// who do not opt in.
    #[test]
    fn tools_default_to_the_legacy_parameters_field() {
        let tools =
            convert_tools(Some(tool_with_constrained_schema()), Default::default()).unwrap();
        let decl = &tools[0]["functionDeclarations"][0];

        assert!(decl.get("parameters").is_some(), "{decl}");
        assert!(decl.get("parametersJsonSchema").is_none(), "{decl}");
    }

    /// The bug this exists to fix: a schema that kept `additionalProperties`
    /// must not be posted under `parameters`. Gemini answers that frame by
    /// closing the socket with WS 1007, so the call dies before any audio
    /// flows — the two fields are mutually exclusive, never interchangeable.
    #[test]
    fn json_schema_dialect_posts_under_parameters_json_schema() {
        let tools = convert_tools(
            Some(tool_with_constrained_schema()),
            adk_gemini::GeminiSchemaDialect::JsonSchema,
        )
        .unwrap();
        let decl = &tools[0]["functionDeclarations"][0];

        assert!(decl.get("parameters").is_none(), "both fields sent: {decl}");
        assert_eq!(decl["parametersJsonSchema"]["additionalProperties"], json!(false));
        assert_eq!(decl["parametersJsonSchema"]["properties"]["n"]["minLength"], 7);
    }

    /// The dialect held on the session — not a default — is what the setup
    /// frame carries. `build_setup_message` takes the dialect as an argument,
    /// so nothing but this test observes whether `send_setup` passes its own.
    /// Getting it wrong sends constraints under `parameters` and Gemini Live
    /// closes the socket with WS 1007.
    #[tokio::test]
    async fn send_setup_posts_tools_under_the_session_dialect() {
        let (session, mut server) = session_on_loopback_with_dialect(
            ActivityDetection::Automatic,
            adk_gemini::GeminiSchemaDialect::JsonSchema,
        )
        .await;

        let config =
            RealtimeConfig { tools: Some(tool_with_constrained_schema()), ..Default::default() };
        session.send_setup("models/test", config).await.unwrap();

        let frame = next_frame(&mut server).await;
        let decl = &frame["setup"]["tools"][0]["functionDeclarations"][0];

        assert!(decl.get("parameters").is_none(), "legacy field used: {frame}");
        assert_eq!(decl["parametersJsonSchema"]["additionalProperties"], json!(false));
    }
}
