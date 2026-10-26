//! "Take over, then Done" on the web home and the phone (spec 3.3 §8.2, §10 H2): the secret
//! a person types reaches the one tool call that asked for it, through the harness's
//! [`TakeOver`], never the inbox, the engine's records or a log line.

use std::fmt;

use adk_core::CredentialSecret;
use adk_harness::take_over::{TAKE_OVER_TOOL, TakeOver, TakeOverAnswer, TakeOverSurface};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Context, Unauthenticated, caller, failed, paths, unauthenticated};
use crate::engine::{CommandOutcome, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::CardStatus;
use crate::origin::{Origin, SurfaceKind};

/// `POST /surface/decisions/{id}/take-over` body: the secret typed on a surface, for the card
/// at the version that surface showed (S-I2). Its `Debug` output redacts the secret.
///
/// # Example
///
/// ```rust
/// use adk_surface::web::TakeOverRequest;
///
/// let request: TakeOverRequest = serde_json::from_value(serde_json::json!({
///     "version": 1,
///     "secret": "492817",
/// }))
/// .unwrap();
/// assert_eq!(request.surface, adk_surface::SurfaceKind::Web);
/// assert!(!format!("{request:?}").contains("492817"));
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TakeOverRequest {
    /// The surface: the web home (default) or the phone, the two that take over (§8.2).
    #[serde(default = "web")]
    pub surface: SurfaceKind,
    /// The card's `version`.
    pub version: u64,
    /// What the person typed.
    pub secret: String,
}

impl fmt::Debug for TakeOverRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TakeOverRequest")
            .field("surface", &self.surface)
            .field("version", &self.version)
            .field("secret", &"[redacted]")
            .finish()
    }
}

fn web() -> SurfaceKind {
    SurfaceKind::Web
}

#[derive(Clone)]
struct TakeOverState {
    engine: SurfaceEngine,
    take_over: TakeOver,
}

/// `POST /surface/decisions/{id}/take-over`, relative to `/api` (spec 3.3 §8.2, §10 H2): the
/// harness's `take_over` delivers what the caller typed to the call that asked, deciding the
/// card at the version sent. Mount it with `ServerBuilder::add_api_routes` beside
/// [`router`](super::router), over the same engine. The caller must own the decision (S-I8);
/// an answer given first is `alreadyDecided`, never retried (S-I2).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_harness::take_over::TakeOver;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, take_over: TakeOver, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// let builder = builder
///     .add_api_routes(adk_surface::web::router(engine.clone()))
///     .add_api_routes(adk_surface::web::take_over_router(engine, take_over));
/// # let _ = builder;
/// # }
/// ```
pub fn take_over_router(engine: SurfaceEngine, take_over: TakeOver) -> Router {
    Router::new()
        .route(paths::TAKE_OVER, post(take_over_route))
        .with_state(TakeOverState { engine, take_over })
}

async fn take_over_route(
    State(state): State<TakeOverState>,
    context: Context,
    Path(id): Path<String>,
    Json(request): Json<TakeOverRequest>,
) -> Response {
    let origin = match request.surface {
        SurfaceKind::Web => Origin::Pointer,
        SurfaceKind::Phone => Origin::Touch,
        other => {
            return failed(SurfaceError::Invalid(format!(
                "a secret is typed on the web home or the phone, not the {other:?} surface"
            )));
        }
    };
    let caller = match caller(context, request.surface, Some(origin)) {
        Ok(caller) => caller,
        Err(Unauthenticated) => return unauthenticated(),
    };
    // A paired device takes over as its kind's surface, whatever the body says, and only a
    // phone may (spec 3.3 §8.2): the record names where the secret was typed.
    let surface = match caller.surface {
        SurfaceKind::Web => TakeOverSurface::Web,
        SurfaceKind::Phone => TakeOverSurface::Phone,
        SurfaceKind::Terminal | SurfaceKind::Tray | SurfaceKind::Voice | SurfaceKind::Chat => {
            return failed(SurfaceError::NotHere(
                "a secret is typed on the web home or a paired phone: take over in the home".into(),
            ));
        }
    };
    // The caller's own decision, and a take over (S-I8).
    let card = match state.engine.decision(&caller, &id).await {
        Ok(card) => card,
        Err(error) => return failed(error),
    };
    if card.call.tool != TAKE_OVER_TOOL {
        return failed(SurfaceError::Invalid(format!(
            "decision '{id}' does not ask for a secret; answer it with a decide command"
        )));
    }
    let answer =
        TakeOverAnswer::new(caller.subject.clone(), surface, CredentialSecret::new(request.secret));
    let provided = state.take_over.provide(&id, request.version, answer).await;
    let card = match state.engine.decision(&caller, &id).await {
        Ok(card) => card,
        Err(error) => return failed(error),
    };
    let outcome = match provided {
        Ok(_) => {
            state.engine.deliver_queued(&caller, &card.session_id);
            CommandOutcome::Answered { card }
        }
        Err(error)
            if matches!(error.code, "approval.version_conflict" | "approval.already_resolved") =>
        {
            match (card.status, card.decided.clone()) {
                (CardStatus::Decided, Some(decided)) => {
                    CommandOutcome::AlreadyDecided { decided, card }
                }
                _ => CommandOutcome::Changed { card },
            }
        }
        Err(error) => return failed(SurfaceError::Source(error)),
    };
    Json(outcome).into_response()
}
