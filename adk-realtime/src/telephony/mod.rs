//! Phone calls for realtime voice agents (feature `telephony`).
//!
//! A provider connects a phone call to a realtime session; this module controls the call
//! itself through one provider-neutral surface:
//!
//! | Item | What it does |
//! |------|--------------|
//! | [`IncomingCall`] | An inbound call a provider announced, with the caller's SIP headers |
//! | [`CallAnswerer`] | Answers or declines an [`IncomingCall`] |
//! | [`CallControl`] | Controls one live call: transfer, hang up, DTMF |
//! | [`OutboundDialer`] | Places an outbound call |
//! | [`hang_up_on_freeze`] | Hangs up a call when a governance kill switch freezes |
//! | [`TransferCallTool`], [`EndCallTool`], [`SendDtmfTool`], [`PlaceCallTool`] | The call controls as `adk_core::Tool`s, so a model's request runs through the governed path |
//!
//! The tools are ordinary [`adk_core::Tool`]s. Under a `RealtimeAgent` each call goes through
//! `adk_core::authorize_tool_call` (kill switch, `ToolPolicy`, confirmation), the run's
//! action ledger for the non-idempotent ones, and the run's tool-call budget. Transfers and
//! outbound calls reach only destinations the application configured: the model picks a
//! destination by name and never supplies a number.
//!
//! Providers: `openai-sip` (OpenAI Realtime SIP) and `livekit-sip` (LiveKit SIP).

mod tools;

pub use tools::{EndCallTool, PlaceCallTool, SendDtmfTool, TransferCallTool};

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use adk_core::GovernanceControl;
use adk_core::egress::ToolEgress;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};

/// The longest URI a [`CallTarget`] accepts.
const MAX_TARGET_LEN: usize = 256;
/// The most tones one [`DtmfTones`] sends.
const MAX_DTMF_TONES: usize = 32;

/// One SIP header from a call's INVITE.
///
/// Headers come from the caller's network, so treat them as untrusted data: never as
/// identity and never as instructions to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SipHeader {
    /// The header name, as sent.
    pub name: String,
    /// The header value, as sent.
    pub value: String,
}

/// An inbound phone call a provider announced.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::{IncomingCall, SipHeader};
///
/// let call = IncomingCall::new("openai", "rtc_123").with_sip_headers(vec![SipHeader {
///     name: "From".into(),
///     value: "sip:+14155550123@sip.example.com".into(),
/// }]);
/// assert_eq!(call.header("from"), Some("sip:+14155550123@sip.example.com"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IncomingCall {
    /// The provider that announced the call, such as `openai` or `livekit`.
    pub provider: String,
    /// The provider's identifier for the call.
    pub call_id: String,
    /// The SIP headers of the call's INVITE, as the provider reported them.
    pub sip_headers: Vec<SipHeader>,
    /// Provider fields that place the call, such as LiveKit's room and participant identity.
    pub attributes: BTreeMap<String, String>,
}

impl IncomingCall {
    /// A call `call_id` from `provider`, with no headers.
    pub fn new(provider: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            call_id: call_id.into(),
            sip_headers: Vec::new(),
            attributes: BTreeMap::new(),
        }
    }

    /// Sets the call's SIP headers.
    pub fn with_sip_headers(mut self, headers: Vec<SipHeader>) -> Self {
        self.sip_headers = headers;
        self
    }

    /// Sets one provider attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// The first SIP header named `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.sip_headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }

    /// The `From` header: who is calling, as their network claims.
    pub fn from(&self) -> Option<&str> {
        self.header("From")
    }

    /// The `To` header: the number or address that was dialled.
    pub fn to(&self) -> Option<&str> {
        self.header("To")
    }
}

/// A destination a call can be transferred to or placed to: a `tel:` URI with an E.164
/// number, or a `sip:`/`sips:` URI.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::CallTarget;
///
/// let desk = CallTarget::phone("+14155550100")?;
/// assert_eq!(desk.as_uri(), "tel:+14155550100");
/// assert_eq!(desk.phone_number(), Some("+14155550100"));
///
/// let queue: CallTarget = "sip:billing@pbx.example.com".parse()?;
/// assert_eq!(queue.phone_number(), None);
///
/// assert!(CallTarget::parse("tel:911").is_err(), "not E.164");
/// # Ok::<(), adk_realtime::RealtimeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CallTarget(String);

impl CallTarget {
    /// Parses a `tel:+<E.164>`, `sip:` or `sips:` URI.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when the URI has another scheme, a `tel:`
    /// number is not E.164, a SIP URI has no `user@host`, or the URI is longer than 256
    /// bytes or contains whitespace or control characters.
    pub fn parse(uri: &str) -> Result<Self> {
        let uri = uri.trim();
        if uri.len() > MAX_TARGET_LEN
            || uri.chars().any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
        {
            return Err(RealtimeError::config(format!(
                "call target must be at most {MAX_TARGET_LEN} bytes with no whitespace, control characters or angle brackets"
            )));
        }
        if let Some(number) = uri.strip_prefix("tel:") {
            validate_e164(number)?;
            return Ok(Self(uri.to_string()));
        }
        let address = uri.strip_prefix("sips:").or_else(|| uri.strip_prefix("sip:"));
        match address.and_then(|rest| rest.split_once('@')) {
            Some((user, host)) if !user.is_empty() && !host.is_empty() => Ok(Self(uri.to_string())),
            _ => Err(RealtimeError::config(format!(
                "call target '{uri}' must be tel:+<E.164 number>, sip:user@host or sips:user@host"
            ))),
        }
    }

    /// The `tel:` target for an E.164 number such as `+14155550100`.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when `number` is not E.164.
    pub fn phone(number: &str) -> Result<Self> {
        validate_e164(number)?;
        Ok(Self(format!("tel:{number}")))
    }

    /// The URI, as a SIP `Refer-To` or dial target carries it.
    pub fn as_uri(&self) -> &str {
        &self.0
    }

    /// The E.164 number of a `tel:` target.
    pub fn phone_number(&self) -> Option<&str> {
        self.0.strip_prefix("tel:")
    }
}

/// Checks `+` followed by 7 to 15 digits, the first not zero.
fn validate_e164(number: &str) -> Result<()> {
    let digits = number.strip_prefix('+').unwrap_or_default();
    let valid = (7..=15).contains(&digits.len())
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('0');
    if valid {
        Ok(())
    } else {
        Err(RealtimeError::config(format!(
            "'{number}' is not an E.164 number: '+' then 7 to 15 digits, the first not zero"
        )))
    }
}

impl FromStr for CallTarget {
    type Err = RealtimeError;

    fn from_str(uri: &str) -> Result<Self> {
        Self::parse(uri)
    }
}

impl TryFrom<String> for CallTarget {
    type Error = RealtimeError;

    fn try_from(uri: String) -> Result<Self> {
        Self::parse(&uri)
    }
}

impl From<CallTarget> for String {
    fn from(target: CallTarget) -> Self {
        target.0
    }
}

impl fmt::Display for CallTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// DTMF tones to send on a call: `0`–`9`, `*`, `#` and `A`–`D`, at most 32.
///
/// # Example
///
/// ```rust
/// use adk_realtime::telephony::DtmfTones;
///
/// let tones = DtmfTones::parse("1234#")?;
/// assert_eq!(tones.as_str(), "1234#");
/// assert!(DtmfTones::parse("12x").is_err());
/// # Ok::<(), adk_realtime::RealtimeError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DtmfTones(String);

impl DtmfTones {
    /// Parses `tones`.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when `tones` is empty, longer than 32, or holds
    /// any other character.
    pub fn parse(tones: &str) -> Result<Self> {
        let valid = !tones.is_empty()
            && tones.len() <= MAX_DTMF_TONES
            && tones.chars().all(|c| c.is_ascii_digit() || matches!(c, '*' | '#' | 'A'..='D'));
        if valid {
            Ok(Self(tones.to_string()))
        } else {
            Err(RealtimeError::config(format!(
                "DTMF tones must be 1 to {MAX_DTMF_TONES} of 0-9, *, # and A-D"
            )))
        }
    }

    /// The tones.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a provider can do on a live call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CallCapabilities {
    /// [`CallControl::transfer`] is supported.
    pub transfer: bool,
    /// [`CallControl::send_dtmf`] is supported.
    pub dtmf: bool,
}

impl CallCapabilities {
    /// Capabilities with transfer and DTMF set as given.
    pub fn new(transfer: bool, dtmf: bool) -> Self {
        Self { transfer, dtmf }
    }
}

/// Controls one live phone call.
///
/// A handle is bound to a single call, so a tool built on it can act only on that call.
#[async_trait]
pub trait CallControl: fmt::Debug + Send + Sync {
    /// The provider's identifier for the call.
    fn call_id(&self) -> &str;

    /// The provider, such as `openai` or `livekit`.
    fn provider(&self) -> &str;

    /// What this call supports.
    fn capabilities(&self) -> CallCapabilities;

    /// The network destinations the controls reach, declared to the run's egress policy.
    fn egress(&self) -> ToolEgress {
        ToolEgress::Configured
    }

    /// Transfers the caller to `target` (a SIP REFER); the agent leaves the call.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses or the call has ended.
    async fn transfer(&self, target: &CallTarget) -> Result<()>;

    /// Ends the call.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses.
    async fn hang_up(&self) -> Result<()>;

    /// Sends DTMF tones to the far end, for example to navigate a menu on an outbound call.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ProviderError`] when the provider does not support DTMF.
    async fn send_dtmf(&self, _tones: &DtmfTones) -> Result<()> {
        Err(RealtimeError::provider(format!(
            "{} calls do not support sending DTMF",
            self.provider()
        )))
    }
}

/// Answers or declines inbound calls.
#[async_trait]
pub trait CallAnswerer: Send + Sync {
    /// Answers `call` with a realtime session configured by `config`, and returns its
    /// control handle.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses, for example when the call was already
    /// answered or declined.
    async fn answer(
        &self,
        call: &IncomingCall,
        config: &RealtimeConfig,
    ) -> Result<Arc<dyn CallControl>>;

    /// Declines `call` with SIP status `status` (for example 486 Busy Here or 603 Decline).
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses or `status` is not 300–699.
    async fn decline(&self, call: &IncomingCall, status: u16) -> Result<()>;
}

/// An outbound call to place.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DialRequest {
    /// Who to call.
    pub to: CallTarget,
    /// Provider-specific placement, such as the LiveKit room the callee joins.
    pub attributes: BTreeMap<String, String>,
}

impl DialRequest {
    /// A call to `to`.
    pub fn new(to: CallTarget) -> Self {
        Self { to, attributes: BTreeMap::new() }
    }

    /// Sets one provider attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// Places outbound calls.
#[async_trait]
pub trait OutboundDialer: fmt::Debug + Send + Sync {
    /// The provider, such as `livekit`.
    fn provider(&self) -> &str;

    /// The network destinations dialling reaches, declared to the run's egress policy.
    fn egress(&self) -> ToolEgress {
        ToolEgress::Configured
    }

    /// Places `request` and returns the new call's control handle once the provider has
    /// accepted the request.
    ///
    /// # Errors
    ///
    /// Returns an error when the provider refuses or the callee does not answer.
    async fn dial(&self, request: &DialRequest) -> Result<Arc<dyn CallControl>>;
}

/// Hangs up a call when a kill switch freezes, until dropped.
///
/// A provider that carries the audio itself (OpenAI SIP) keeps talking to the caller after
/// the agent's session ends, so a freeze has to end the call, not only the session.
#[derive(Debug)]
#[must_use = "the call is hung up on a freeze only while the guard is held"]
pub struct CallFreezeGuard {
    task: tokio::task::JoinHandle<()>,
}

impl CallFreezeGuard {
    /// Whether the guard has already acted on a freeze.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for CallFreezeGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Hangs up `call` as soon as any of `controls` is frozen, while the returned guard is held.
///
/// Pass the same controls the agent's run checks — `Runner::governance()` and any in
/// `RunConfig::governance` — so one freeze both refuses the agent's tool calls and ends the
/// phone call. With no controls the guard never acts.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::GovernanceControl;
/// use adk_realtime::telephony::{
///     CallCapabilities, CallControl, CallTarget, hang_up_on_freeze,
/// };
/// use async_trait::async_trait;
///
/// #[derive(Debug, Default)]
/// struct Call(std::sync::atomic::AtomicBool);
///
/// #[async_trait]
/// impl CallControl for Call {
///     fn call_id(&self) -> &str { "call-1" }
///     fn provider(&self) -> &str { "example" }
///     fn capabilities(&self) -> CallCapabilities { CallCapabilities::default() }
///     async fn transfer(&self, _target: &CallTarget) -> adk_realtime::Result<()> { Ok(()) }
///     async fn hang_up(&self) -> adk_realtime::Result<()> {
///         self.0.store(true, std::sync::atomic::Ordering::SeqCst);
///         Ok(())
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let control = GovernanceControl::new();
/// let call = Arc::new(Call::default());
/// let guard = hang_up_on_freeze(call.clone(), [control.clone()]);
/// control.freeze("incident 4101");
/// while !guard.is_finished() {
///     tokio::task::yield_now().await;
/// }
/// assert!(call.0.load(std::sync::atomic::Ordering::SeqCst));
/// # }
/// ```
pub fn hang_up_on_freeze(
    call: Arc<dyn CallControl>,
    controls: impl IntoIterator<Item = GovernanceControl>,
) -> CallFreezeGuard {
    let controls: Vec<GovernanceControl> = controls.into_iter().collect();
    let task = tokio::spawn(async move {
        if controls.is_empty() {
            return std::future::pending().await;
        }
        let refusals = controls.iter().map(|control| Box::pin(control.refused()));
        let (refusal, _, _) = futures::future::select_all(refusals).await;
        tracing::warn!(
            call.provider = call.provider(),
            call.id = call.call_id(),
            error = %refusal.diagnostics(),
            "governance froze; hanging up the call"
        );
        if let Err(error) = call.hang_up().await {
            tracing::error!(
                call.provider = call.provider(),
                call.id = call.call_id(),
                error = %error,
                "the call could not be hung up after a freeze"
            );
        }
    });
    CallFreezeGuard { task }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_accept_e164_numbers_and_sip_addresses() {
        for uri in [
            "tel:+14155550100",
            "sip:billing@pbx.example.com",
            "sips:+14155550100@trunk.example.com:5061",
        ] {
            assert_eq!(CallTarget::parse(uri).unwrap().as_uri(), uri);
        }
    }

    /// A target becomes a SIP `Refer-To` header, so anything that could end the header or add
    /// another is refused.
    #[test]
    fn targets_refuse_header_injection_and_other_schemes() {
        for uri in [
            "tel:+14155550100\r\nX-Injected: 1",
            "tel:+1 415 555 0100",
            "<sip:billing@pbx.example.com>",
            "tel:14155550100",
            "tel:+0123456789",
            "tel:+123",
            "sip:pbx.example.com",
            "sip:@pbx.example.com",
            "https://example.com",
            "",
        ] {
            assert!(CallTarget::parse(uri).is_err(), "{uri:?} must be refused");
        }
        assert!(CallTarget::parse(&format!("sip:{}@example.com", "a".repeat(300))).is_err());
    }

    #[test]
    fn targets_round_trip_through_json() {
        let target: CallTarget = serde_json::from_str("\"tel:+14155550100\"").unwrap();
        assert_eq!(serde_json::to_string(&target).unwrap(), "\"tel:+14155550100\"");
        assert!(serde_json::from_str::<CallTarget>("\"tel:911\"").is_err());
    }

    #[test]
    fn dtmf_tones_are_bounded() {
        assert!(DtmfTones::parse("0123456789*#ABCD").is_ok());
        assert!(DtmfTones::parse("").is_err());
        assert!(DtmfTones::parse("1w2").is_err());
        assert!(DtmfTones::parse(&"1".repeat(33)).is_err());
    }
}
