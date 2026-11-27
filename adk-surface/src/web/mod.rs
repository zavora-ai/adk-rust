//! The web home (spec 3.3 §5.1, §6; feature `web`): its HTTP API and its page.
//!
//! [`home`] serves the page — the React app built from `adk-surface/web`, embedded — at
//! `/ui/` as root routes (§10 S1); mount it with `ServerBuilder::add_root_routes`.
//!
//! [`router`] serves the engine under `/surface`, relative to the `/api` prefix: mount it
//! with `ServerBuilder::add_api_routes`, beside the kernel's control-plane routes, so the
//! server's auth middleware puts the caller's `RequestContext` in every request. Every route
//! needs that caller (`401` without one) and reads or acts only on the caller's own sessions
//! and decisions (spec 3.3 §11 S-I8): anything else answers `404`. **Stop all** and **Resume
//! all** are the exception: they freeze and unfreeze every run of the source, so a server
//! that admits more than one person builds its engine with
//! [`SurfaceEngineBuilder::stops_need`](crate::SurfaceEngineBuilder::stops_need), and a caller
//! without that group (the `RequestContext`'s scopes) gets `403 surface.operator_only`
//! (§11 S-I10).
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/decisions` | [`DecisionsResponse`]: the caller's open decisions as cards; `?session=` for one session's; `?decidedSince=` adds those answered since, newest first |
//! | `GET /surface/decisions/{id}` | [`DecisionCard`]: waiting, or decided with who, where and when |
//! | `GET /surface/sessions/{session}/thread` | [`ThreadResponse`]: the session's entries, numbered by store position |
//! | `GET /surface/sessions/{session}/feed` | Server-sent events: `entry` (with `id` = `seq`), `decisions`, `reset`, and `changed` for an entry already sent whose turn then failed; resumes after `Last-Event-ID` or `?after=` |
//! | `GET /surface/sessions/{session}/queued` | [`QueuedResponse`]: what the caller typed while a decision of the session was open or a turn ran, not yet sent; a message the running turn holds is listed until the turn takes it |
//! | `GET /surface/agents` | [`AgentsResponse`] |
//! | `GET /surface/status` | [`StatusLine`]; `?session=` for that session's mode |
//! | `GET /surface/briefing` | [`Briefing`] since `?since=` (default: the last 24 hours) |
//! | `POST /surface/commands` | [`CommandRequest`] → [`CommandOutcome`]; `send` answers at once, and `steer` once the running turn took the message or handed it back |
//! | `GET /surface/openapi.json` | The OpenAPI 3.1 document of these routes |
//! | `POST /surface/decisions/{id}/take-over` | [`TakeOverRequest`] → [`CommandOutcome`]; mounted by [`take_over_router`] beside a harness that takes over |
//! | `/surface/workflow/…` | The Runs tab's routes (feature `workflow`), mounted by `workflow::router` beside these |
//! | `GET /surface/genui/capabilities`, `GET /surface/genui/sessions/{session}/forms` | Agent-drawn UI (spec 3.3 §6.1 `genui`): mounted by [`genui::router`](mod@genui) with feature `genui` |
//!
//! Reads take `?surface=` (`web`, `terminal`, `phone`, `tray`, `chat`, `voice`) so a card
//! lists the answers that surface may give (§7.2). A declared surface or origin can only
//! narrow what a caller may decide. What widens it is set by the server, never the client:
//! [`PRESENCE_METADATA_KEY`] (the person confirmed it is them, which high-risk approvals on the
//! web home and the phone need) and [`PAIRED_LOCAL_METADATA_KEY`] (a paired local terminal),
//! read from the `RequestContext` metadata the server's extractor builds. Errors are problem
//! documents with the engine's `surface.*` code.
//!
//! A paired device's context (a device token, spec 3.3 §10 K2) is never the web home and never
//! a paired local terminal, whatever it declares: it reads as the terminal row, typed, and
//! carries its device to every audit record. `POST /surface/commands` answers it
//! `403 surface.not_here`. **Resume all** and resuming an agent are the web home's only when
//! the request comes from the home's own page — a browser's same-origin request
//! (`Sec-Fetch-Site: same-origin`) — and otherwise read as a terminal elsewhere, which may not
//! resume (`403 surface.not_here`, spec 3.3 §11 S-I10). A `decide` declaring the voice surface or origin answers
//! `400 surface.invalid`, because a spoken answer reaches the engine through the voice
//! surface; `cancelAnswer` stays open (§5.6, §7.2).

mod devices;
pub mod genui;
mod home;
mod openapi;
pub(crate) mod phone;
mod take_over;
#[cfg(feature = "verb-ui")]
pub mod verbs;
mod vocabulary;
#[cfg(feature = "website")]
pub mod website;
#[cfg(feature = "workflow")]
pub mod workflow;

use std::time::Duration;

use adk_core::{AdkError, RequestContext};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use futures::{FutureExt as _, StreamExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use devices::{device_scope_for, devices_router, router_with_devices};
#[cfg(feature = "genui-apps")]
pub use home::home_with_apps_origin;
pub use home::{HOME_PATH, home};
pub use openapi::{openapi, typescript};
pub use phone::{
    AgentConfirmOptionsRequest, AnswerRequest, ComputerPasskey, ConfirmOptions,
    ConfirmOptionsRequest, ConfirmRequest, DeliveriesResponse, Delivery, PasskeyOptions,
    PasskeyRegistration, PhoneDevice, PhoneDevicesResponse, PhoneInfo, PushKeys,
    PushReceivedRequest, PushSubscriptionRequest, QrRequest, StopRequest,
};
pub use take_over::{TakeOverRequest, take_over_router};
pub use vocabulary::words_typescript;

use crate::engine::{Command, CommandOutcome, FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::{AgentRef, AgentRow, Briefing, DecisionCard, FeedEntry, StatusLine};
use crate::origin::{Caller, Origin, SurfaceKind};

/// The largest command body: a message's images, each at its largest in base64, and room for
/// the rest of the command.
const COMMAND_BODY_MAX: usize =
    crate::images::MAX_IMAGES * (crate::images::MAX_IMAGE_BYTES / 3 * 4 + 4) + 256 * 1024;

/// The route paths, relative to the `/api` prefix (spec 3.3 §6).
pub mod paths {
    /// Every route's prefix.
    pub const PREFIX: &str = "/surface";
    /// `GET`: open decisions.
    pub const DECISIONS: &str = "/surface/decisions";
    /// `GET`: one decision.
    pub const DECISION: &str = "/surface/decisions/{id}";
    /// `GET`: a session's thread.
    pub const THREAD: &str = "/surface/sessions/{session}/thread";
    /// `GET`: a session's feed, as server-sent events.
    pub const FEED: &str = "/surface/sessions/{session}/feed";
    /// `GET`: the input queued behind a session's open decisions or its running turn.
    pub const QUEUED: &str = "/surface/sessions/{session}/queued";
    /// `POST`: the secret a person typed for a take-over decision.
    pub const TAKE_OVER: &str = "/surface/decisions/{id}/take-over";
    /// `GET`: one of the images a decision holds as evidence, its digest checked, for a viewer
    /// who may see the decision (spec 3.7 §8.2, §8.3).
    pub const EVIDENCE: &str = "/surface/decisions/{id}/evidence/{index}";
    /// `GET`: the annotated viewer of one image of a session — a digest, or the name of an
    /// image the session holds — with every answer the vision tools gave for it (spec 3.7
    /// §8.2, X12).
    pub const VISION_VIEW: &str = "/surface/vision/{session}/images/{image}/view";
    /// `GET`: one image of a session, its digest checked, for the session's owner (spec 3.7
    /// §8.3).
    pub const VISION_IMAGE: &str = "/surface/vision/{session}/images/{image}";
    /// `GET`: one answer's `RegionSet` as a file (board choice V14).
    pub const VISION_REGIONS: &str = "/surface/vision/{session}/answers/{answer}/regions";
    /// `POST`: save a labelled example from an answer (board choice V13).
    pub const VISION_EXAMPLES: &str = "/surface/vision/{session}/examples";
    /// `GET`: every camera the caller may see, as a grid (spec 3.7 §8.2, board choice C8).
    pub const CAMERAS: &str = "/surface/cameras";
    /// `GET`: one camera, watched large (board choice C1).
    pub const CAMERA: &str = "/surface/cameras/{camera}";
    /// `GET`: a frame the hub holds in memory, its digest checked; never cached.
    pub const CAMERA_FRAME: &str = "/surface/cameras/{camera}/frames/{frame}";
    /// `GET`: a camera's picture after each frame, as server-sent events.
    pub const CAMERA_FEED: &str = "/surface/cameras/{camera}/feed";
    /// `POST`: one frame this screen's camera captured, for a camera that takes them.
    pub const CAMERA_FRAMES: &str = "/surface/cameras/{camera}/frames";
    /// `POST`: switch a camera on (board choice C2).
    pub const CAMERA_ON: &str = "/surface/cameras/{camera}/on";
    /// `POST`: switch a camera off: its frames are dropped on the server from then on.
    pub const CAMERA_OFF: &str = "/surface/cameras/{camera}/off";
    /// `POST`: end a camera's stream.
    pub const CAMERA_STOP: &str = "/surface/cameras/{camera}/stop";
    /// `POST`: try a lost camera again (board choice C6).
    pub const CAMERA_RETRY: &str = "/surface/cameras/{camera}/retry";
    /// `PUT`: change a zone (board choice C12).
    pub const CAMERA_ZONE: &str = "/surface/cameras/{camera}/zones/{zone}";
    /// `POST`: freeze the frame on screen for a card; held in memory, never written.
    pub const CAMERA_FREEZE: &str = "/surface/cameras/{camera}/freeze";
    /// `POST`: raise a card from a frozen frame (board choice C11).
    pub const CAMERA_CARDS: &str = "/surface/cameras/{camera}/cards";
    /// `GET`: the roster.
    pub const AGENTS: &str = "/surface/agents";
    /// `GET`: one agent a person added (spec 3.3 §5.7).
    pub const AGENT: &str = "/surface/agents/{id}";
    /// `GET`: how the caller answered since a time (spec 3.3 §5.1 "How you answered").
    pub const ANSWERS: &str = "/surface/answers";
    /// `GET`: an agent's threads, newest first (spec 3.3 §5.1 "Thread history").
    pub const AGENT_THREADS: &str = "/surface/agents/{id}/threads";
    /// `GET`: what a person may choose for an agent (spec 3.3 §5.7).
    pub const AGENT_CHOICES: &str = "/surface/agents/choices";
    /// `POST`: **Here is what Finance can do** — the preview of a change (spec 3.3 §5.7).
    pub const AGENT_PREVIEW: &str = "/surface/agents/preview";
    /// `GET`: the status line.
    pub const STATUS: &str = "/surface/status";
    /// `GET`: "While you were away".
    pub const BRIEFING: &str = "/surface/briefing";
    /// `POST`: a command.
    pub const COMMANDS: &str = "/surface/commands";
    /// `GET`: the OpenAPI document.
    pub const OPENAPI: &str = "/surface/openapi.json";
    /// `POST`: a paired device's answer (spec 3.3 §10 K2).
    pub const ANSWER: &str = "/surface/decisions/{id}/answer";
    /// `POST`: a paired device's Stop all (spec 3.3 §11 S-I10).
    pub const STOP: &str = "/surface/stop";
    /// `POST`: starts "Confirm it's you" for one answer (spec 3.3 §8.2).
    pub const CONFIRM_OPTIONS: &str = "/surface/decisions/{id}/confirm/options";
    /// `POST`: the passkey's assertion for the bound answer (spec 3.3 §8.2).
    pub const CONFIRM: &str = "/surface/decisions/{id}/confirm";
    /// `POST`: starts "Confirm it's you" for one change that adds an agent or lets one do more
    /// at high risk (spec 3.3 §7.2).
    pub const AGENT_CONFIRM_OPTIONS: &str = "/surface/agents/confirm/options";
    /// `POST`: the passkey's assertion for the bound agent change, then made with presence.
    pub const AGENT_CONFIRM: &str = "/surface/agents/confirm";
    /// `GET`: what the phone needs to subscribe and to confirm it is you (spec 3.3 §5.4).
    pub const PHONE: &str = "/surface/phone";
    /// `PUT`, `DELETE`: the phone's push subscription.
    pub const PHONE_PUSH: &str = "/surface/phone/push";
    /// `POST`: the phone received a wake-up.
    pub const PHONE_PUSH_RECEIVED: &str = "/surface/phone/push/received";
    /// `GET`: the latest acknowledged wake-ups, for the operator.
    pub const PHONE_PUSH_DELIVERIES: &str = "/surface/phone/push/deliveries";
    /// `POST`: starts setting up a passkey.
    pub const PHONE_PASSKEY_OPTIONS: &str = "/surface/phone/passkey/options";
    /// `POST`: finishes setting up a passkey.
    pub const PHONE_PASSKEY: &str = "/surface/phone/passkey";
    /// `DELETE`: removes a computer passkey of the caller.
    pub const PHONE_PASSKEY_ONE: &str = "/surface/phone/passkey/{id}";
    /// `POST`: a pairing link as a QR code, for the operator.
    pub const PHONE_QR: &str = "/surface/phone/qr";
    /// `GET`: the paired devices' passkeys and push, for the operator.
    pub const PHONE_DEVICES: &str = "/surface/phone/devices";
}

/// `RequestContext` metadata key the server's extractor sets to `confirmed` when the person
/// confirmed it is them for this request, by passkey or device check (spec 3.3 §7.2, §8.2).
pub const PRESENCE_METADATA_KEY: &str = "surface.presence";

/// `RequestContext` metadata key set to `true` for a paired local terminal (spec 3.3 §7.2,
/// §11 S-I10).
pub const PAIRED_LOCAL_METADATA_KEY: &str = "surface.pairedLocal";

/// The engine's routes, relative to `/api` (spec 3.3 §6).
///
/// **Stop all** and **Resume all** act on every run of the engine's source: build the engine
/// with [`stops_need`](crate::SurfaceEngineBuilder::stops_need) so only the operator gives them
/// (spec 3.3 §11 S-I10).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).stops_need("operator").build();
/// let builder = builder.add_api_routes(adk_surface::web::router(engine));
/// # let _ = builder;
/// # }
/// ```
pub fn router(engine: SurfaceEngine) -> Router {
    Router::new()
        .route(paths::DECISIONS, get(decisions))
        .route(paths::DECISION, get(decision))
        .route(paths::EVIDENCE, get(evidence))
        .route(paths::VISION_VIEW, get(vision_view))
        .route(paths::VISION_IMAGE, get(vision_image))
        .route(paths::VISION_REGIONS, get(vision_regions))
        .route(paths::VISION_EXAMPLES, post(vision_example))
        .route(paths::CAMERAS, get(cameras))
        .route(paths::CAMERA, get(camera))
        .route(paths::CAMERA_FRAME, get(camera_frame))
        .route(paths::CAMERA_FEED, get(camera_feed))
        .route(
            paths::CAMERA_FRAMES,
            post(camera_frames).layer(DefaultBodyLimit::max(crate::camera::MAX_SENT_FRAME_BYTES)),
        )
        .route(paths::CAMERA_ON, post(camera_on))
        .route(paths::CAMERA_OFF, post(camera_off))
        .route(paths::CAMERA_STOP, post(camera_stop))
        .route(paths::CAMERA_RETRY, post(camera_retry))
        .route(paths::CAMERA_ZONE, put(camera_zone))
        .route(paths::CAMERA_FREEZE, post(camera_freeze))
        .route(paths::CAMERA_CARDS, post(camera_card))
        .route(paths::THREAD, get(thread))
        .route(paths::FEED, get(feed))
        .route(paths::QUEUED, get(queued))
        .route(paths::AGENTS, get(agents))
        .route(paths::AGENT_CHOICES, get(agent_choices))
        .route(paths::AGENT_PREVIEW, post(agent_preview))
        .route(paths::AGENT, get(agent))
        .route(paths::AGENT_THREADS, get(agent_threads))
        .route(paths::ANSWERS, get(answers))
        .route(paths::STATUS, get(status))
        .route(paths::BRIEFING, get(briefing))
        // A message may carry images (spec 3.3 §5.1): room for each at its largest, base64.
        .route(paths::COMMANDS, post(command).layer(DefaultBodyLimit::max(COMMAND_BODY_MAX)))
        .route(paths::OPENAPI, get(openapi_document))
        .with_state(engine)
}

/// `GET /surface/decisions` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionsResponse {
    /// The open decisions, oldest first.
    pub decisions: Vec<DecisionCard>,
}

/// `GET /surface/sessions/{session}/thread` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResponse {
    /// The session.
    pub session_id: String,
    /// Every event of the session store, numbered by position from 1.
    pub entries: Vec<FeedEntry>,
}

/// `GET /surface/sessions/{session}/queued` answer (spec 3.3 §7.4 rule 2, S-I4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueuedResponse {
    /// The session.
    pub session_id: String,
    /// What the caller typed while a decision of the session was open or a turn ran, oldest
    /// first; each is sent once the session is free, or leaves the list when the running turn
    /// takes it.
    pub queued: Vec<String>,
}

/// `GET /surface/agents` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentsResponse {
    /// The caller's agents.
    pub agents: Vec<AgentRow>,
    /// The agent a message goes to when the person names none: the tray's "Ask Finance"
    /// (spec 3.3 §5.5). Absent when the source has no single primary agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<AgentRef>,
    /// The roster ends with **New agent**: the source keeps agents a person adds and the
    /// caller may add one here (spec 3.3 §5.7). Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_add: bool,
    /// The agents the person removed, whose threads stay readable through "Removed agents"
    /// at the roster's foot (spec 3.3 §5.7). Absent when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<crate::agents::RemovedAgent>,
}

/// `POST /surface/commands` body: the command, the surface it was given on and how.
///
/// # Example
///
/// ```rust
/// use adk_surface::web::CommandRequest;
///
/// let request: CommandRequest = serde_json::from_value(serde_json::json!({
///     "surface": "phone",
///     "origin": { "kind": "touch" },
///     "command": { "type": "stopAll" },
/// }))
/// .unwrap();
/// assert_eq!(request.surface, adk_surface::SurfaceKind::Phone);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRequest {
    /// The surface; defaults to the web home.
    #[serde(default = "web")]
    pub surface: SurfaceKind,
    /// How the input was given.
    pub origin: Origin,
    /// The command.
    pub command: Command,
}

fn web() -> SurfaceKind {
    SurfaceKind::Web
}

/// Query of the reads: the surface the answers are computed for.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceQuery {
    /// The surface; defaults to the web home.
    #[serde(default)]
    pub surface: Option<SurfaceKind>,
    /// `GET /surface/status`: the session whose mode to show; `GET /surface/decisions`: only
    /// that session's decisions.
    #[serde(default)]
    pub session: Option<String>,
    /// `GET /surface/briefing` and `GET /surface/answers`: since when.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// `GET .../feed`: resume after this `seq` when no `Last-Event-ID` is sent.
    #[serde(default)]
    pub after: Option<u64>,
    /// `GET /surface/decisions`: also the decisions answered or ended since then, newest
    /// first, after the open ones (spec 3.3 §5.4).
    #[serde(default)]
    pub decided_since: Option<DateTime<Utc>>,
}

pub(crate) type Context = Option<Extension<Option<RequestContext>>>;

/// No authenticated caller: the route answers `401`.
pub(crate) struct Unauthenticated;

pub(crate) fn unauthenticated() -> Response {
    problem(&AdkError::new(
        adk_core::ErrorComponent::Server,
        adk_core::ErrorCategory::Unauthorized,
        "surface.unauthenticated",
        "the surface API needs an authenticated caller; mount it with \
         ServerBuilder::add_api_routes behind a RequestContextExtractor",
    ))
}

/// The paired device a context authenticated as, when it carries a device token's metadata
/// (spec 3.3 §10 K2).
pub(crate) fn device_of(context: &RequestContext) -> Option<&String> {
    context.metadata.get(adk_kernel::pairing::DEVICE_ID_METADATA)
}

/// The kind of a paired device's context — `phone`, `terminal` or `tray` — which the issuer
/// of its pairing code set; `None` for a device paired without one, a remote terminal.
pub(crate) fn device_kind(context: &RequestContext) -> Option<&str> {
    context.metadata.get(adk_kernel::pairing::DEVICE_KIND_METADATA).map(String::as_str)
}

/// The caller from the server's authentication; the client chooses only the surface and the
/// origin, which can narrow what it may decide and never widen it.
///
/// A device token's context is never the web home and never a paired local terminal, whatever
/// the query or body declares: its kind, which the issuer of its pairing code set, picks the
/// row — a phone by touch, a tray by pointer, anything else a terminal by keyboard — and the
/// caller carries its device (spec 3.3 §7.2, §11 S-I10). Its presence comes only from a route
/// that checked it, never from metadata.
pub(crate) fn caller(
    context: Context,
    surface: SurfaceKind,
    origin: Option<Origin>,
) -> Result<Caller, Unauthenticated> {
    let Some(Extension(Some(context))) = context else {
        return Err(Unauthenticated);
    };
    if let Some(device) = device_of(&context) {
        let (surface, origin) = match device_kind(&context) {
            Some("phone") => (SurfaceKind::Phone, Origin::Touch),
            Some("tray") => (SurfaceKind::Tray, Origin::Pointer),
            _ => (SurfaceKind::Terminal, Origin::Keyboard),
        };
        return Ok(Caller::human(context.user_id.clone(), surface, origin)
            .with_groups(context.scopes.clone())
            .with_device(device.clone()));
    }
    let origin = origin.unwrap_or(match surface {
        SurfaceKind::Web | SurfaceKind::Tray => Origin::Pointer,
        SurfaceKind::Phone => Origin::Touch,
        SurfaceKind::Terminal => Origin::Keyboard,
        SurfaceKind::Voice => Origin::Voice,
        SurfaceKind::Chat => Origin::Chat { channel: "chat".to_string() },
    });
    let mut caller =
        Caller::human(context.user_id.clone(), surface, origin).with_groups(context.scopes.clone());
    if context.metadata.get(PRESENCE_METADATA_KEY).map(String::as_str) == Some("confirmed") {
        caller = caller.with_presence_confirmed();
    }
    if context.metadata.get(PAIRED_LOCAL_METADATA_KEY).map(String::as_str) == Some("true") {
        caller = caller.with_paired_local();
    }
    Ok(caller)
}

pub(crate) fn problem(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_problem_json())).into_response()
}

pub(crate) fn failed(error: SurfaceError) -> Response {
    problem(&AdkError::from(error))
}

macro_rules! try_or_respond {
    ($result:expr) => {
        match $result {
            Ok(value) => value,
            Err(Unauthenticated) => return unauthenticated(),
        }
    };
}

async fn decisions(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    let decisions = match &query.session {
        Some(session) => engine.session_decisions(&caller, session).await,
        None => engine.decisions(&caller).await,
    };
    let decisions = match (decisions, query.decided_since) {
        (Ok(mut open), Some(since)) => engine.decided_since(&caller, since).await.map(|decided| {
            open.extend(decided);
            open
        }),
        (decisions, _) => decisions,
    };
    match decisions {
        Ok(decisions) => Json(DecisionsResponse { decisions }).into_response(),
        Err(error) => failed(error),
    }
}

async fn decision(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(id): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.decision(&caller, &id).await {
        Ok(card) => Json(card).into_response(),
        Err(error) => failed(error),
    }
}

/// An image a decision holds as evidence, for a viewer who may see the decision: the exact
/// bytes whose digest the card shows, never cached and never sniffed (spec 3.7 §8.2).
async fn evidence(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((id, index)): Path<(String, usize)>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.evidence_image(&caller, &id, index).await {
        Ok(image) => image_response(image),
        Err(error) => failed(error),
    }
}

/// `GET /surface/vision/{session}/images/{image}/view` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewerQuery {
    /// The surface asking; defaults to the web home.
    #[serde(default)]
    pub surface: Option<SurfaceKind>,
    /// The answer to show, by its call; the newest otherwise.
    #[serde(default)]
    pub answer: Option<String>,
    /// A second answer to compare with it.
    #[serde(default)]
    pub compare: Option<String>,
}

/// `GET /surface/vision/{session}/answers/{answer}/regions` query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionsQuery {
    /// The surface asking; defaults to the web home.
    #[serde(default)]
    pub surface: Option<SurfaceKind>,
    /// A second answer: both `RegionSet`s, in an array (board choice V14).
    #[serde(default)]
    pub with: Option<String>,
}

/// The annotated viewer of one image (spec 3.7 §8.2, X12), for the session's owner.
async fn vision_view(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((session, image)): Path<(String, String)>,
    Query(query): Query<ViewerQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine
        .viewer(&caller, &session, &image, query.answer.as_deref(), query.compare.as_deref())
        .await
    {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// One image of a session, for the session's owner: the exact bytes whose digest the viewer
/// shows, never cached and never sniffed (spec 3.7 §8.3).
async fn vision_image(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((session, image)): Path<(String, String)>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.viewer_image(&caller, &session, &image).await {
        Ok(image) => image_response(image),
        Err(error) => failed(error),
    }
}

/// An image's bytes with the headers every evidence and viewer image is served with.
fn image_response(image: crate::evidence::EvidenceBytes) -> Response {
    let mime = match image.mime_type.as_str() {
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" => image.mime_type,
        _ => "application/octet-stream".to_string(),
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CACHE_CONTROL, "private, no-store".to_string()),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        image.bytes,
    )
        .into_response()
}

/// One answer's `RegionSet` as a file to save (board choice V14).
async fn vision_regions(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((session, answer)): Path<(String, String)>,
    Query(query): Query<RegionsQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.viewer_export(&caller, &session, &answer, query.with.as_deref()).await {
        Ok(set) => {
            let safe: String = answer
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect();
            let name = if query.with.is_some() {
                format!("{safe}-compared.regions.json")
            } else {
                format!("{safe}.regions.json")
            };
            (
                [
                    (axum::http::header::CACHE_CONTROL, "private, no-store".to_string()),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{name}\""),
                    ),
                ],
                Json(set),
            )
                .into_response()
        }
        Err(error) => failed(error),
    }
}

/// Saves a labelled example from an answer (board choice V13). A paired device reads the
/// viewer and saves nothing: the phone is read-only (board choice V15).
async fn vision_example(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(session): Path<String>,
    Json(example): Json<crate::viewer::LabelledExample>,
) -> Response {
    if let Some(Extension(Some(context))) = &context
        && device_of(context).is_some()
    {
        return failed(SurfaceError::NotHere(
            "a phone shows the viewer read-only; save examples on a computer".into(),
        ));
    }
    let caller = try_or_respond!(caller(context, SurfaceKind::Web, None));
    match engine.save_example(&caller, &session, &example).await {
        Ok(saved) => Json(saved).into_response(),
        Err(error) => failed(error),
    }
}

/// Every camera the caller may see (spec 3.7 §8.2, board choice C8).
async fn cameras(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.cameras_view(&caller).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// One camera, watched large (board choice C1).
async fn camera(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.camera_view(&caller, &camera).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// A frame the hub holds in memory: the exact bytes whose digest the picture names, never cached
/// and never sniffed. A camera that is off holds none.
async fn camera_frame(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((camera, frame)): Path<(String, String)>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.camera_frame(&caller, &camera, &frame).await {
        Ok(image) => image_response(image),
        Err(error) => failed(error),
    }
}

/// A camera's picture after each frame (`frame`, a [`StageView`](crate::camera::StageView)) and
/// a note when anything else changed (`changed`), as server-sent events.
async fn camera_feed(
    State(engine): State<SurfaceEngine>,
    context: Context,
    ends: Option<Extension<devices::DeviceEnds>>,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    // Refuse a camera the caller may not see before the stream starts (S-I8).
    if let Err(error) = engine.camera_view(&caller, &camera).await {
        return failed(error);
    }
    let Some(hub) = engine.camera_hub() else {
        return failed(SurfaceError::unsupported("the live camera surface"));
    };
    let device_ends = match (ends, caller.device.clone()) {
        (Some(Extension(devices::DeviceEnds(pairing))), Some(device)) => {
            pairing.until_device_ends(device).boxed()
        }
        _ => futures::future::pending().boxed(),
    };
    let updates = camera_updates(hub.subscribe());
    let frames = updates
        .filter(move |update| futures::future::ready(update.camera() == camera))
        .then(move |update| {
            let engine = engine.clone();
            let caller = caller.clone();
            async move {
                let event = match update {
                    crate::camera::CameraUpdate::Frame { camera, .. } => {
                        match engine.camera_stage_after_check(&caller, &camera).await {
                            Ok(stage) => SseEvent::default().event("frame").json_data(&stage),
                            Err(error) => SseEvent::default()
                                .event("error")
                                .json_data(AdkError::from(error).to_problem_json()),
                        }
                    }
                    crate::camera::CameraUpdate::Changed { .. } => {
                        SseEvent::default().event("changed").json_data(serde_json::json!({}))
                    }
                };
                Ok::<_, std::convert::Infallible>(event.unwrap_or_else(|_| {
                    SseEvent::default().comment("a frame could not be encoded")
                }))
            }
        })
        .take_until(device_ends);
    Sse::new(frames).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

/// Every update of the hub, a slow reader skipping what it missed.
fn camera_updates(
    updates: tokio::sync::broadcast::Receiver<crate::camera::CameraUpdate>,
) -> impl futures::Stream<Item = crate::camera::CameraUpdate> + Send {
    futures::stream::unfold(updates, |mut updates| async move {
        loop {
            match updates.recv().await {
                Ok(update) => return Some((update, updates)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
}

/// One frame this screen's camera captured, its type from `Content-Type`: handed to the camera's
/// pipeline through the server's gate, or dropped with the reason while the camera is off.
async fn camera_frames(
    State(engine): State<SurfaceEngine>,
    context: Context,
    headers: HeaderMap,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
    body: axum::body::Bytes,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    let mime_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match engine.send_camera_frame(&caller, &camera, mime_type, body.to_vec()).await {
        Ok(taken) => Json(taken).into_response(),
        Err(error) => failed(error),
    }
}

/// Switches a camera on.
async fn camera_on(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.switch_camera(&caller, &camera, true).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// Switches a camera off: the server drops its frames from then on (spec 3.7 §8.2).
async fn camera_off(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.switch_camera(&caller, &camera, false).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// Ends a camera's stream.
async fn camera_stop(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.stop_camera(&caller, &camera).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// Tries a lost camera again now.
async fn camera_retry(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.retry_camera(&caller, &camera).await {
        Ok(view) => Json(view).into_response(),
        Err(error) => failed(error),
    }
}

/// Changes a zone; closed to paired devices, which open the zone in the web home.
async fn camera_zone(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path((camera, zone)): Path<(String, String)>,
    Json(change): Json<crate::camera::ZoneChange>,
) -> Response {
    let caller = try_or_respond!(caller(context, SurfaceKind::Web, None));
    match engine.save_camera_zone(&caller, &camera, &zone, &change).await {
        Ok(saved) => Json(saved).into_response(),
        Err(error) => failed(error),
    }
}

/// Freezes the frame on screen for a card: held in memory while the person writes it.
async fn camera_freeze(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
    Json(freeze): Json<crate::camera::FreezeFrame>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.freeze_camera_frame(&caller, &camera, &freeze.frame).await {
        Ok(stage) => Json(stage).into_response(),
        Err(error) => failed(error),
    }
}

/// Raises a card from a frozen frame: that frame alone is kept as evidence.
async fn camera_card(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(camera): Path<String>,
    Query(query): Query<SurfaceQuery>,
    Json(card): Json<crate::camera::RaiseCard>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.raise_camera_card(&caller, &camera, &card).await {
        Ok(raised) => Json(raised).into_response(),
        Err(error) => failed(error),
    }
}

async fn thread(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(session): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.thread(&caller, &session).await {
        Ok(entries) => Json(ThreadResponse { session_id: session, entries }).into_response(),
        Err(error) => failed(error),
    }
}

async fn feed(
    State(engine): State<SurfaceEngine>,
    context: Context,
    ends: Option<Extension<devices::DeviceEnds>>,
    headers: HeaderMap,
    Path(session): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    let after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or(query.after);
    // Refuse a session the caller does not own before the stream starts (S-I8).
    if let Err(error) = engine.thread(&caller, &session).await {
        return failed(error);
    }
    // A paired device's feed ends with the device: removed, or its pairing over (S-I7).
    let device_ends = match (ends, caller.device.clone()) {
        (Some(Extension(devices::DeviceEnds(pairing))), Some(device)) => {
            pairing.until_device_ends(device).boxed()
        }
        _ => futures::future::pending().boxed(),
    };
    let frames = engine.feed(caller, session, after).take_until(device_ends).map(|frame| {
        let event = match frame {
            Ok(FeedFrame::Entry { entry }) => {
                SseEvent::default().id(entry.seq.to_string()).event("entry").json_data(&entry)
            }
            Ok(FeedFrame::Decisions { cards }) => {
                SseEvent::default().event("decisions").json_data(&cards)
            }
            Ok(FeedFrame::Reset { len }) => {
                SseEvent::default().event("reset").json_data(serde_json::json!({ "len": len }))
            }
            // No `id`: a changed entry is not where a reconnect resumes.
            Ok(FeedFrame::Changed { entry }) => {
                SseEvent::default().event("changed").json_data(&entry)
            }
            Err(error) => SseEvent::default()
                .event("error")
                .json_data(AdkError::from(error).to_problem_json()),
        };
        Ok::<_, std::convert::Infallible>(
            event.unwrap_or_else(|_| SseEvent::default().comment("a frame could not be encoded")),
        )
    });
    Sse::new(frames).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))).into_response()
}

async fn queued(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(session): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    // The queue is kept per caller, so a caller only ever reads their own.
    let queued = engine.queued(&caller, &session);
    Json(QueuedResponse { session_id: session, queued }).into_response()
}

async fn agents(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.agents(&caller).await {
        Ok(agents) => Json(AgentsResponse {
            agents,
            primary: engine.primary_agent(),
            can_add: engine.can_add_agents(&caller),
            removed: engine.removed_agents(&caller).await.unwrap_or_default(),
        })
        .into_response(),
        Err(error) => failed(error),
    }
}

async fn agent(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(id): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.agent(&caller, &id).await {
        Ok(detail) => Json::<crate::agents::AgentDetail>(detail).into_response(),
        Err(error) => failed(error),
    }
}

async fn agent_threads(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Path(id): Path<String>,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.agent_threads(&caller, &id).await {
        Ok(threads) => Json::<crate::agents::AgentThreads>(threads).into_response(),
        Err(error) => failed(error),
    }
}

async fn agent_choices(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.agent_choices(&caller).await {
        Ok(choices) => Json::<crate::agents::AgentChoices>(choices).into_response(),
        Err(error) => failed(error),
    }
}

async fn agent_preview(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
    Json(request): Json<crate::agents::AgentPreviewRequest>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.preview_agent(&caller, request).await {
        Ok(preview) => Json::<crate::agents::AgentPreview>(preview).into_response(),
        Err(error) => failed(error),
    }
}

async fn status(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    match engine.status(&caller, query.session.as_deref()).await {
        Ok(line) => Json::<StatusLine>(line).into_response(),
        Err(error) => failed(error),
    }
}

async fn answers(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    let since = query.since.unwrap_or_else(|| Utc::now() - chrono::Duration::days(7));
    match engine.answers_since(&caller, since).await {
        Ok(summary) => Json::<crate::answers::AnswerSummary>(summary).into_response(),
        Err(error) => failed(error),
    }
}

async fn briefing(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let caller = try_or_respond!(caller(context, query.surface.unwrap_or(SurfaceKind::Web), None));
    let since = query.since.unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24));
    match engine.briefing(&caller, since).await {
        Ok(briefing) => Json::<Briefing>(briefing).into_response(),
        Err(error) => failed(error),
    }
}

/// Whether `headers` are those of the web home's own page: a browser's same-origin request.
/// Browsers set `Sec-Fetch-Site` themselves and page scripts cannot change it, so another
/// client sends it only by imitating a browser on purpose.
fn from_home_page(headers: &HeaderMap) -> bool {
    headers.get("sec-fetch-site").and_then(|value| value.to_str().ok()) == Some("same-origin")
}

async fn command(
    State(engine): State<SurfaceEngine>,
    context: Context,
    headers: HeaderMap,
    Json(request): Json<CommandRequest>,
) -> Response {
    // A device has its own routes, each opened by its token's scopes; commands would let it
    // send, change the mode or resume (spec 3.3 §10 K2, §11 S-I10).
    if let Some(Extension(Some(context))) = &context
        && device_of(context).is_some()
    {
        return failed(SurfaceError::NotHere(
            "a paired device answers and stops through its own routes, not commands".into(),
        ));
    }
    // Chat and agent input reach the engine in process, from their adapters; over HTTP a
    // client could otherwise put a channel it never used on the record (spec 3.3 §11).
    if matches!(request.origin, Origin::Chat { .. } | Origin::Agent { .. })
        || matches!(request.surface, SurfaceKind::Chat)
    {
        return failed(SurfaceError::Invalid(
            "chat and agent input reach the engine through their adapters, not this API".into(),
        ));
    }
    // A spoken answer comes from the voice surface's own transcript, never a declared one;
    // cancelling a spoken answer stays open to every surface (spec 3.3 §5.6, §7.2).
    if matches!(request.command, Command::Decide { .. })
        && (request.surface == SurfaceKind::Voice || request.origin == Origin::Voice)
    {
        return failed(SurfaceError::Invalid(
            "a spoken answer reaches the engine through the voice surface, not this API".into(),
        ));
    }
    // Resuming happens only in the web home or on a paired local terminal (spec 3.3 S-I10),
    // and a declared surface never shows the web home: a resume is the home's only from its
    // own page; from any other client it is a terminal's elsewhere, which may not resume.
    let surface = match request.command {
        Command::ResumeAll | Command::ResumeAgent { .. } if !from_home_page(&headers) => {
            SurfaceKind::Terminal
        }
        _ => request.surface,
    };
    let caller = try_or_respond!(caller(context, surface, Some(request.origin)));
    match engine.execute(&caller, request.command).await {
        Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
        Err(error) => failed(error),
    }
}

async fn openapi_document() -> Response {
    Json(openapi()).into_response()
}
