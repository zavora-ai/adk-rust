//! The Microsoft Teams adapter (parity with Google ADK's `TeamsRunner`, on spec 3.3 §5.3): a
//! bot registered with Azure Bot Service — the Bot Framework's activity webhook in, every
//! request's Bot Connector token checked; the Bot Connector REST API out, with a Microsoft
//! Entra ID client-credentials token.

mod api;
mod auth;
mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelGateway, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, InboundBody,
    StopNotice,
};
use async_trait::async_trait;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::mpsc;

use super::shared::{
    ANSWER_NOT_TAKEN, AbortOnDrop, ChannelConnection, HANDLING, Handling, Kit, MESSAGE_NOT_TAKEN,
    PressedCard, Taker, Update, checked_base, checked_token, dispatch, kit_builder, notice_words,
    patient, stopped_words,
};
use crate::error::{Result, SurfaceError};
use api::{ConnectorApi, TokenSource};
use auth::{ConnectorAuth, Refusal};

/// The channel the Teams adapter serves, as the kernel names it: pairings, thread sessions
/// and `kernel.channel` audit records carry it.
pub const TEAMS_CHANNEL: &str = "teams";

/// Microsoft Teams as a person reads it, in "You said yes in Microsoft Teams at 07:31" (spec
/// 3.3 §8.1).
pub const TEAMS: &str = "Microsoft Teams";

/// The Bot Connector address for a conversation the adapter has not heard from since it
/// started: Teams' global address for proactive messages in the public cloud.
const DEFAULT_SERVICE_URL: &str = "https://smba.trafficmanager.net/teams/";
/// Where Microsoft Entra ID issues access tokens.
const DEFAULT_LOGIN_BASE: &str = "https://login.microsoftonline.com";
/// How many activity ids the webhook remembers, so a redelivery is taken once.
const SEEN_KEPT: usize = 4096;
/// What Teams shows on a card when a pressed answer is handed over; the outcome follows as a
/// reply to the card.
const ANSWER_RECEIVED: &str = "Answer received.";

/// Why the Teams adapter could not talk to Microsoft Teams.
///
/// Never carries the app password or an access token. Converts into [`AdkError`] with a
/// `surface.teams.*` code and provider `teams`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::TeamsError;
///
/// let error = TeamsError::Unauthorized;
/// assert_eq!(error.code(), "surface.teams.unauthorized");
/// assert!(error.to_string().contains("TEAMS_APP_PASSWORD"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TeamsError {
    /// Microsoft Entra ID refused the bot's credentials, or the Bot Connector refused its
    /// token.
    #[error(
        "Microsoft Teams refused the bot's credentials: check TEAMS_APP_ID and \
         TEAMS_APP_PASSWORD (a client secret of the bot's app registration), and TEAMS_TENANT_ID \
         for a single-tenant bot"
    )]
    Unauthorized,
    /// The Bot Connector refused the send, such as a person who blocked or uninstalled the
    /// app (`MessageWritesBlocked`).
    #[error(
        "Microsoft Teams refused the message (HTTP 403): {message}; the person may have blocked or removed the app"
    )]
    Forbidden {
        /// The Bot Connector's message.
        message: String,
    },
    /// The Bot Connector throttled the bot (HTTP 429, or 412, 502 or 504, which Teams asks
    /// bots to retry).
    #[error("Microsoft Teams throttled the bot (HTTP {status}): try again later")]
    RateLimited {
        /// The HTTP status.
        status: u16,
    },
    /// The Bot Connector refused the send, such as an unknown conversation or a message too
    /// large.
    #[error("Microsoft Teams refused the message (HTTP {status}): {message}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The Bot Connector's message.
        message: String,
    },
    /// Microsoft Teams or Microsoft Entra ID could not be reached.
    #[error("Microsoft Teams could not be reached: {reason}; check the network")]
    Unreachable {
        /// What failed.
        reason: String,
    },
    /// An answer that was not the Bot Framework's JSON.
    #[error("Microsoft Teams' answer could not be read: {reason}")]
    InvalidResponse {
        /// What was wrong with it.
        reason: String,
    },
}

impl TeamsError {
    /// The error code: `surface.teams.unauthorized`, `.forbidden`, `.rate_limited`,
    /// `.refused`, `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.teams.unauthorized",
            Self::Forbidden { .. } => "surface.teams.forbidden",
            Self::RateLimited { .. } => "surface.teams.rate_limited",
            Self::Refused { .. } => "surface.teams.refused",
            Self::Unreachable { .. } => "surface.teams.unreachable",
            Self::InvalidResponse { .. } => "surface.teams.invalid_response",
        }
    }
}

impl From<TeamsError> for AdkError {
    fn from(error: TeamsError) -> Self {
        let category = match &error {
            TeamsError::Unauthorized => ErrorCategory::Unauthorized,
            TeamsError::Forbidden { .. } => ErrorCategory::Forbidden,
            TeamsError::RateLimited { .. } => ErrorCategory::RateLimited,
            TeamsError::Refused { .. } => ErrorCategory::InvalidInput,
            TeamsError::Unreachable { .. } => ErrorCategory::Unavailable,
            TeamsError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("teams")
    }
}

impl From<TeamsError> for KernelError {
    fn from(error: TeamsError) -> Self {
        Self::Core(error.into())
    }
}

/// Builds a [`TeamsAdapter`].
pub struct TeamsAdapterBuilder {
    app_id: String,
    password: String,
    tenant: Option<String>,
    service_url: String,
    login_base: String,
    openid_configuration: String,
    issuer: String,
    kit: Kit,
}

impl fmt::Debug for TeamsAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeamsAdapterBuilder")
            .field("app_id", &self.app_id)
            .field("tenant", &self.tenant)
            .field("service_url", &self.service_url)
            .finish_non_exhaustive()
    }
}

impl TeamsAdapterBuilder {
    /// A single-tenant bot's Microsoft Entra tenant: its access token comes from that tenant,
    /// and only activities from conversations in it are taken (`403` otherwise). Without it the
    /// bot is multi-tenant and its token comes from `botframework.com`.
    #[must_use]
    pub fn tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    /// The Bot Connector address for a conversation the adapter has not heard from since it
    /// started, such as a wake-up after a restart; defaults to
    /// `https://smba.trafficmanager.net/teams/`. Set it for a sovereign cloud — GCC
    /// `https://smba.infra.gcc.teams.microsoft.com/teams`, GCC High
    /// `https://smba.infra.gov.teams.microsoft.us/teams`. Each activity's own `serviceUrl` is
    /// used for its conversation once one arrives.
    #[must_use]
    pub fn service_url(mut self, url: impl Into<String>) -> Self {
        self.service_url = url.into();
        self
    }

    /// Microsoft Entra ID's login address; defaults to `https://login.microsoftonline.com`.
    /// Plain `http` is accepted on loopback only (a test double).
    #[must_use]
    pub fn login_base(mut self, url: impl Into<String>) -> Self {
        self.login_base = url.into();
        self
    }

    /// The Bot Connector's OpenID metadata document, whose `jwks_uri` lists the keys that sign
    /// every activity's token; defaults to
    /// `https://login.botframework.com/v1/.well-known/openidconfiguration`. Plain `http` is
    /// accepted on loopback only (a test double).
    #[must_use]
    pub fn openid_configuration(mut self, url: impl Into<String>) -> Self {
        self.openid_configuration = url.into();
        self
    }

    /// The issuer every activity's token must carry; defaults to
    /// `https://api.botframework.com`. Set it, with
    /// [`openid_configuration`](Self::openid_configuration), for a sovereign cloud.
    #[must_use]
    pub fn issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = issuer.into();
        self
    }

    kit_builder!("Microsoft Teams");

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for an empty or malformed app id, password or tenant, or an
    /// address that is not `https` (or `http` on loopback).
    pub fn build(self) -> Result<TeamsAdapter> {
        checked_token(&self.app_id, "the bot's app id (TEAMS_APP_ID)")
            .map_err(SurfaceError::Invalid)?;
        if self.password.is_empty() || self.password.chars().any(char::is_control) {
            return Err(SurfaceError::Invalid(
                "the bot's password (TEAMS_APP_PASSWORD) is empty or malformed: copy the client \
                 secret's value again from the app registration"
                    .into(),
            ));
        }
        if let Some(tenant) = &self.tenant {
            checked_token(tenant, "the tenant id (TEAMS_TENANT_ID)")
                .map_err(SurfaceError::Invalid)?;
        }
        let service_url = checked_base(&self.service_url, "the Bot Connector address", false)
            .map_err(SurfaceError::Invalid)?;
        let client = reqwest::Client::builder().build().map_err(|error| {
            SurfaceError::Invalid(format!(
                "the Teams HTTP client could not be built: {}",
                error.without_url()
            ))
        })?;
        let tokens = TokenSource::new(
            client.clone(),
            &self.login_base,
            self.tenant.as_deref(),
            &self.app_id,
            &self.password,
        )
        .map_err(SurfaceError::Invalid)?;
        let auth = ConnectorAuth::new(
            client.clone(),
            &self.openid_configuration,
            &self.issuer,
            &self.app_id,
        )
        .map_err(SurfaceError::Invalid)?;
        Ok(TeamsAdapter {
            inner: Arc::new(Inner {
                api: ConnectorApi::new(client, tokens),
                auth,
                kit: self.kit,
                app_id: self.app_id,
                tenant: self.tenant,
                service_url,
                state: Mutex::default(),
            }),
        })
    }
}

/// The Microsoft Teams adapter: a bot's one-on-one chats as a person's threads with their
/// agents, over the kernel's channel contract (spec 3.3 §10 K1), through Azure Bot Service.
///
/// It is the gateway's [`ChannelSink`] for the channel [`TEAMS_CHANNEL`].
/// [`start`](Self::start) returns the webhook as an Axum router the host serves at the bot's
/// messaging endpoint, such as `https://agents.example.com/api/messages`: Teams has no other
/// way in.
///
/// | Direction | What |
/// |---|---|
/// | In | `POST` is refused before it is read unless its `Authorization` carries a Bot Connector token: signed `RS256` by a key in the Bot Connector's OpenID metadata (cached a day), `iss` `https://api.botframework.com`, `aud` the bot's app id, within its validity with five minutes' skew (`401`), its key endorsing `msteams` and its `serviceurl` the activity's (`403`). Messages and pressed answers from one-on-one chats reach the gateway; group chats, channels and other Bot Framework channels are not read; an activity delivered again is taken once. Typed text never answers a decision (S-I4) and loses the characters a chat app shows as nothing (S-I6) |
/// | Out | `POST {serviceUrl}/v3/conversations/{id}/activities` with an Entra ID token, renewed before it expires. An agent's text and every notice as `plain` text, so nothing formats (S-I9). A card as an Adaptive Card 1.4: the chat screen's lines as `TextRun`s, an `Action.Execute` for each answer the card offers and no others — `Action.Submit` on older clients — and "Open in home"; never the call's arguments (S-I5). A notice about a pressed card replies to it |
/// | Answers | An `adaptiveCard/action` invoke is answered at once with "Answer received."; the gateway decides it with the version the card showed, as every channel does |
/// | Wake-ups | Proactive: a card or a reply to a conversation the adapter has not heard from since it started goes to the configured [`service_url`](TeamsAdapterBuilder::service_url) |
/// | Throttling | HTTP 429, 412, 502 and 504 are retried after `Retry-After`, else 1 and 2 seconds with jitter; never for a stranger |
/// | Commands | "stop all" and `/stop` stop everything; resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::TeamsAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let teams = TeamsAdapter::builder(std::env::var("TEAMS_APP_ID")?, std::env::var("TEAMS_APP_PASSWORD")?)
///     .build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(teams.clone()))
///     .home_url("https://home.example/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// let webhook = teams.start(gateway);
/// let app = axum::Router::new().nest("/api/messages", webhook.router());
/// # let _ = app;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TeamsAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: ConnectorApi,
    auth: ConnectorAuth,
    kit: Kit,
    app_id: String,
    tenant: Option<String>,
    service_url: String,
    state: Mutex<Conversations>,
}

/// The service address of each conversation, and the activities taken.
#[derive(Default)]
struct Conversations {
    service_urls: HashMap<String, String>,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl Conversations {
    /// Notes activity `id`; `false` when it was taken before.
    fn first_time(&mut self, id: &str) -> bool {
        if !self.seen.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > SEEN_KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }
}

impl fmt::Debug for TeamsAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeamsAdapter")
            .field("channel", &TEAMS_CHANNEL)
            .field("app_id", &self.inner.app_id)
            .finish_non_exhaustive()
    }
}

/// The webhook a [`TeamsAdapter::start`] serves: the router the host mounts, and the handing
/// over of what it receives; dropping it stops the handing over.
#[derive(Debug)]
pub struct TeamsWebhook {
    router: axum::Router,
    connection: ChannelConnection<TeamsError>,
}

impl TeamsWebhook {
    /// The webhook's route, `POST /`: mount it with `Router::nest` at the path of the bot's
    /// messaging endpoint, such as `/api/messages`, on a public HTTPS address.
    pub fn router(&self) -> axum::Router {
        self.router.clone()
    }

    /// Stops handing activities over; the router then answers `503`, so the Bot Connector
    /// delivers them again later.
    pub fn stop(&self) {
        self.connection.stop();
    }

    /// Whether the handing over stopped.
    pub fn is_finished(&self) -> bool {
        self.connection.is_finished()
    }

    /// Waits for the handing over to stop: once stopped, or once every router is dropped.
    ///
    /// # Errors
    ///
    /// Never fails today; the error type leaves room for the transport.
    pub async fn join(self) -> std::result::Result<(), TeamsError> {
        drop(self.router);
        self.connection.join().await
    }
}

/// What the webhook's handler shares.
struct Hook {
    adapter: TeamsAdapter,
    sender: mpsc::UnboundedSender<Update<Carried>>,
}

/// What an activity carries beyond its inbound value: the card a pressed answer was on.
pub(crate) struct Carried {
    card: Option<String>,
}

/// What became of one verified activity.
enum Delivery {
    /// Handed over, or nothing to hand over.
    Queued,
    /// A redelivery, taken before.
    Again,
    /// The handing over stopped.
    Closed,
}

impl TeamsAdapter {
    /// An adapter for the bot with app id `app_id` and password `password` — a client secret
    /// of the bot's Microsoft Entra app registration.
    pub fn builder(app_id: impl Into<String>, password: impl Into<String>) -> TeamsAdapterBuilder {
        TeamsAdapterBuilder {
            app_id: app_id.into(),
            password: password.into(),
            tenant: None,
            service_url: DEFAULT_SERVICE_URL.to_string(),
            login_base: DEFAULT_LOGIN_BASE.to_string(),
            openid_configuration: auth::OPENID_CONFIGURATION.to_string(),
            issuer: auth::ISSUER.to_string(),
            kit: Kit::default(),
        }
    }

    /// What the adapter makes of one Bot Framework activity (spec 3.3 S-I4, S-I6): a typed
    /// message or a pressed answer from a one-on-one chat, or `None`. The webhook checks the
    /// activity's token before this; this reads only.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::TeamsAdapter;
    ///
    /// let teams = TeamsAdapter::builder("00000000-0000-0000-0000-000000000001", "secret").build().unwrap();
    /// let activity = serde_json::json!({
    ///     "type": "message", "id": "1485983408511", "channelId": "msteams",
    ///     "serviceUrl": "https://smba.trafficmanager.net/amer/",
    ///     "from": { "id": "29:ana", "name": "Ana" },
    ///     "conversation": { "conversationType": "personal", "id": "a:dm-ana" },
    ///     "text": "stop all",
    /// });
    /// let inbound = teams.inbound(&activity).unwrap();
    /// assert_eq!(inbound.body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!(inbound.thread, "a:dm-ana");
    /// ```
    pub fn inbound(&self, activity: &Value) -> Option<ChannelInbound> {
        wire::parse(activity).and_then(|received| received.inbound)
    }

    /// Serves the webhook, handing each activity to `gateway`, which must have this adapter as
    /// its sink, until [`TeamsWebhook::stop`].
    ///
    /// An activity is answered once it is queued — `200`, or the invoke response for a
    /// pressed answer — and taken by the gateway after: a paired account's first, each
    /// account's in the order sent (S-I6, S-I10); one the gateway fails on is taken again
    /// only when it is a paired account's and the failure may clear. An activity whose id was
    /// taken before is acknowledged and not taken again.
    pub fn start(&self, gateway: ChannelGateway) -> TeamsWebhook {
        let (sender, task) = dispatch(Arc::new(self.clone()), gateway);
        let hook = Arc::new(Hook { adapter: self.clone(), sender });
        let router = axum::Router::new().route("/", post(receive)).with_state(hook);
        let connection = ChannelConnection::spawn(async move {
            let mut dispatcher = AbortOnDrop(task);
            let _ = (&mut dispatcher.0).await;
            Ok(())
        });
        TeamsWebhook { router, connection }
    }

    /// Notes `received`'s conversation and queues what it carries.
    fn take(
        &self,
        received: wire::Received,
        sender: &mpsc::UnboundedSender<Update<Carried>>,
    ) -> Delivery {
        {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.service_urls.insert(received.thread.clone(), received.service_url.clone());
            if !state.first_time(&received.id) {
                return Delivery::Again;
            }
        }
        let Some(inbound) = received.inbound else { return Delivery::Queued };
        let update =
            Update { inbound: Some(inbound), press: Some(Carried { card: received.card }) };
        if sender.send(update).is_err() { Delivery::Closed } else { Delivery::Queued }
    }

    /// The Bot Connector address of `thread`: the one its last activity named, else the
    /// configured one.
    fn service_url(&self, thread: &str) -> String {
        let state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.service_urls.get(thread).cloned().unwrap_or_else(|| self.inner.service_url.clone())
    }

    /// Posts `activity` to `thread`, as a reply to `reply_to` when set; returns its id.
    async fn send(
        &self,
        thread: &str,
        reply_to: Option<&str>,
        activity: &Value,
    ) -> adk_kernel::Result<String> {
        let service = self.service_url(thread);
        // A throttled reply to an account that is not paired is not waited out.
        let patient = patient(TEAMS_CHANNEL, thread);
        Ok(self.inner.api.post(&service, thread, reply_to, activity, patient).await?)
    }
}

/// The status a refused token is answered with; nothing of why.
fn refused(refusal: &Refusal) -> Response {
    let status = match refusal {
        Refusal::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
        Refusal::Forbidden(_) => StatusCode::FORBIDDEN,
        Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    status.into_response()
}

/// An activity: refused before it is read unless the Bot Connector's token vouches for it.
async fn receive(State(hook): State<Arc<Hook>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let verified = match hook.adapter.inner.auth.verify(authorization).await {
        Ok(verified) => verified,
        Err(refusal) => {
            tracing::warn!(reason = ?refusal, "a teams activity without a valid bot connector token was refused");
            return refused(&refusal);
        }
    };
    let Ok(activity) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let channel = activity["channelId"].as_str().unwrap_or_default();
    let service_url = activity["serviceUrl"].as_str().unwrap_or_default();
    if let Err(refusal) = verified.vouches_for(channel, service_url) {
        tracing::warn!(reason = ?refusal, "a teams activity its token does not vouch for was refused");
        return refused(&refusal);
    }
    // The token travels to this address with every reply.
    if checked_base(service_url, "the activity's serviceUrl", false).is_err() {
        tracing::warn!("a teams activity naming a service address that is not https was refused");
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(received) = wire::parse(&activity) else { return StatusCode::OK.into_response() };
    if let Some(tenant) = &hook.adapter.inner.tenant
        && received.tenant.as_deref() != Some(tenant.as_str())
    {
        tracing::warn!("a teams activity from another tenant was refused");
        return StatusCode::FORBIDDEN.into_response();
    }
    let invoke = received.invoke;
    let answers = received
        .inbound
        .as_ref()
        .is_some_and(|inbound| matches!(inbound.body, InboundBody::Answer { .. }));
    match hook.adapter.take(received, &hook.sender) {
        Delivery::Closed => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Delivery::Queued | Delivery::Again if invoke && answers => {
            Json(wire::invoke_message(ANSWER_RECEIVED)).into_response()
        }
        Delivery::Queued | Delivery::Again if invoke => {
            Json(wire::invoke_refused("this card action is not one of the agents' answers"))
                .into_response()
        }
        Delivery::Queued | Delivery::Again => StatusCode::OK.into_response(),
    }
}

#[async_trait]
impl Taker for TeamsAdapter {
    /// The card a pressed answer was on.
    type Press = Carried;

    fn channel_name(&self) -> &'static str {
        TEAMS_CHANNEL
    }

    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Carried>,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(inbound) = update.inbound.clone() else { return Ok(()) };
        let card = match &inbound.body {
            InboundBody::Answer { decision, .. } => Some(PressedCard {
                card: update.press.as_ref().and_then(|carried| carried.card.clone()),
                decision: decision.clone(),
            }),
            _ => None,
        };
        let handling = Handling::of(TEAMS_CHANNEL, gateway, &inbound, paired, card);
        HANDLING.scope(handling, gateway.receive(inbound)).await.map(drop)
    }

    async fn not_taken(&self, update: &Update<Carried>) {
        let Some(inbound) = &update.inbound else { return };
        let card = update.press.as_ref().and_then(|carried| carried.card.as_deref());
        let (text, reply_to) = match card {
            Some(card) => (ANSWER_NOT_TAKEN, Some(card)),
            None => (MESSAGE_NOT_TAKEN, Some(inbound.message_id.as_str())),
        };
        if let Err(error) = self.send(&inbound.thread, reply_to, &wire::text(text)).await {
            tracing::debug!(error = %error, "a person could not be told their message was not taken");
        }
    }
}

#[async_trait]
impl ChannelSink for TeamsAdapter {
    fn channel(&self) -> &str {
        TEAMS_CHANNEL
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        for activity in wire::plain(text) {
            self.send(thread, None, &activity).await?;
        }
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        let (heading, card) = self.inner.kit.prepare(card).await;
        let activity = wire::card(&card, &heading, self.inner.kit.offset);
        self.send(thread, None, &activity).await.map(drop)
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send(thread, None, &wire::message(&stopped_words(notice))).await.map(drop)
    }

    /// A notice about a pressed card records the answer with where it was given before the
    /// confirmation is sent, as a reply to the card (spec 3.3 §8.1, §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let card = self.inner.kit.before_notice(TEAMS_CHANNEL, TEAMS, thread, notice).await;
        let card = card.filter(|card| !card.is_empty());
        let words = notice_words(notice, self.inner.kit.offset, Utc::now(), card.is_some());
        self.send(thread, card.as_deref(), &wire::message(&words)).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_ID: &str = "00000000-0000-0000-0000-000000000001";

    #[test]
    fn errors_say_what_to_do_and_keep_their_category() {
        let error: AdkError = TeamsError::Unauthorized.into();
        assert_eq!(error.category, ErrorCategory::Unauthorized);
        assert_eq!(error.code, "surface.teams.unauthorized");
        let throttled: AdkError = TeamsError::RateLimited { status: 429 }.into();
        assert!(throttled.is_retryable());
        let blocked: AdkError =
            TeamsError::Forbidden { message: "MessageWritesBlocked".into() }.into();
        assert_eq!(blocked.category, ErrorCategory::Forbidden);
    }

    #[test]
    fn the_builder_checks_its_inputs_and_never_echoes_the_password() {
        let adapter = TeamsAdapter::builder(APP_ID, "s3cret~pass").build().unwrap();
        assert!(!format!("{adapter:?}").contains("s3cret"));
        assert!(!format!("{:?}", TeamsAdapter::builder(APP_ID, "s3cret~pass")).contains("s3cret"));
        assert_eq!(ChannelSink::channel(&adapter), "teams");
        let empty = TeamsAdapter::builder(APP_ID, "").build().unwrap_err();
        assert!(empty.to_string().contains("TEAMS_APP_PASSWORD"), "{empty}");
        let id = TeamsAdapter::builder("app id", "p").build().unwrap_err();
        assert!(id.to_string().contains("TEAMS_APP_ID"), "{id}");
        let plain = TeamsAdapter::builder(APP_ID, "p")
            .service_url("http://smba.example/teams")
            .build()
            .unwrap_err();
        assert!(plain.to_string().contains("https"), "{plain}");
        assert!(TeamsAdapter::builder(APP_ID, "p").tenant("a tenant").build().is_err());
    }

    #[test]
    fn a_conversation_is_answered_where_it_last_wrote_from() {
        let adapter = TeamsAdapter::builder(APP_ID, "p").build().unwrap();
        assert_eq!(adapter.service_url("a:1"), "https://smba.trafficmanager.net/teams");
        let (sender, _receiver) = mpsc::unbounded_channel();
        let received = wire::Received {
            id: "1".into(),
            thread: "a:1".into(),
            service_url: "https://smba.trafficmanager.net/emea/".into(),
            tenant: None,
            inbound: None,
            card: None,
            invoke: false,
        };
        assert!(matches!(adapter.take(received.clone(), &sender), Delivery::Queued));
        assert!(matches!(adapter.take(received, &sender), Delivery::Again));
        assert_eq!(adapter.service_url("a:1"), "https://smba.trafficmanager.net/emea/");
        let mut state = Conversations::default();
        assert!(state.first_time("x") && !state.first_time("x"));
        for index in 0..SEEN_KEPT + 1 {
            state.first_time(&format!("y{index}"));
        }
        assert!(state.first_time("x"), "forgotten after {SEEN_KEPT}");
    }
}
