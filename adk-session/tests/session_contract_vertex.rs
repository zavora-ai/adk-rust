#![cfg(feature = "vertex-session")]

mod common;

use adk_core::{
    AdkIdentity, AppName, Content, FileDataPart, FunctionResponseData, InlineDataPart, Part,
    SessionId, UserId,
};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, GetRequest, ListRequest,
    SessionService, VertexAiSessionConfig, VertexAiSessionService,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use google_cloud_auth::credentials::api_key_credentials;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

const VERTEX_IDENTITY_STATE_KEY: &str = "__adk_vertex_identity_v1";

#[derive(Clone, Default)]
struct MockVertexState {
    db: Arc<Mutex<MockVertexDb>>,
}

#[derive(Default)]
struct MockVertexDb {
    next_session: usize,
    next_event: usize,
    next_operation: usize,
    sessions: HashMap<String, MockSession>,
    events: HashMap<String, Vec<Value>>,
    operations: HashMap<String, MockOperation>,
    create_bodies: Vec<Value>,
    append_bodies: Vec<Value>,
    list_requests: usize,
    session_requests: usize,
    event_list_requests: usize,
    event_list_queries: Vec<ListEventsQuery>,
    operation_polls: usize,
    repeat_session_page_token: bool,
    repeat_event_page_token: bool,
    changing_session_page_token: bool,
    aggregate_session_pages: bool,
    duplicate_event_across_pages: bool,
}

#[derive(Clone)]
struct MockSession {
    user_id: String,
    state: HashMap<String, Value>,
    update_time: String,
}

#[derive(Clone)]
struct MockOperation {
    pending_polls: usize,
    poll_error_status: Option<StatusCode>,
    response: Option<Value>,
    error: Option<Value>,
    delete_session: Option<String>,
    reported_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSessionBody {
    user_id: String,
    #[serde(default)]
    session_state: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSessionsQuery {
    session_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListSessionsQuery {
    filter: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListEventsQuery {
    page_size: Option<usize>,
    page_token: Option<String>,
    filter: Option<String>,
    order_by: Option<String>,
}

fn session_name(project: &str, location: &str, engine: &str, session_id: &str) -> String {
    format!(
        "projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/{session_id}"
    )
}

fn parse_user_filter(filter: &str) -> Option<String> {
    let filter = filter.trim();
    let prefix = "user_id=\"";
    if !filter.starts_with(prefix) || !filter.ends_with('"') {
        return None;
    }
    Some(filter[prefix.len()..filter.len() - 1].to_string())
}

fn local_session_id(state: &HashMap<String, Value>) -> Option<String> {
    let encoded = state.get(VERTEX_IDENTITY_STATE_KEY)?.as_str()?;
    let marker: Value = serde_json::from_str(encoded).ok()?;
    marker.get("sessionId")?.as_str().map(str::to_string)
}

fn event_timestamp(event: &Value) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(event.get("timestamp")?.as_str()?)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn stored_session_name(db: &MockVertexDb, local_id: &str) -> Option<String> {
    db.sessions
        .iter()
        .find(|(_, session)| local_session_id(&session.state).as_deref() == Some(local_id))
        .map(|(name, _)| name.clone())
}

async fn seed_mock_session(
    state: &MockVertexState,
    engine: &str,
    session_id: &str,
    user_id: &str,
    marker: Option<Value>,
) -> String {
    let name = session_name("test-project", "us-central1", engine, session_id);
    let mut session_state = HashMap::new();
    if let Some(marker) = marker {
        session_state.insert(VERTEX_IDENTITY_STATE_KEY.to_string(), marker);
    }
    let mut db = state.db.lock().await;
    db.sessions.insert(
        name.clone(),
        MockSession {
            user_id: user_id.to_string(),
            state: session_state,
            update_time: Utc::now().to_rfc3339(),
        },
    );
    db.events.entry(name.clone()).or_default();
    name
}

fn identity(app_name: &str, user_id: &str, session_id: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(app_name).unwrap(),
        UserId::try_from(user_id).unwrap(),
        SessionId::try_from(session_id).unwrap(),
    )
}

fn raw_adk_event(payload: &Value) -> Value {
    serde_json::from_str(
        payload["rawEvent"]["_adkRust"]["adkEvent"]
            .as_str()
            .expect("rawEvent._adkRust.adkEvent JSON string"),
    )
    .expect("parse rawEvent._adkRust.adkEvent")
}

async fn create_session(
    State(state): State<MockVertexState>,
    Path((project, location, engine)): Path<(String, String, String)>,
    Query(query): Query<CreateSessionsQuery>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let raw: Value = match serde_json::from_slice(&body) {
        Ok(raw) => raw,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": error.to_string() } })),
            );
        }
    };
    if raw.get("session").is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "create body must be the Session object" } })),
        );
    }
    let session: CreateSessionBody = match serde_json::from_value(raw.clone()) {
        Ok(session) => session,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": error.to_string() } })),
            );
        }
    };

    let mut db = state.db.lock().await;
    db.create_bodies.push(raw);
    db.next_session += 1;
    let session_id = query.session_id.unwrap_or_else(|| format!("s-{}", db.next_session));
    let local_session_id =
        local_session_id(&session.session_state).unwrap_or_else(|| session_id.clone());
    let create_http_unavailable = local_session_id == "create-http-unavailable";
    let create_lro_unavailable = local_session_id == "create-lro-unavailable";
    let create_lro_rate_limited = local_session_id == "create-lro-rate-limited";
    let name = session_name(&project, &location, &engine, &session_id);
    if db.sessions.contains_key(&name) {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": { "message": "session already exists" } })),
        );
    }
    let session_payload = json!({
        "@type": "type.googleapis.com/google.cloud.aiplatform.v1.Session",
        "name": name,
        "userId": session.user_id,
        "sessionState": session.session_state,
        "updateTime": Utc::now().to_rfc3339(),
    });

    if matches!(
        local_session_id.as_str(),
        "operation-error" | "operation-error-unavailable" | "operation-error-rate-limited"
    ) {
        let (code, message) = match local_session_id.as_str() {
            "operation-error-unavailable" => (14, "mock terminal create unavailable"),
            "operation-error-rate-limited" => (8, "mock terminal create rate limit"),
            _ => (13, "mock create failure"),
        };
        return (
            StatusCode::OK,
            Json(json!({
                "name": format!("projects/{project}/locations/{location}/operations/create-error"),
                "done": true,
                "error": { "code": code, "message": message },
            })),
        );
    }
    if local_session_id == "missing-response" {
        return (
            StatusCode::OK,
            Json(json!({
                "name": format!("projects/{project}/locations/{location}/operations/create-missing"),
                "done": true,
            })),
        );
    }

    db.sessions.insert(
        name.clone(),
        MockSession {
            user_id: session.user_id,
            state: session.session_state,
            update_time: Utc::now().to_rfc3339(),
        },
    );
    db.events.entry(name).or_default();

    if local_session_id == "create-lro-both-results" {
        return (
            StatusCode::OK,
            Json(json!({
                "name": format!("projects/{project}/locations/{location}/operations/create-both"),
                "done": true,
                "error": { "code": 13, "message": "contradictory result" },
                "response": session_payload,
            })),
        );
    }
    if local_session_id == "create-lro-early-response" {
        return (
            StatusCode::OK,
            Json(json!({
                "name": format!("projects/{project}/locations/{location}/operations/create-early"),
                "done": false,
                "response": session_payload,
            })),
        );
    }
    if create_http_unavailable {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": { "message": "create response lost after commit" } })),
        );
    }
    db.next_operation += 1;
    let operation_suffix = if create_lro_rate_limited {
        "rate-limited".to_string()
    } else if create_lro_unavailable {
        "unavailable".to_string()
    } else {
        db.next_operation.to_string()
    };
    let operation_name =
        format!("projects/{project}/locations/{location}/operations/create-{operation_suffix}");
    db.operations.insert(
        operation_name.clone(),
        MockOperation {
            pending_polls: 1,
            poll_error_status: if create_lro_rate_limited {
                Some(StatusCode::TOO_MANY_REQUESTS)
            } else if create_lro_unavailable {
                Some(StatusCode::SERVICE_UNAVAILABLE)
            } else {
                None
            },
            response: (!create_lro_unavailable && !create_lro_rate_limited)
                .then_some(session_payload),
            error: None,
            delete_session: None,
            reported_name: (local_session_id == "operation-name-swap").then(|| {
                format!("projects/{project}/locations/{location}/operations/swapped-operation")
            }),
        },
    );

    (StatusCode::OK, Json(json!({ "name": operation_name, "done": false })))
}

async fn get_operation(
    State(state): State<MockVertexState>,
    Path((project, location, operation_id)): Path<(String, String, String)>,
) -> (StatusCode, Json<Value>) {
    let name = format!("projects/{project}/locations/{location}/operations/{operation_id}");
    let mut db = state.db.lock().await;
    db.operation_polls += 1;

    let Some(operation) = db.operations.get_mut(&name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": { "message": "operation not found" } })),
        );
    };
    if operation.pending_polls > 0 {
        operation.pending_polls -= 1;
        return (StatusCode::OK, Json(json!({ "name": name, "done": false })));
    }

    let operation = operation.clone();
    if let Some(status) = operation.poll_error_status {
        if let Some(session_name) = operation.delete_session {
            db.sessions.remove(&session_name);
            db.events.remove(&session_name);
        }
        return (status, Json(json!({ "error": { "message": "mock operation poll failed" } })));
    }
    let reported_name = operation.reported_name.clone().unwrap_or(name);
    if let Some(session_name) = operation.delete_session {
        db.sessions.remove(&session_name);
        db.events.remove(&session_name);
    }

    let mut result = Map::from_iter([
        ("name".to_string(), Value::String(reported_name)),
        ("done".to_string(), Value::Bool(true)),
    ]);
    if let Some(response) = operation.response {
        result.insert("response".to_string(), response);
    }
    if let Some(error) = operation.error {
        result.insert("error".to_string(), error);
    }
    (StatusCode::OK, Json(Value::Object(result)))
}

async fn list_sessions(
    State(state): State<MockVertexState>,
    Path((project, location, engine)): Path<(String, String, String)>,
    Query(query): Query<ListSessionsQuery>,
) -> (StatusCode, Json<Value>) {
    let (request_number, changing_page_token, aggregate_pages) = {
        let mut db = state.db.lock().await;
        db.list_requests += 1;
        (db.list_requests, db.changing_session_page_token, db.aggregate_session_pages)
    };
    let prefix =
        format!("projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/");
    let filtered_user = match query.filter.as_deref() {
        Some(filter) => match parse_user_filter(filter) {
            Some(user_id) => user_id,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": { "message": "filter must use user_id" } })),
                );
            }
        },
        None => String::new(),
    };
    if changing_page_token {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        return (
            StatusCode::OK,
            Json(json!({
                "sessions": [],
                "nextPageToken": format!("changing-{request_number}"),
            })),
        );
    }
    if aggregate_pages {
        return (
            StatusCode::OK,
            Json(json!({
                "sessions": [],
                "nextPageToken": if query.page_token.is_none() { "aggregate-next" } else { "" },
            })),
        );
    }

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
    sessions.sort_by(|left, right| {
        left["name"].as_str().unwrap_or_default().cmp(right["name"].as_str().unwrap_or_default())
    });
    let start = query
        .page_token
        .as_deref()
        .and_then(|token| token.strip_prefix("page-"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if query.page_size.is_some_and(|page_size| page_size > 100) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "pageSize must not exceed 100" } })),
        );
    }
    let page_size = query.page_size.unwrap_or(100).clamp(1, 2);
    let end = start.saturating_add(page_size).min(sessions.len());
    let page = sessions.get(start..end).unwrap_or_default().to_vec();
    let next_page_token = if end < sessions.len() {
        if db.repeat_session_page_token { "page-repeat".to_string() } else { format!("page-{end}") }
    } else {
        String::new()
    };

    (
        StatusCode::OK,
        Json(json!({
            "sessions": page,
            "nextPageToken": next_page_token,
        })),
    )
}

async fn session_routes(
    State(state): State<MockVertexState>,
    Path((project, location, engine, rest)): Path<(String, String, String, String)>,
    Query(query): Query<ListEventsQuery>,
    method: Method,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    state.db.lock().await.session_requests += 1;
    if method == Method::POST && rest.ends_with(":appendEvent") {
        let session_id = rest.trim_end_matches(":appendEvent");
        return append_event(state, &project, &location, &engine, session_id, body).await;
    }

    if method == Method::GET && rest.ends_with("/events") {
        let session_id = rest.trim_end_matches("/events");
        return list_events(state, &project, &location, &engine, session_id, query).await;
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
    session_id: &str,
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
    session_id: &str,
) -> (StatusCode, Json<Value>) {
    let session_name = session_name(project, location, engine, session_id);
    let mut db = state.db.lock().await;
    if !db.sessions.contains_key(&session_name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": { "message": "session not found" } })),
        );
    }

    db.next_operation += 1;
    let local_session_id = db
        .sessions
        .get(&session_name)
        .and_then(|session| local_session_id(&session.state))
        .unwrap_or_else(|| session_id.to_string());
    if local_session_id == "delete-http-timeout" {
        db.sessions.remove(&session_name);
        db.events.remove(&session_name);
        return (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({ "error": { "message": "delete response lost after commit" } })),
        );
    }
    let delete_lro_unavailable = local_session_id == "delete-lro-unavailable";
    let delete_terminal_unavailable = local_session_id == "delete-terminal-unavailable";
    let operation_name =
        format!("projects/{project}/locations/{location}/operations/delete-{}", db.next_operation);
    db.operations.insert(
        operation_name.clone(),
        MockOperation {
            pending_polls: 1,
            poll_error_status: delete_lro_unavailable.then_some(StatusCode::SERVICE_UNAVAILABLE),
            response: (!delete_lro_unavailable
                && !delete_terminal_unavailable
                && local_session_id != "missing-delete-response")
                .then(|| {
                    json!({
                        "@type": "type.googleapis.com/google.protobuf.Empty"
                    })
                }),
            error: delete_terminal_unavailable
                .then(|| json!({ "code": 14, "message": "mock terminal delete unavailable" })),
            delete_session: (!delete_terminal_unavailable).then_some(session_name),
            reported_name: None,
        },
    );

    (StatusCode::OK, Json(json!({ "name": operation_name, "done": false })))
}

async fn append_event(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &str,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let mut event: Value = match serde_json::from_slice(&body) {
        Ok(event) => event,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": error.to_string() } })),
            );
        }
    };
    if event.get("event").is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "append body must be the SessionEvent object" } })),
        );
    }
    let Some(event_map) = event.as_object_mut() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "event must be an object" } })),
        );
    };
    let allowed_fields = [
        "timestamp",
        "author",
        "invocationId",
        "content",
        "rawEvent",
        "actions",
        "eventMetadata",
        "errorCode",
        "errorMessage",
    ];
    if let Some(field) = event_map.keys().find(|field| !allowed_fields.contains(&field.as_str())) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": format!("unknown append field {field}") } })),
        );
    }
    if event_map.get("invocationId").and_then(Value::as_str).is_none()
        || event_map.get("author").and_then(Value::as_str).is_none()
        || event_map.get("timestamp").and_then(Value::as_str).is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": { "message": "event must contain invocationId, author, and timestamp" } }),
            ),
        );
    }

    let mut db = state.db.lock().await;
    if !db.sessions.contains_key(&name) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": { "message": "session not found" } })),
        );
    }
    let invocation_id = event_map
        .get("invocationId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    if invocation_id == "inv-rate-limited" {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": { "message": "explicit rate-limit rejection" } })),
        );
    }
    db.append_bodies.push(Value::Object(event_map.clone()));

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
        && let Some(session) = db.sessions.get_mut(&name)
    {
        for (key, value) in actions {
            session.state.insert(key.clone(), value.clone());
        }
        if let Some(timestamp) = event_map.get("timestamp").and_then(Value::as_str) {
            session.update_time = timestamp.to_string();
        }
    }

    let nonempty_response = invocation_id == "inv-nonempty-response";
    db.events.entry(name).or_default().push(Value::Object(event_map.clone()));

    match invocation_id.as_str() {
        "inv-ambiguous-unavailable-legacy" => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": { "message": "response lost after commit" } })),
        ),
        "inv-ambiguous-timeout-typed" => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({ "error": { "message": "gateway timed out after commit" } })),
        ),
        _ if nonempty_response => (StatusCode::OK, Json(json!({ "unexpected": true }))),
        _ => (StatusCode::OK, Json(json!({}))),
    }
}

async fn list_events(
    state: MockVertexState,
    project: &str,
    location: &str,
    engine: &str,
    session_id: &str,
    query: ListEventsQuery,
) -> (StatusCode, Json<Value>) {
    let name = session_name(project, location, engine, session_id);

    let mut db = state.db.lock().await;
    db.event_list_requests += 1;
    db.event_list_queries.push(query.clone());
    let mut events = db.events.get(&name).cloned().unwrap_or_default();
    if let Some(filter) = query.filter.as_deref() {
        let Some(timestamp) = filter.strip_prefix("timestamp>=") else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "unsupported event filter" } })),
            );
        };
        let Ok(timestamp) = serde_json::from_str::<String>(timestamp) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "invalid event filter string" } })),
            );
        };
        let Ok(after) = DateTime::parse_from_rfc3339(&timestamp) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "invalid event filter timestamp" } })),
            );
        };
        let after = after.with_timezone(&Utc);
        events.retain(|event| event_timestamp(event).is_some_and(|timestamp| timestamp >= after));
    }
    if let Some(order_by) = query.order_by.as_deref() {
        if order_by != "timestamp desc" {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": { "message": "unsupported event ordering" } })),
            );
        }
        events.sort_by_key(|event| std::cmp::Reverse(event_timestamp(event)));
    }
    let mut start = query
        .page_token
        .as_deref()
        .and_then(|token| token.strip_prefix("page-"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if db.duplicate_event_across_pages && start > 0 {
        start -= 1;
    }
    if query.page_size.is_some_and(|page_size| page_size > 100) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "pageSize must not exceed 100" } })),
        );
    }
    let page_size = query.page_size.unwrap_or(100).clamp(1, 2);
    let end = start.saturating_add(page_size).min(events.len());
    let page = events.get(start..end).unwrap_or_default().to_vec();
    let next_page_token = if end < events.len() {
        if db.repeat_event_page_token { "page-repeat".to_string() } else { format!("page-{end}") }
    } else {
        String::new()
    };

    (
        StatusCode::OK,
        Json(json!({
            "sessionEvents": page,
            "nextPageToken": next_page_token,
        })),
    )
}

async fn test_service() -> (MockVertexState, VertexAiSessionService, tokio::task::JoinHandle<()>) {
    test_service_with_reasoning_engine(None).await
}

async fn test_service_with_reasoning_engine(
    reasoning_engine: Option<&str>,
) -> (MockVertexState, VertexAiSessionService, tokio::task::JoinHandle<()>) {
    let state = MockVertexState::default();
    let app = Router::new()
        .route(
            "/v1/projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions",
            post(create_session).get(list_sessions),
        )
        .route(
            "/v1/projects/{project}/locations/{location}/reasoningEngines/{engine}/sessions/{*rest}",
            get(session_routes).post(session_routes).delete(session_routes),
        )
        .route(
            "/v1/projects/{project}/locations/{location}/operations/{operation_id}",
            get(get_operation),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock vertex server should run");
    });

    let mut config = VertexAiSessionConfig::new("test-project", "us-central1")
        .with_endpoint(format!("http://{addr}"));
    if let Some(reasoning_engine) = reasoning_engine {
        config = config.with_reasoning_engine(reasoning_engine);
    }
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    let service =
        VertexAiSessionService::with_credentials(config, credentials).expect("build test service");

    (state, service, server)
}

#[tokio::test]
async fn test_vertex_service_contract_uses_canonical_bodies_and_lros() {
    let (state, service, server) = test_service().await;

    common::session_contract::assert_session_contract(&service, "1001", "2002").await;

    let db = state.db.lock().await;
    assert!(!db.create_bodies.is_empty());
    assert!(db.create_bodies.iter().all(|body| body.get("session").is_none()));
    assert!(!db.append_bodies.is_empty());
    assert!(db.append_bodies.iter().all(|body| body.get("event").is_none()));
    assert!(db.operation_polls >= 12, "create and delete operations must be polled to completion");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_pagination_applies_limit_offset_and_rejects_token_loops() {
    let (state, service, server) = test_service().await;
    for session_id in ["page-a", "page-b", "page-c", "page-d", "page-e"] {
        service
            .create(CreateRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("create paginated session");
    }

    let all_sessions = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("list all paginated sessions");
    let all_ids = all_sessions.iter().map(|session| session.id().to_string()).collect::<Vec<_>>();
    let sessions = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            limit: Some(2),
            offset: Some(1),
        })
        .await
        .expect("list paginated sessions");
    assert_eq!(
        sessions.iter().map(|session| session.id().to_string()).collect::<Vec<_>>(),
        all_ids[1..3]
    );

    state.db.lock().await.repeat_session_page_token = true;
    let session_loop = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("repeated session token must fail");
    assert!(session_loop.message.contains("repeated page token"));
    state.db.lock().await.repeat_session_page_token = false;

    let mut event = Event::new("inv-page");
    event.author = "model".to_string();
    for _ in 0..3 {
        service.append_event("page-a", event.clone()).await.expect("append paginated event");
    }
    state.db.lock().await.repeat_event_page_token = true;
    let event_loop = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "page-a".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("repeated event token must fail");
    assert!(event_loop.message.contains("repeated page token"));
    {
        let mut db = state.db.lock().await;
        db.repeat_event_page_token = false;
        db.duplicate_event_across_pages = true;
    }
    let duplicate_event = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "page-a".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("an event repeated across pages must fail");
    assert!(duplicate_event.message.contains("duplicate event ID"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_get_pushes_event_bounds_into_vertex_pagination() {
    let (state, service, server) = test_service().await;
    service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("bounded-events".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create bounded event session");

    for minute in 0..5 {
        let mut event = Event::new(format!("inv-{minute}"));
        event.author = "model".to_string();
        event.timestamp = DateTime::parse_from_rfc3339(&format!("2026-01-01T00:{minute:02}:00Z"))
            .expect("fixed event timestamp")
            .with_timezone(&Utc);
        service.append_event("bounded-events", event).await.expect("append bounded event");
    }

    let zero = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "bounded-events".to_string(),
            num_recent_events: Some(0),
            after: None,
        })
        .await
        .expect("zero recent events");
    assert!(zero.events().is_empty());
    assert_eq!(state.db.lock().await.event_list_requests, 0);

    let after = DateTime::parse_from_rfc3339("2026-01-01T00:02:00Z")
        .expect("fixed lower bound")
        .with_timezone(&Utc);
    let bounded = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "bounded-events".to_string(),
            num_recent_events: Some(2),
            after: Some(after),
        })
        .await
        .expect("bounded recent events");
    assert_eq!(
        bounded.events().all().iter().map(|event| event.invocation_id.as_str()).collect::<Vec<_>>(),
        ["inv-3", "inv-4"],
    );

    let db = state.db.lock().await;
    assert_eq!(db.event_list_requests, 1);
    assert_eq!(
        db.event_list_queries,
        [ListEventsQuery {
            page_size: Some(2),
            page_token: None,
            filter: Some(format!(
                "timestamp>={}",
                serde_json::to_string(&after.to_rfc3339()).expect("encode expected timestamp"),
            )),
            order_by: Some("timestamp desc".to_string()),
        }],
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_pagination_has_aggregate_byte_and_elapsed_deadlines() {
    let (state, service, server) = test_service().await;
    state.db.lock().await.aggregate_session_pages = true;
    let service = service.with_max_response_bytes(60);
    let aggregate_error = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("aggregate pagination bytes must be bounded");
    assert_eq!(aggregate_error.code, "session.vertex.response_too_large");
    assert!(!aggregate_error.is_retryable());

    {
        let mut db = state.db.lock().await;
        db.aggregate_session_pages = false;
        db.changing_session_page_token = true;
        db.list_requests = 0;
    }
    let service = service
        .with_max_response_bytes(4096)
        .with_pagination_timeout(std::time::Duration::from_millis(35));
    let timeout = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("changing page tokens must hit the total elapsed deadline");
    assert_eq!(timeout.category, adk_core::ErrorCategory::Timeout);
    assert!(timeout.is_retryable());
    assert!(state.db.lock().await.list_requests >= 2);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_rejects_unscoped_lists_without_sending_a_request() {
    let (state, service, server) = test_service().await;

    let error = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: String::new(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("an empty list user must fail before transport");
    assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(state.db.lock().await.list_requests, 0);

    server.abort();
}

#[tokio::test]
async fn test_vertex_oversized_request_bodies_fail_before_transport() {
    let (state, service, server) = test_service().await;
    let service = service.with_max_request_bytes(64);
    let error = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("oversized-body".to_string()),
            state: HashMap::from([("large".to_string(), Value::String("x".repeat(256)))]),
        })
        .await
        .err()
        .expect("oversized create body must fail");
    assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(error.code, "session.vertex.request_too_large");

    let mut event = Event::new("inv-oversized");
    event.author = "model".to_string();
    event.llm_response.content = Some(Content::new("model").with_text("oversized".repeat(64)));
    let append_error = service
        .append_event("uncached-session", event)
        .await
        .expect_err("oversized append body must fail before scope lookup");
    assert_eq!(append_error.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(append_error.code, "session.vertex.request_too_large");
    assert!(!append_error.is_retryable());

    let db = state.db.lock().await;
    assert_eq!(db.session_requests, 0);
    assert!(db.create_bodies.is_empty());
    drop(db);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_append_retry_hints_distinguish_ambiguous_outcomes() {
    let (state, service, server) = test_service().await;
    service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("append-retry-hints".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create append retry test session");

    let pre_send = service
        .append_event("append-retry-hints", Event::new("inv-pre-send"))
        .await
        .expect_err("missing author must fail before transmission");
    assert_eq!(pre_send.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(pre_send.code, "session.vertex.invalid_input");
    assert!(!pre_send.is_retryable());

    let mut rate_limited = Event::new("inv-rate-limited");
    rate_limited.author = "model".to_string();
    let rate_limit = service
        .append_event("append-retry-hints", rate_limited)
        .await
        .expect_err("explicit rate-limit rejection must surface");
    assert_eq!(rate_limit.category, adk_core::ErrorCategory::RateLimited);
    assert_eq!(rate_limit.code, "session.vertex.rate_limited");
    assert_eq!(rate_limit.details.upstream_status_code, Some(429));
    assert!(rate_limit.is_retryable());

    let mut unavailable = Event::new("inv-ambiguous-unavailable-legacy");
    unavailable.author = "model".to_string();
    let unavailable = service
        .append_event("append-retry-hints", unavailable)
        .await
        .expect_err("legacy append unavailable outcome must be ambiguous");
    assert_eq!(unavailable.category, adk_core::ErrorCategory::Unavailable);
    assert_eq!(unavailable.code, "session.vertex.append_outcome_ambiguous");
    assert_eq!(unavailable.details.upstream_status_code, Some(503));
    assert_eq!(unavailable.details.provider.as_deref(), Some("vertex_ai"));
    assert!(!unavailable.is_retryable());
    assert!(unavailable.message.contains("Inspect or list"));

    let mut timeout = Event::new("inv-ambiguous-timeout-typed");
    timeout.author = "model".to_string();
    let timeout = service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("1001", "user1", "append-retry-hints"),
            event: timeout,
        })
        .await
        .expect_err("typed append timeout outcome must be ambiguous");
    assert_eq!(timeout.category, adk_core::ErrorCategory::Timeout);
    assert_eq!(timeout.code, "session.vertex.append_outcome_ambiguous");
    assert_eq!(timeout.details.upstream_status_code, Some(504));
    assert_eq!(timeout.details.provider.as_deref(), Some("vertex_ai"));
    assert!(!timeout.is_retryable());
    assert!(timeout.message.contains("avoid duplicates"));

    let fetched = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "append-retry-hints".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("inspect ambiguous append outcomes");
    assert_eq!(
        fetched.events().all().iter().map(|event| event.invocation_id.as_str()).collect::<Vec<_>>(),
        ["inv-ambiguous-unavailable-legacy", "inv-ambiguous-timeout-typed"],
    );
    assert_eq!(state.db.lock().await.append_bodies.len(), 2);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_create_delete_ambiguous_outcomes_require_reconciliation() {
    let (_state, service, server) = test_service().await;

    for (session_id, category, expected_status) in [
        ("create-http-unavailable", adk_core::ErrorCategory::Unavailable, Some(503)),
        ("create-lro-unavailable", adk_core::ErrorCategory::Unavailable, Some(503)),
        ("create-lro-rate-limited", adk_core::ErrorCategory::RateLimited, Some(429)),
        ("create-lro-both-results", adk_core::ErrorCategory::Internal, None),
        ("create-lro-early-response", adk_core::ErrorCategory::Internal, None),
    ] {
        let error = service
            .create(CreateRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .err()
            .expect("ambiguous create must surface an error");
        assert_eq!(error.category, category);
        assert_eq!(error.code, "session.vertex.create_outcome_ambiguous");
        assert_eq!(error.details.upstream_status_code, expected_status);
        assert_eq!(error.details.provider.as_deref(), Some("vertex_ai"));
        assert!(!error.is_retryable());
        assert!(error.message.contains("Inspect the target session"));
        if session_id.starts_with("create-lro-") {
            assert!(error.message.contains("operations/create-"));
        }

        assert_eq!(
            service
                .get(GetRequest {
                    app_name: "1001".to_string(),
                    user_id: "user1".to_string(),
                    session_id: session_id.to_string(),
                    num_recent_events: Some(0),
                    after: None,
                })
                .await
                .expect("reconcile committed create")
                .id(),
            session_id,
        );
    }

    for session_id in ["delete-http-timeout", "delete-lro-unavailable"] {
        service
            .create(CreateRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("create delete ambiguity test session");
    }

    let timeout = service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "delete-http-timeout".to_string(),
        })
        .await
        .expect_err("ambiguous delete timeout must surface");
    assert_eq!(timeout.category, adk_core::ErrorCategory::Timeout);
    assert_eq!(timeout.code, "session.vertex.delete_outcome_ambiguous");
    assert_eq!(timeout.details.upstream_status_code, Some(504));
    assert_eq!(timeout.details.provider.as_deref(), Some("vertex_ai"));
    assert!(!timeout.is_retryable());

    let unavailable = service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: "delete-lro-unavailable".to_string(),
        })
        .await
        .expect_err("ambiguous delete LRO failure must surface");
    assert_eq!(unavailable.category, adk_core::ErrorCategory::Unavailable);
    assert_eq!(unavailable.code, "session.vertex.delete_outcome_ambiguous");
    assert_eq!(unavailable.details.upstream_status_code, Some(503));
    assert_eq!(unavailable.details.provider.as_deref(), Some("vertex_ai"));
    assert!(!unavailable.is_retryable());

    for session_id in ["delete-http-timeout", "delete-lro-unavailable"] {
        let error = service
            .get(GetRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: session_id.to_string(),
                num_recent_events: Some(0),
                after: None,
            })
            .await
            .err()
            .expect("reconcile committed delete");
        assert!(error.is_not_found());
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_delete_validates_complete_identity_before_transport() {
    let (state, service, server) = test_service().await;
    let too_long = "x".repeat(513);
    for (app_name, user_id, session_id) in [
        ("bad\0app".to_string(), "user".to_string(), "session".to_string()),
        ("app".to_string(), "bad\0user".to_string(), "session".to_string()),
        ("app".to_string(), "user".to_string(), "bad\0session".to_string()),
        (too_long.clone(), "user".to_string(), "session".to_string()),
        ("app".to_string(), too_long.clone(), "session".to_string()),
        ("app".to_string(), "user".to_string(), too_long),
    ] {
        let error = service
            .delete(DeleteRequest { app_name, user_id, session_id })
            .await
            .expect_err("invalid delete identity must fail");
        assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput);
    }
    let mut event = Event::new("invalid-legacy-append");
    event.author = "model".to_string();
    let append_error = service
        .append_event(&"x".repeat(513), event)
        .await
        .expect_err("invalid legacy append session ID must fail");
    assert_eq!(append_error.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(state.db.lock().await.session_requests, 0);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_user_id_provider_limit_is_unicode_aware_and_pre_transport() {
    let (state, service, server) = test_service_with_reasoning_engine(Some("9999")).await;
    let invalid_user = "é".repeat(129);
    let mut event = Event::new("invalid-user");
    event.author = "model".to_string();
    let errors = [
        service
            .create(CreateRequest {
                app_name: "app".to_string(),
                user_id: invalid_user.clone(),
                session_id: Some("session".to_string()),
                state: HashMap::new(),
            })
            .await
            .err()
            .expect("create must reject oversized Vertex user"),
        service
            .get(GetRequest {
                app_name: "app".to_string(),
                user_id: invalid_user.clone(),
                session_id: "session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .expect("get must reject oversized Vertex user"),
        service
            .list(ListRequest {
                app_name: "app".to_string(),
                user_id: invalid_user.clone(),
                limit: None,
                offset: None,
            })
            .await
            .err()
            .expect("list must reject oversized Vertex user"),
        service
            .delete(DeleteRequest {
                app_name: "app".to_string(),
                user_id: invalid_user.clone(),
                session_id: "session".to_string(),
            })
            .await
            .expect_err("delete must reject oversized Vertex user"),
        service
            .append_event_for_identity(AppendEventRequest {
                identity: identity("app", &invalid_user, "session"),
                event,
            })
            .await
            .expect_err("typed append must reject oversized Vertex user"),
    ];
    assert!(errors.iter().all(|error| {
        error.category == adk_core::ErrorCategory::InvalidInput
            && error.message.contains("128 Unicode characters")
    }));
    {
        let db = state.db.lock().await;
        assert_eq!(db.create_bodies.len(), 0);
        assert_eq!(db.list_requests, 0);
        assert_eq!(db.session_requests, 0);
    }

    for (session_id, user_id) in
        [("ascii-boundary", "a".repeat(128)), ("unicode-boundary", "😀".repeat(128))]
    {
        service
            .create(CreateRequest {
                app_name: "app".to_string(),
                user_id,
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("128 Unicode characters must be accepted");
    }

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_composite_ids_isolate_same_logical_id_across_apps_and_users() {
    let (state, service, server) = test_service_with_reasoning_engine(Some("9999")).await;
    let shared_id = "shared-session";
    for (app_name, user_id) in [("1001", "alice"), ("1001", "bob"), ("2002", "alice")] {
        let created = service
            .create(CreateRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: Some(shared_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("same logical ID must coexist across identities");
        assert_eq!(created.id(), shared_id);
        assert!(created.state().get(VERTEX_IDENTITY_STATE_KEY).is_none());
    }

    {
        let db = state.db.lock().await;
        assert_eq!(db.sessions.len(), 3);
        let remote_names = db.sessions.keys().collect::<std::collections::HashSet<_>>();
        assert_eq!(remote_names.len(), 3);
        assert!(db.sessions.values().all(|session| {
            session.state.get(VERTEX_IDENTITY_STATE_KEY).is_some_and(Value::is_string)
        }));
    }

    let mut event = Event::new("inv-alice");
    event.author = "model".to_string();
    service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("1001", "alice", shared_id),
            event,
        })
        .await
        .expect("append exact identity");

    for (app_name, user_id, expected_events) in
        [("1001", "alice", 1), ("1001", "bob", 0), ("2002", "alice", 0)]
    {
        let fetched = service
            .get(GetRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: shared_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("fetch exact identity");
        assert_eq!(fetched.events().len(), expected_events);
        let listed = service
            .list(ListRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                limit: None,
                offset: None,
            })
            .await
            .expect("list exact identity");
        assert_eq!(listed.iter().filter(|session| session.id() == shared_id).count(), 1);
    }
    let mut ambiguous_legacy_event = Event::new("inv-ambiguous-cache");
    ambiguous_legacy_event.author = "model".to_string();
    let ambiguous_legacy = service
        .append_event(shared_id, ambiguous_legacy_event)
        .await
        .expect_err("legacy append must reject an ambiguous cached scope");
    assert_eq!(ambiguous_legacy.category, adk_core::ErrorCategory::InvalidInput);
    assert!(ambiguous_legacy.message.contains("ambiguous"));

    service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("private-session".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create private identity");
    let wrong_get = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "bob".to_string(),
            session_id: "private-session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("cross-user get must be not found");
    assert!(wrong_get.is_not_found());
    let mut wrong_event = Event::new("inv-wrong");
    wrong_event.author = "model".to_string();
    let wrong_append = service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("2002", "alice", "private-session"),
            event: wrong_event,
        })
        .await
        .expect_err("cross-app append must fail");
    assert!(wrong_append.is_not_found());
    let wrong_delete = service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "bob".to_string(),
            session_id: "private-session".to_string(),
        })
        .await
        .expect_err("cross-user delete must fail");
    assert!(wrong_delete.is_not_found());
    let original = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            session_id: "private-session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("cross-identity operations must not mutate original");
    assert_eq!(original.events().len(), 0);

    server.abort();
}

#[tokio::test]
async fn test_vertex_automatic_legacy_fallback_is_exact_and_full_lifecycle() {
    let (state, service, server) = test_service().await;
    seed_mock_session(&state, "1001", "legacy-session", "alice", None).await;

    let duplicate = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("legacy-session".to_string()),
            state: HashMap::new(),
        })
        .await
        .err()
        .expect("legacy duplicate must block a computed-ID create");
    assert_eq!(duplicate.category, adk_core::ErrorCategory::InvalidInput);

    let wrong_user = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "bob".to_string(),
            session_id: "legacy-session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("legacy fallback must require the exact user");
    assert!(wrong_user.is_not_found());

    let fetched = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            session_id: "legacy-session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("automatic legacy get");
    assert_eq!(fetched.id(), "legacy-session");
    let listed = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .expect("automatic legacy list");
    assert_eq!(listed.iter().map(|session| session.id()).collect::<Vec<_>>(), ["legacy-session"]);

    let mut typed_event = Event::new("legacy-typed");
    typed_event.author = "model".to_string();
    service
        .append_event_for_identity(AppendEventRequest {
            identity: identity("1001", "alice", "legacy-session"),
            event: typed_event,
        })
        .await
        .expect("typed legacy append");
    let mut cached_event = Event::new("legacy-cached");
    cached_event.author = "model".to_string();
    service.append_event("legacy-session", cached_event).await.expect("cached legacy append");
    assert_eq!(
        service
            .get(GetRequest {
                app_name: "1001".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("get appended legacy session")
            .events()
            .len(),
        2
    );

    service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            session_id: "legacy-session".to_string(),
        })
        .await
        .expect("legacy delete");
    assert!(
        service
            .get(GetRequest {
                app_name: "1001".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .is_some_and(|error| error.is_not_found())
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_automatic_parent_selection_rejects_app_aliases() {
    let (state, service, server) = test_service().await;
    seed_mock_session(&state, "123", "legacy-session", "alice", None).await;

    assert_eq!(
        service
            .get(GetRequest {
                app_name: "123".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("canonical numeric app owns the legacy parent")
            .id(),
        "legacy-session"
    );
    let requests_after_owner_get = state.db.lock().await.session_requests;
    let alias = "projects/test-project/locations/us-central1/reasoningEngines/123";
    for error in [
        service
            .get(GetRequest {
                app_name: alias.to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .expect("full-resource app alias must fail"),
        service
            .delete(DeleteRequest {
                app_name: alias.to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
            })
            .await
            .expect_err("full-resource delete alias must fail"),
    ] {
        assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput);
    }
    assert_eq!(state.db.lock().await.session_requests, requests_after_owner_get);
    let list_alias = service
        .list(ListRequest {
            app_name: alias.to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("full-resource list alias must fail");
    assert_eq!(list_alias.category, adk_core::ErrorCategory::InvalidInput);
    assert_eq!(state.db.lock().await.list_requests, 0);
    assert!(
        service
            .create(CreateRequest {
                app_name: "00123".to_string(),
                user_id: "alice".to_string(),
                session_id: Some("noncanonical".to_string()),
                state: HashMap::new(),
            })
            .await
            .err()
            .is_some_and(|error| error.category == adk_core::ErrorCategory::InvalidInput)
    );

    assert!(
        service
            .get(GetRequest {
                app_name: "123".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .is_ok()
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_legacy_list_rejects_oversized_unmarked_session_id() {
    let (state, service, server) = test_service().await;
    seed_mock_session(&state, "1001", &"x".repeat(513), "alice", None).await;
    let error = service
        .list(ListRequest {
            app_name: "1001".to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await
        .err()
        .expect("oversized unmarked logical ID must fail");
    assert_eq!(error.category, adk_core::ErrorCategory::Internal);
    assert!(error.message.contains("invalid session resource ID"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_fixed_engine_legacy_opt_in_fails_closed_on_corruption() {
    let (state, service, server) = test_service_with_reasoning_engine(Some("9999")).await;
    let direct_name = seed_mock_session(&state, "9999", "legacy-session", "alice", None).await;

    assert!(
        service
            .get(GetRequest {
                app_name: "legacy-app".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .is_some_and(|error| error.is_not_found())
    );
    assert!(
        service
            .list(ListRequest {
                app_name: "legacy-app".to_string(),
                user_id: "alice".to_string(),
                limit: None,
                offset: None,
            })
            .await
            .expect("marker-only shared-engine list")
            .is_empty()
    );

    let service = service.allow_unmarked_sessions_for_app("legacy-app");
    assert_eq!(
        service
            .get(GetRequest {
                app_name: "legacy-app".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("opted-in legacy get")
            .id(),
        "legacy-session"
    );
    assert_eq!(
        service
            .list(ListRequest {
                app_name: "legacy-app".to_string(),
                user_id: "alice".to_string(),
                limit: None,
                offset: None,
            })
            .await
            .expect("opted-in legacy list")
            .len(),
        1
    );
    assert!(
        service
            .get(GetRequest {
                app_name: "other-app".to_string(),
                user_id: "alice".to_string(),
                session_id: "legacy-session".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .err()
            .is_some_and(|error| error.is_not_found())
    );

    let marker = Value::String(
        json!({
            "schemaVersion": 1,
            "appName": "legacy-app",
            "userId": "alice",
            "sessionId": "legacy-session"
        })
        .to_string(),
    );
    state
        .db
        .lock()
        .await
        .sessions
        .get_mut(&direct_name)
        .expect("seeded direct session")
        .state
        .insert(VERTEX_IDENTITY_STATE_KEY.to_string(), marker);
    let marked_direct = service
        .get(GetRequest {
            app_name: "legacy-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "legacy-session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("marked direct resource must fail closed");
    assert_eq!(marked_direct.category, adk_core::ErrorCategory::Internal);

    service
        .create(CreateRequest {
            app_name: "legacy-app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("ambiguous".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create computed identity");
    seed_mock_session(&state, "9999", "ambiguous", "alice", None).await;
    let ambiguous = service
        .get(GetRequest {
            app_name: "legacy-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "ambiguous".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("computed and legacy resources must be ambiguous");
    assert_eq!(ambiguous.category, adk_core::ErrorCategory::Internal);
    assert!(ambiguous.message.contains("ambiguous"));

    seed_mock_session(&state, "9999", "adk1-manual", "alice", None).await;
    let reserved = service
        .get(GetRequest {
            app_name: "legacy-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "adk1-manual".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("unmarked reserved ID must fail closed");
    assert_eq!(reserved.category, adk_core::ErrorCategory::Internal);
    assert!(reserved.message.contains("reserved computed-ID namespace"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_vertex_rejects_malformed_append_and_event_list_responses() {
    let (state, service, server) = test_service().await;
    let created = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("malformed-responses".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create malformed response test session");

    let mut event = Event::new("inv-nonempty-response");
    event.author = "model".to_string();
    let append_error = service
        .append_event(created.id(), event)
        .await
        .expect_err("append requires an empty AppendEventResponse");
    assert_eq!(append_error.category, adk_core::ErrorCategory::Internal);
    assert_eq!(append_error.code, "session.vertex.append_outcome_ambiguous");
    assert!(!append_error.is_retryable());
    assert!(append_error.message.contains("Inspect or list"));
    assert!(append_error.message.contains("google.protobuf.Empty"));

    let name =
        stored_session_name(&*state.db.lock().await, created.id()).expect("stored session name");
    state.db.lock().await.events.insert(
        name.clone(),
        vec![json!({
            "timestamp": Utc::now().to_rfc3339(),
            "invocationId": "inv-missing-name",
            "author": "model"
        })],
    );
    let list_error = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("a listed SessionEvent must have a resource name");
    assert_eq!(list_error.category, adk_core::ErrorCategory::Internal);
    assert!(list_error.message.contains("required resource name"));

    state.db.lock().await.events.insert(
        name.clone(),
        vec![
            json!({
                "name": format!("{name}/events/newer"),
                "timestamp": "2026-01-01T00:01:00Z",
                "invocationId": "inv-newer",
                "author": "model"
            }),
            json!({
                "name": format!("{name}/events/older"),
                "timestamp": "2026-01-01T00:00:00Z",
                "invocationId": "inv-older",
                "author": "model"
            }),
        ],
    );
    let order_error = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("default event listing must be timestamp-ascending");
    assert_eq!(order_error.category, adk_core::ErrorCategory::Internal);
    assert!(order_error.message.contains("ascending timestamp order"));

    server.abort();
}

#[tokio::test]
async fn test_vertex_content_round_trip_preserves_supported_parts() {
    let (state, service, server) = test_service().await;
    let created = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("content-session".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create content session");
    assert_eq!(created.id(), "content-session");

    let mut event = Event::new("inv-content");
    event.author = "model".to_string();
    event.llm_response.content = Some(Content {
        role: "assistant".to_string(),
        parts: vec![
            Part::Thinking {
                thinking: "reasoning".to_string(),
                signature: Some("dGhvdWdodC1zaWduYXR1cmU=".to_string()),
            },
            Part::Text { text: "answer".to_string() },
            Part::InlineData {
                mime_type: "application/octet-stream".to_string(),
                data: vec![0, 1, 2, 253, 254, 255],
            },
            Part::FileData {
                mime_type: "image/png".to_string(),
                file_uri: "gs://bucket/image.png".to_string(),
            },
            Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({ "key": "value" }),
                id: None,
                thought_signature: Some("ZnVuY3Rpb24tc2lnbmF0dXJl".to_string()),
            },
            Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: "lookup".to_string(),
                    response: json!({ "ok": true }),
                    inline_data: vec![InlineDataPart {
                        mime_type: "image/gif".to_string(),
                        data: vec![71, 73, 70],
                    }],
                    file_data: vec![FileDataPart {
                        mime_type: "application/pdf".to_string(),
                        file_uri: "gs://bucket/result.pdf".to_string(),
                    }],
                },
                id: None,
            },
        ],
    });
    event.actions.artifact_delta.insert("report".to_string(), 7);
    event.actions.skip_summarization = true;
    event.actions.transfer_to_agent = Some("reviewer".to_string());
    event.actions.escalate = true;
    event.actions.route = Some(vec!["review".to_string(), "publish".to_string()]);
    event.llm_response.provider_metadata = Some(json!({ "responseId": "response-123" }));
    event.llm_response.interaction_id = Some("interaction-123".to_string());
    event.llm_request = Some(r#"{"model":"test"}"#.to_string());
    event.provider_metadata.insert("traceId".to_string(), "trace-123".to_string());
    let expected = event.llm_response.content.clone().expect("expected content");
    let expected_event = serde_json::to_value(&event).expect("serialize expected event");

    service.append_event(created.id(), event).await.expect("append event with supported content");

    let mut function_event = Event::new("inv-function");
    function_event.author = "tool".to_string();
    function_event.llm_response.content = Some(Content {
        role: "function".to_string(),
        parts: vec![Part::Text { text: "tool output".to_string() }],
    });
    let expected_function_event =
        serde_json::to_value(&function_event).expect("serialize function event");
    service.append_event(created.id(), function_event).await.expect("append function-role event");

    let fetched = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("fetch event with content");

    let restored_event = fetched.events().at(0).expect("first restored event");
    let restored = restored_event.llm_response.content.as_ref().expect("restored content");
    assert_eq!(restored.role, expected.role);
    assert_eq!(restored.parts, expected.parts);
    assert_eq!(
        serde_json::to_value(restored_event).expect("serialize restored event"),
        expected_event
    );
    assert_eq!(
        serde_json::to_value(fetched.events().at(1).expect("second restored event"))
            .expect("serialize restored function event"),
        expected_function_event
    );

    let db = state.db.lock().await;
    let body = &db.append_bodies[db.append_bodies.len() - 2];
    assert!(body.get("event").is_none());
    assert_eq!(body["content"]["role"], "model");
    assert_eq!(body["eventMetadata"]["customMetadata"]["adkContentRole"], "assistant");
    assert_eq!(body["content"]["parts"][2]["inlineData"]["data"], "AAEC/f7/");
    assert!(body["content"]["parts"][4]["functionCall"].get("id").is_none());
    assert_eq!(body["content"]["parts"][4]["thoughtSignature"], "ZnVuY3Rpb24tc2lnbmF0dXJl");
    assert!(body["content"]["parts"][5]["functionResponse"].get("id").is_none());
    assert_eq!(body["actions"]["artifactDelta"]["report"], 7);
    assert_eq!(body["actions"]["skipSummarization"], true);
    assert_eq!(body["actions"]["transferAgent"], "reviewer");
    assert_eq!(body["actions"]["escalate"], true);
    assert_eq!(body["rawEvent"]["_adkRust"]["schemaVersion"], 1);
    assert_eq!(body["rawEvent"]["_adkRust"]["contentSource"], "canonical");
    assert_eq!(body["rawEvent"]["content"], body["content"]);
    assert_eq!(body["rawEvent"]["actions"]["artifactDelta"]["report"], 7);
    assert_eq!(body["rawEvent"]["actions"]["transferToAgent"], "reviewer");
    let mut expected_raw_event = expected_event.clone();
    expected_raw_event["content"] = Value::Null;
    assert_eq!(raw_adk_event(body), expected_raw_event);

    let function_body = db.append_bodies.last().expect("captured function-role append body");
    assert_eq!(function_body["content"]["role"], "user");
    assert_eq!(function_body["eventMetadata"]["customMetadata"]["adkContentRole"], "function");
    assert_eq!(function_body["rawEvent"]["_adkRust"]["contentSource"], "canonical");
    assert!(raw_adk_event(function_body)["content"].is_null());

    server.abort();
}

#[tokio::test]
async fn test_vertex_create_reports_operation_errors_and_missing_responses() {
    let (_state, service, server) = test_service().await;

    let operation_error = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("operation-error".to_string()),
            state: HashMap::new(),
        })
        .await
        .err()
        .expect("operation error must fail create");
    assert_eq!(operation_error.code, "session.vertex.operation_failed");
    assert!(operation_error.message.contains("code 13"));
    assert!(operation_error.message.contains("mock create failure"));
    assert!(!operation_error.is_retryable());

    for (session_id, category) in [
        ("operation-error-unavailable", adk_core::ErrorCategory::Unavailable),
        ("operation-error-rate-limited", adk_core::ErrorCategory::RateLimited),
    ] {
        let error = service
            .create(CreateRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .err()
            .expect("terminal operation error must fail create");
        assert_eq!(error.category, category);
        assert_eq!(error.code, "session.vertex.operation_failed");
        assert!(error.is_retryable());
        assert!(error.message.contains("operations/create-error"));
    }

    let missing_response = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("missing-response".to_string()),
            state: HashMap::new(),
        })
        .await
        .err()
        .expect("missing operation response must fail create");
    assert_eq!(missing_response.code, "session.vertex.create_outcome_ambiguous");
    assert!(!missing_response.is_retryable());
    assert!(missing_response.message.contains("operations/create-missing"));
    assert!(missing_response.message.contains("completed without a response"));

    let swapped_operation = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("operation-name-swap".to_string()),
            state: HashMap::new(),
        })
        .await
        .err()
        .expect("operation identity swap must fail");
    assert!(swapped_operation.message.contains("changed operation identity"));
    assert_eq!(swapped_operation.code, "session.vertex.create_outcome_ambiguous");
    assert!(!swapped_operation.is_retryable());

    let delete_session = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("missing-delete-response".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create delete-response test session");
    let missing_delete_response = service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: delete_session.id().to_string(),
        })
        .await
        .expect_err("delete operation requires google.protobuf.Empty response");
    assert!(missing_delete_response.message.contains("completed without a response"));
    assert_eq!(missing_delete_response.code, "session.vertex.delete_outcome_ambiguous");
    assert!(!missing_delete_response.is_retryable());

    let terminal_delete = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("delete-terminal-unavailable".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create terminal delete test session");
    let terminal_delete_error = service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: terminal_delete.id().to_string(),
        })
        .await
        .expect_err("terminal delete operation error must surface");
    assert_eq!(terminal_delete_error.category, adk_core::ErrorCategory::Unavailable);
    assert_eq!(terminal_delete_error.code, "session.vertex.operation_failed");
    assert!(terminal_delete_error.is_retryable());
    assert_eq!(
        service
            .get(GetRequest {
                app_name: "1001".to_string(),
                user_id: "user1".to_string(),
                session_id: terminal_delete.id().to_string(),
                num_recent_events: Some(0),
                after: None,
            })
            .await
            .expect("terminal delete failure must preserve the session")
            .id(),
        terminal_delete.id(),
    );

    server.abort();
}

#[tokio::test]
async fn test_vertex_rejects_invalid_ids_and_uses_raw_event_for_adk_only_content() {
    let (state, service, server) = test_service().await;

    let invalid_id = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("invalid\0id".to_string()),
            state: HashMap::new(),
        })
        .await
        .err()
        .expect("invalid logical session ID must fail");
    assert_eq!(invalid_id.category, adk_core::ErrorCategory::InvalidInput);

    let reserved_state = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("reserved-state".to_string()),
            state: HashMap::from([(
                VERTEX_IDENTITY_STATE_KEY.to_string(),
                Value::String("caller-controlled".to_string()),
            )]),
        })
        .await
        .err()
        .expect("caller identity marker must fail");
    assert_eq!(reserved_state.category, adk_core::ErrorCategory::InvalidInput);

    let created = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("unsupported-content".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create unsupported content test session");
    let mut reserved_delta = Event::new("inv-reserved-delta");
    reserved_delta.author = "model".to_string();
    reserved_delta.actions.state_delta.insert(
        VERTEX_IDENTITY_STATE_KEY.to_string(),
        Value::String("caller-controlled".to_string()),
    );
    let reserved_delta = service
        .append_event(created.id(), reserved_delta)
        .await
        .expect_err("caller identity delta must fail");
    assert_eq!(reserved_delta.category, adk_core::ErrorCategory::InvalidInput);

    let mut event = Event::new("inv-unsupported");
    event.author = "model".to_string();
    event.llm_response.content = Some(Content {
        role: "custom-provider-role".to_string(),
        parts: vec![
            Part::ServerToolCall { server_tool_call: json!({ "name": "search" }) },
            Part::FunctionCall {
                name: "scalar_args".to_string(),
                args: json!("not-a-struct"),
                id: Some("call-scalar".to_string()),
                thought_signature: None,
            },
        ],
    });
    let expected = serde_json::to_value(&event).expect("serialize ADK-only event");
    service.append_event(created.id(), event).await.expect("ADK-only content should use rawEvent");

    let fetched = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("restore ADK-only content");
    assert_eq!(
        serde_json::to_value(fetched.events().at(0).expect("restored ADK-only event"))
            .expect("serialize restored ADK-only event"),
        expected
    );
    let db = state.db.lock().await;
    let body = db.append_bodies.last().expect("captured ADK-only append body");
    assert!(body.get("content").is_none());
    assert_eq!(body["rawEvent"]["_adkRust"]["contentSource"], "raw");
    assert_eq!(raw_adk_event(body), expected);
    drop(db);

    service
        .delete(DeleteRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
        })
        .await
        .expect("delete test session");

    server.abort();
}

#[tokio::test]
async fn test_vertex_rejects_invalid_base64_in_stored_content() {
    let (state, service, server) = test_service().await;
    let created = service
        .create(CreateRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("invalid-base64".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("create invalid base64 test session");
    let name =
        stored_session_name(&*state.db.lock().await, created.id()).expect("stored session name");
    state.db.lock().await.events.entry(name.clone()).or_default().push(json!({
        "name": format!("{name}/events/bad"),
        "timestamp": Utc::now().to_rfc3339(),
        "invocationId": "inv-bad",
        "author": "model",
        "content": {
            "role": "model",
            "parts": [{
                "inlineData": {
                    "mimeType": "application/octet-stream",
                    "data": "not base64!",
                }
            }]
        }
    }));

    let error = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .err()
        .expect("invalid stored base64 must fail");
    assert!(error.message.contains("invalid base64"));

    let mut raw_event = Event::new("inv-schema");
    raw_event.author = "model".to_string();
    let raw_event_json = serde_json::to_string(&raw_event).expect("serialize raw event");
    let arbitrary_raw_event = json!({
        "schemaVersion": 2,
        "adkEvent": raw_event_json,
        "custom": { "kept": true },
    });
    state.db.lock().await.events.insert(
        name.clone(),
        vec![json!({
            "name": format!("{name}/events/schema"),
            "timestamp": raw_event.timestamp.to_rfc3339(),
            "invocationId": raw_event.invocation_id,
            "author": raw_event.author,
            "eventMetadata": {
                "customMetadata": {
                    "adkEventId": raw_event.id,
                }
            },
            "rawEvent": arbitrary_raw_event
        })],
    );
    let fetched = service
        .get(GetRequest {
            app_name: "1001".to_string(),
            user_id: "user1".to_string(),
            session_id: created.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("top-level adkEvent without _adkRust must remain opaque");
    let event = fetched.events().at(0).expect("opaque event");
    let preserved: Value = serde_json::from_str(
        event.provider_metadata.get("adk.vertex.session.raw_event_json").unwrap(),
    )
    .expect("parse opaque rawEvent sidecar");
    assert_eq!(preserved, arbitrary_raw_event);

    service
        .append_event(created.id(), event.clone())
        .await
        .expect("reappend opaque top-level adkEvent");
    let db = state.db.lock().await;
    let mut reappended = db.append_bodies.last().unwrap()["rawEvent"].clone();
    reappended.as_object_mut().unwrap().remove("_adkRust");
    assert_eq!(reappended, arbitrary_raw_event);
    drop(db);

    server.abort();
}

#[tokio::test]
#[ignore = "requires explicit Google Cloud project, location, reasoning engine, and ADC"]
async fn test_vertex_live_ga_v1_canary() {
    if std::env::var("ADK_VERTEX_LIVE_TEST").as_deref() != Ok("1") {
        return;
    }
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")
        .expect("GOOGLE_CLOUD_PROJECT is required when ADK_VERTEX_LIVE_TEST=1");
    let location = std::env::var("GOOGLE_CLOUD_LOCATION")
        .expect("GOOGLE_CLOUD_LOCATION is required when ADK_VERTEX_LIVE_TEST=1");
    let reasoning_engine = std::env::var("GOOGLE_CLOUD_REASONING_ENGINE_ID")
        .expect("GOOGLE_CLOUD_REASONING_ENGINE_ID is required when ADK_VERTEX_LIVE_TEST=1");
    for (name, value) in [
        ("GOOGLE_CLOUD_PROJECT", &project),
        ("GOOGLE_CLOUD_LOCATION", &location),
        ("GOOGLE_CLOUD_REASONING_ENGINE_ID", &reasoning_engine),
    ] {
        assert!(!value.trim().is_empty(), "{name} must not be empty when ADK_VERTEX_LIVE_TEST=1");
    }

    let service = VertexAiSessionService::new_with_adc(
        VertexAiSessionConfig::new(project, location).with_reasoning_engine(reasoning_engine),
    )
    .expect("build live GA v1 Vertex session service");
    let app_name = "adk-rust-ga-v1-canary".to_string();
    let user_id = format!("canary-{}", SessionId::generate());
    let session_id = SessionId::generate().to_string();

    service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: Some(session_id.clone()),
            state: HashMap::from([("canary".to_string(), json!("ga-v1"))]),
        })
        .await
        .expect("create live canary session");

    let verification: std::result::Result<(), String> = async {
        let mut event = Event::new("live-ga-v1");
        event.author = "model".to_string();
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::Text { text: "GA v1 canary".to_string() },
                Part::FunctionCall {
                    name: "canary_tool".to_string(),
                    args: json!({ "probe": true }),
                    id: Some("canary-call".to_string()),
                    thought_signature: None,
                },
            ],
        });
        let expected_event = serde_json::to_value(&event)
            .map_err(|error| format!("serialize expected event: {error}"))?;
        service
            .append_event_for_identity(AppendEventRequest {
                identity: identity(&app_name, &user_id, &session_id),
                event,
            })
            .await
            .map_err(|error| format!("append representative event: {error}"))?;

        let fetched = service
            .get(GetRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
            .map_err(|error| format!("get canary session: {error}"))?;
        if fetched.state().get("canary") != Some(json!("ga-v1")) {
            return Err("get did not preserve canary state".to_string());
        }
        let Some(restored_event) = fetched.events().at(0) else {
            return Err("get did not return the appended event".to_string());
        };
        let restored_event = serde_json::to_value(restored_event)
            .map_err(|error| format!("serialize restored event: {error}"))?;
        if restored_event != expected_event {
            return Err("get did not preserve representative event fidelity".to_string());
        }

        let listed = service
            .list(ListRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                limit: None,
                offset: None,
            })
            .await
            .map_err(|error| format!("list canary session: {error}"))?;
        if !listed.iter().any(|session| session.id() == session_id) {
            return Err("list did not return the canary session".to_string());
        }
        Ok(())
    }
    .await;

    let cleanup = service
        .delete(DeleteRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: session_id.clone(),
        })
        .await;
    match (verification, cleanup) {
        (Err(verification), Err(cleanup)) => {
            panic!("live canary failed: {verification}; cleanup also failed: {cleanup}")
        }
        (Err(verification), Ok(())) => panic!("live canary failed: {verification}"),
        (Ok(()), Err(cleanup)) => panic!("live canary cleanup failed: {cleanup}"),
        (Ok(()), Ok(())) => {}
    }

    match service
        .get(GetRequest { app_name, user_id, session_id, num_recent_events: None, after: None })
        .await
    {
        Ok(_) => panic!("deleted live canary session still exists"),
        Err(error) => assert!(error.is_not_found(), "expected not-found after delete: {error}"),
    }
}
