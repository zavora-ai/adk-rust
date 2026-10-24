//! The web home (spec 3.3 §5.1, §6; feature `web`): its HTTP API and its page.
//!
//! [`home`] serves the page — the React app built from `adk-surface/web`, embedded — at
//! `/ui/` as root routes (§10 S1); mount it with `ServerBuilder::add_root_routes`.
//!
//! [`router`] serves the engine under `/surface`, relative to the `/api` prefix: mount it
//! with `ServerBuilder::add_api_routes`, beside the kernel's control-plane routes, so the
//! server's auth middleware puts the caller's `RequestContext` in every request. Every route
//! needs that caller (`401` without one) and reads or acts only on the caller's own sessions
//! and decisions (spec 3.3 §11 S-I8): anything else answers `404`.
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/decisions` | [`DecisionsResponse`]: the caller's open decisions as cards; `?session=` for one session's |
//! | `GET /surface/decisions/{id}` | [`DecisionCard`]: waiting, or decided with who, where and when |
//! | `GET /surface/sessions/{session}/thread` | [`ThreadResponse`]: the session's entries, numbered by store position |
//! | `GET /surface/sessions/{session}/feed` | Server-sent events: `entry` (with `id` = `seq`), `decisions`, `reset`; resumes after `Last-Event-ID` or `?after=` |
//! | `GET /surface/sessions/{session}/queued` | [`QueuedResponse`]: what the caller typed while a decision of the session was open, sent once it is answered |
//! | `GET /surface/agents` | [`AgentsResponse`] |
//! | `GET /surface/status` | [`StatusLine`]; `?session=` for that session's mode |
//! | `GET /surface/briefing` | [`Briefing`] since `?since=` (default: the last 24 hours) |
//! | `POST /surface/commands` | [`CommandRequest`] → [`CommandOutcome`] |
//! | `GET /surface/openapi.json` | The OpenAPI 3.1 document of these routes |
//! | `POST /surface/decisions/{id}/take-over` | [`TakeOverRequest`] → [`CommandOutcome`]; mounted by [`take_over_router`] beside a harness that takes over |
//!
//! Reads take `?surface=` (`web`, `terminal`, `phone`, `tray`, `chat`, `voice`) so a card
//! lists the answers that surface may give (§7.2). A declared surface or origin can only
//! narrow what a caller may decide. What widens it is set by the server, never the client:
//! [`PRESENCE_METADATA_KEY`] (the person confirmed it is them, which high-risk approvals on the
//! web home and the phone need) and [`PAIRED_LOCAL_METADATA_KEY`] (a paired local terminal),
//! read from the `RequestContext` metadata the server's extractor builds. Errors are problem
//! documents with the engine's `surface.*` code.

mod home;
mod openapi;
mod take_over;
mod vocabulary;

use std::time::Duration;

use adk_core::{AdkError, RequestContext};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use home::{HOME_PATH, home};
pub use openapi::{openapi, typescript};
pub use take_over::{TakeOverRequest, take_over_router};
pub use vocabulary::words_typescript;

use crate::engine::{Command, CommandOutcome, FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::{AgentRow, Briefing, DecisionCard, FeedEntry, StatusLine};
use crate::origin::{Caller, Origin, SurfaceKind};

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
    /// `GET`: the input queued behind a session's open decisions.
    pub const QUEUED: &str = "/surface/sessions/{session}/queued";
    /// `POST`: the secret a person typed for a take-over decision.
    pub const TAKE_OVER: &str = "/surface/decisions/{id}/take-over";
    /// `GET`: the roster.
    pub const AGENTS: &str = "/surface/agents";
    /// `GET`: the status line.
    pub const STATUS: &str = "/surface/status";
    /// `GET`: "While you were away".
    pub const BRIEFING: &str = "/surface/briefing";
    /// `POST`: a command.
    pub const COMMANDS: &str = "/surface/commands";
    /// `GET`: the OpenAPI document.
    pub const OPENAPI: &str = "/surface/openapi.json";
}

/// `RequestContext` metadata key the server's extractor sets to `confirmed` when the person
/// confirmed it is them for this request, by passkey or device check (spec 3.3 §7.2, §8.2).
pub const PRESENCE_METADATA_KEY: &str = "surface.presence";

/// `RequestContext` metadata key set to `true` for a paired local terminal (spec 3.3 §7.2,
/// §11 S-I10).
pub const PAIRED_LOCAL_METADATA_KEY: &str = "surface.pairedLocal";

/// The engine's routes, relative to `/api` (spec 3.3 §6).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// let builder = builder.add_api_routes(adk_surface::web::router(engine));
/// # let _ = builder;
/// # }
/// ```
pub fn router(engine: SurfaceEngine) -> Router {
    Router::new()
        .route(paths::DECISIONS, get(decisions))
        .route(paths::DECISION, get(decision))
        .route(paths::THREAD, get(thread))
        .route(paths::FEED, get(feed))
        .route(paths::QUEUED, get(queued))
        .route(paths::AGENTS, get(agents))
        .route(paths::STATUS, get(status))
        .route(paths::BRIEFING, get(briefing))
        .route(paths::COMMANDS, post(command))
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
    /// What the caller typed while a decision of the session was open, oldest first; each is
    /// sent once the session's decisions are answered.
    pub queued: Vec<String>,
}

/// `GET /surface/agents` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentsResponse {
    /// The caller's agents.
    pub agents: Vec<AgentRow>,
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
    /// `GET /surface/briefing`: since when.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// `GET .../feed`: resume after this `seq` when no `Last-Event-ID` is sent.
    #[serde(default)]
    pub after: Option<u64>,
}

type Context = Option<Extension<Option<RequestContext>>>;

/// No authenticated caller: the route answers `401`.
struct Unauthenticated;

fn unauthenticated() -> Response {
    problem(&AdkError::new(
        adk_core::ErrorComponent::Server,
        adk_core::ErrorCategory::Unauthorized,
        "surface.unauthenticated",
        "the surface API needs an authenticated caller; mount it with \
         ServerBuilder::add_api_routes behind a RequestContextExtractor",
    ))
}

/// The caller from the server's authentication; the client chooses only the surface and the
/// origin, which can narrow what it may decide and never widen it.
fn caller(
    context: Context,
    surface: SurfaceKind,
    origin: Option<Origin>,
) -> Result<Caller, Unauthenticated> {
    let Some(Extension(Some(context))) = context else {
        return Err(Unauthenticated);
    };
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

fn problem(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_problem_json())).into_response()
}

fn failed(error: SurfaceError) -> Response {
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
    let frames = engine.feed(caller, session, after).map(|frame| {
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
        Ok(agents) => Json(AgentsResponse { agents }).into_response(),
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

async fn command(
    State(engine): State<SurfaceEngine>,
    context: Context,
    Json(request): Json<CommandRequest>,
) -> Response {
    // Chat and agent input reach the engine in process, from their adapters; over HTTP a
    // client could otherwise put a channel it never used on the record (spec 3.3 §11).
    if matches!(request.origin, Origin::Chat { .. } | Origin::Agent { .. })
        || matches!(request.surface, SurfaceKind::Chat)
    {
        return failed(SurfaceError::Invalid(
            "chat and agent input reach the engine through their adapters, not this API".into(),
        ));
    }
    let caller = try_or_respond!(caller(context, request.surface, Some(request.origin)));
    match engine.execute(&caller, request.command).await {
        Ok(outcome) => Json::<CommandOutcome>(outcome).into_response(),
        Err(error) => failed(error),
    }
}

async fn openapi_document() -> Response {
    Json(openapi()).into_response()
}
