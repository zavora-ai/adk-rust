//! The screen actions' routes on the web home (spec 3.3 §2 item 7, §6.1; features `verb-ui`
//! and `web`), with their own OpenAPI document, so the engine's document stays the same in
//! every `web` build.
//!
//! | Route (under `/api`) | Answer |
//! |---|---|
//! | `GET /surface/verbs` | [`VerbsResponse`]: the catalogue and the caller's Coming up with the times each item may move to |
//! | `POST /surface/verbs/call` | [`VerbCallRequest`] → [`VerbDone`](crate::verbs::VerbDone), once it ran |
//! | `GET /surface/verbs/openapi.json` | This API's OpenAPI document |
//!
//! Every route needs the server's authenticated caller (`401` without one) and acts only on the
//! caller's own items (`404` for anyone else's, S-I8). A declared voice or agent origin, or
//! the voice or chat surface, answers `400`: spoken actions come through the voice surface's
//! own transcript, and agents ask through their tools. A paired device's token reads as its
//! row: a phone may snooze; a terminal paired from another machine gets `403` (owner question
//! 27).

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Context, SurfaceQuery, Unauthenticated, caller, device_of, problem, unauthenticated};
use crate::origin::{Origin, SurfaceKind};
use crate::verbs::{ComingUpItem, VerbError, VerbGate, VerbIntent, VerbView, words};

/// The route paths, relative to the `/api` prefix.
pub mod paths {
    /// `GET`: the catalogue and the caller's Coming up.
    pub const VERBS: &str = "/surface/verbs";
    /// `POST`: takes an action.
    pub const CALL: &str = "/surface/verbs/call";
    /// `GET`: the OpenAPI document.
    pub const OPENAPI: &str = "/surface/verbs/openapi.json";
}

/// `GET /surface/verbs` answer (spec 3.3 §2 item 7).
///
/// # Example
///
/// ```rust
/// use adk_surface::web::verbs::VerbsResponse;
///
/// let answer: VerbsResponse = serde_json::from_value(serde_json::json!({ "verbs": [], "comingUp": [] })).unwrap();
/// assert!(answer.coming_up.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerbsResponse {
    /// The actions this home offers.
    pub verbs: Vec<VerbView>,
    /// The caller's Coming up, soonest first, with the times each item may move to.
    pub coming_up: Vec<ComingUpItem>,
}

/// `POST /surface/verbs/call` body: the action, and the surface it was given on and how. The
/// caller is the server's; a declared surface or origin only says where the person was.
///
/// # Example
///
/// ```rust
/// use adk_surface::web::verbs::VerbCallRequest;
///
/// let request: VerbCallRequest = serde_json::from_value(serde_json::json!({
///     "origin": { "kind": "pointer" },
///     "intent": { "verb": "snooze", "objects": [{ "kind": "comingUp", "id": "agenda-7f3" }],
///                 "args": { "to": "2026-10-26T19:00:00+03:00" } },
/// }))
/// .unwrap();
/// assert_eq!(request.surface, adk_surface::SurfaceKind::Web);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerbCallRequest {
    /// The surface; defaults to the web home.
    #[serde(default = "web")]
    pub surface: SurfaceKind,
    /// How the input was given.
    pub origin: Origin,
    /// The action.
    pub intent: VerbIntent,
}

fn web() -> SurfaceKind {
    SurfaceKind::Web
}

/// The screen actions' routes, relative to `/api` (spec 3.3 §2 item 7): mount with
/// `ServerBuilder::add_api_routes` beside [`router`](super::router).
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::verbs::VerbGate;
///
/// # fn example(gate: VerbGate, builder: adk_server::ServerBuilder) {
/// let builder = builder.add_api_routes(adk_surface::web::verbs::router(gate));
/// # let _ = builder;
/// # }
/// ```
pub fn router(gate: VerbGate) -> Router {
    Router::new()
        .route(paths::VERBS, get(verbs))
        .route(paths::CALL, post(call))
        .route(paths::OPENAPI, get(openapi_document))
        .with_state(gate)
}

fn failed(error: VerbError) -> Response {
    problem(&adk_core::AdkError::from(error))
}

/// A terminal paired from another machine cannot take screen actions (owner question 27).
fn remote_terminal(context: &Context, surface: SurfaceKind) -> bool {
    match context {
        Some(Extension(Some(context))) => {
            device_of(context).is_some()
                && super::device_kind(context) != Some("phone")
                && super::device_kind(context) != Some("tray")
                && surface != SurfaceKind::Chat
        }
        _ => false,
    }
}

async fn verbs(
    State(gate): State<VerbGate>,
    context: Context,
    Query(query): Query<SurfaceQuery>,
) -> Response {
    let surface = query.surface.unwrap_or(SurfaceKind::Web);
    let views = gate.registry().views();
    if remote_terminal(&context, surface) {
        return Json(VerbsResponse { verbs: views, coming_up: Vec::new() }).into_response();
    }
    let caller = match caller(context, surface, None) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return unauthenticated(),
    };
    match gate.coming_up(&caller).await {
        Ok(coming_up) => Json(VerbsResponse { verbs: views, coming_up }).into_response(),
        // Chat and the tray list nothing to act on.
        Err(VerbError::NotHere(_)) => {
            Json(VerbsResponse { verbs: views, coming_up: Vec::new() }).into_response()
        }
        Err(error) => failed(error),
    }
}

async fn call(
    State(gate): State<VerbGate>,
    context: Context,
    Json(request): Json<VerbCallRequest>,
) -> Response {
    // Spoken actions come from the voice surface's own transcript, agents' through their
    // tools, chat's never (spec 3.3 §5.6, §7.2).
    if matches!(request.origin, Origin::Voice | Origin::Agent { .. } | Origin::Chat { .. })
        || matches!(request.surface, SurfaceKind::Voice | SurfaceKind::Chat)
    {
        return failed(VerbError::invalid(
            "a spoken action reaches the home through the voice surface, an agent's through its \
             own tool, and chat takes none: this API takes a person's click, tap or key",
        ));
    }
    if remote_terminal(&context, request.surface) {
        return failed(VerbError::NotHere(words::NOT_FROM_A_REMOTE_TERMINAL.to_string()));
    }
    let caller = match caller(context, request.surface, Some(request.origin)) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return unauthenticated(),
    };
    match gate.call(&caller, request.intent).await {
        Ok(done) => Json(done).into_response(),
        Err(error) => failed(error),
    }
}

async fn openapi_document() -> Response {
    Json(super::openapi::verbs::openapi()).into_response()
}

/// The OpenAPI 3.1 document of [`router`], its schemas generated from the Rust types
/// (spec 3.3 §6).
///
/// # Example
///
/// ```rust
/// let document = adk_surface::web::verbs::openapi();
/// assert!(document["components"]["schemas"]["ComingUpItem"].is_object());
/// ```
pub fn openapi() -> serde_json::Value {
    super::openapi::verbs::openapi()
}

/// TypeScript declarations of every schema in [`openapi`], for the web home's screen actions.
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::web::verbs::typescript().contains("export interface VerbsResponse {"));
/// ```
pub fn typescript() -> String {
    super::openapi::verbs::typescript()
}
