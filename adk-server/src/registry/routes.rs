//! Axum route handlers for the Agent Registry REST API.
//!
//! Every request authenticates through the [`RequestContextExtractor`] given to
//! [`registry_router`] — the trait `ServerConfig::with_request_context` installs on the
//! server, so one extractor can serve both. A card belongs to the caller who registered it
//! ([`AgentCard::owner`]). Any authenticated caller lists and reads every card, since
//! discovery is the registry's purpose; only the owner updates or deletes one.
//!
//! # Routes
//!
//! Paths are relative to where the router is nested; the examples nest it under `/api`.
//!
//! | Method | Path | Checks | Answers |
//! |--------|------|--------|---------|
//! | POST | `/api/agents` | Authenticated caller; the name is not registered | `201`, the caller owns the card; `409` when the name is registered, by anyone |
//! | GET | `/api/agents` | Authenticated caller | `200`, every matching card |
//! | GET | `/api/agents/{name}` | Authenticated caller | `200`; `404` |
//! | PUT | `/api/agents/{name}` | Authenticated caller who owns the card | `200`, the stored card; `403` for another user's card or a card without an owner; `404` |
//! | DELETE | `/api/agents/{name}` | Authenticated caller who owns the card | `204`; `403` for another user's card or a card without an owner; `404` |
//!
//! A request the extractor refuses answers `401` (`MissingAuth`, `InvalidToken`, or a
//! context whose `user_id` is empty) or `500` (`ExtractionFailed`).
//! The routes set `owner` from the caller and ignore one sent in a body. A card without an
//! owner — stored before cards had owners, or inserted through the store directly — stays
//! readable and changes only through the [`AgentRegistryStore`].
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::RequestContext;
//! use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
//! use adk_server::registry::routes::registry_router;
//! use adk_server::registry::InMemoryAgentRegistryStore;
//!
//! struct BearerUser;
//!
//! #[async_trait::async_trait]
//! impl RequestContextExtractor for BearerUser {
//!     async fn extract(
//!         &self,
//!         parts: &axum::http::request::Parts,
//!     ) -> Result<RequestContext, RequestContextError> {
//!         let token = parts
//!             .headers
//!             .get("authorization")
//!             .and_then(|value| value.to_str().ok())
//!             .and_then(|value| value.strip_prefix("Bearer "))
//!             .ok_or(RequestContextError::MissingAuth)?;
//!         // Validate the token with your identity provider here.
//!         let user_id = match token {
//!             "alice-token" => "alice",
//!             _ => return Err(RequestContextError::InvalidToken("unknown token".into())),
//!         };
//!         Ok(RequestContext {
//!             user_id: user_id.to_string(),
//!             scopes: vec![],
//!             metadata: Default::default(),
//!         })
//!     }
//! }
//!
//! let store = Arc::new(InMemoryAgentRegistryStore::new());
//! let app = axum::Router::new().nest("/api", registry_router(store, Arc::new(BearerUser)));
//! ```

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::store::{AgentFilter, AgentRegistryStore, OwnerWrite};
use super::types::AgentCard;
use crate::auth_bridge::{RequestContextError, RequestContextExtractor};

/// Shared state for registry route handlers.
#[derive(Clone)]
struct RegistryState {
    store: Arc<dyn AgentRegistryStore>,
    extractor: Arc<dyn RequestContextExtractor>,
}

/// JSON error response body.
#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(ErrorResponse { error: message.into() })).into_response()
}

fn store_failure(action: &str, error: &adk_core::AdkError) -> Response {
    tracing::error!(error = %error, "registry store error {action}");
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal store error")
}

/// The user ID of the caller the registry's extractor authenticated.
struct Caller(String);

impl FromRequestParts<RegistryState> for Caller {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &RegistryState,
    ) -> Result<Self, Self::Rejection> {
        let reason = match state.extractor.extract(parts).await {
            // An empty user ID would make every such caller the owner of every such card.
            Ok(context) if !context.user_id.is_empty() => return Ok(Self(context.user_id)),
            Ok(_) => "the request context names no user".to_string(),
            Err(RequestContextError::MissingAuth) => "missing authorization".to_string(),
            Err(RequestContextError::InvalidToken(message)) => format!("invalid token: {message}"),
            Err(RequestContextError::ExtractionFailed(message)) => {
                tracing::warn!(error = %message, "registry auth extraction failed");
                return Err(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "auth extraction failed",
                ));
            }
        };
        tracing::debug!(reason = %reason, "registry request refused");
        Err(error_response(StatusCode::UNAUTHORIZED, reason))
    }
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

/// `POST /api/agents` — Register a new agent card owned by the caller.
///
/// Validates the payload, refuses a registered name (409) whoever owns it, inserts the card
/// with the caller as its owner, and returns 201 with the agent's name and version.
async fn create_agent(
    State(state): State<RegistryState>,
    Caller(caller): Caller,
    Json(mut card): Json<AgentCard>,
) -> Response {
    if card.name.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "agent name must not be empty");
    }
    if card.version.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "agent version must not be empty");
    }

    match state.store.get(&card.name).await {
        Ok(Some(existing)) => {
            let message = if existing.owner.as_deref() == Some(caller.as_str()) {
                format!("agent '{}' is already registered; update it with PUT", card.name)
            } else {
                format!("agent '{}' is registered and the caller does not own it", card.name)
            };
            return error_response(StatusCode::CONFLICT, message);
        }
        Ok(None) => {}
        Err(e) => return store_failure("checking existence", &e),
    }

    card.owner = Some(caller.clone());
    let name = card.name.clone();
    let version = card.version.clone();

    match state.store.insert(card).await {
        Ok(()) => {
            info!(agent.name = %name, agent.version = %version, agent.owner = %caller, "agent registered");
            (StatusCode::CREATED, Json(CreateAgentResponse { name, version })).into_response()
        }
        Err(e) => {
            // The store rejects a name registered since the check above.
            tracing::warn!(error = %e, "registry insert failed");
            error_response(StatusCode::CONFLICT, format!("agent already exists: {e}"))
        }
    }
}

/// `GET /api/agents` — List registered agents with optional filters.
///
/// Supports query parameters: `namePrefix`, `tag`, `versionRange`.
/// Returns 200 with a JSON array of matching agent cards, whoever owns them.
async fn list_agents(
    State(state): State<RegistryState>,
    _caller: Caller,
    Query(query): Query<ListAgentsQuery>,
) -> Response {
    let filter = AgentFilter {
        name_prefix: query.name_prefix,
        tag: query.tag,
        version_range: query.version_range,
    };

    match state.store.list(&filter).await {
        Ok(cards) => (StatusCode::OK, Json(cards)).into_response(),
        Err(e) => store_failure("listing agents", &e),
    }
}

/// `GET /api/agents/{name}` — Retrieve a single agent card by name.
///
/// Returns 200 with the agent card, whoever owns it, or 404 if not found.
async fn get_agent(
    State(state): State<RegistryState>,
    _caller: Caller,
    Path(name): Path<String>,
) -> Response {
    match state.store.get(&name).await {
        Ok(Some(card)) => (StatusCode::OK, Json(card)).into_response(),
        Ok(None) => not_found(&name),
        Err(e) => store_failure("getting an agent", &e),
    }
}

fn not_found(name: &str) -> Response {
    error_response(StatusCode::NOT_FOUND, format!("agent '{name}' not found"))
}

fn not_owner(name: &str, action: &str) -> Response {
    error_response(
        StatusCode::FORBIDDEN,
        format!("only the owner of agent '{name}' can {action} it"),
    )
}

/// `PUT /api/agents/{name}` — Replace a card the caller owns.
///
/// The body is the whole card; its `name` must match the path. The stored card keeps its
/// `createdAt` and owner and gets a new `updatedAt`. Returns 200 with the stored card, 403
/// when the caller does not own the card, or 404 if not found.
async fn update_agent(
    State(state): State<RegistryState>,
    Caller(caller): Caller,
    Path(name): Path<String>,
    Json(mut card): Json<AgentCard>,
) -> Response {
    if card.name != name {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("the card names '{}' but the path names '{name}'", card.name),
        );
    }
    if card.version.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "agent version must not be empty");
    }

    let existing = match state.store.get(&name).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found(&name),
        Err(e) => return store_failure("getting an agent", &e),
    };
    if existing.owner.as_deref() != Some(caller.as_str()) {
        return not_owner(&name, "update");
    }

    card.owner = Some(caller.clone());
    card.created_at = existing.created_at;
    card.updated_at = Some(chrono::Utc::now().to_rfc3339());

    match state.store.replace_owned(card.clone(), &caller).await {
        Ok(OwnerWrite::Applied) => {
            info!(agent.name = %name, agent.version = %card.version, agent.owner = %caller, "agent updated");
            (StatusCode::OK, Json(card)).into_response()
        }
        Ok(OwnerWrite::NotFound) => not_found(&name),
        Ok(OwnerWrite::NotOwner) => not_owner(&name, "update"),
        Err(e) => store_failure("updating an agent", &e),
    }
}

/// `DELETE /api/agents/{name}` — Remove a card the caller owns.
///
/// Returns 204 on success, 403 when the caller does not own the card, or 404 if the agent
/// was not found.
async fn delete_agent(
    State(state): State<RegistryState>,
    Caller(caller): Caller,
    Path(name): Path<String>,
) -> Response {
    match state.store.delete_owned(&name, &caller).await {
        Ok(OwnerWrite::Applied) => {
            info!(agent.name = %name, agent.owner = %caller, "agent removed from registry");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(OwnerWrite::NotFound) => not_found(&name),
        Ok(OwnerWrite::NotOwner) => not_owner(&name, "delete"),
        Err(e) => store_failure("deleting an agent", &e),
    }
}

/// Build an Axum [`Router`] with all Agent Registry routes.
///
/// `extractor` authenticates every request; pass the extractor given to
/// `ServerConfig::with_request_context` to share identities with the rest of the server.
///
/// # Routes
///
/// - `POST /agents` — register a new agent card, owned by the caller
/// - `GET /agents` — list agents with optional filters
/// - `GET /agents/{name}` — get a single agent card
/// - `PUT /agents/{name}` — replace a card the caller owns
/// - `DELETE /agents/{name}` — remove a card the caller owns
///
/// A request the extractor refuses answers 401; see the [module docs](self) for each
/// route's checks.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_server::auth_bridge::RequestContextExtractor;
/// use adk_server::registry::routes::registry_router;
/// use adk_server::registry::InMemoryAgentRegistryStore;
///
/// fn mount(extractor: Arc<dyn RequestContextExtractor>) -> axum::Router {
///     let store = Arc::new(InMemoryAgentRegistryStore::new());
///     axum::Router::new().nest("/api", registry_router(store, extractor))
/// }
/// ```
pub fn registry_router(
    store: Arc<dyn AgentRegistryStore>,
    extractor: Arc<dyn RequestContextExtractor>,
) -> Router {
    let state = RegistryState { store, extractor };

    Router::new()
        .route("/agents", post(create_agent).get(list_agents))
        .route("/agents/{name}", get(get_agent).put(update_agent).delete(delete_agent))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_bridge::RequestContext;
    use crate::registry::InMemoryAgentRegistryStore;
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// `Bearer test-token` authenticates as `alice`; any other value is an invalid token.
    struct TestTokens;

    #[async_trait::async_trait]
    impl RequestContextExtractor for TestTokens {
        async fn extract(&self, parts: &Parts) -> Result<RequestContext, RequestContextError> {
            match parts.headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()) {
                None => Err(RequestContextError::MissingAuth),
                Some("Bearer test-token") => Ok(RequestContext {
                    user_id: "alice".to_string(),
                    scopes: vec![],
                    metadata: Default::default(),
                }),
                Some(_) => Err(RequestContextError::InvalidToken("unknown token".to_string())),
            }
        }
    }

    fn test_store() -> Arc<dyn AgentRegistryStore> {
        Arc::new(InMemoryAgentRegistryStore::new())
    }

    fn test_app(store: Arc<dyn AgentRegistryStore>) -> Router {
        registry_router(store, Arc::new(TestTokens))
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
            owner: Some("alice".to_string()),
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

    #[tokio::test]
    async fn test_update_agent_returns_401_without_auth() {
        let app = test_app(test_store());
        let card = make_card("test", "2.0.0");

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/agents/test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&card).unwrap()))
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
        assert_eq!(json["owner"], "alice");
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
