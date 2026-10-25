//! What the voice view and the server say to each other (spec 3.3 §5.6, §6).
//!
//! These types are plain serde and JSON Schema types with no realtime dependency, so they
//! compile under `web` with the rest of the OpenAPI document, whether or not `voice` is on:
//! the document is the same in every build. The routes that use them are served only with
//! `voice`.
//!
//! The socket carries the person's microphone, camera and screen up and the model's audio down
//! as binary frames, each with a one-byte tag ([`FRAME_AUDIO`], [`FRAME_CAMERA`],
//! [`FRAME_SCREEN`]), and transient voice state as camelCase JSON text frames:
//! [`VoiceClientMessage`] up, [`VoiceUpdate`] down. Decision cards never travel on it: the view
//! reads them from `GET /surface/decisions?surface=voice` and the session feed (§6).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Tag of a binary frame of PCM16 mono audio: up at the model's input rate, down at its
/// output rate.
pub const FRAME_AUDIO: u8 = 0x01;
/// Tag of a binary frame holding one camera JPEG.
pub const FRAME_CAMERA: u8 = 0x02;
/// Tag of a binary frame holding one screen JPEG.
pub const FRAME_SCREEN: u8 = 0x03;

/// A realtime model the person can talk on (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceModel {
    /// What a session asks for: `POST /surface/voice/sessions {model}`.
    pub id: String,
    /// The name the select shows, such as "Gemini Live".
    pub name: String,
    /// The sample rate of the PCM16 mono audio the model takes.
    pub input_rate: u32,
    /// The sample rate of the PCM16 mono audio the model speaks.
    pub output_rate: u32,
}

/// `GET /surface/voice` answer: the models the person can talk on, first the default
/// (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceModels {
    /// The agent the person talks with, by its display name: "Talking with Chief of staff".
    pub agent: String,
    /// The models, the default first.
    pub models: Vec<VoiceModel>,
}

/// `POST /surface/voice/sessions` body: a ticket for the voice socket (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTicketRequest {
    /// The model's `id`.
    pub model: String,
    /// Continue this voice conversation instead of starting one: only one of the caller's own
    /// voice sessions (§11 S-I8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// `POST /surface/voice/sessions` answer (spec 3.3 §5.6).
///
/// The ticket works once, within 30 seconds, for the caller it was issued to; the view sends
/// it in its first message on the socket, never in a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTicket {
    /// The single-use ticket.
    pub ticket: String,
    /// The socket's path: `/voice/socket`.
    pub socket: String,
    /// The voice conversation's session, listed in Threads under the agent.
    pub session_id: String,
    /// The sample rate of the audio the view sends up.
    pub input_rate: u32,
    /// The sample rate of the audio the view plays.
    pub output_rate: u32,
}

/// The camera or the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VisualSource {
    /// The camera.
    Camera,
    /// A shared screen or window.
    Screen,
}

/// One of the three switches of "What it can see" (spec 3.3 §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InputSource {
    /// The microphone.
    Mic,
    /// The camera.
    Camera,
    /// A shared screen or window.
    Screen,
}

/// A text frame the view sends up the voice socket (spec 3.3 §5.6).
///
/// # Example
///
/// ```rust
/// use adk_surface::voice::{InputSource, VoiceClientMessage};
///
/// let message: VoiceClientMessage =
///     serde_json::from_str(r#"{"type":"switch","source":"screen","on":false}"#).unwrap();
/// assert_eq!(message, VoiceClientMessage::Switch { source: InputSource::Screen, on: false });
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum VoiceClientMessage {
    /// The first message: the ticket `POST /surface/voice/sessions` issued.
    #[serde(rename_all = "camelCase")]
    Hello {
        /// The ticket.
        ticket: String,
    },
    /// A switch of "What it can see" changed; sent before any later frame.
    #[serde(rename_all = "camelCase")]
    Switch {
        /// Which.
        source: InputSource,
        /// On or off.
        on: bool,
    },
    /// The model's audio finished playing, or was flushed.
    PlaybackEnded,
    /// End the conversation: a spoken answer in its cancel window is cancelled.
    End,
}

/// A text frame the server sends down the voice socket: transient voice state only
/// (spec 3.3 §5.6).
///
/// # Example
///
/// ```rust
/// use adk_surface::voice::VoiceUpdate;
///
/// let update = VoiceUpdate::Heard { text: "allow once".into() };
/// assert_eq!(serde_json::to_value(&update).unwrap(), serde_json::json!({ "type": "heard", "text": "allow once" }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum VoiceUpdate {
    /// The provider session is open.
    #[serde(rename_all = "camelCase")]
    Ready {
        /// The voice conversation's session.
        session_id: String,
        /// The sample rate of the audio the view sends up.
        input_rate: u32,
        /// The sample rate of the audio the view plays.
        output_rate: u32,
    },
    /// The model waits for the person.
    Listening,
    /// The model speaks.
    Speaking,
    /// What the person said, as transcribed.
    #[serde(rename_all = "camelCase")]
    Heard {
        /// The transcript.
        text: String,
    },
    /// The decision a spoken answer would apply to now, or none.
    #[serde(rename_all = "camelCase")]
    Prompted {
        /// The card's `id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_id: Option<String>,
        /// The card's `version`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
    },
    /// A spoken "allow once" takes effect at `cancelUntil` unless cancelled.
    #[serde(rename_all = "camelCase")]
    Pending {
        /// The decision.
        decision_id: String,
        /// When it takes effect.
        cancel_until: DateTime<Utc>,
        /// "Allowing once in 5 seconds".
        text: String,
    },
    /// A spoken answer ended: read the card again for how.
    #[serde(rename_all = "camelCase")]
    Outcome {
        /// The decision.
        decision_id: String,
        /// The answer was recorded.
        decided: bool,
        /// What happened, in plain words, when it was not recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// The open decisions voice cannot approve: "Decide on screen".
    #[serde(rename_all = "camelCase")]
    NeedsScreen {
        /// The cards' `id`s, oldest first.
        decision_ids: Vec<String>,
    },
    /// The person spoke over the model: flush the audio not yet played.
    BargeIn,
    /// The conversation ended.
    Ended,
    /// Something failed or was refused.
    #[serde(rename_all = "camelCase")]
    Error {
        /// The engine's `surface.*` code, or the source's.
        code: String,
        /// What happened, in plain words.
        message: String,
    },
}
