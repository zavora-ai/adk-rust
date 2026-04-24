//! AWP route registration for Axum.

use axum::Router;
use axum::middleware::from_fn;
use axum::routing::{delete, get, post};

use crate::handlers;
use crate::middleware::version_negotiation;
use crate::state::AwpState;

/// Build an Axum [`Router`] with all AWP protocol endpoints.
///
/// Registers the following routes:
/// - `GET  /.well-known/awp.json` — discovery document
/// - `GET  /awp/manifest` — capability manifest
/// - `GET  /awp/health` — health state
/// - `POST /awp/events/subscribe` — create event subscription
/// - `GET  /awp/events/subscriptions` — list subscriptions
/// - `DELETE /awp/events/subscriptions/{id}` — delete subscription
/// - `POST /awp/a2a` — A2A message handler
///
/// Version negotiation middleware is applied to all routes.
pub fn awp_routes(state: AwpState) -> Router {
    Router::new()
        .route("/.well-known/awp.json", get(handlers::discovery))
        .route("/awp/manifest", get(handlers::manifest))
        .route("/awp/health", get(handlers::health))
        .route("/awp/events/subscribe", post(handlers::subscribe))
        .route("/awp/events/subscriptions", get(handlers::list_subscriptions))
        .route("/awp/events/subscriptions/{id}", delete(handlers::delete_subscription))
        .route("/awp/a2a", post(handlers::a2a_message))
        .layer(from_fn(version_negotiation))
        .with_state(state)
}
