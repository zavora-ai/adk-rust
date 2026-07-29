//! AWP route registration for Axum.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{delete, get, post};

use crate::handlers;
use crate::middleware::{enforce_rate_limit, version_negotiation};
use crate::state::AwpState;

/// Build an Axum [`Router`] with the publicly reachable AWP endpoints.
///
/// - `GET  /.well-known/awp.json` — discovery document
/// - `GET  /awp/manifest` — capability manifest
/// - `GET  /awp/health` — health state
/// - `POST /awp/a2a` — A2A message handler
///
/// Discovery, manifest, and health are read-only descriptions of the service.
/// The configured [`crate::AwpA2aHandler`] remains responsible for
/// application authentication and capability authorization.
///
/// Subscription management is **not** included — see [`awp_management_routes`].
pub fn awp_public_routes(state: AwpState) -> Router {
    Router::new()
        .route("/.well-known/awp.json", get(handlers::discovery))
        .route("/awp/manifest", get(handlers::manifest))
        .route("/awp/health", get(handlers::health))
        .route("/awp/a2a", post(handlers::a2a_message).layer(DefaultBodyLimit::max(64 * 1024)))
        .layer(from_fn_with_state(state.clone(), enforce_rate_limit))
        .layer(from_fn(version_negotiation))
        .with_state(state)
}

/// Build an Axum [`Router`] with the AWP subscription-management endpoints.
///
/// - `POST   /awp/events/subscribe` — create event subscription
/// - `GET    /awp/events/subscriptions` — list subscriptions
/// - `DELETE /awp/events/subscriptions/{id}` — delete subscription
///
/// # Authentication
///
/// These routes are returned **without any authentication layer**, because this crate has no
/// authentication model of its own. Apply one before serving them:
///
/// ```rust,ignore
/// let app = Router::new()
///     .merge(awp_public_routes(state.clone()))
///     .merge(awp_management_routes(state).layer(your_auth_layer));
/// ```
///
/// They create, enumerate, and delete webhook destinations for this service. An unauthenticated
/// caller could point deliveries at a host of their choosing or remove a legitimate
/// subscriber's, which is why they are separated from the public set rather than mounted
/// alongside it.
pub fn awp_management_routes(state: AwpState) -> Router {
    Router::new()
        .route(
            "/awp/events/subscribe",
            post(handlers::subscribe).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/awp/events/subscriptions", get(handlers::list_subscriptions))
        .route("/awp/events/subscriptions/{id}", delete(handlers::delete_subscription))
        .layer(from_fn_with_state(state.clone(), enforce_rate_limit))
        .layer(from_fn(version_negotiation))
        .with_state(state)
}

/// Build the safe default AWP [`Router`].
///
/// This is an alias for [`awp_public_routes`]. Subscription management is
/// excluded because it requires an application authentication layer. Merge
/// [`awp_management_routes`] explicitly after applying that layer.
pub fn awp_routes(state: AwpState) -> Router {
    awp_public_routes(state)
}
