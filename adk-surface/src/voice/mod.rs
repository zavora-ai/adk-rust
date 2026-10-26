//! Voice and video (spec 3.3 §5.6; feature `voice`).
//!
//! A spoken answer is one more way to give the answer a click or a key gives, under the same
//! rules: the person says the label of the engine's action — "allow once", "deny", "cancel",
//! "stop all" (`spoken`) — and the engine decides it, never the model and never the realtime
//! session. What voice may decide is the voice row of §7.2: **Allow once** for everyday calls,
//! after a cancel window, and **Deny** for any call. A high-risk or money call needs the
//! person to confirm it is them, which voice cannot do, so the view says "Decide on screen".
//!
//! | Part | What | Feature |
//! |---|---|---|
//! | `spoken` | The words that answer, and nothing else | `voice` |
//! | `VisualInputs` | The camera and the screen reach the model only while their switches are on, no faster than it takes them | `voice` |
//! | `VoiceSource` | Sends a decision on a call a live voice conversation holds back to that conversation, never to the text runner | `voice` |
//! | `VoiceHub` | Opens voice conversations (`VoiceLink`): one realtime session of the harness each, in its own session | `voice` |
//! | `router`, `socket_routes` | `GET /surface/voice`, `POST /surface/voice/sessions` and the voice socket | `voice` and `web` |
//! | The wire types ([`VoiceModels`], [`VoiceTicket`], [`VoiceUpdate`], ...) | What the view and the server say, in the OpenAPI document of every `web` build | `web` or `voice` |
//!
//! # How a spoken answer binds to a decision
//!
//! One card is prompted at a time: the conversation's own call just held, else the oldest
//! open decision voice may answer. A spoken answer applies to the card that was prompted when
//! the person started speaking, and only when it had been on screen for a second by then; if
//! the card or its version changed before the transcript arrived, nothing is decided. Only the
//! person's own completed transcript counts, never the model's words, and speech that started
//! while the model's audio was playing is ignored. Another agent's decision is shown on
//! screen and never sent to the model.
//!
//! # Records
//!
//! Every `surface.decide` record of a spoken answer carries `transcriptDigest`, the SHA-256 of
//! the transcript, and none holds audio, frames or the transcript's text (§5.6: "a note of the
//! answer, never the recording"). The conversation itself is the session, as typed text is:
//! the person's utterances stay in it as their messages.

#[cfg(feature = "voice")]
mod hub;
#[cfg(feature = "voice")]
mod session;
#[cfg(feature = "voice")]
mod source;
#[cfg(feature = "voice")]
pub mod spoken;
#[cfg(feature = "voice")]
mod visual;
#[cfg(all(feature = "voice", feature = "web"))]
mod web;
mod wire;

#[cfg(feature = "voice")]
pub use hub::{VoiceHub, VoiceHubBuilder};
#[cfg(feature = "voice")]
pub use session::{VoiceInput, VoiceLink, VoiceOutput};
#[cfg(feature = "voice")]
pub use source::{LiveVoice, VoiceSource};
#[cfg(feature = "voice")]
pub use visual::{FramePolicy, VisualInputs};
#[cfg(all(feature = "voice", feature = "web"))]
pub use web::{SOCKET_PATH, TICKET_TTL, router, socket_routes};
pub use wire::{
    FRAME_AUDIO, FRAME_CAMERA, FRAME_SCREEN, InputSource, VisualSource, VoiceClientMessage,
    VoiceModel, VoiceModels, VoiceTicket, VoiceTicketRequest, VoiceUpdate,
};

/// The route paths of voice, relative to the `/api` prefix, and the socket's root path
/// (spec 3.3 §5.6).
pub mod paths {
    /// `GET`: the models the person can talk on.
    pub const VOICE: &str = "/surface/voice";
    /// `POST`: a ticket for the voice socket.
    pub const SESSIONS: &str = "/surface/voice/sessions";
    /// The voice socket, a root route.
    pub const SOCKET: &str = "/voice/socket";
}

#[cfg(feature = "voice")]
pub use error::VoiceError;

/// What [`SurfaceEngine::decide_spoken`](crate::SurfaceEngine::decide_spoken) did with a
/// spoken answer (spec 3.3 §5.6).
#[cfg(feature = "voice")]
#[derive(Debug)]
pub struct SpokenAnswer {
    /// What the answer did now: [`Pending`](crate::CommandOutcome::Pending) for "allow once"
    /// inside its cancel window, otherwise the answer's outcome.
    pub outcome: crate::CommandOutcome,
    /// For a pending answer, how its cancel window ended: the outcome of recording it, or its
    /// error. The sender is dropped, without a value, when the answer was cancelled — by
    /// **Cancel**, by ending the conversation or by **Stop all**.
    pub settled: Option<tokio::sync::oneshot::Receiver<crate::Result<crate::CommandOutcome>>>,
}

#[cfg(feature = "voice")]
mod error {
    use adk_core::{AdkError, ErrorCategory, ErrorComponent};

    use crate::error::SurfaceError;

    /// Why voice refused or failed (spec 3.3 §5.6). Each converts into a [`SurfaceError`] with
    /// its `surface.voice.*` code.
    #[derive(Debug, thiserror::Error)]
    pub enum VoiceError {
        /// No model of that id is offered.
        #[error("no voice model '{0}' is offered here; pick one GET /api/surface/voice lists")]
        UnknownModel(String),
        /// The session named is not one of the caller's voice conversations (S-I8).
        #[error(
            "session '{0}' is not one of your voice conversations; start a new one without a sessionId"
        )]
        NotYourSession(String),
        /// The ticket is unknown, used, expired or another caller's.
        #[error(
            "the voice ticket is not valid: ask POST /api/surface/voice/sessions for a new one"
        )]
        Ticket,
        /// The realtime provider could not be reached or failed.
        #[error("the voice model failed: {0}")]
        Provider(String),
        /// The harness could not open its voice session.
        #[error("the voice conversation could not start: {0}")]
        Harness(String),
        /// The conversation already ended.
        #[error("the voice conversation has ended; start a new one")]
        Ended,
    }

    impl VoiceError {
        /// The error code: `surface.voice.unknown_model`, `surface.voice.not_your_session`,
        /// `surface.voice.ticket`, `surface.voice.provider`, `surface.voice.harness` or
        /// `surface.voice.ended`.
        pub fn code(&self) -> &'static str {
            match self {
                Self::UnknownModel(_) => "surface.voice.unknown_model",
                Self::NotYourSession(_) => "surface.voice.not_your_session",
                Self::Ticket => "surface.voice.ticket",
                Self::Provider(_) => "surface.voice.provider",
                Self::Harness(_) => "surface.voice.harness",
                Self::Ended => "surface.voice.ended",
            }
        }

        fn category(&self) -> ErrorCategory {
            match self {
                Self::UnknownModel(_) => ErrorCategory::InvalidInput,
                Self::NotYourSession(_) => ErrorCategory::Forbidden,
                Self::Ticket => ErrorCategory::Unauthorized,
                Self::Provider(_) => ErrorCategory::Unavailable,
                Self::Harness(_) => ErrorCategory::Internal,
                Self::Ended => ErrorCategory::NotFound,
            }
        }
    }

    impl From<VoiceError> for SurfaceError {
        fn from(error: VoiceError) -> Self {
            SurfaceError::Source(AdkError::new(
                ErrorComponent::Server,
                error.category(),
                error.code(),
                error.to_string(),
            ))
        }
    }
}
