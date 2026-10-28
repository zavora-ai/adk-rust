//! Agent registry authentication and card ownership over HTTP.
//!
//! Every request authenticates through the router's `RequestContextExtractor`; a card belongs
//! to the caller who registered it, and only that caller updates or deletes it.

#![cfg(feature = "agent-registry")]

use std::sync::Arc;

use adk_server::auth_bridge::{RequestContext, RequestContextError, RequestContextExtractor};
use adk_server::registry::{
    AgentCard, AgentRegistryStore, InMemoryAgentRegistryStore, registry_router,
};
use axum::Router;
use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ALICE: &str = "Bearer alice-token";
const BOB: &str = "Bearer bob-token";
const DUMMY: &str = "Bearer anything-at-all";

/// A token table: `alice-token` and `bob-token` name their users, `nobody-token` yields an
/// empty user ID, `broken-token` fails extraction, and any other value is an invalid token.
struct TokenTable;

#[async_trait::async_trait]
impl RequestContextExtractor for TokenTable {
    async fn extract(&self, parts: &Parts) -> Result<RequestContext, RequestContextError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        let user_id = match token {
            "alice-token" => "alice",
            "bob-token" => "bob",
            "nobody-token" => "",
            "broken-token" => {
                return Err(RequestContextError::ExtractionFailed("key set unavailable".into()));
            }
            _ => return Err(RequestContextError::InvalidToken("unknown token".into())),
        };
        Ok(RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

fn app(store: Arc<dyn AgentRegistryStore>) -> Router {
    registry_router(store, Arc::new(TokenTable))
}

fn card(name: &str, version: &str) -> AgentCard {
    AgentCard {
        name: name.to_string(),
        version: version.to_string(),
        description: Some(format!("{name} {version}")),
        tags: vec!["support".to_string()],
        endpoint_url: Some(format!("http://127.0.0.1:8080/api/run/{name}")),
        capabilities: vec!["text".to_string()],
        input_modes: vec!["text".to_string()],
        output_modes: vec!["text".to_string()],
        created_at: "2026-10-01T00:00:00Z".to_string(),
        updated_at: None,
        owner: None,
    }
}

async fn send(
    store: &Arc<dyn AgentRegistryStore>,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app(store.clone()).oneshot(request.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn store() -> Arc<dyn AgentRegistryStore> {
    Arc::new(InMemoryAgentRegistryStore::new())
}

async fn register(store: &Arc<dyn AgentRegistryStore>, auth: &str, card: &AgentCard) {
    let (status, body) =
        send(store, "POST", "/agents", Some(auth), Some(serde_json::to_value(card).unwrap())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn a_token_the_extractor_rejects_is_refused_on_every_route() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;
    let replacement = serde_json::to_value(card("support", "6.6.6")).unwrap();

    let statuses = vec![
        send(&store, "POST", "/agents", Some(DUMMY), Some(json!(card("dummy", "1.0.0")))).await.0,
        send(&store, "GET", "/agents", Some(DUMMY), None).await.0,
        send(&store, "GET", "/agents/support", Some(DUMMY), None).await.0,
        send(&store, "PUT", "/agents/support", Some(DUMMY), Some(replacement)).await.0,
        send(&store, "DELETE", "/agents/support", Some(DUMMY), None).await.0,
    ];

    assert_eq!(statuses, vec![StatusCode::UNAUTHORIZED; 5]);
    assert_eq!(store.get("dummy").await.unwrap(), None);
    assert_eq!(
        store.get("support").await.unwrap().map(|card| card.version).as_deref(),
        Some("1.0.0")
    );
}

#[tokio::test]
async fn an_empty_user_id_is_refused() {
    let store = store();
    let (status, body) = send(
        &store,
        "POST",
        "/agents",
        Some("Bearer nobody-token"),
        Some(json!(card("x", "1.0.0"))),
    )
    .await;

    assert_eq!(
        (status, body),
        (StatusCode::UNAUTHORIZED, json!({"error": "the request context names no user"}))
    );
    assert_eq!(store.get("x").await.unwrap(), None);
}

#[tokio::test]
async fn an_extraction_failure_is_a_server_error() {
    let store = store();
    let (status, body) = send(
        &store,
        "POST",
        "/agents",
        Some("Bearer broken-token"),
        Some(json!(card("x", "1.0.0"))),
    )
    .await;

    assert_eq!(
        (status, body),
        (StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "auth extraction failed"}))
    );
    assert_eq!(store.get("x").await.unwrap(), None);
}

#[tokio::test]
async fn registering_records_the_caller_as_owner() {
    let store = store();
    let mut body = serde_json::to_value(card("support", "1.0.0")).unwrap();
    body["owner"] = json!("mallory");

    let (status, _) = send(&store, "POST", "/agents", Some(ALICE), Some(body)).await;
    let (_, stored) = send(&store, "GET", "/agents/support", Some(ALICE), None).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(stored["owner"], "alice");
}

#[tokio::test]
async fn another_user_cannot_delete_a_card() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;

    let (status, body) = send(&store, "DELETE", "/agents/support", Some(BOB), None).await;
    let (read, _) = send(&store, "GET", "/agents/support", Some(ALICE), None).await;

    assert_eq!(
        (status, body),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "only the owner of agent 'support' can delete it"})
        )
    );
    assert_eq!(read, StatusCode::OK);
}

#[tokio::test]
async fn another_user_cannot_update_a_card() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;
    let before = store.get("support").await.unwrap();

    let replacement = serde_json::to_value(card("support", "6.6.6")).unwrap();
    let (status, body) = send(&store, "PUT", "/agents/support", Some(BOB), Some(replacement)).await;

    assert_eq!(
        (status, body),
        (
            StatusCode::FORBIDDEN,
            json!({"error": "only the owner of agent 'support' can update it"})
        )
    );
    assert_eq!(store.get("support").await.unwrap(), before);
}

#[tokio::test]
async fn a_name_owned_by_another_user_is_a_conflict() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;
    let before = store.get("support").await.unwrap();

    let (status, _) =
        send(&store, "POST", "/agents", Some(BOB), Some(json!(card("support", "2.0.0")))).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(store.get("support").await.unwrap(), before);
}

#[tokio::test]
async fn listing_and_reading_are_open_to_authenticated_callers() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;

    let (listed, cards) = send(&store, "GET", "/agents", Some(BOB), None).await;
    let (read, card) = send(&store, "GET", "/agents/support", Some(BOB), None).await;

    assert_eq!((listed, cards.as_array().map(Vec::len)), (StatusCode::OK, Some(1)));
    assert_eq!((read, card["name"].clone()), (StatusCode::OK, json!("support")));
}

#[tokio::test]
async fn the_owner_updates_and_deletes_a_card() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;

    let mut replacement = serde_json::to_value(card("support", "1.1.0")).unwrap();
    replacement["createdAt"] = json!("1999-01-01T00:00:00Z");
    let (updated, body) =
        send(&store, "PUT", "/agents/support", Some(ALICE), Some(replacement)).await;
    let stored = serde_json::to_value(store.get("support").await.unwrap()).unwrap();

    assert_eq!(updated, StatusCode::OK);
    assert_eq!(body, stored);
    assert_eq!(
        (&stored["version"], &stored["createdAt"], &stored["owner"]),
        (&json!("1.1.0"), &json!("2026-10-01T00:00:00Z"), &json!("alice"))
    );
    assert!(stored["updatedAt"].is_string());

    let (deleted, _) = send(&store, "DELETE", "/agents/support", Some(ALICE), None).await;
    let (read, _) = send(&store, "GET", "/agents/support", Some(ALICE), None).await;

    assert_eq!((deleted, read), (StatusCode::NO_CONTENT, StatusCode::NOT_FOUND));
}

#[tokio::test]
async fn an_update_cannot_rename_a_card() {
    let store = store();
    register(&store, ALICE, &card("support", "1.0.0")).await;

    let renamed = serde_json::to_value(card("other", "1.1.0")).unwrap();
    let (status, _) = send(&store, "PUT", "/agents/support", Some(ALICE), Some(renamed)).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(store.get("other").await.unwrap(), None);
}

#[tokio::test]
async fn a_card_without_an_owner_is_read_only() {
    let store = store();
    // A card stored before cards had owners deserializes with no owner.
    let legacy: AgentCard = serde_json::from_value(json!({
        "name": "legacy",
        "version": "1.0.0",
        "createdAt": "2025-01-01T00:00:00Z",
    }))
    .unwrap();
    store.insert(legacy.clone()).await.unwrap();
    let replacement = serde_json::to_value(card("legacy", "2.0.0")).unwrap();

    let (read, _) = send(&store, "GET", "/agents/legacy", Some(BOB), None).await;
    let mut writes = Vec::new();
    for auth in [ALICE, BOB] {
        writes.push(
            send(&store, "PUT", "/agents/legacy", Some(auth), Some(replacement.clone())).await.0,
        );
        writes.push(send(&store, "DELETE", "/agents/legacy", Some(auth), None).await.0);
        writes.push(send(&store, "POST", "/agents", Some(auth), Some(replacement.clone())).await.0);
    }

    assert_eq!(read, StatusCode::OK);
    assert_eq!(
        writes,
        [StatusCode::FORBIDDEN, StatusCode::FORBIDDEN, StatusCode::CONFLICT].repeat(2)
    );
    assert_eq!(store.get("legacy").await.unwrap(), Some(legacy));
}
