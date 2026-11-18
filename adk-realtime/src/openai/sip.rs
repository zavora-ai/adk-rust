//! OpenAI Realtime SIP: phone calls that reach a Realtime session through OpenAI's SIP
//! connector (feature `openai-sip`).
//!
//! | Step | API |
//! |------|-----|
//! | A call arrives; OpenAI posts a `realtime.call.incoming` webhook | [`OpenAIWebhookVerifier::verify`] checks the Standard Webhooks signature and returns the [`IncomingCall`] |
//! | Answer or decline it | [`OpenAISipClient::accept`] (`POST /v1/realtime/calls/{call_id}/accept`) with the session's `RealtimeConfig`; [`OpenAISipClient::reject`] |
//! | Run the agent on the call | [`OpenAISipClient::realtime_model`] attaches a WebSocket to the call's session (`wss://api.openai.com/v1/realtime?call_id=...`), so a `RealtimeAgent` dispatches the model's tool calls through the governed path |
//! | Transfer or hang up | [`OpenAISipCall`], a [`CallControl`]: `POST .../refer` and `POST .../hangup` |
//!
//! OpenAI carries the call's audio between the SIP trunk and the model; the WebSocket carries
//! events and tool calls only. When the agent's session ends the call does not, so hang up
//! explicitly, and hold a [`hang_up_on_freeze`](crate::telephony::hang_up_on_freeze) guard so
//! a kill-switch freeze ends the call. This flow documents neither outbound calls nor DTMF;
//! [`OpenAISipCall::capabilities`] reports DTMF unsupported.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_realtime::openai::sip::{OpenAISipClient, OpenAIWebhookEvent, OpenAIWebhookVerifier};
//! use adk_realtime::telephony::{CallTarget, EndCallTool, TransferCallTool};
//! use adk_realtime::{RealtimeAgent, RealtimeConfig};
//!
//! # async fn on_webhook(headers: &reqwest::header::HeaderMap, body: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
//! let verifier = OpenAIWebhookVerifier::new(&std::env::var("OPENAI_WEBHOOK_SECRET")?)?;
//! let header = |name: &'static str| headers.get(name).and_then(|value| value.to_str().ok());
//! let OpenAIWebhookEvent::IncomingCall(call) = verifier.verify(header, body).await? else {
//!     return Ok(());
//! };
//!
//! let client = OpenAISipClient::new(std::env::var("OPENAI_API_KEY")?);
//! let config = RealtimeConfig::default().with_instruction("You answer calls for Example Corp.");
//! let handle = Arc::new(client.accept(&call.call_id, &config).await?);
//!
//! let agent = RealtimeAgent::builder("receptionist")
//!     .model(Arc::new(client.realtime_model(&call.call_id)))
//!     .instruction("You answer calls for Example Corp.")
//!     .tool(Arc::new(
//!         TransferCallTool::new(handle.clone())
//!             .destination("billing", CallTarget::phone("+14155550100")?),
//!     ))
//!     .tool(Arc::new(EndCallTool::new(handle)))
//!     .build()?;
//! # let _ = agent;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use adk_core::egress::ToolEgress;
use adk_core::webhook::{
    WebhookHeaders, WebhookReplayGuard, WebhookSecret, WebhookVerifier, replay_window,
};
use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

use super::DEFAULT_MODEL;
use super::OpenAIRealtimeModel;
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::telephony::{
    CallAnswerer, CallCapabilities, CallControl, CallTarget, IncomingCall, SipHeader,
};

/// The OpenAI REST API base URL.
pub const OPENAI_API_BASE: &str = "https://api.openai.com/v1";

/// The webhook event type OpenAI posts when a SIP call reaches the project.
pub const REALTIME_CALL_INCOMING: &str = "realtime.call.incoming";

/// The provider name an OpenAI call reports.
const PROVIDER: &str = "openai";

/// A verified OpenAI webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenAIWebhookEvent {
    /// `realtime.call.incoming`: a SIP call is ringing and waits to be accepted or rejected.
    IncomingCall(IncomingCall),
    /// Any other event type the endpoint is subscribed to.
    Other {
        /// The event's `type`.
        event_type: String,
        /// The event's `data`.
        data: Value,
    },
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    data: Value,
}

#[derive(Deserialize)]
struct IncomingCallData {
    call_id: String,
    #[serde(default)]
    sip_headers: Vec<SipHeader>,
}

/// Verifies OpenAI webhook deliveries and reads `realtime.call.incoming`.
///
/// OpenAI signs deliveries with the [Standard Webhooks](adk_core::webhook) scheme: HMAC-SHA256
/// over `{webhook-id}.{webhook-timestamp}.{body}`, keyed with the endpoint's signing secret.
/// A secret that starts with `whsec_` is base64-decoded, and any other secret is used as its
/// UTF-8 bytes, as OpenAI's SDKs do. Deliveries older or newer than five minutes are refused.
/// OpenAI retries a delivery under the same `webhook-id` and may send duplicates, so a
/// [`WebhookReplayGuard`] makes each id count once.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::{WebhookSecret, WebhookSigner};
/// use adk_realtime::openai::sip::{OpenAIWebhookEvent, OpenAIWebhookVerifier};
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let secret = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
/// let body = br#"{"object":"event","id":"evt_1","type":"realtime.call.incoming","created_at":1750287018,"data":{"call_id":"rtc_1","sip_headers":[{"name":"From","value":"sip:+14155550123@sip.example.com"}]}}"#;
/// let signed = WebhookSigner::new(WebhookSecret::from_base64(secret)?).sign_now("wh_1", body);
///
/// let verifier = OpenAIWebhookVerifier::new(secret)?;
/// let headers = signed.header_pairs();
/// let header = |name: &'static str| {
///     headers.iter().find(|(key, _)| *key == name).map(|(_, value)| value.as_str())
/// };
/// let OpenAIWebhookEvent::IncomingCall(call) = verifier.verify(header, body).await? else {
///     unreachable!()
/// };
/// assert_eq!(call.call_id, "rtc_1");
/// assert_eq!(call.from(), Some("sip:+14155550123@sip.example.com"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
#[derive(Clone)]
pub struct OpenAIWebhookVerifier {
    verifier: WebhookVerifier,
    replay: Option<Arc<dyn WebhookReplayGuard>>,
}

impl std::fmt::Debug for OpenAIWebhookVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIWebhookVerifier")
            .field("verifier", &self.verifier)
            .field("replay_guard", &self.replay.is_some())
            .finish()
    }
}

impl OpenAIWebhookVerifier {
    /// A verifier for the endpoint's signing secret, as the OpenAI dashboard shows it.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when the secret is empty or a `whsec_` secret is
    /// not base64.
    pub fn new(secret: &str) -> Result<Self> {
        Ok(Self::with_verifier(WebhookVerifier::new(Self::secret(secret)?)))
    }

    /// A verifier with a configured [`WebhookVerifier`], for several secrets during a
    /// rotation, another tolerance, or a test clock.
    pub fn with_verifier(verifier: WebhookVerifier) -> Self {
        Self { verifier, replay: None }
    }

    /// The HMAC key of an OpenAI signing secret.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when the secret is empty or a `whsec_` secret is
    /// not base64.
    pub fn secret(secret: &str) -> Result<WebhookSecret> {
        let secret = secret.trim();
        let key = if secret.starts_with("whsec_") {
            WebhookSecret::from_base64(secret)
        } else {
            WebhookSecret::from_bytes(secret.as_bytes())
        };
        key.map_err(|error| RealtimeError::config(format!("OpenAI webhook secret: {error}")))
    }

    /// Refuses a second delivery of an accepted `webhook-id`.
    pub fn with_replay_guard(mut self, guard: Arc<dyn WebhookReplayGuard>) -> Self {
        self.replay = Some(guard);
        self
    }

    /// Verifies one delivery and reads its event.
    ///
    /// `header` looks up a request header by its lowercase name; `body` is the raw request
    /// body, since re-serialized JSON changes the signed bytes.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::AuthError`] when a header is missing, the signature does not
    /// match, the timestamp is outside the tolerance, or the replay guard has seen the
    /// `webhook-id`; [`RealtimeError::MessageError`] when the body is not an OpenAI event.
    /// Answer an authentication failure with `400` and do not act on the body.
    pub async fn verify<'a>(
        &self,
        header: impl Fn(&'static str) -> Option<&'a str>,
        body: &[u8],
    ) -> Result<OpenAIWebhookEvent> {
        let rejected = |error: adk_core::webhook::WebhookError| {
            RealtimeError::AuthError(format!("OpenAI webhook rejected: {error}"))
        };
        let headers = WebhookHeaders::from_lookup(header).map_err(rejected)?;
        let verified = self.verifier.verify(&headers, body).map_err(rejected)?;
        if let Some(guard) = &self.replay {
            let window = chrono::Duration::from_std(replay_window(self.verifier.tolerance()))
                .unwrap_or(chrono::Duration::MAX);
            let keep_until = chrono::Utc::now() + window;
            let first = guard.first_seen(&verified.id, keep_until).await.map_err(|error| {
                RealtimeError::AuthError(format!(
                    "OpenAI webhook replay guard unavailable: {}",
                    error.redacted()
                ))
            })?;
            if !first {
                return Err(RealtimeError::AuthError(format!(
                    "OpenAI webhook {} was already delivered",
                    verified.id
                )));
            }
        }

        let envelope: Envelope = serde_json::from_slice(body)
            .map_err(|error| RealtimeError::protocol(format!("OpenAI webhook body: {error}")))?;
        if envelope.event_type != REALTIME_CALL_INCOMING {
            return Ok(OpenAIWebhookEvent::Other {
                event_type: envelope.event_type,
                data: envelope.data,
            });
        }
        let data: IncomingCallData = serde_json::from_value(envelope.data).map_err(|error| {
            RealtimeError::protocol(format!("{REALTIME_CALL_INCOMING} data: {error}"))
        })?;
        tracing::info!(call.provider = PROVIDER, call.id = %data.call_id, "incoming SIP call");
        Ok(OpenAIWebhookEvent::IncomingCall(
            IncomingCall::new(PROVIDER, data.call_id).with_sip_headers(data.sip_headers),
        ))
    }
}

/// Controls OpenAI Realtime SIP calls over the REST API.
///
/// # Example
///
/// ```rust
/// use adk_realtime::openai::sip::OpenAISipClient;
///
/// let client = OpenAISipClient::new("sk-...").with_model("gpt-realtime-2.1");
/// let model = client.realtime_model("rtc_123");
/// assert_eq!(model.call_id(), Some("rtc_123"));
/// assert_eq!(model.websocket_url(), "wss://api.openai.com/v1/realtime?call_id=rtc_123");
/// ```
#[derive(Clone)]
pub struct OpenAISipClient {
    http: reqwest::Client,
    api_key: SecretString,
    base_url: String,
    realtime_url: Option<String>,
    model: String,
}

impl std::fmt::Debug for OpenAISipClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAISipClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl OpenAISipClient {
    /// A client for the project of `api_key`, answering with the default realtime model.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: SecretString::from(api_key.into()),
            base_url: OPENAI_API_BASE.to_string(),
            realtime_url: None,
            model: DEFAULT_MODEL.to_string(),
        }
    }

    /// Sets the realtime model calls are answered with.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets the REST base URL (default `https://api.openai.com/v1`), for a proxy or a test
    /// server.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Sets the WebSocket URL [`realtime_model`](Self::realtime_model) attaches through
    /// (default `wss://api.openai.com/v1/realtime`).
    pub fn with_realtime_url(mut self, url: impl Into<String>) -> Self {
        self.realtime_url = Some(url.into());
        self
    }

    /// Uses `http` for the REST calls, for timeouts or an egress-guarded client.
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// The realtime model calls are answered with.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Accepts the ringing call `call_id` with a session configured by `config`, and returns
    /// its control handle.
    ///
    /// The session object is the one a `session.update` carries, with the model added and the
    /// audio format left to the call. Include the agent's tools in `config` (as
    /// `RealtimeAgent` sends them) or let the attached session's first `session.update` set
    /// them.
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ServerError`] when OpenAI refuses, for example because the
    /// call was already accepted or rejected, or [`RealtimeError::ConnectionError`] when the
    /// request fails.
    pub async fn accept(&self, call_id: &str, config: &RealtimeConfig) -> Result<OpenAISipCall> {
        let mut session = crate::openai::protocol::call_session_config(config);
        session["model"] = json!(self.model);
        self.post(call_id, "accept", Some(session)).await?;
        tracing::info!(call.provider = PROVIDER, call.id = %call_id, "SIP call accepted");
        Ok(self.call(call_id))
    }

    /// Rejects the ringing call `call_id` with SIP status `status_code` (OpenAI sends
    /// `603 Decline` when it is `None`).
    ///
    /// # Errors
    ///
    /// Returns [`RealtimeError::ConfigError`] when the status is not 300–699, and the errors
    /// of [`accept`](Self::accept).
    pub async fn reject(&self, call_id: &str, status_code: Option<u16>) -> Result<()> {
        let body = match status_code {
            Some(status) if (300..=699).contains(&status) => Some(json!({ "status_code": status })),
            Some(status) => {
                return Err(RealtimeError::config(format!(
                    "SIP status {status} cannot reject a call; use 300 to 699"
                )));
            }
            None => None,
        };
        self.post(call_id, "reject", body).await?;
        tracing::info!(call.provider = PROVIDER, call.id = %call_id, "SIP call rejected");
        Ok(())
    }

    /// Transfers the call `call_id` to `target` with a SIP REFER; `target` becomes the
    /// `Refer-To` header.
    ///
    /// # Errors
    ///
    /// The errors of [`accept`](Self::accept).
    pub async fn refer(&self, call_id: &str, target: &CallTarget) -> Result<()> {
        self.post(call_id, "refer", Some(json!({ "target_uri": target.as_uri() }))).await
    }

    /// Hangs up the call `call_id`.
    ///
    /// # Errors
    ///
    /// The errors of [`accept`](Self::accept).
    pub async fn hang_up(&self, call_id: &str) -> Result<()> {
        self.post(call_id, "hangup", None).await
    }

    /// The control handle of a call already accepted.
    pub fn call(&self, call_id: impl Into<String>) -> OpenAISipCall {
        OpenAISipCall { client: self.clone(), call_id: call_id.into() }
    }

    /// A realtime model whose sessions attach to the call `call_id`, for a `RealtimeAgent`
    /// or `RealtimeRunner` to drive.
    pub fn realtime_model(&self, call_id: &str) -> OpenAIRealtimeModel {
        let model = OpenAIRealtimeModel::new(self.api_key.expose_secret(), &self.model)
            .with_call_id(call_id);
        match &self.realtime_url {
            Some(url) => model.with_base_url(url),
            None => model,
        }
    }

    async fn post(&self, call_id: &str, action: &str, body: Option<Value>) -> Result<()> {
        let call: String = url::form_urlencoded::byte_serialize(call_id.as_bytes()).collect();
        let url = format!("{}/realtime/calls/{call}/{action}", self.base_url);
        let mut request = self.http.post(&url).bearer_auth(self.api_key.expose_secret());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|error| {
            RealtimeError::connection(format!("OpenAI SIP {action} request failed: {error}"))
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().await.unwrap_or_default();
        let detail: Value = serde_json::from_str(&text).unwrap_or_default();
        let code = detail
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        let message = detail
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("OpenAI SIP {action} returned {status}"));
        tracing::warn!(
            call.provider = PROVIDER,
            call.id = %call_id,
            call.action = action,
            http.status = status.as_u16(),
            error.code = %code,
            "OpenAI SIP request refused"
        );
        Err(RealtimeError::ServerError { code, message })
    }
}

#[async_trait]
impl CallAnswerer for OpenAISipClient {
    async fn answer(
        &self,
        call: &IncomingCall,
        config: &RealtimeConfig,
    ) -> Result<Arc<dyn CallControl>> {
        Ok(Arc::new(self.accept(&call.call_id, config).await?))
    }

    async fn decline(&self, call: &IncomingCall, status: u16) -> Result<()> {
        self.reject(&call.call_id, Some(status)).await
    }
}

/// One OpenAI Realtime SIP call: transfer (`refer`) and hang up.
#[derive(Debug, Clone)]
pub struct OpenAISipCall {
    client: OpenAISipClient,
    call_id: String,
}

#[async_trait]
impl CallControl for OpenAISipCall {
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
        ToolEgress::for_urls([self.client.base_url.as_str()])
    }

    async fn transfer(&self, target: &CallTarget) -> Result<()> {
        self.client.refer(&self.call_id, target).await
    }

    async fn hang_up(&self) -> Result<()> {
        self.client.hang_up(&self.call_id).await
    }
}
