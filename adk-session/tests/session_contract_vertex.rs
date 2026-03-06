#![cfg(feature = "vertex-session")]

mod common;

use adk_session::{VertexAiSessionConfig, VertexAiSessionService};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use google_cloud_auth::credentials::api_key_credentials;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct MockVertexState {
    db: Arc<Mutex<MockVertexDb>>,
}

#[derive(Default)]
struct MockVertexDb {
    next_session: usize,
    next_event: usize,
    sessions: HashMap<String, MockSession>,
    events: HashMap<String, Vec<Value>>,
}

#[derive(Clone)]
struct MockSession {
    user_id: UserId,
    state: HashMap<String, Value>,
    update_time: String,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    session: Option<CreateSessionBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionBody {
    user_id: UserId,
    #[serde(default)]
    session_state: HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct AppendEventRequest {
    event: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
struct ListSessionsQuery {
    filter: Option<String>,
}

fn session_name(project: &str, location: &str, engine: &str, session_id: &str) -> String {
    format!(
        "projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/{session_id}"
    )
}

fn parse_user_filter(filter: &str) -> Option<String> {
    let filter = filter.trim();
    let prefix = "userId=\"";
    if !filter.starts_with(prefix) || !filter.ends_with('"') {
        return None;
    }
    Some(filter[prefix.len()..filter.len() - 1].to_string())
}

async fn create_session(
    State(state): State<MockVertexState>,
    Path((project, location, engine)): Path<(String, String, String)>,
    Json(request): Json<CreateSessionRequest>,
) -> (StatusCode, Json<Value>) {
    let Some(session) = request.session else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "missing session payload" } })),
        );
    };

    let mut db = state.db.lock().await;
    db.next_session += 1;
    let session_id = format!("s-{}", db.next_session);
    let name = session_name(&project, &location, &engine, &session_id);

    db.sessions.insert(
        name.clone(),
        MockSession {
            user_id: session.user_id,
            state: session.session_state,
            update_time: Utc::now().to_rfc3339(),
        },
    );
    db.events.entry(name.clone()).or_default();

    (
        StatusCode::OK,
        Json(json!({
            "name": format!("{name}/operations/create-{session_id}")
        })),
    )
}

async fn list_sessions(
    State(state): State<MockVertexState>,
    Path((project, location, engine)): Path<(String, String, String)>,
    Query(query): Query<ListSessionsQuery>,
) -> (StatusCode, Json<Value>) {
    let prefix =
        format!("projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/");
    let filtered_user = query.filter.as_deref().and_then(parse_user_filter).unwrap_or_default();

    let db = state.db.lock().await;
    let mut sessions = Vec::new();
    for (name, session) in &db.sessions {
        if !name.starts_with(&prefix) {
            continue;
        }
        if !filtered_user.is_empty() && session.user_id != filtered_user {
            continue;
        }

        sessions.push(json!({
            "name": name,
            "userId": session.user_id,
            "sessionState": session.state,
            "updateTime": session.update_time,
        }));
    }

    (
        StatusCode::OK,
        Json(json!({
            "sessions": sessions,
            "nextPageToken": "",
        })),
    )
}

async fn session_routes(
    State(state): State<MockVertexState>,
    Path((project, location, engine, rest)): Path<(String, String, String, String)>,
    method: Method,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    if method == Method::POST && rest.ends_with(":appendEvent") {
        let session_id = rest.trim_end_matches(":appendEvent");
        return append_event(state, &project, &location, &engine, session_id, body).await;
    }

    if method == Method::GET && rest.ends_with("/events") {
        let session_id = rest.trim_end_matches("/events");
        return list_events(state, &project, &location, &engine, session_id).await;
    }

    if rest.contains('/') {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "route not found" } })));
    }

    match method {
        Method::GET => get_session(state, &project, &location, &engine, &rest).await,
        Method::DELETE => delete_session(state, &project, &location, &engine, &rest).await,
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({ "error": { "message": "method not allowed" } })),
        ),
    }
}

async fn get_session(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &SessionId,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let db = state.db.lock().await;
    let Some(session) = db.sessions.get(&name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": { "message": "session not found" } })),
        );
    };

    (
        StatusCode::OK,
        Json(json!({
            "name": name,
            "userId": session.user_id,
            "sessionState": session.state,
            "updateTime": session.update_time,
        })),
    )
}

async fn delete_session(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &SessionId,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let mut db = state.db.lock().await;
    db.sessions.remove(&name);
    db.events.remove(&name);

    (StatusCode::OK, Json(json!({})))
}

async fn append_event(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &SessionId,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let mut db = state.db.lock().await;
    if !db.sessions.contains_key(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": { "message": "session not found" } })),
        );
    }

    let request = serde_json::from_slice::<AppendEventRequest>(&body)
        .unwrap_or(AppendEventRequest { event: Some(json!({})) });
    let mut event = request.event.unwrap_or_else(|| json!({}));
    let Some(event_map) = event.as_object_mut() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "event must be an object" } })),
        );
    };

    db.next_event += 1;
    let event_id = format!("e-{}", db.next_event);
    event_map
        .entry("name".to_string())
        .or_insert_with(|| Value::String(format!("{name}/events/{event_id}")));
    event_map
        .entry("timestamp".to_string())
        .or_insert_with(|| Value::String(Utc::now().to_rfc3339()));

    if let Some(actions) = event_map
        .get("actions")
        .and_then(Value::as_object)
        .and_then(|actions| actions.get("stateDelta"))
        .and_then(Value::as_object)
    {
        if let Some(session) = db.sessions.get_mut(&name) {
            for (key, value) in actions {
                session.state.insert(key.clone(), value.clone());
            }
            if let Some(timestamp) = event_map.get("timestamp").and_then(Value::as_str) {
                session.update_time = timestamp.to_string();
            }
        }
    }

    db.events.entry(name).or_default().push(Value::Object(Map::from_iter(
        event_map.iter().map(|(key, value)| (key.clone(), value.clone())),
    )));

    (StatusCode::OK, Json(json!({})))
}

async fn list_events(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &SessionId,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let db = state.db.lock().await;
    let events = db.events.get(&name).cloned().unwrap_or_default();

    (
        StatusCode::OK,
        Json(json!({
            "sessionEvents": events,
            "nextPageToken": "",
        })),
    )
}

#[tokio::test]
async fn test_vertex_service_contract() {
    let app = Router::new()
        .route(
            "/v1beta1/projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions",
            post(create_session).get(list_sessions),
        )
        .route(
            "/v1beta1/projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/{*rest}",
            get(session_routes).post(session_routes).delete(session_routes),
        )
        .with_state(MockVertexState::default());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock vertex server should run");
    });

    let endpoint = format!("http://{addr}");
    let config = VertexAiSessionConfig::new("test-project", "us-central1").with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    let service = VertexAiSessionService::with_credentials(config, credentials);

    common::session_contract::assert_session_contract(&service, "1001", "2002").await;

    server.abort();
}
