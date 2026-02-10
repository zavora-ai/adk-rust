use crate::{
    CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP, ListRequest, Session,
    SessionService, State,
};
use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const SESSION_API_VERSION: &str = "v1beta1";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Debug, Clone)]
pub struct VertexAiSessionConfig {
    pub project_id: String,
    pub location: String,
    pub reasoning_engine: Option<String>,
    pub endpoint: Option<String>,
}

impl VertexAiSessionConfig {
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            reasoning_engine: None,
            endpoint: None,
        }
    }

    pub fn with_reasoning_engine(mut self, reasoning_engine: impl Into<String>) -> Self {
        self.reasoning_engine = Some(reasoning_engine.into());
        self
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        let ep = self
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location));
        // Enforce HTTPS for non-localhost endpoints to prevent cleartext transmission
        if !ep.starts_with("https://")
            && !ep.contains("://127.0.0.1")
            && !ep.contains("://localhost")
        {
            format!("https://{}", ep.trim_start_matches("http://"))
        } else {
            ep
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionScope {
    app_name: String,
    user_id: String,
}

pub struct VertexAiSessionService {
    http_client: Client,
    endpoint: String,
    project_id: String,
    location: String,
    reasoning_engine: Option<String>,
    credentials: Credentials,
    auth_headers: Arc<RwLock<Option<reqwest::header::HeaderMap>>>,
    session_scopes: Arc<RwLock<HashMap<String, Vec<SessionScope>>>>,
}

impl VertexAiSessionService {
    pub fn new_with_adc(config: VertexAiSessionConfig) -> Result<Self> {
        let credentials = credentials::Builder::default()
            .with_scopes([CLOUD_PLATFORM_SCOPE])
            .build()
            .map_err(|e| {
                AdkError::Session(format!("failed to build vertex session ADC credentials: {e}"))
            })?;

        Ok(Self::with_credentials(config, credentials))
    }

    pub fn with_credentials(config: VertexAiSessionConfig, credentials: Credentials) -> Self {
        Self {
            http_client: Client::new(),
            endpoint: config.endpoint(),
            project_id: config.project_id,
            location: config.location,
            reasoning_engine: config.reasoning_engine,
            credentials,
            auth_headers: Arc::new(RwLock::new(None)),
            session_scopes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn session_error(message: impl Into<String>) -> AdkError {
        AdkError::Session(message.into())
    }

    fn endpoint_base(&self) -> &str {
        self.endpoint.trim_end_matches('/')
    }

    /// Build a URL from the endpoint base, ensuring HTTPS for non-localhost endpoints.
    /// This prevents cleartext transmission of sensitive session data.
    fn build_url(&self, path: &str) -> Result<String> {
        let base = self.endpoint_base();
        let url = format!("{}/{}", base, path);
        // Verify the constructed URL uses HTTPS (except localhost for testing)
        if !url.starts_with("https://")
            && !url.starts_with("http://127.0.0.1")
            && !url.starts_with("http://localhost")
        {
            return Err(Self::session_error(
                "Vertex AI endpoint must use HTTPS for secure transmission of session data",
            ));
        }
        Ok(url)
    }

    fn resolve_reasoning_engine_id(&self, app_name: &str) -> Result<String> {
        if let Some(reasoning_engine) = &self.reasoning_engine {
            if reasoning_engine.trim().is_empty() {
                return Err(Self::session_error("reasoning_engine cannot be empty"));
            }
            return Ok(reasoning_engine.clone());
        }

        if app_name.trim().is_empty() {
            return Err(Self::session_error(
                "app_name is required to resolve a Vertex reasoning engine",
            ));
        }

        if app_name.chars().all(|c| c.is_ascii_digit()) {
            return Ok(app_name.to_string());
        }

        if let Some(reasoning_engine) = extract_reasoning_engine_id_from_resource_name(app_name) {
            return Ok(reasoning_engine);
        }

        Err(Self::session_error(format!(
            "app_name '{app_name}' is not valid. Provide a reasoning engine numeric ID, or a full resource name projects/*/locations/*/reasoningEngines/*",
        )))
    }

    fn session_parent(&self, app_name: &str) -> Result<String> {
        let reasoning_engine = self.resolve_reasoning_engine_id(app_name)?;
        Ok(format!(
            "projects/{}/locations/{}/reasoningEngines/{reasoning_engine}",
            self.project_id, self.location,
        ))
    }

    fn session_name_from_app(&self, app_name: &str, session_id: &str) -> Result<String> {
        if session_id.trim().is_empty() {
            return Err(Self::session_error("session_id cannot be empty"));
        }

        Ok(format!("{}/sessions/{session_id}", self.session_parent(app_name)?))
    }

    fn session_name_from_engine_id(&self, reasoning_engine: &str, session_id: &str) -> String {
        format!(
            "projects/{}/locations/{}/reasoningEngines/{reasoning_engine}/sessions/{session_id}",
            self.project_id, self.location,
        )
    }

    fn remember_session_scope(&self, session_id: &str, app_name: &str, user_id: &str) {
        let mut scopes = self.session_scopes.write().expect("vertex session scope lock poisoned");
        let entry = scopes.entry(session_id.to_string()).or_default();
        let scope = SessionScope { app_name: app_name.to_string(), user_id: user_id.to_string() };
        if !entry.contains(&scope) {
            entry.push(scope);
        }
    }

    fn forget_session_scope(&self, session_id: &str, app_name: &str, user_id: &str) {
        let mut scopes = self.session_scopes.write().expect("vertex session scope lock poisoned");
        if let Some(existing) = scopes.get_mut(session_id) {
            existing.retain(|scope| !(scope.app_name == app_name && scope.user_id == user_id));
            if existing.is_empty() {
                scopes.remove(session_id);
            }
        }
    }

    fn resolve_session_name_for_append(&self, session_id: &str) -> Result<String> {
        if let Some(reasoning_engine) = &self.reasoning_engine {
            return Ok(self.session_name_from_engine_id(reasoning_engine, session_id));
        }

        let scopes = self.session_scopes.read().expect("vertex session scope lock poisoned");
        let candidates = scopes.get(session_id).ok_or_else(|| {
            Self::session_error(format!(
                "session '{session_id}' is not in the vertex session scope cache. Call create/get/list first.",
            ))
        })?;

        let mut app_names =
            candidates.iter().map(|scope| scope.app_name.as_str()).collect::<Vec<_>>();
        app_names.sort_unstable();
        app_names.dedup();

        if app_names.len() != 1 {
            return Err(Self::session_error(format!(
                "session_id '{session_id}' is ambiguous across app_name scopes; cannot resolve append_event target",
            )));
        }

        self.session_name_from_app(app_names[0], session_id)
    }

    async fn auth_headers(&self) -> Result<reqwest::header::HeaderMap> {
        let cacheable_headers =
            self.credentials.headers(Default::default()).await.map_err(|e| {
                Self::session_error(format!("failed to obtain google cloud auth headers: {e}"))
            })?;

        match cacheable_headers {
            CacheableResource::New { data, .. } => {
                *self.auth_headers.write().expect("vertex auth header cache lock poisoned") =
                    Some(data.clone());
                Ok(data)
            }
            CacheableResource::NotModified => self
                .auth_headers
                .read()
                .expect("vertex auth header cache lock poisoned")
                .clone()
                .ok_or_else(|| {
                    Self::session_error(
                        "google cloud credentials returned NotModified before any cached auth headers were available",
                    )
                }),
        }
    }

    async fn apply_auth(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let headers = self.auth_headers().await?;
        Ok(request.headers(headers))
    }

    async fn send_value(&self, request: RequestBuilder) -> Result<Value> {
        match self.send_value_internal(request, false).await? {
            Some(value) => Ok(value),
            None => Ok(Value::Object(Map::new())),
        }
    }

    async fn send_value_allow_not_found(&self, request: RequestBuilder) -> Result<Option<Value>> {
        self.send_value_internal(request, true).await
    }

    async fn send_value_internal(
        &self,
        request: RequestBuilder,
        allow_not_found: bool,
    ) -> Result<Option<Value>> {
        let response = request.send().await.map_err(|e| {
            Self::session_error(format!("failed to send vertex session request: {e}"))
        })?;
        let status = response.status();
        let body = response.text().await.map_err(|e| {
            Self::session_error(format!("failed to decode vertex session response body: {e}"))
        })?;

        if allow_not_found && status == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !status.is_success() {
            let body = if body.trim().is_empty() { "<empty body>".to_string() } else { body };
            return Err(Self::session_error(format!(
                "vertex session request failed with status {}: {}",
                status.as_u16(),
                truncate_for_error(&body),
            )));
        }

        if body.trim().is_empty() {
            return Ok(Some(Value::Object(Map::new())));
        }

        let value = serde_json::from_str(&body).map_err(|e| {
            Self::session_error(format!("failed to parse vertex session response JSON: {e}"))
        })?;
        Ok(Some(value))
    }

    async fn fetch_session(&self, session_name: &str) -> Result<Option<VertexSessionPayload>> {
        let url = self.build_url(&format!("{}/{}", SESSION_API_VERSION, session_name))?;
        let request = self.apply_auth(self.http_client.get(url)).await?;
        let value = match self.send_value_allow_not_found(request).await? {
            Some(value) => value,
            None => return Ok(None),
        };

        let session = serde_json::from_value(value).map_err(|e| {
            Self::session_error(format!("failed to parse vertex session payload: {e}"))
        })?;

        Ok(Some(session))
    }

    async fn list_session_events(&self, session_name: &str) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let url =
                self.build_url(&format!("{}/{}/events", SESSION_API_VERSION, session_name,))?;

            let mut request = self.http_client.get(url);
            if let Some(token) = page_token.as_ref().filter(|token| !token.is_empty()) {
                request = request.query(&[("pageToken", token)]);
            }
            request = self.apply_auth(request).await?;

            let value = self.send_value(request).await?;
            let response: VertexListEventsResponse =
                serde_json::from_value(value).map_err(|e| {
                    Self::session_error(format!("failed to parse vertex list-events response: {e}"))
                })?;

            for event in response.session_events {
                events.push(event.into_event());
            }

            if response.next_page_token.is_empty() {
                break;
            }
            page_token = Some(response.next_page_token);
        }

        Ok(events)
    }
}

#[async_trait]
impl SessionService for VertexAiSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        if req.app_name.trim().is_empty() || req.user_id.trim().is_empty() {
            return Err(Self::session_error(format!(
                "app_name and user_id are required, got app_name: '{}' user_id: '{}'",
                req.app_name, req.user_id,
            )));
        }

        if let Some(session_id) =
            req.session_id.as_ref().filter(|session_id| !session_id.is_empty())
        {
            return Err(Self::session_error(format!(
                "user-provided session_id is not supported for VertexAiSessionService: '{session_id}'",
            )));
        }

        let sanitized_state = sanitize_state_map(req.state);
        let parent = self.session_parent(&req.app_name)?;
        let url = self.build_url(&format!("{}/{}/sessions", SESSION_API_VERSION, parent))?;

        let body = VertexCreateSessionRequest {
            session: VertexCreateSession {
                user_id: req.user_id.clone(),
                session_state: (!sanitized_state.is_empty()).then_some(sanitized_state.clone()),
            },
        };

        let request = self.apply_auth(self.http_client.post(url).json(&body)).await?;
        let create_response = self.send_value(request).await?;
        let session_id =
            extract_session_id_from_create_response(&create_response).ok_or_else(|| {
                Self::session_error(format!(
                    "failed to extract session_id from create session response: {}",
                    truncate_for_error(&create_response.to_string()),
                ))
            })?;

        self.remember_session_scope(&session_id, &req.app_name, &req.user_id);

        Ok(Box::new(VertexSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: sanitized_state,
            events: Vec::new(),
            updated_at: Utc::now(),
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        if req.app_name.trim().is_empty()
            || req.user_id.trim().is_empty()
            || req.session_id.trim().is_empty()
        {
            return Err(Self::session_error(format!(
                "app_name, user_id, and session_id are required, got app_name: '{}' user_id: '{}' session_id: '{}'",
                req.app_name, req.user_id, req.session_id,
            )));
        }

        let session_name = self.session_name_from_app(&req.app_name, &req.session_id)?;
        let payload = self
            .fetch_session(&session_name)
            .await?
            .ok_or_else(|| Self::session_error("session not found"))?;

        if payload.user_id != req.user_id {
            return Err(Self::session_error(format!(
                "session '{}' does not belong to user '{}'",
                req.session_id, req.user_id,
            )));
        }

        self.remember_session_scope(&req.session_id, &req.app_name, &req.user_id);

        let mut events = self.list_session_events(&session_name).await?;

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|event| event.timestamp >= after);
        }

        let updated_at =
            payload.update_time.as_deref().and_then(parse_rfc3339_utc).unwrap_or_else(Utc::now);

        Ok(Box::new(VertexSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state: sanitize_state_map(payload.session_state),
            events,
            updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        if req.app_name.trim().is_empty() {
            return Err(Self::session_error("app_name is required"));
        }

        let parent = self.session_parent(&req.app_name)?;
        let mut sessions = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let url = self.build_url(&format!("{}/{}/sessions", SESSION_API_VERSION, parent))?;
            let mut request = self.http_client.get(url);

            if !req.user_id.trim().is_empty() {
                let filter = format!("userId=\"{}\"", req.user_id);
                request = request.query(&[("filter", filter)]);
            }
            if let Some(token) = page_token.as_ref().filter(|token| !token.is_empty()) {
                request = request.query(&[("pageToken", token.to_string())]);
            }
            request = self.apply_auth(request).await?;

            let value = self.send_value(request).await?;
            let response: VertexListSessionsResponse =
                serde_json::from_value(value).map_err(|e| {
                    Self::session_error(format!(
                        "failed to parse vertex list-sessions response: {e}"
                    ))
                })?;

            for payload in response.sessions {
                if !req.user_id.trim().is_empty() && payload.user_id != req.user_id {
                    continue;
                }

                let session_id = session_id_from_session_name(&payload.name).ok_or_else(|| {
                    Self::session_error(format!(
                        "failed to parse session id from vertex session resource name '{}'",
                        payload.name,
                    ))
                })?;

                self.remember_session_scope(&session_id, &req.app_name, &payload.user_id);

                let updated_at = payload
                    .update_time
                    .as_deref()
                    .and_then(parse_rfc3339_utc)
                    .unwrap_or_else(Utc::now);

                sessions.push(Box::new(VertexSession {
                    app_name: req.app_name.clone(),
                    user_id: payload.user_id,
                    session_id,
                    state: sanitize_state_map(payload.session_state),
                    events: Vec::new(),
                    updated_at,
                }) as Box<dyn Session>);
            }

            if response.next_page_token.is_empty() {
                break;
            }
            page_token = Some(response.next_page_token);
        }

        Ok(sessions)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        if req.app_name.trim().is_empty()
            || req.user_id.trim().is_empty()
            || req.session_id.trim().is_empty()
        {
            return Err(Self::session_error(
                "app_name, user_id, and session_id are all required and must be non-empty",
            ));
        }

        let session_name = self.session_name_from_app(&req.app_name, &req.session_id)?;
        let url = self.build_url(&format!("{}/{}", SESSION_API_VERSION, session_name))?;

        let request = self.apply_auth(self.http_client.delete(url)).await?;
        let _ = self.send_value_allow_not_found(request).await?;

        self.forget_session_scope(&req.session_id, &req.app_name, &req.user_id);

        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        if session_id.trim().is_empty() {
            return Err(Self::session_error("session_id is required for append_event"));
        }

        event.actions.state_delta = sanitize_state_map(event.actions.state_delta);

        let session_name = self.resolve_session_name_for_append(session_id)?;
        let url =
            self.build_url(&format!("{}/{}:appendEvent", SESSION_API_VERSION, session_name,))?;

        let body = build_append_event_payload(&event);

        let request = self.apply_auth(self.http_client.post(url).json(&body)).await?;
        self.send_value(request).await?;

        Ok(())
    }
}

struct VertexSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for VertexSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for VertexSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        self.state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for VertexSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

#[derive(Debug, Serialize)]
struct VertexCreateSessionRequest {
    session: VertexCreateSession,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VertexCreateSession {
    user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_state: Option<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexSessionPayload {
    name: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    session_state: HashMap<String, Value>,
    #[serde(default)]
    update_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexListSessionsResponse {
    #[serde(default)]
    sessions: Vec<VertexSessionPayload>,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexListEventsResponse {
    #[serde(default, rename = "sessionEvents", alias = "events")]
    session_events: Vec<VertexEventPayload>,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventPayload {
    #[serde(default)]
    name: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    invocation_id: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    actions: VertexEventActionsPayload,
    #[serde(default)]
    event_metadata: VertexEventMetadataPayload,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

impl VertexEventPayload {
    fn into_event(self) -> Event {
        let invocation_id = if self.invocation_id.trim().is_empty() {
            "vertex-event".to_string()
        } else {
            self.invocation_id
        };

        let mut event = Event::new(invocation_id.clone());

        if let Some(event_id) = event_id_from_resource_name(&self.name) {
            event.id = event_id;
        }
        if let Some(timestamp) = self.timestamp.as_deref().and_then(parse_rfc3339_utc) {
            event.timestamp = timestamp;
        }

        event.invocation_id = invocation_id;
        event.author = self.author;
        event.branch = self.event_metadata.branch;
        event.actions.state_delta = sanitize_state_map(self.actions.state_delta);
        event.long_running_tool_ids = self.event_metadata.long_running_tool_ids;
        event.llm_response.partial = self.event_metadata.partial;
        event.llm_response.turn_complete = self.event_metadata.turn_complete;
        event.llm_response.interrupted = self.event_metadata.interrupted;
        event.llm_response.error_code = self.error_code;
        event.llm_response.error_message = self.error_message;

        event
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventActionsPayload {
    #[serde(default)]
    state_delta: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventMetadataPayload {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    partial: bool,
    #[serde(default)]
    turn_complete: bool,
    #[serde(default)]
    interrupted: bool,
    #[serde(default)]
    long_running_tool_ids: Vec<String>,
}

fn sanitize_state_map(mut state: HashMap<String, Value>) -> HashMap<String, Value> {
    state.retain(|key, value| !key.starts_with(KEY_PREFIX_TEMP) && !value.is_null());
    state
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|dt| dt.with_timezone(&Utc))
}

fn extract_reasoning_engine_id_from_resource_name(app_name: &str) -> Option<String> {
    let segments = app_name.split('/').collect::<Vec<_>>();
    if segments.len() != 6 {
        return None;
    }

    if segments[0] != "projects" || segments[2] != "locations" || segments[4] != "reasoningEngines"
    {
        return None;
    }

    let engine_id = segments[5].trim();
    if engine_id.is_empty() {
        return None;
    }

    Some(engine_id.to_string())
}

fn session_id_from_session_name(name: &str) -> Option<String> {
    let marker = "/sessions/";
    let idx = name.rfind(marker)?;
    let remainder = &name[idx + marker.len()..];
    let session_id = remainder.split('/').next()?;
    if session_id.is_empty() {
        return None;
    }
    Some(session_id.to_string())
}

fn session_id_from_operation_name(name: &str) -> Option<String> {
    let session_marker = "/sessions/";
    let operation_marker = "/operations/";

    let start = name.rfind(session_marker)? + session_marker.len();
    let end = name.rfind(operation_marker)?;

    if start > end {
        return None;
    }

    let session_id = &name[start..end];
    if session_id.is_empty() {
        return None;
    }

    Some(session_id.to_string())
}

fn extract_session_id_from_create_response(value: &Value) -> Option<String> {
    let candidates = [
        value.get("name").and_then(Value::as_str),
        value.get("response").and_then(|response| response.get("name")).and_then(Value::as_str),
        value.get("session").and_then(|session| session.get("name")).and_then(Value::as_str),
    ];

    for candidate in candidates.into_iter().flatten() {
        if let Some(session_id) = session_id_from_operation_name(candidate) {
            return Some(session_id);
        }
        if let Some(session_id) = session_id_from_session_name(candidate) {
            return Some(session_id);
        }
    }

    None
}

fn event_id_from_resource_name(name: &str) -> Option<String> {
    let marker = "/events/";
    let idx = name.rfind(marker)?;
    let event_id = &name[idx + marker.len()..];
    if event_id.is_empty() {
        return None;
    }
    Some(event_id.to_string())
}

fn build_append_event_payload(event: &Event) -> Value {
    let mut event_payload = Map::new();
    event_payload.insert("timestamp".to_string(), Value::String(event.timestamp.to_rfc3339()));
    event_payload.insert("author".to_string(), Value::String(event.author.clone()));
    event_payload.insert("invocationId".to_string(), Value::String(event.invocation_id.clone()));

    if !event.actions.state_delta.is_empty() {
        event_payload.insert(
            "actions".to_string(),
            Value::Object(Map::from_iter([(
                "stateDelta".to_string(),
                Value::Object(Map::from_iter(
                    event
                        .actions
                        .state_delta
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone())),
                )),
            )])),
        );
    }

    let mut metadata = Map::new();
    metadata.insert("branch".to_string(), Value::String(event.branch.clone()));
    metadata.insert("partial".to_string(), Value::Bool(event.llm_response.partial));
    metadata.insert("turnComplete".to_string(), Value::Bool(event.llm_response.turn_complete));
    metadata.insert("interrupted".to_string(), Value::Bool(event.llm_response.interrupted));
    metadata.insert(
        "longRunningToolIds".to_string(),
        Value::Array(
            event
                .long_running_tool_ids
                .iter()
                .map(|tool_id| Value::String(tool_id.clone()))
                .collect(),
        ),
    );
    metadata.insert(
        "customMetadata".to_string(),
        Value::Object(Map::from_iter([(
            "adkEventId".to_string(),
            Value::String(event.id.clone()),
        )])),
    );
    event_payload.insert("eventMetadata".to_string(), Value::Object(metadata));

    if let Some(error_code) = &event.llm_response.error_code {
        event_payload.insert("errorCode".to_string(), Value::String(error_code.clone()));
    }
    if let Some(error_message) = &event.llm_response.error_message {
        event_payload.insert("errorMessage".to_string(), Value::String(error_message.clone()));
    }

    Value::Object(Map::from_iter([("event".to_string(), Value::Object(event_payload))]))
}

fn truncate_for_error(value: &str) -> String {
    const MAX_LEN: usize = 512;
    if value.len() <= MAX_LEN {
        return value.to_string();
    }
    format!("{}...", &value[..MAX_LEN])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_reasoning_engine_id_from_resource_name() {
        assert_eq!(
            extract_reasoning_engine_id_from_resource_name(
                "projects/my-project/locations/us-central1/reasoningEngines/123456",
            ),
            Some("123456".to_string())
        );
        assert_eq!(extract_reasoning_engine_id_from_resource_name("123456"), None);
    }

    #[test]
    fn test_extract_session_id_from_create_response() {
        let operation = serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/e/sessions/s-1/operations/op-1"
        });
        assert_eq!(extract_session_id_from_create_response(&operation), Some("s-1".to_string()));

        let session = serde_json::json!({
            "response": {
                "name": "projects/p/locations/l/reasoningEngines/e/sessions/s-2"
            }
        });
        assert_eq!(extract_session_id_from_create_response(&session), Some("s-2".to_string()));
    }

    #[test]
    fn test_sanitize_state_map_removes_temp_and_null() {
        let mut state = HashMap::new();
        state.insert("k".to_string(), Value::String("v".to_string()));
        state.insert("temp:k".to_string(), Value::String("temp".to_string()));
        state.insert("null".to_string(), Value::Null);

        let sanitized = sanitize_state_map(state);
        assert_eq!(sanitized.get("k"), Some(&Value::String("v".to_string())));
        assert!(!sanitized.contains_key("temp:k"));
        assert!(!sanitized.contains_key("null"));
    }
}
