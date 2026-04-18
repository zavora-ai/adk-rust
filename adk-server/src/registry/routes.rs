//! Axum route handlers for the Agent Registry REST API.
//!
//! Provides CRUD operations for agent cards with authentication
//! and conflict detection.
//!
//! # Routes
//!
//! | Method | Path | Handler | Auth |
//! |--------|------|---------|------|
//! | POST | `/api/agents` | [`create_agent`] | Required |
//! | GET | `/api/agents` | [`list_agents`] | Required |
//! | GET | `/api/agents/{name}` | [`get_agent`] | Required |
//! | DELETE | `/api/agents/{name}` | [`delete_agent`] | Required |
//!
//! All routes return JSON with appropriate HTTP status codes
//! (201, 200, 204, 404, 409, 401).
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_server::registry::routes::registry_router;
//! use adk_server::registry::InMemoryAgentRegistryStore;
//! use std::sync::Arc;
//!
//! let store = Arc::new(InMemoryAgentRegistryStore::new());
//! let router = registry_router(store);
//! ```

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::store::{AgentFilter, AgentRegistryStore};
use super::types::AgentCard;

/// Shared state for registry route handlers.
#[derive(Clone)]
struct RegistryState {
    store: Arc<dyn AgentRegistryStore>,
}

/// JSON error response body.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Query parameters for the `GET /api/agents` endpoint.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListAgentsQuery {
    /// Filter agents whose name starts with this prefix.
    #[serde(default)]
    pub name_prefix: Option<String>,
    /// Filter agents that contain this tag.
    #[serde(default)]
    pub tag: Option<String>,
    /// Filter agents by version range (reserved for future use).
    #[serde(default)]
    pub version_range: Option<String>,
}

/// Response body for a successfully created agent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateAgentResponse {
    name: String,
    version: String,
}

/// Check that the request has an `Authorization` header.
/// Returns `Err` with a 401 response if the header is missing or empty.
fn require_auth(headers: &HeaderMap) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match headers.get(axum::http::header::AUTHORIZATION) {
        Some(value) if !value.is_empty() => Ok(()),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse { error: "missing or empty Authorization header".to_string() }),
        )),
    }
}

/// `POST /api/agents` — Register a new agent card.
///
/// Validates the payload, checks for name conflicts (409), inserts the card,
/// and returns 201 with the agent's name and version.
async fn create_agent(
    State(state): State<RegistryState>,
    headers: HeaderMap,
    Json(card): Json<AgentCard>,
) -> impl IntoResponse {
    if let Err(resp) = require_auth(&headers) {
        return resp.into_response();
    }

    if card.name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "agent name must not be empty".to_string() }),
        )
            .into_response();
    }

    if card.version.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: "agent version must not be empty".to_string() }),
        )
            .into_response();
    }

    // Check for conflicts before inserting.
    match state.store.exists(&card.name, &card.version).await {
        Ok(true) => {
            return (
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: format!(
                        "agent '{}' version '{}' already exists",
                        card.name, card.version
                    ),
                }),
            )
                .into_response();
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!("registry store error checking existence: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "internal store error".to_string() }),
            )
                .into_response();
        }
    }

    let name = card.name.clone();
    let version = card.version.clone();

    match state.store.insert(card).await {
        Ok(()) => {
            info!(agent.name = %name, agent.version = %version, "agent registered");
            (StatusCode::CREATED, Json(CreateAgentResponse { name, version })).into_response()
        }
        Err(e) => {
            // The store may also reject duplicates — treat as conflict.
            tracing::warn!("registry insert failed: {e}");
            (
                StatusCode::CONFLICT,
                Json(ErrorResponse { error: format!("agent already exists: {e}") }),
            )
                .into_response()
        }
    }
}

/// `GET /api/agents` — List registered agents with optional filters.
///
/// Supports query parameters: `namePrefix`, `tag`, `versionRange`.
/// Returns 200 with a JSON array of matching agent cards.
async fn list_agents(
    State(state): State<RegistryState>,
    headers: HeaderMap,
    Query(query): Query<ListAgentsQuery>,
) -> impl IntoResponse {
    if let Err(resp) = require_auth(&headers) {
        return resp.into_response();
    }

    let filter = AgentFilter {
        name_prefix: query.name_prefix,
        tag: query.tag,
        version_range: query.version_range,
    };

    match state.store.list(&filter).await {
        Ok(cards) => (StatusCode::OK, Json(cards)).into_response(),
        Err(e) => {
            tracing::error!("registry store error listing agents: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "internal store error".to_string() }),
            )
                .into_response()
        }
    }
}

/// `GET /api/agents/{name}` — Retrieve a single agent card by name.
///
/// Returns 200 with the agent card, or 404 if not found.
async fn get_agent(
    State(state): State<RegistryState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = require_auth(&headers) {
        return resp.into_response();
    }

    match state.store.get(&name).await {
        Ok(Some(card)) => (StatusCode::OK, Json(card)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse { error: format!("agent '{name}' not found") }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("registry store error getting agent '{name}': {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "internal store error".to_string() }),
            )
                .into_response()
        }
    }
}

/// `DELETE /api/agents/{name}` — Remove an agent card by name.
///
/// Returns 204 on success, or 404 if the agent was not found.
async fn delete_agent(
    State(state): State<RegistryState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(resp) = require_auth(&headers) {
        return resp.into_response();
    }

    match state.store.delete(&name).await {
        Ok(true) => {
            info!(agent.name = %name, "agent removed from registry");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse { error: format!("agent '{name}' not found") }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("registry store error deleting agent '{name}': {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: "internal store error".to_string() }),
            )
                .into_response()
        }
    }
}

/// Build an Axum [`Router`] with all Agent Registry routes.
///
/// The store is passed as shared state to all handlers.
///
/// # Routes
///
/// - `POST /agents` — register a new agent card
/// - `GET /agents` — list agents with optional filters
/// - `GET /agents/{name}` — get a single agent card
/// - `DELETE /agents/{name}` — remove an agent card
///
/// All routes require an `Authorization` header (returns 401 if missing).
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::registry::routes::registry_router;
/// use adk_server::registry::InMemoryAgentRegistryStore;
/// use std::sync::Arc;
///
/// let store = Arc::new(InMemoryAgentRegistryStore::new());
/// let app = axum::Router::new().nest("/api", registry_router(store));
/// ```
pub fn registry_router(store: Arc<dyn AgentRegistryStore>) -> Router {
    let state = RegistryState { store };

    Router::new()
        .route("/agents", post(create_agent).get(list_agents))
        .route("/agents/{name}", get(get_agent).delete(delete_agent))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::InMemoryAgentRegistryStore;
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_store() -> Arc<dyn AgentRegistryStore> {
        Arc::new(InMemoryAgentRegistryStore::new())
    }

    fn test_app(store: Arc<dyn AgentRegistryStore>) -> Router {
        registry_router(store)
    }

    fn make_card(name: &str, version: &str) -> AgentCard {
        AgentCard {
            name: name.to_string(),
            version: version.to_string(),
            description: Some("test agent".to_string()),
            tags: vec!["test".to_string()],
            endpoint_url: None,
            capabilities: vec![],
            input_modes: vec![],
            output_modes: vec![],
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: None,
        }
    }

    async fn body_json(body: Body) -> serde_json::Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // --- Authentication tests ---

    #[tokio::test]
    async fn test_create_agent_returns_401_without_auth() {
        let app = test_app(test_store());
        let card = make_card("agent-a", "1.0.0");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&card).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_list_agents_returns_401_without_auth() {
        let app = test_app(test_store());

        let response = app
            .oneshot(Request::builder().uri("/agents").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_agent_returns_401_without_auth() {
        let app = test_app(test_store());

        let response = app
            .oneshot(Request::builder().uri("/agents/test").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_delete_agent_returns_401_without_auth() {
        let app = test_app(test_store());

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // --- CRUD tests ---

    #[tokio::test]
    async fn test_create_agent_returns_201() {
        let store = test_store();
        let app = test_app(store);
        let card = make_card("agent-a", "1.0.0");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&card).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let json = body_json(response.into_body()).await;
        assert_eq!(json["name"], "agent-a");
        assert_eq!(json["version"], "1.0.0");
    }

    #[tokio::test]
    async fn test_create_agent_conflict_returns_409() {
        let store = test_store();
        let card = make_card("agent-a", "1.0.0");

        // Insert directly into the store first.
        store.insert(card.clone()).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&card).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_list_agents_returns_200() {
        let store = test_store();
        store.insert(make_card("agent-a", "1.0.0")).await.unwrap();
        store.insert(make_card("agent-b", "2.0.0")).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response.into_body()).await;
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_agents_with_name_prefix_filter() {
        let store = test_store();
        store.insert(make_card("search-agent", "1.0.0")).await.unwrap();
        store.insert(make_card("search-bot", "1.0.0")).await.unwrap();
        store.insert(make_card("qa-agent", "1.0.0")).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents?namePrefix=search-")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response.into_body()).await;
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_list_agents_with_tag_filter() {
        let store = test_store();
        let mut card_a = make_card("agent-a", "1.0.0");
        card_a.tags = vec!["search".to_string(), "qa".to_string()];
        let mut card_b = make_card("agent-b", "1.0.0");
        card_b.tags = vec!["chat".to_string()];

        store.insert(card_a).await.unwrap();
        store.insert(card_b).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents?tag=search")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response.into_body()).await;
        assert_eq!(json.as_array().unwrap().len(), 1);
        assert_eq!(json[0]["name"], "agent-a");
    }

    #[tokio::test]
    async fn test_get_agent_returns_200() {
        let store = test_store();
        store.insert(make_card("agent-a", "1.0.0")).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents/agent-a")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response.into_body()).await;
        assert_eq!(json["name"], "agent-a");
        assert_eq!(json["version"], "1.0.0");
    }

    #[tokio::test]
    async fn test_get_agent_returns_404_when_not_found() {
        let app = test_app(test_store());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents/nonexistent")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_agent_returns_204() {
        let store = test_store();
        store.insert(make_card("agent-a", "1.0.0")).await.unwrap();

        let app = test_app(store);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/agent-a")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_delete_agent_returns_404_when_not_found() {
        let app = test_app(test_store());

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/nonexistent")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_then_get_round_trip() {
        let store = test_store();
        let card = make_card("round-trip", "1.0.0");

        // Create
        let app = test_app(store.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::from(serde_json::to_string(&card).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Get
        let app = test_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents/round-trip")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response.into_body()).await;
        assert_eq!(json["name"], "round-trip");
        assert_eq!(json["version"], "1.0.0");
        assert_eq!(json["description"], "test agent");
    }

    #[tokio::test]
    async fn test_create_then_delete_then_get_returns_404() {
        let store = test_store();
        store.insert(make_card("ephemeral", "1.0.0")).await.unwrap();

        // Delete
        let app = test_app(store.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/ephemeral")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Get should now 404
        let app = test_app(store);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/agents/ephemeral")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
