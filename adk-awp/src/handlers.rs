//! Axum route handlers for AWP endpoints.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::discovery::generate_discovery_document;
use crate::error_response::AwpErrorResponse;
use crate::events::EventSubscription;
use crate::manifest::build_manifest;
use crate::state::AwpState;

/// GET `/.well-known/awp.json` — serve the AWP discovery document.
pub async fn discovery(State(state): State<AwpState>) -> impl IntoResponse {
    let ctx = state.business_context.load();
    let doc = generate_discovery_document(&ctx);
    Json(doc)
}

/// GET `/awp/manifest` — serve the capability manifest.
pub async fn manifest(State(state): State<AwpState>) -> impl IntoResponse {
    let ctx = state.business_context.load();
    let m = build_manifest(&ctx);
    Json(m)
}

/// GET `/awp/health` — return the current health state snapshot.
pub async fn health(State(state): State<AwpState>) -> impl IntoResponse {
    let snapshot = state.health.snapshot().await;
    Json(snapshot)
}

/// Request body for creating an event subscription.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSubscriptionRequest {
    pub subscriber: String,
    pub callback_url: String,
    pub event_types: Vec<String>,
    pub secret: String,
}

/// POST `/awp/events/subscribe` — create a new event subscription.
pub async fn subscribe(
    State(state): State<AwpState>,
    Json(body): Json<CreateSubscriptionRequest>,
) -> Response {
    let subscription = EventSubscription {
        id: Uuid::now_v7(),
        subscriber: body.subscriber,
        callback_url: body.callback_url,
        event_types: body.event_types,
        secret: body.secret,
    };

    match state.event_service.create(subscription.clone()).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => AwpErrorResponse(e).into_response(),
    }
}

/// GET `/awp/events/subscriptions` — list all event subscriptions.
pub async fn list_subscriptions(State(state): State<AwpState>) -> Response {
    match state.event_service.list().await {
        Ok(subs) => Json(subs).into_response(),
        Err(e) => AwpErrorResponse(e).into_response(),
    }
}

/// DELETE `/awp/events/subscriptions/{id}` — delete a subscription by ID.
pub async fn delete_subscription(State(state): State<AwpState>, Path(id): Path<Uuid>) -> Response {
    match state.event_service.delete(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => AwpErrorResponse(e).into_response(),
    }
}

/// POST `/awp/a2a` — handle an A2A message.
///
/// Currently returns a placeholder acknowledgment. Full A2A routing will be
/// integrated with `adk-server`'s A2A handler in a future task.
pub async fn a2a_message(
    State(_state): State<AwpState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "acknowledged",
            "messageId": id,
        })),
    )
        .into_response()
}
