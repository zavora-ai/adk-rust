use crate::error::Result;
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

#[cfg(feature = "livekit")]
pub mod livekit;
#[cfg(feature = "livekit")]
pub use livekit::LiveKitCallControlProvider;

#[cfg(feature = "twilio")]
pub mod twilio;
#[cfg(feature = "twilio")]
pub use twilio::TwilioCallControlProvider;

/// Trait representing a generic call control provider.
#[async_trait]
pub trait CallControlProvider: Send + Sync {
    /// Originate an outbound call. Returns a handle; does not block for
    /// pickup/acceptance — that's observed via `events()`.
    async fn originate(&self, phone_number: &str, context: OriginateContext) -> Result<CallHandle>;

    /// Generic call-control events for a given handle — not business
    /// outcomes. The caller interprets these into whatever
    /// "accepted"/"no response"/"failed" means for its own domain.
    fn events(
        &self,
        handle: &CallHandle,
    ) -> Pin<Box<dyn Stream<Item = Result<CallControlEvent>> + Send>>;

    /// Redirect an active call to a new destination.
    async fn redirect(&self, handle: &CallHandle, destination: RedirectTarget) -> Result<()>;

    /// Hang up an active call.
    async fn hangup(&self, handle: &CallHandle) -> Result<()>;
}

/// A handle representing an active or originated call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CallHandle {
    /// Provider-specific identifier for the call.
    pub provider_call_id: String,
    /// Optional room or group identifier (e.g., LiveKit room name).
    pub room_name: Option<String>,
}

/// Context for originating a call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OriginateContext {
    /// Optional metadata to associate with the call.
    pub metadata: Option<String>,
    /// Provider-specific extra configuration.
    pub extra: Option<serde_json::Value>,
}

/// Target for redirecting a call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RedirectTarget {
    /// Redirect to another phone number.
    PhoneNumber(String),
    /// Redirect to a URL (e.g., Twilio TwiML).
    Url(String),
}

/// Generic events for call control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallControlEvent {
    /// The call is ringing.
    Ringing,
    /// The call has been answered.
    Answered,
    /// A DTMF digit was received.
    Dtmf {
        /// The digit received.
        digit: String,
    },
    /// A participant joined a conference (if applicable).
    ConferenceParticipantJoined {
        /// The identity of the participant.
        identity: String,
    },
    /// The call has ended.
    Ended {
        /// Reason the call ended.
        reason: String,
    },
}
