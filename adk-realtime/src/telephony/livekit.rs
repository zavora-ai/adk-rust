//! LiveKit SIP (feature `livekit-sip`): inbound trunks and dispatch rules, outbound calls,
//! transfers and hang-up over LiveKit's server API.
//!
//! A caller is a SIP participant in a LiveKit room. The LiveKit bridge (feature `livekit`)
//! joins the room with any realtime model — OpenAI Realtime, Gemini Live, Vertex Live — so
//! that model takes the call, and the call controls here act on the caller's participant.
//!
//! | Step | API |
//! |------|-----|
//! | Accept calls to a number | [`LiveKitSip::create_inbound_trunk`], then [`LiveKitSip::create_dispatch_rule`] (a room per caller) |
//! | Learn a caller joined | [`LiveKitWebhookVerifier::verify`] reads a signed `participant_joined` webhook for an inbound SIP participant |
//! | Place a call | [`LiveKitSipDialer`], an [`OutboundDialer`]: `CreateSIPParticipant` on an outbound trunk, waiting until answered |
//! | Transfer, hang up | [`LiveKitSipCall`], a [`CallControl`]: `TransferSIPParticipant` (SIP REFER) and `RemoveParticipant` |
//!
//! The full SIP admin API (list, update and delete trunks and rules) is on
//! [`LiveKitSip::sip`], `livekit-api`'s `SIPClient`.
//!
//! In-call DTMF needs a participant connected to the room (`publishDtmf`), which the server
//! API does not offer, so [`LiveKitSipCall`] reports DTMF unsupported; tones sent while
//! dialling (an extension) go in the dial request's [`DTMF_ATTRIBUTE`].

use std::sync::Arc;
use std::time::Duration;

use adk_core::egress::ToolEgress;
use async_trait::async_trait;
use livekit_api::access_token::TokenVerifier;
use livekit_api::services::ServiceError;
use livekit_api::services::room::RoomClient;
use livekit_api::services::sip::{
    CreateSIPDispatchRuleOptions, CreateSIPInboundTrunkOptions, CreateSIPOutboundTrunkOptions,
    CreateSIPParticipantOptions, SIPClient, TransferSIPParticipantOptions,
};
use livekit_api::webhooks::WebhookReceiver;
use livekit_protocol as proto;

use super::{
    CallAnswerer, CallCapabilities, CallControl, CallTarget, DialRequest, IncomingCall,
    OutboundDialer,
};
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};

/// The provider name a LiveKit call reports.
const PROVIDER: &str = "livekit";

/// The [`IncomingCall`] / [`DialRequest`] attribute naming the call's LiveKit room.
pub const ROOM_ATTRIBUTE: &str = "room";
/// The [`IncomingCall`] / [`DialRequest`] attribute naming the SIP participant's identity.
pub const PARTICIPANT_IDENTITY_ATTRIBUTE: &str = "participant_identity";
/// The [`DialRequest`] attribute holding tones to send once the callee answers, such as an
/// extension (`0`–`9`, `*`, `#`, and `w` for a half-second pause).
pub const DTMF_ATTRIBUTE: &str = "dtmf";

/// LiveKit's SIP participant attributes an [`IncomingCall`] carries over.
const SIP_ATTRIBUTES: [&str; 5] =
    ["sip.callID", "sip.phoneNumber", "sip.trunkPhoneNumber", "sip.trunkID", "sip.ruleID"];

/// A LiveKit server-API failure as a realtime error.
fn service_error(action: &str, error: ServiceError) -> RealtimeError {
    match error {
        ServiceError::Twirp(livekit_api::services::ServerError::Twirp(server)) => {
            let sip_status = server.meta.get("sip_status_code").map(String::as_str);
            let message = match sip_status {
                Some(status) => format!("LiveKit {action}: {} (SIP {status})", server.msg),
                None => format!("LiveKit {action}: {}", server.msg),
            };
            RealtimeError::ServerError { code: server.code, message }
        }
        ServiceError::Twirp(other) => {
            RealtimeError::connection(format!("LiveKit {action} request failed: {other}"))
        }
        other => RealtimeError::config(format!("LiveKit {action}: {other}")),
    }
}

struct Inner {
    host: String,
    sip: SIPClient,
    rooms: RoomClient,
}

/// LiveKit SIP over the server API, authenticated with an API key and secret.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::livekit::LiveKitSip;
///
/// let livekit = LiveKitSip::new("wss://example.livekit.cloud", "APIkey", "secret");
/// let call = livekit.call("call-_+14155550123_abc", "sip_+14155550123");
/// # let _ = call;
/// ```
#[derive(Clone)]
pub struct LiveKitSip {
    inner: Arc<Inner>,
    ringing_timeout: Option<Duration>,
}

impl std::fmt::Debug for LiveKitSip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitSip").field("host", &self.inner.host).finish_non_exhaustive()
    }
}

impl LiveKitSip {
    /// A client for the LiveKit server at `host` (`wss://`, `https://` or `http://`).
    pub fn new(host: &str, api_key: &str, api_secret: &str) -> Self {
        Self {
            inner: Arc::new(Inner {
                host: host.to_string(),
                sip: SIPClient::with_api_key(host, api_key, api_secret),
                rooms: RoomClient::with_api_key(host, api_key, api_secret),
            }),
            ringing_timeout: None,
        }
    }

    /// Sets how long a transfer or outbound call rings before it fails (LiveKit's default
    /// otherwise).
    pub fn with_ringing_timeout(mut self, timeout: Duration) -> Self {
        self.ringing_timeout = Some(timeout);
        self
    }

    /// `livekit-api`'s SIP client, for the rest of the SIP admin API.
    pub fn sip(&self) -> &SIPClient {
        &self.inner.sip
    }

    /// Creates an inbound trunk that accepts calls to `numbers` (E.164, or SIP usernames).
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ServerError`] with LiveKit's Twirp code when LiveKit refuses.
    pub async fn create_inbound_trunk(
        &self,
        name: &str,
        numbers: Vec<String>,
        options: CreateSIPInboundTrunkOptions,
    ) -> Result<proto::SipInboundTrunkInfo> {
        self.inner
            .sip
            .create_sip_inbound_trunk(name.to_string(), numbers, options)
            .await
            .map_err(|error| service_error("CreateSIPInboundTrunk", error))
    }

    /// Creates an outbound trunk through the SIP provider at `address` (the trunk's domain),
    /// calling from `numbers`.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ServerError`] with LiveKit's Twirp code when LiveKit refuses.
    pub async fn create_outbound_trunk(
        &self,
        name: &str,
        address: &str,
        numbers: Vec<String>,
        options: CreateSIPOutboundTrunkOptions,
    ) -> Result<proto::SipOutboundTrunkInfo> {
        self.inner
            .sip
            .create_sip_outbound_trunk(name.to_string(), address.to_string(), numbers, options)
            .await
            .map_err(|error| service_error("CreateSIPOutboundTrunk", error))
    }

    /// Creates a dispatch rule that places each caller in a new room named
    /// `{room_prefix}{caller}_{random}`, for the trunks in `options.trunk_ids` (all inbound
    /// trunks when empty).
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ServerError`] with LiveKit's Twirp code when LiveKit refuses.
    pub async fn create_dispatch_rule(
        &self,
        room_prefix: &str,
        options: CreateSIPDispatchRuleOptions,
    ) -> Result<proto::SipDispatchRuleInfo> {
        let rule = proto::sip_dispatch_rule::Rule::DispatchRuleIndividual(
            proto::SipDispatchRuleIndividual {
                room_prefix: room_prefix.to_string(),
                ..Default::default()
            },
        );
        self.inner
            .sip
            .create_sip_dispatch_rule(rule, options)
            .await
            .map_err(|error| service_error("CreateSIPDispatchRule", error))
    }

    /// The control handle of the SIP participant `participant_identity` in `room`.
    pub fn call(&self, room: &str, participant_identity: &str) -> LiveKitSipCall {
        LiveKitSipCall {
            livekit: self.clone(),
            room: room.to_string(),
            identity: participant_identity.to_string(),
            call_id: participant_identity.to_string(),
        }
    }

    /// A dialler placing calls through the outbound trunk `trunk_id`.
    pub fn dialer(&self, trunk_id: &str) -> LiveKitSipDialer {
        LiveKitSipDialer { livekit: self.clone(), trunk_id: trunk_id.to_string(), room: None }
    }

    fn egress(&self) -> ToolEgress {
        let host = self.inner.host.as_str();
        let host = host
            .strip_prefix("wss://")
            .map(|rest| format!("https://{rest}"))
            .or_else(|| host.strip_prefix("ws://").map(|rest| format!("http://{rest}")))
            .unwrap_or_else(|| host.to_string());
        ToolEgress::for_urls([host])
    }
}

#[async_trait]
impl CallAnswerer for LiveKitSip {
    /// LiveKit connected the caller to a room when the dispatch rule matched, so answering
    /// returns the caller's handle; the application joins the room with the LiveKit bridge
    /// and the model of its choice. `config` configures that model, not LiveKit.
    async fn answer(
        &self,
        call: &IncomingCall,
        _config: &RealtimeConfig,
    ) -> Result<Arc<dyn CallControl>> {
        Ok(Arc::new(LiveKitSipCall::for_incoming(self, call)?))
    }

    /// Removes the caller from the room, which hangs up; LiveKit chooses the SIP status.
    async fn decline(&self, call: &IncomingCall, _status: u16) -> Result<()> {
        LiveKitSipCall::for_incoming(self, call)?.hang_up().await
    }
}

/// One LiveKit SIP call: the SIP participant in its room.
#[derive(Clone)]
pub struct LiveKitSipCall {
    livekit: LiveKitSip,
    room: String,
    identity: String,
    call_id: String,
}

impl std::fmt::Debug for LiveKitSipCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitSipCall")
            .field("room", &self.room)
            .field("participant_identity", &self.identity)
            .field("call_id", &self.call_id)
            .finish()
    }
}

impl LiveKitSipCall {
    fn for_incoming(livekit: &LiveKitSip, call: &IncomingCall) -> Result<Self> {
        let attribute = |key: &str| {
            call.attributes.get(key).cloned().ok_or_else(|| {
                RealtimeError::config(format!(
                    "a LiveKit call needs the `{key}` attribute; build it from a verified webhook"
                ))
            })
        };
        Ok(Self {
            livekit: livekit.clone(),
            room: attribute(ROOM_ATTRIBUTE)?,
            identity: attribute(PARTICIPANT_IDENTITY_ATTRIBUTE)?,
            call_id: call.call_id.clone(),
        })
    }

    /// The room the caller is in, which the LiveKit bridge joins.
    pub fn room(&self) -> &str {
        &self.room
    }

    /// The caller's participant identity.
    pub fn participant_identity(&self) -> &str {
        &self.identity
    }
}

#[async_trait]
impl CallControl for LiveKitSipCall {
    fn call_id(&self) -> &str {
        &self.call_id
    }

    fn provider(&self) -> &str {
        PROVIDER
    }

    fn capabilities(&self) -> CallCapabilities {
        CallCapabilities::new(true, false)
    }

    fn egress(&self) -> ToolEgress {
        self.livekit.egress()
    }

    async fn transfer(&self, target: &CallTarget) -> Result<()> {
        let options = TransferSIPParticipantOptions {
            play_dialtone: Some(false),
            ringing_timeout: self.livekit.ringing_timeout,
            ..Default::default()
        };
        self.livekit
            .inner
            .sip
            .transfer_sip_participant(
                self.room.clone(),
                self.identity.clone(),
                target.as_uri().to_string(),
                options,
            )
            .await
            .map_err(|error| service_error("TransferSIPParticipant", error))
    }

    async fn hang_up(&self) -> Result<()> {
        self.livekit
            .inner
            .rooms
            .remove_participant(&self.room, &self.identity)
            .await
            .map_err(|error| service_error("RemoveParticipant", error))
    }
}

/// Places outbound calls through one LiveKit outbound trunk (`CreateSIPParticipant`).
///
/// The callee joins the room in the request's [`ROOM_ATTRIBUTE`] (or the dialler's default
/// room) as participant [`PARTICIPANT_IDENTITY_ATTRIBUTE`] (default `sip_<number>`). `dial`
/// waits until the callee answers, so a busy or unanswered call is an error carrying the SIP
/// status. Only `tel:` targets are dialled.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::livekit::LiveKitSip;
///
/// let dialer = LiveKitSip::new("wss://example.livekit.cloud", "APIkey", "secret")
///     .dialer("ST_outbound")
///     .with_room("outbound-calls");
/// # let _ = dialer;
/// ```
#[derive(Debug, Clone)]
pub struct LiveKitSipDialer {
    livekit: LiveKitSip,
    trunk_id: String,
    room: Option<String>,
}

impl LiveKitSipDialer {
    /// Places callees in `room` when a request names none.
    pub fn with_room(mut self, room: impl Into<String>) -> Self {
        self.room = Some(room.into());
        self
    }
}

#[async_trait]
impl OutboundDialer for LiveKitSipDialer {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn egress(&self) -> ToolEgress {
        self.livekit.egress()
    }

    async fn dial(&self, request: &DialRequest) -> Result<Arc<dyn CallControl>> {
        let number = request.to.phone_number().ok_or_else(|| {
            RealtimeError::config(format!(
                "LiveKit dials phone numbers; '{}' is not a tel: target",
                request.to
            ))
        })?;
        let room =
            request.attributes.get(ROOM_ATTRIBUTE).or(self.room.as_ref()).cloned().ok_or_else(
                || {
                    RealtimeError::config(
                        "a LiveKit call needs a room: set the dialler's or the request's `room`",
                    )
                },
            )?;
        let identity = request
            .attributes
            .get(PARTICIPANT_IDENTITY_ATTRIBUTE)
            .cloned()
            .unwrap_or_else(|| format!("sip_{number}"));
        let dtmf = request.attributes.get(DTMF_ATTRIBUTE).cloned();
        if let Some(tones) = &dtmf
            && !tones.chars().all(|c| c.is_ascii_digit() || matches!(c, '*' | '#' | 'w'))
        {
            return Err(RealtimeError::config("dial DTMF may hold only 0-9, *, # and w"));
        }
        let options = CreateSIPParticipantOptions {
            participant_identity: identity.clone(),
            dtmf,
            wait_until_answered: Some(true),
            ringing_timeout: self.livekit.ringing_timeout,
            ..Default::default()
        };
        let info = self
            .livekit
            .inner
            .sip
            .create_sip_participant(
                self.trunk_id.clone(),
                number.to_string(),
                room.clone(),
                options,
                None,
            )
            .await
            .map_err(|error| service_error("CreateSIPParticipant", error))?;
        tracing::info!(
            call.provider = PROVIDER,
            call.id = %info.sip_call_id,
            call.room = %info.room_name,
            "outbound SIP call answered"
        );
        Ok(Arc::new(LiveKitSipCall {
            livekit: self.livekit.clone(),
            room: if info.room_name.is_empty() { room } else { info.room_name },
            identity: if info.participant_identity.is_empty() {
                identity
            } else {
                info.participant_identity
            },
            call_id: info.sip_call_id,
        }))
    }
}

/// A verified LiveKit webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveKitWebhookEvent {
    /// `participant_joined` for an inbound SIP participant: a caller reached a room through
    /// a dispatch rule. The call carries [`ROOM_ATTRIBUTE`], [`PARTICIPANT_IDENTITY_ATTRIBUTE`]
    /// and LiveKit's `sip.*` participant attributes.
    IncomingCall(IncomingCall),
    /// Any other event.
    Other {
        /// The event name, such as `room_finished`.
        event: String,
    },
}

/// Verifies LiveKit webhooks: the `Authorization` header is a JWT signed with the API secret
/// whose `sha256` claim is the body's SHA-256.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::livekit::{LiveKitWebhookEvent, LiveKitWebhookVerifier};
///
/// let verifier = LiveKitWebhookVerifier::new("APIkey", "secret");
/// assert!(verifier.verify(r#"{"event":"room_started"}"#, "not-a-jwt").is_err());
/// ```
#[derive(Debug, Clone)]
pub struct LiveKitWebhookVerifier {
    receiver: WebhookReceiver,
}

impl LiveKitWebhookVerifier {
    /// A verifier for webhooks signed with `api_key`'s secret.
    pub fn new(api_key: &str, api_secret: &str) -> Self {
        Self { receiver: WebhookReceiver::new(TokenVerifier::with_api_key(api_key, api_secret)) }
    }

    /// Verifies one delivery: `body` is the raw request body and `authorization` the
    /// `Authorization` header, with or without a `Bearer ` prefix.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::AuthError`] when the token is invalid or expired or the body
    /// does not match its hash; answer it with `401` and do not act on the body.
    pub fn verify(&self, body: &str, authorization: &str) -> Result<LiveKitWebhookEvent> {
        let token = authorization.strip_prefix("Bearer ").unwrap_or(authorization);
        let event = self.receiver.receive(body, token).map_err(|error| {
            RealtimeError::AuthError(format!("LiveKit webhook rejected: {error}"))
        })?;
        let inbound_sip = event.event == "participant_joined"
            && event.participant.as_ref().is_some_and(|participant| {
                participant.kind == proto::participant_info::Kind::Sip as i32
                    && participant.attributes.get("sip.ruleID").is_some_and(|rule| !rule.is_empty())
            });
        let (true, Some(room), Some(participant)) = (inbound_sip, event.room, event.participant)
        else {
            return Ok(LiveKitWebhookEvent::Other { event: event.event });
        };
        let call_id = participant
            .attributes
            .get("sip.callID")
            .cloned()
            .unwrap_or_else(|| participant.identity.clone());
        let mut call = IncomingCall::new(PROVIDER, call_id)
            .with_attribute(ROOM_ATTRIBUTE, room.name)
            .with_attribute(PARTICIPANT_IDENTITY_ATTRIBUTE, participant.identity.clone());
        for key in SIP_ATTRIBUTES {
            if let Some(value) = participant.attributes.get(key) {
                call = call.with_attribute(key, value.clone());
            }
        }
        tracing::info!(call.provider = PROVIDER, call.id = %call.call_id, "incoming SIP call");
        Ok(LiveKitWebhookEvent::IncomingCall(call))
    }
}
