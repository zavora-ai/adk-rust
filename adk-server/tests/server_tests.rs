use adk_server::create_app;
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use adk_ui::{TOOL_ENVELOPE_VERSION, UI_DEFAULT_PROTOCOL, UI_PROTOCOL_CAPABILITIES};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::stream;
use std::pin::Pin;
use std::sync::Arc;
use tower::ServiceExt;

struct MockAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for MockAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Err(adk_core::AdkError::Agent("not implemented".to_string()))
    }

    fn list_agents(&self) -> Vec<String> {
        vec![]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        panic!("MockAgentLoader has no root agent")
    }
}

struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id.unwrap_or_else(|| "generated-id".to_string()),
            app_name: req.app_name,
            user_id: req.user_id,
        }))
    }

    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: req.app_name,
            user_id: req.user_id,
        }))
    }

    async fn list(&self, _req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }

    async fn delete(&self, _req: DeleteRequest) -> adk_core::Result<()> {
        Ok(())
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> adk_core::Result<()> {
        Ok(())
    }
}

struct MockSession {
    id: String,
    app_name: String,
    user_id: String,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn adk_session::State {
        &MockState
    }

    fn events(&self) -> &dyn adk_session::Events {
        &MockEvents
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

struct MockState;
impl adk_session::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

struct MockEvents;
impl adk_session::Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        vec![]
    }
    fn len(&self) -> usize {
        0
    }
    fn at(&self, _index: usize) -> Option<&Event> {
        None
    }
}

struct StreamingTestAgent;

#[async_trait]
impl adk_core::Agent for StreamingTestAgent {
    fn name(&self) -> &str {
        "stream-test-agent"
    }

    fn description(&self) -> &str {
        "Streaming test agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<Pin<Box<dyn futures::Stream<Item = adk_core::Result<Event>> + Send>>>
    {
        let invocation_id = ctx.invocation_id().to_string();
        let output = stream::once(async move {
            let mut event = Event::new(invocation_id);
            event.author = "stream-test-agent".to_string();
            event.set_content(adk_core::Content::new("model").with_text("hello from stream"));
            Ok(event)
        });
        Ok(Box::pin(output))
    }
}

#[tokio::test]
async fn test_health_check() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ui_capabilities() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/api/ui/capabilities").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["default_protocol"], UI_DEFAULT_PROTOCOL);
    assert_eq!(json["tool_envelope_version"], TOOL_ENVELOPE_VERSION);

    let protocols = json["protocols"].as_array().unwrap();
    assert_eq!(protocols.len(), UI_PROTOCOL_CAPABILITIES.len());

    for expected in UI_PROTOCOL_CAPABILITIES {
        let entry = protocols
            .iter()
            .find(|entry| entry["protocol"] == expected.protocol)
            .unwrap_or_else(|| panic!("missing protocol capability for {}", expected.protocol));

        let versions: Vec<&str> =
            entry["versions"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        let features: Vec<&str> =
            entry["features"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();

        assert_eq!(versions, expected.versions, "version mismatch for {}", expected.protocol);
        assert_eq!(features, expected.features, "feature mismatch for {}", expected.protocol);

        match expected.deprecation {
            Some(deprecation) => {
                assert_eq!(
                    entry["deprecation"]["stage"], deprecation.stage,
                    "deprecation stage mismatch for {}",
                    expected.protocol
                );
                assert_eq!(
                    entry["deprecation"]["announcedOn"], deprecation.announced_on,
                    "deprecation announcedOn mismatch for {}",
                    expected.protocol
                );
                let sunset_target = entry["deprecation"]["sunsetTargetOn"].as_str();
                assert_eq!(
                    sunset_target, deprecation.sunset_target_on,
                    "deprecation sunsetTargetOn mismatch for {}",
                    expected.protocol
                );
                let replacements: Vec<&str> = entry["deprecation"]["replacementProtocols"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect();
                assert_eq!(
                    replacements, deprecation.replacement_protocols,
                    "deprecation replacement mismatch for {}",
                    expected.protocol
                );
            }
            None => {
                assert!(
                    entry.get("deprecation").is_none() || entry["deprecation"].is_null(),
                    "unexpected deprecation metadata for {}",
                    expected.protocol
                );
            }
        }
    }
}

#[tokio::test]
async fn test_run_sse_compat_rejects_unknown_ui_protocol() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user1",
        "sessionId": "session1",
        "newMessage": {
            "role": "user",
            "parts": [{ "text": "hello" }]
        },
        "uiProtocol": "unknown_protocol"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("Supported profiles"));
    assert!(body_str.contains("adk_ui"));
    assert!(body_str.contains("a2ui"));
}

#[tokio::test]
async fn test_run_sse_compat_header_precedence_over_body_protocol() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user1",
        "sessionId": "session1",
        "newMessage": {
            "role": "user",
            "parts": [{ "text": "hello" }]
        },
        "uiProtocol": "unknown_protocol"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .header("x-adk-ui-protocol", "a2ui")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // The header protocol takes precedence, so request proceeds past protocol validation.
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn test_run_path_rejects_unknown_ui_protocol_header() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let body = serde_json::json!({
        "new_message": "hello"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user1/session1")
                .header("content-type", "application/json")
                .header("x-adk-ui-protocol", "bad-profile")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_run_sse_compat_emits_profile_wrapped_event() {
    let agent = Arc::new(StreamingTestAgent);
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "stream-test-agent",
        "userId": "user1",
        "sessionId": "session1",
        "newMessage": {
            "role": "user",
            "parts": [{ "text": "hello" }]
        },
        "uiProtocol": "ag_ui"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"ui_protocol\":\"ag_ui\""));
    assert!(body_str.contains("\"event\""));
}

#[tokio::test]
async fn test_run_sse_compat_default_profile_emits_legacy_event_shape() {
    let agent = Arc::new(StreamingTestAgent);
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "stream-test-agent",
        "userId": "user1",
        "sessionId": "session1",
        "newMessage": {
            "role": "user",
            "parts": [{ "text": "hello" }]
        }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"author\":\"stream-test-agent\""));
    assert!(!body_str.contains("\"ui_protocol\""));
}

#[tokio::test]
async fn test_run_path_honors_ui_protocol_header() {
    let agent = Arc::new(StreamingTestAgent);
    let session_service = Arc::new(adk_session::InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: "stream-test-agent".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();

    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        session_service,
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "new_message": "hello"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/stream-test-agent/user1/session1")
                .header("content-type", "application/json")
                .header("x-adk-ui-protocol", "mcp_apps")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("\"ui_protocol\":\"mcp_apps\""));
}

#[tokio::test]
async fn test_run_sse_cors_preflight_allows_protocol_header() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/run_sse")
                .header("origin", "http://localhost:5173")
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type,x-adk-ui-protocol")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let allow_headers = response
        .headers()
        .get("access-control-allow-headers")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        allow_headers.to_ascii_lowercase().contains("x-adk-ui-protocol"),
        "cors allow headers missing x-adk-ui-protocol: {}",
        allow_headers
    );
}

#[tokio::test]
async fn test_ui_resources_register_list_and_read() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let register_body = serde_json::json!({
        "uri": "ui://tests/surface-1",
        "name": "test-surface",
        "mimeType": "text/html;profile=mcp-app",
        "text": "<html><body>surface</body></html>",
        "_meta": {
            "ui": {
                "domain": "https://example.com"
            }
        }
    });

    let register_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/resources/register")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&register_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(register_response.status(), StatusCode::CREATED);

    let list_response = app
        .clone()
        .oneshot(Request::builder().uri("/api/ui/resources").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = axum::body::to_bytes(list_response.into_body(), usize::MAX).await.unwrap();
    let list_json: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
    assert!(list_json["resources"].as_array().unwrap().iter().any(|resource| {
        resource["uri"] == "ui://tests/surface-1"
            && resource["mimeType"] == "text/html;profile=mcp-app"
    }));

    let read_response = app
        .oneshot(
            Request::builder()
                .uri("/api/ui/resources/read?uri=ui://tests/surface-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_response.status(), StatusCode::OK);
    let read_body = axum::body::to_bytes(read_response.into_body(), usize::MAX).await.unwrap();
    let read_json: serde_json::Value = serde_json::from_slice(&read_body).unwrap();
    assert_eq!(read_json["contents"][0]["uri"], "ui://tests/surface-1");
    assert_eq!(read_json["contents"][0]["mimeType"], "text/html;profile=mcp-app");
}

#[tokio::test]
async fn test_ui_resources_reject_invalid_uri() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let register_body = serde_json::json!({
        "uri": "http://invalid-uri",
        "name": "test-surface",
        "mimeType": "text/html;profile=mcp-app",
        "text": "<html><body>surface</body></html>"
    });

    let register_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/resources/register")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&register_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(register_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_ui_resources_reject_invalid_meta_domain() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let register_body = serde_json::json!({
        "uri": "ui://tests/surface-invalid-domain",
        "name": "test-surface",
        "mimeType": "text/html;profile=mcp-app",
        "text": "<html><body>surface</body></html>",
        "_meta": {
            "ui": {
                "domain": "ftp://example.com"
            }
        }
    });

    let register_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/resources/register")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&register_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(register_response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_ui_resources_reject_invalid_csp_domains() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let register_body = serde_json::json!({
        "uri": "ui://tests/surface-invalid-csp",
        "name": "test-surface",
        "mimeType": "text/html;profile=mcp-app",
        "text": "<html><body>surface</body></html>",
        "_meta": {
            "ui": {
                "csp": {
                    "connectDomains": ["https://example.com", "javascript:alert(1)"]
                }
            }
        }
    });

    let register_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/resources/register")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&register_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(register_response.status(), StatusCode::BAD_REQUEST);
}
