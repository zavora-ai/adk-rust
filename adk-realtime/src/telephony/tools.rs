//! The call controls as governed `adk_core::Tool`s.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use adk_core::egress::ToolEgress;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{CallControl, CallTarget, DialRequest, DtmfTones, OutboundDialer};
use crate::error::RealtimeError;

/// A call-control failure as the tool error the model is answered with.
fn call_error(error: RealtimeError) -> AdkError {
    let (category, code) = match &error {
        RealtimeError::ConfigError(_) => (ErrorCategory::InvalidInput, "telephony.invalid_request"),
        RealtimeError::ProviderError(_) => (ErrorCategory::Unsupported, "telephony.unsupported"),
        RealtimeError::Timeout(_) => (ErrorCategory::Timeout, "telephony.timeout"),
        _ => (ErrorCategory::Unavailable, "telephony.provider_error"),
    };
    AdkError::new(ErrorComponent::Tool, category, code, error.to_string())
}

/// A destination name the model gave that the application did not configure.
fn unknown_name(kind: &str, name: &str, known: &BTreeMap<String, CallTarget>) -> AdkError {
    let names: Vec<&str> = known.keys().map(String::as_str).collect();
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "telephony.unknown_destination",
        format!("'{name}' is not a configured {kind}; use one of: {}", names.join(", ")),
    )
}

/// The string argument `key` of a call.
fn string_arg<'a>(args: &'a Value, key: &str) -> adk_core::Result<&'a str> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "telephony.invalid_request",
            format!("the `{key}` argument is required"),
        )
    })
}

/// The schema of a tool whose one required argument names a configured destination.
fn named_destination_schema(key: &str, names: &BTreeMap<String, CallTarget>) -> Value {
    json!({
        "type": "object",
        "properties": {
            key: {
                "type": "string",
                "enum": names.keys().collect::<Vec<_>>(),
            },
            "reason": {
                "type": "string",
                "description": "Why, in a short sentence for the call record.",
            },
        },
        "required": [key],
    })
}

/// Transfers the caller to a destination the application configured (`transfer_call`).
///
/// The model names a destination; it never supplies a number. The call is a
/// [`ToolEffect::NonIdempotent`] action, so under a `RealtimeAgent` it is authorized by the
/// governed path, begun in the run's action ledger and counted against the run's budget. A
/// `ToolPolicy` can require a person's approval before any transfer.
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "openai-sip")]
/// # fn main() -> adk_realtime::Result<()> {
/// use std::sync::Arc;
///
/// use adk_realtime::openai::sip::OpenAISipClient;
/// use adk_realtime::telephony::{CallTarget, TransferCallTool};
///
/// let client = OpenAISipClient::new(std::env::var("OPENAI_API_KEY").unwrap_or_default());
/// let call = Arc::new(client.call("rtc_123"));
/// let transfer = TransferCallTool::new(call)
///     .destination("billing", CallTarget::phone("+14155550100")?)
///     .destination("support", "sip:support@pbx.example.com".parse()?);
/// # let _ = transfer;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "openai-sip"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct TransferCallTool {
    call: Arc<dyn CallControl>,
    destinations: BTreeMap<String, CallTarget>,
    description: String,
}

impl TransferCallTool {
    /// A transfer tool for `call` with no destinations; add them with
    /// [`destination`](Self::destination).
    pub fn new(call: Arc<dyn CallControl>) -> Self {
        let mut tool = Self { call, destinations: BTreeMap::new(), description: String::new() };
        tool.describe();
        tool
    }

    /// Lets the model transfer the caller to `target` under `name`.
    pub fn destination(mut self, name: impl Into<String>, target: CallTarget) -> Self {
        self.destinations.insert(name.into(), target);
        self.describe();
        self
    }

    fn describe(&mut self) {
        let names: Vec<&str> = self.destinations.keys().map(String::as_str).collect();
        self.description = format!(
            "Transfers the caller to a person or team and ends your part of the call. Tell the caller before you transfer. Destinations: {}.",
            names.join(", ")
        );
    }
}

#[async_trait]
impl Tool for TransferCallTool {
    fn name(&self) -> &str {
        "transfer_call"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(named_destination_schema("destination", &self.destinations))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        self.call.egress()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let name = string_arg(&args, "destination")?;
        let target = self
            .destinations
            .get(name)
            .ok_or_else(|| unknown_name("destination", name, &self.destinations))?;
        self.call.transfer(target).await.map_err(call_error)?;
        tracing::info!(
            call.provider = self.call.provider(),
            call.id = self.call.call_id(),
            call.destination = name,
            "call transferred"
        );
        Ok(json!({ "status": "transferred", "destination": name }))
    }
}

/// Ends the call (`end_call`).
///
/// Hanging up twice has the effect of hanging up once, so the tool is
/// [`ToolEffect::Idempotent`].
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "openai-sip")]
/// # fn main() {
/// use std::sync::Arc;
///
/// use adk_realtime::openai::sip::OpenAISipClient;
/// use adk_realtime::telephony::EndCallTool;
///
/// let client = OpenAISipClient::new(std::env::var("OPENAI_API_KEY").unwrap_or_default());
/// let end_call = EndCallTool::new(Arc::new(client.call("rtc_123")));
/// # let _ = end_call;
/// # }
/// # #[cfg(not(feature = "openai-sip"))]
/// # fn main() {}
/// ```
#[derive(Debug)]
pub struct EndCallTool {
    call: Arc<dyn CallControl>,
}

impl EndCallTool {
    /// An end-call tool for `call`.
    pub fn new(call: Arc<dyn CallControl>) -> Self {
        Self { call }
    }
}

#[async_trait]
impl Tool for EndCallTool {
    fn name(&self) -> &str {
        "end_call"
    }

    fn description(&self) -> &str {
        "Hangs up the phone call. Say goodbye first."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why, in a short sentence for the call record.",
                },
            },
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        self.call.egress()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.call.hang_up().await.map_err(call_error)?;
        tracing::info!(
            call.provider = self.call.provider(),
            call.id = self.call.call_id(),
            "call ended by the agent"
        );
        Ok(json!({ "status": "ended" }))
    }
}

/// Sends DTMF tones on the call (`send_dtmf`), for example to navigate a phone menu.
///
/// Offer it only on a call whose [`CallCapabilities::dtmf`](super::CallCapabilities::dtmf)
/// is set; elsewhere every call fails with `telephony.unsupported`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::Tool;
/// use adk_realtime::telephony::{
///     CallCapabilities, CallControl, CallTarget, SendDtmfTool,
/// };
/// use async_trait::async_trait;
///
/// #[derive(Debug)]
/// struct Call;
///
/// #[async_trait]
/// impl CallControl for Call {
///     fn call_id(&self) -> &str { "call-1" }
///     fn provider(&self) -> &str { "example" }
///     fn capabilities(&self) -> CallCapabilities { CallCapabilities::new(false, true) }
///     async fn transfer(&self, _target: &CallTarget) -> adk_realtime::Result<()> { Ok(()) }
///     async fn hang_up(&self) -> adk_realtime::Result<()> { Ok(()) }
/// }
///
/// let tool = SendDtmfTool::new(Arc::new(Call));
/// assert_eq!(tool.name(), "send_dtmf");
/// ```
#[derive(Debug)]
pub struct SendDtmfTool {
    call: Arc<dyn CallControl>,
}

impl SendDtmfTool {
    /// A DTMF tool for `call`.
    pub fn new(call: Arc<dyn CallControl>) -> Self {
        Self { call }
    }
}

#[async_trait]
impl Tool for SendDtmfTool {
    fn name(&self) -> &str {
        "send_dtmf"
    }

    fn description(&self) -> &str {
        "Presses keys on the phone keypad, for example to choose an option in an automated menu."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "tones": {
                    "type": "string",
                    "description": "The keys to press: 0-9, * and #, at most 32.",
                    "pattern": "^[0-9*#A-D]{1,32}$",
                },
            },
            "required": ["tones"],
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        self.call.egress()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let tones = DtmfTones::parse(string_arg(&args, "tones")?).map_err(call_error)?;
        self.call.send_dtmf(&tones).await.map_err(call_error)?;
        Ok(json!({ "status": "sent", "tones": tones.as_str() }))
    }
}

/// Places an outbound call to a contact the application configured (`place_call`).
///
/// Outbound calling is opt-in: the tool exists only when the application builds it, and it
/// reaches only the contacts the application names. Each tool places at most
/// [`max_calls`](Self::max_calls) calls (one by default), attempts included, on top of the
/// run's tool-call budget. Every call is [`ToolEffect::NonIdempotent`], so under a
/// `RealtimeAgent` it is authorized by the governed path and begun in the run's action
/// ledger. The calls placed are available from [`placed_calls`](Self::placed_calls), so the
/// application can attach an agent to them.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::Tool;
/// use adk_realtime::telephony::{
///     CallControl, CallTarget, DialRequest, OutboundDialer, PlaceCallTool,
/// };
/// use async_trait::async_trait;
///
/// #[derive(Debug)]
/// struct Dialer;
///
/// #[async_trait]
/// impl OutboundDialer for Dialer {
///     fn provider(&self) -> &str { "example" }
///     async fn dial(&self, _request: &DialRequest) -> adk_realtime::Result<Arc<dyn CallControl>> {
///         Err(adk_realtime::RealtimeError::provider("no trunk in this example"))
///     }
/// }
///
/// let tool = PlaceCallTool::new(Arc::new(Dialer))
///     .contact("on-call engineer", CallTarget::phone("+14155550199")?)
///     .max_calls(2);
/// assert_eq!(tool.name(), "place_call");
/// # Ok::<(), adk_realtime::RealtimeError>(())
/// ```
#[derive(Debug)]
pub struct PlaceCallTool {
    dialer: Arc<dyn OutboundDialer>,
    contacts: BTreeMap<String, CallTarget>,
    attributes: BTreeMap<String, String>,
    max_calls: u32,
    attempts: AtomicU32,
    placed: std::sync::Mutex<Vec<Arc<dyn CallControl>>>,
    description: String,
}

impl PlaceCallTool {
    /// A dialling tool on `dialer` with no contacts and a limit of one call.
    pub fn new(dialer: Arc<dyn OutboundDialer>) -> Self {
        let mut tool = Self {
            dialer,
            contacts: BTreeMap::new(),
            attributes: BTreeMap::new(),
            max_calls: 1,
            attempts: AtomicU32::new(0),
            placed: std::sync::Mutex::new(Vec::new()),
            description: String::new(),
        };
        tool.describe();
        tool
    }

    /// Lets the model call `target` under `name`.
    pub fn contact(mut self, name: impl Into<String>, target: CallTarget) -> Self {
        self.contacts.insert(name.into(), target);
        self.describe();
        self
    }

    /// Sets a provider attribute on every call placed, such as LiveKit's `room`.
    pub fn attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Sets how many calls the tool may place, attempts that fail included.
    pub fn max_calls(mut self, max_calls: u32) -> Self {
        self.max_calls = max_calls;
        self
    }

    /// The calls placed so far.
    pub fn placed_calls(&self) -> Vec<Arc<dyn CallControl>> {
        self.placed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn describe(&mut self) {
        let names: Vec<&str> = self.contacts.keys().map(String::as_str).collect();
        self.description =
            format!("Places a phone call to one of these contacts: {}.", names.join(", "));
    }
}

#[async_trait]
impl Tool for PlaceCallTool {
    fn name(&self) -> &str {
        "place_call"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(named_destination_schema("contact", &self.contacts))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        self.dialer.egress()
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let name = string_arg(&args, "contact")?;
        let target =
            self.contacts.get(name).ok_or_else(|| unknown_name("contact", name, &self.contacts))?;
        // Reserved before dialling and never released: a failed attempt may still have rung.
        let reserved = self.attempts.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            (used < self.max_calls).then_some(used + 1)
        });
        if reserved.is_err() {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::ResourceExhausted,
                "telephony.call_limit",
                format!("this agent may place at most {} call(s), and has", self.max_calls),
            ));
        }
        let mut request = DialRequest::new(target.clone());
        request.attributes = self.attributes.clone();
        let call = self.dialer.dial(&request).await.map_err(call_error)?;
        let call_id = call.call_id().to_string();
        tracing::info!(
            call.provider = self.dialer.provider(),
            call.id = %call_id,
            call.contact = name,
            "outbound call placed"
        );
        self.placed.lock().unwrap_or_else(|e| e.into_inner()).push(call);
        Ok(json!({ "status": "placed", "contact": name, "call_id": call_id }))
    }
}
