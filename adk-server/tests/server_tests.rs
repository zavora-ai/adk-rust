use adk_artifact::InMemoryArtifactService;
use adk_server::ui_protocol::{
    TOOL_ENVELOPE_VERSION, UI_DEFAULT_PROTOCOL, UI_PROTOCOL_CAPABILITIES,
};
use adk_server::ui_types::{
    McpUiBridgeSnapshot, McpUiToolResultBridge, default_mcp_ui_host_capabilities,
    default_mcp_ui_host_info,
};
use adk_server::{RequestContextError, RequestContextExtractor, create_app};
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, request::Parts};
use futures::stream;
use serde_json::{Value, json};
use std::collections::HashMap;
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

struct UnhealthySessionService;

#[async_trait]
impl SessionService for UnhealthySessionService {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        MockSessionService.create(req).await
    }

    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        MockSessionService.get(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        MockSessionService.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        MockSessionService.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> adk_core::Result<()> {
        MockSessionService.append_event(session_id, event).await
    }

    async fn health_check(&self) -> adk_core::Result<()> {
        Err(adk_core::AdkError::Session("backend unavailable".to_string()))
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

struct PartialReasoningStreamingTestAgent;

#[async_trait]
impl adk_core::Agent for PartialReasoningStreamingTestAgent {
    fn name(&self) -> &str {
        "partial-reasoning-test-agent"
    }

    fn description(&self) -> &str {
        "Streaming partial AG-UI test agent"
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
        let output = stream::iter(vec![
            {
                let mut event = Event::with_id("stream-001", &invocation_id);
                event.author = "partial-reasoning-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.content =
                    Some(adk_core::Content::new("model").with_thinking("Thinking step 1"));
                Ok(event)
            },
            {
                let mut event = Event::with_id("stream-001", &invocation_id);
                event.author = "partial-reasoning-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.content =
                    Some(adk_core::Content::new("model").with_thinking("Thinking step 2"));
                Ok(event)
            },
            {
                let mut event = Event::with_id("stream-001", &invocation_id);
                event.author = "partial-reasoning-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.content =
                    Some(adk_core::Content::new("model").with_text("Hello "));
                Ok(event)
            },
            {
                let mut event = Event::with_id("stream-001", &invocation_id);
                event.author = "partial-reasoning-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.turn_complete = true;
                event.llm_response.content =
                    Some(adk_core::Content::new("model").with_text("world"));
                Ok(event)
            },
        ]);
        Ok(Box::pin(output))
    }
}

struct PartialToolCallStreamingTestAgent;

#[async_trait]
impl adk_core::Agent for PartialToolCallStreamingTestAgent {
    fn name(&self) -> &str {
        "partial-tool-call-test-agent"
    }

    fn description(&self) -> &str {
        "Streaming partial AG-UI tool-call test agent"
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
        let output = stream::iter(vec![
            {
                let mut event = Event::with_id("tool-stream-001", &invocation_id);
                event.author = "partial-tool-call-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.content = Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::FunctionCall {
                        name: "render_screen".to_string(),
                        args: json!(
                            "{\"surface\":{\"surfaceId\":\"chunk-main\",\"catalogId\":\"catalog\",\"components\":["
                        ),
                        id: Some("tool-call-001".to_string()),
                        thought_signature: None,
                    }],
                });
                Ok(event)
            },
            {
                let mut event = Event::with_id("tool-stream-001", &invocation_id);
                event.author = "partial-tool-call-test-agent".to_string();
                event.llm_response.partial = true;
                event.llm_response.turn_complete = true;
                event.llm_response.content = Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::FunctionCall {
                        name: "render_screen".to_string(),
                        args: json!(
                            "{\"id\":\"root\",\"component\":\"Column\",\"children\":[]}],\"dataModel\":{\"mode\":\"chunked\"}}}"
                        ),
                        id: Some("tool-call-001".to_string()),
                        thought_signature: None,
                    }],
                });
                Ok(event)
            },
        ]);
        Ok(Box::pin(output))
    }
}

struct HeaderExtractor;

#[async_trait]
impl RequestContextExtractor for HeaderExtractor {
    async fn extract(
        &self,
        parts: &Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let value = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        let user_id = value.strip_prefix("Bearer ").ok_or_else(|| {
            RequestContextError::InvalidToken("expected bearer token".to_string())
        })?;

        Ok(adk_core::RequestContext {
            user_id: user_id.to_string(),
            scopes: vec!["read".to_string()],
            metadata: HashMap::new(),
        })
    }
}

fn extract_sse_json_events(body_str: &str) -> Vec<Value> {
    body_str
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| !line.trim().is_empty() && *line != "[DONE]")
        .map(|line| serde_json::from_str::<Value>(line).expect("valid SSE JSON payload"))
        .collect()
}

fn summarize_ag_ui_native_events(events: &[Value]) -> Value {
    Value::Array(
        events
            .iter()
            .map(|event| match event.get("type").and_then(Value::as_str) {
                Some("RUN_STARTED") => json!({
                    "type": "RUN_STARTED",
                    "threadId": event["threadId"],
                    "runId": event["runId"],
                    "forwardedSource": event
                        .get("input")
                        .and_then(|input| input.get("forwardedProps"))
                        .and_then(|forwarded| forwarded.get("source"))
                        .cloned()
                        .unwrap_or(Value::Null),
                }),
                Some("STATE_SNAPSHOT") => json!({
                    "type": "STATE_SNAPSHOT",
                    "snapshot": event["snapshot"],
                }),
                Some("MESSAGES_SNAPSHOT") => {
                    let messages = event
                        .get("messages")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let first_message = messages.first().cloned().unwrap_or(Value::Null);
                    let first_text = first_message
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|content| content.first())
                        .and_then(|part| part.get("text"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    json!({
                        "type": "MESSAGES_SNAPSHOT",
                        "messageCount": messages.len(),
                        "firstRole": first_message.get("role").cloned().unwrap_or(Value::Null),
                        "firstText": first_text,
                    })
                }
                Some("ACTIVITY_SNAPSHOT") => json!({
                    "type": "ACTIVITY_SNAPSHOT",
                    "messageId": event["messageId"],
                    "activityType": event["activityType"],
                    "content": event["content"],
                    "replace": event.get("replace").cloned().unwrap_or(Value::Bool(true)),
                }),
                Some("ACTIVITY_DELTA") => json!({
                    "type": "ACTIVITY_DELTA",
                    "messageId": event["messageId"],
                    "activityType": event["activityType"],
                    "patch": event["patch"],
                }),
                Some("TEXT_MESSAGE_START") => json!({
                    "type": "TEXT_MESSAGE_START",
                    "role": event["role"],
                }),
                Some("TEXT_MESSAGE_CONTENT") => json!({
                    "type": "TEXT_MESSAGE_CONTENT",
                    "delta": event["delta"],
                }),
                Some("TEXT_MESSAGE_END") => json!({
                    "type": "TEXT_MESSAGE_END",
                }),
                Some("TEXT_MESSAGE_CHUNK") => json!({
                    "type": "TEXT_MESSAGE_CHUNK",
                    "messageId": event["messageId"],
                    "role": event["role"],
                    "delta": event["delta"],
                }),
                Some("REASONING_MESSAGE_CHUNK") => json!({
                    "type": "REASONING_MESSAGE_CHUNK",
                    "messageId": event["messageId"],
                    "delta": event["delta"],
                }),
                Some("TOOL_CALL_CHUNK") => json!({
                    "type": "TOOL_CALL_CHUNK",
                    "toolCallId": event["toolCallId"],
                    "toolCallName": event["toolCallName"],
                    "delta": event["delta"],
                }),
                Some("REASONING_START") => json!({
                    "type": "REASONING_START",
                    "messageId": event["messageId"],
                }),
                Some("REASONING_END") => json!({
                    "type": "REASONING_END",
                    "messageId": event["messageId"],
                }),
                Some("RUN_FINISHED") => json!({
                    "type": "RUN_FINISHED",
                    "threadId": event["threadId"],
                    "runId": event["runId"],
                }),
                Some(other) => json!({
                    "type": other,
                }),
                None => json!({
                    "type": Value::Null,
                }),
            })
            .collect(),
    )
}

fn summarize_ui_capabilities(payload: &Value) -> Value {
    let protocols = payload.get("protocols").and_then(Value::as_array).cloned().unwrap_or_default();

    json!({
        "defaultProtocol": payload["default_protocol"],
        "toolEnvelopeVersion": payload["tool_envelope_version"],
        "protocols": protocols
            .iter()
            .map(|entry| json!({
                "protocol": entry["protocol"],
                "versions": entry["versions"],
                "implementationTier": entry["implementationTier"],
                "specTrack": entry["specTrack"],
                "features": entry["features"],
                "limitations": entry["limitations"],
            }))
            .collect::<Vec<_>>(),
    })
}

fn summarize_mcp_ui_notifications_flow(
    initialize: &Value,
    resource_change: &Value,
    tool_change: &Value,
    poll_keep: &Value,
    poll_drain: &Value,
    poll_after_drain: &Value,
) -> Value {
    fn summarize_notifications(payload: &Value) -> Vec<Value> {
        payload
            .get("notifications")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                json!({
                    "id": entry["notificationId"],
                    "method": entry["method"],
                    "revision": entry["revision"],
                })
            })
            .collect()
    }

    json!({
        "initialize": {
            "initialized": initialize["initialized"],
            "protocolVersion": initialize["protocolVersion"],
            "resourceListRevision": initialize["resourceListRevision"],
            "toolListRevision": initialize["toolListRevision"],
            "serverResourcesListChanged": initialize["hostCapabilities"]["serverResources"]["listChanged"],
            "serverToolsListChanged": initialize["hostCapabilities"]["serverTools"]["listChanged"],
        },
        "resourceChange": {
            "method": resource_change["method"],
            "revision": resource_change["revision"],
            "resourceListRevision": resource_change["resourceListRevision"],
            "pendingNotificationCount": resource_change["pendingNotificationCount"],
        },
        "toolChange": {
            "method": tool_change["method"],
            "revision": tool_change["revision"],
            "toolListRevision": tool_change["toolListRevision"],
            "pendingNotificationCount": tool_change["pendingNotificationCount"],
        },
        "pollKeep": {
            "resourceListRevision": poll_keep["resourceListRevision"],
            "toolListRevision": poll_keep["toolListRevision"],
            "notifications": summarize_notifications(poll_keep),
        },
        "pollDrain": {
            "notifications": summarize_notifications(poll_drain),
        },
        "pollAfterDrain": {
            "notifications": summarize_notifications(poll_after_drain),
        },
    })
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
    let request_id = response.headers().get("x-request-id").unwrap().to_str().unwrap();
    uuid::Uuid::parse_str(request_id).unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["components"]["session"]["status"], "healthy");
    assert_eq!(json["components"]["memory"]["status"], "not_configured");
    assert_eq!(json["components"]["artifact"]["status"], "not_configured");
}

#[tokio::test]
async fn test_health_check_reports_unhealthy_backend() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(UnhealthySessionService));
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "unhealthy");
    assert_eq!(json["components"]["session"]["status"], "unhealthy");
    assert_eq!(json["components"]["session"]["error"], "Session error: backend unavailable");
}

#[tokio::test]
async fn test_session_route_requires_auth_when_extractor_is_configured() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
            .with_request_context(Arc::new(HeaderExtractor));
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/test-app/user123/session456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_session_route_forbids_mismatched_authenticated_user() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
            .with_request_context(Arc::new(HeaderExtractor));
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/test-app/other-user/session456")
                .header("authorization", "Bearer user123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_create_session_uses_authenticated_user_id() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
            .with_request_context(Arc::new(HeaderExtractor));
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "spoofed-user",
        "sessionId": "session456"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("content-type", "application/json")
                .header("authorization", "Bearer user123")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["userId"], "user123");
}

#[tokio::test]
async fn test_artifact_route_requires_auth_when_extractor_is_configured() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
            .with_artifact_service(Arc::new(InMemoryArtifactService::new()))
            .with_request_context(Arc::new(HeaderExtractor));
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/test-app/user123/session456/artifacts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_debug_route_requires_auth_when_extractor_is_configured() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
            .with_request_context(Arc::new(HeaderExtractor));
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/debug/trace/session/session456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        let limitations: Vec<&str> =
            entry["limitations"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();

        assert_eq!(versions, expected.versions, "version mismatch for {}", expected.protocol);
        assert_eq!(
            entry["implementationTier"],
            serde_json::to_value(expected.implementation_tier).unwrap(),
            "implementationTier mismatch for {}",
            expected.protocol
        );
        assert_eq!(
            entry["specTrack"],
            serde_json::to_value(expected.spec_track).unwrap(),
            "specTrack mismatch for {}",
            expected.protocol
        );
        assert_eq!(
            entry["summary"], expected.summary,
            "summary mismatch for {}",
            expected.protocol
        );
        assert_eq!(features, expected.features, "feature mismatch for {}", expected.protocol);
        assert_eq!(
            limitations, expected.limitations,
            "limitations mismatch for {}",
            expected.protocol
        );

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

    let summary = summarize_ui_capabilities(&json);
    let expected_summary: Value =
        serde_json::from_str(include_str!("fixtures/ui_capabilities_summary.json")).unwrap();
    assert_eq!(summary, expected_summary);
}

#[tokio::test]
async fn test_mcp_ui_initialize_accepts_jsonrpc_envelope() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "ui/initialize",
        "params": {
            "appName": "demo-app",
            "userId": "user-init",
            "sessionId": "session-init",
            "protocolVersion": "2025-11-25",
            "appInfo": {
                "name": "DemoApp",
                "version": "1.2.3"
            },
            "hostContext": {
                "theme": "dark",
                "locale": "en-GB"
            }
        }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/initialize")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["jsonrpc"], "2.0");
    assert_eq!(json["id"], 7);
    assert_eq!(json["result"]["initialized"], true);
    assert_eq!(json["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(json["result"]["hostInfo"]["name"], "adk-server");
    assert!(json["result"]["hostCapabilities"]["message"].is_object());
    assert_eq!(json["result"]["hostContext"]["theme"], "dark");
    assert_eq!(json["result"]["hostContext"]["locale"], "en-GB");
    assert_eq!(json["result"]["hostContext"]["sessionId"], "session-init");
}

#[test]
fn test_mcp_ui_tool_result_helper_serializes_bridge_and_fallbacks() {
    let snapshot = McpUiBridgeSnapshot::new(
        "2025-11-25",
        true,
        default_mcp_ui_host_info(),
        default_mcp_ui_host_capabilities(),
        serde_json::json!({
            "appName": "demo-app",
            "sessionId": "session-tool-result"
        }),
    )
    .with_app_metadata(
        serde_json::json!({
            "name": "Demo UI App",
            "version": "1.2.3"
        }),
        serde_json::json!({
            "availableDisplayModes": ["inline"]
        }),
    );

    let result = snapshot.build_tool_result(
        Some(serde_json::json!({
            "surface": {
                "title": "Executive Dashboard"
            }
        })),
        Some("ui://demo/surface".to_string()),
        Some("<main>dashboard fallback</main>".to_string()),
    );

    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["resourceUri"], "ui://demo/surface");
    assert_eq!(json["html"], "<main>dashboard fallback</main>");
    assert_eq!(json["bridge"]["protocolVersion"], "2025-11-25");
    assert_eq!(json["bridge"]["structuredContent"]["surface"]["title"], "Executive Dashboard");
    assert_eq!(json["bridge"]["hostInfo"]["name"], "adk-server");
    assert!(json["bridge"]["hostCapabilities"]["message"].is_object());
    assert_eq!(json["bridge"]["hostContext"]["sessionId"], "session-tool-result");
    assert_eq!(json["bridge"]["appInfo"]["name"], "Demo UI App");
    assert_eq!(json["bridge"]["appCapabilities"]["availableDisplayModes"][0], "inline");
    assert_eq!(json["bridge"]["initialized"], true);
}

#[test]
fn test_mcp_ui_tool_result_bridge_from_host_bridge_matches_snapshot_conversion() {
    let host_info = default_mcp_ui_host_info();
    let host_capabilities = default_mcp_ui_host_capabilities();
    let host_context = serde_json::json!({
        "appName": "demo-app",
        "sessionId": "session-compare"
    });

    let from_bridge = McpUiToolResultBridge::from_host_bridge(
        "2025-11-25",
        true,
        host_info.clone(),
        host_capabilities.clone(),
        host_context.clone(),
    );
    let from_snapshot =
        McpUiBridgeSnapshot::new("2025-11-25", true, host_info, host_capabilities, host_context)
            .into_tool_result_bridge();

    let from_bridge_json = serde_json::to_value(from_bridge).unwrap();
    let from_snapshot_json = serde_json::to_value(from_snapshot).unwrap();
    assert_eq!(from_bridge_json, from_snapshot_json);
}

#[tokio::test]
async fn test_mcp_ui_message_and_model_context_endpoints_persist_session_state() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let initialize = serde_json::json!({
        "appName": "demo-app",
        "userId": "user-bridge",
        "sessionId": "session-bridge",
        "appCapabilities": {
            "availableDisplayModes": ["inline"]
        }
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/initialize")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&initialize).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let model_context_update = serde_json::json!({
        "method": "ui/update-model-context",
        "params": {
            "appName": "demo-app",
            "userId": "user-bridge",
            "sessionId": "session-bridge",
            "mode": "append",
            "content": [
                { "type": "text", "text": "alpha" }
            ],
            "structuredContent": {
                "kpi": 42
            }
        }
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/update-model-context")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&model_context_update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"]["accepted"], true);
    assert_eq!(json["result"]["modelContextRevision"], 1);
    assert_eq!(json["result"]["modelContext"].as_array().unwrap().len(), 2);

    let message = serde_json::json!({
        "appName": "demo-app",
        "userId": "user-bridge",
        "sessionId": "session-bridge",
        "role": "user",
        "content": [
            { "type": "text", "text": "approve this change" }
        ],
        "metadata": {
            "source": "test"
        }
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/message")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&message).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["accepted"], true);
    assert_eq!(json["initialized"], true);
    assert_eq!(json["messageCount"], 1);
    assert_eq!(json["lastMessage"]["metadata"]["source"], "test");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/initialize")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "appName": "demo-app",
                        "userId": "user-bridge",
                        "sessionId": "session-bridge"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["messageCount"], 1);
    assert_eq!(json["modelContextRevision"], 1);
    assert_eq!(json["modelContext"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_mcp_ui_notification_endpoints_emit_list_changed_contract() {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    let app = create_app(config);

    let initialize = serde_json::json!({
        "appName": "demo-app",
        "userId": "user-notify",
        "sessionId": "session-notify"
    });

    let initialize_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/initialize")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&initialize).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initialize_response.status(), StatusCode::OK);
    let initialize_body =
        axum::body::to_bytes(initialize_response.into_body(), usize::MAX).await.unwrap();
    let initialize_json: Value = serde_json::from_slice(&initialize_body).unwrap();

    let resource_change = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "ui/notifications/resources/list_changed",
        "params": {
            "appName": "demo-app",
            "userId": "user-notify",
            "sessionId": "session-notify",
            "params": {
                "resourceUri": "ui://demo/dashboard"
            }
        }
    });

    let resource_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/notifications/resources-list-changed")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&resource_change).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resource_response.status(), StatusCode::OK);
    let resource_body =
        axum::body::to_bytes(resource_response.into_body(), usize::MAX).await.unwrap();
    let resource_json: Value = serde_json::from_slice(&resource_body).unwrap();
    assert_eq!(resource_json["jsonrpc"], "2.0");
    assert_eq!(resource_json["id"], 11);
    assert_eq!(resource_json["result"]["method"], "ui/notifications/resources/list_changed");
    assert_eq!(resource_json["result"]["revision"], 1);

    let tool_change = serde_json::json!({
        "appName": "demo-app",
        "userId": "user-notify",
        "sessionId": "session-notify",
        "params": {
            "toolName": "render_screen"
        }
    });

    let tool_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/notifications/tools-list-changed")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&tool_change).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tool_response.status(), StatusCode::OK);
    let tool_body = axum::body::to_bytes(tool_response.into_body(), usize::MAX).await.unwrap();
    let tool_json: Value = serde_json::from_slice(&tool_body).unwrap();
    assert_eq!(tool_json["method"], "ui/notifications/tools/list_changed");
    assert_eq!(tool_json["revision"], 1);

    let poll_keep = serde_json::json!({
        "appName": "demo-app",
        "userId": "user-notify",
        "sessionId": "session-notify",
        "drain": false
    });
    let poll_keep_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/notifications/poll")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&poll_keep).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll_keep_response.status(), StatusCode::OK);
    let poll_keep_body =
        axum::body::to_bytes(poll_keep_response.into_body(), usize::MAX).await.unwrap();
    let poll_keep_json: Value = serde_json::from_slice(&poll_keep_body).unwrap();
    assert_eq!(poll_keep_json["notifications"].as_array().unwrap().len(), 2);

    let poll_drain_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/notifications/poll")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&initialize).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll_drain_response.status(), StatusCode::OK);
    let poll_drain_body =
        axum::body::to_bytes(poll_drain_response.into_body(), usize::MAX).await.unwrap();
    let poll_drain_json: Value = serde_json::from_slice(&poll_drain_body).unwrap();
    assert_eq!(poll_drain_json["notifications"].as_array().unwrap().len(), 2);

    let poll_after_drain_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/notifications/poll")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&initialize).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(poll_after_drain_response.status(), StatusCode::OK);
    let poll_after_drain_body =
        axum::body::to_bytes(poll_after_drain_response.into_body(), usize::MAX).await.unwrap();
    let poll_after_drain_json: Value = serde_json::from_slice(&poll_after_drain_body).unwrap();
    assert_eq!(poll_after_drain_json["notifications"].as_array().unwrap().len(), 0);

    let summary = summarize_mcp_ui_notifications_flow(
        &initialize_json,
        &resource_json["result"],
        &tool_json,
        &poll_keep_json,
        &poll_drain_json,
        &poll_after_drain_json,
    );
    let expected_summary: Value =
        serde_json::from_str(include_str!("fixtures/mcp_ui_notifications_summary.json")).unwrap();
    assert_eq!(summary, expected_summary);
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
async fn test_run_sse_compat_supports_protocol_native_ag_ui_transport_and_input() {
    let agent = Arc::new(StreamingTestAgent);
    let session_service = Arc::new(adk_session::InMemorySessionService::new());
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        session_service.clone(),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "stream-test-agent",
        "userId": "user1",
        "sessionId": "session-native",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": {
            "threadId": "thread-native",
            "runId": "run-native",
            "state": {
                "app:mode": "inspect",
                "count": 2
            },
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "hello native" }
                    ]
                },
                {
                    "id": "activity-native-1",
                    "role": "activity",
                    "activityType": "PROMPT_CONTEXT",
                    "content": {
                        "status": "queued",
                        "label": "Prompt queued"
                    }
                },
                {
                    "id": "activity-native-1",
                    "role": "activity",
                    "activityType": "PROMPT_CONTEXT",
                    "patch": [
                        {
                            "op": "replace",
                            "path": "/status",
                            "value": "streaming"
                        },
                        {
                            "op": "add",
                            "path": "/transport",
                            "value": "protocol_native"
                        }
                    ],
                    "replace": false,
                    "content": {
                        "status": "streaming"
                    }
                }
            ],
            "forwardedProps": {
                "source": "test-suite"
            }
        }
    });

    let response = app
        .clone()
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
    assert!(body_str.contains("\"type\":\"RUN_STARTED\""));
    assert!(body_str.contains("\"threadId\":\"thread-native\""));
    assert!(body_str.contains("\"runId\":\"run-native\""));
    assert!(body_str.contains("\"type\":\"STATE_SNAPSHOT\""));
    assert!(body_str.contains("\"app:mode\":\"inspect\""));
    assert!(body_str.contains("\"type\":\"MESSAGES_SNAPSHOT\""));
    assert!(body_str.contains("hello native"));
    assert!(body_str.contains("\"type\":\"ACTIVITY_SNAPSHOT\""));
    assert!(body_str.contains("\"type\":\"ACTIVITY_DELTA\""));
    assert!(body_str.contains("\"activityType\":\"PROMPT_CONTEXT\""));
    assert!(body_str.contains("test-suite"));
    assert!(body_str.contains("\"type\":\"TEXT_MESSAGE_START\""));
    assert!(body_str.contains("\"type\":\"TEXT_MESSAGE_CONTENT\""));
    assert!(body_str.contains("\"delta\":\"hello from stream\""));
    assert!(body_str.contains("\"type\":\"RUN_FINISHED\""));
    assert!(
        !body_str.contains("\"ui_protocol\""),
        "protocol-native ag_ui transport should not wrap generic runtime events: {}",
        body_str
    );
    let parsed_events = extract_sse_json_events(&body_str);
    let summary = summarize_ag_ui_native_events(&parsed_events);
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/ag_ui_protocol_native_summary.json"))
            .expect("valid AG-UI native fixture");
    assert_eq!(summary, expected);

    let session = session_service
        .get(GetRequest {
            app_name: "stream-test-agent".to_string(),
            user_id: "user1".to_string(),
            session_id: "session-native".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let state = session.state().all();
    assert_eq!(state.get("app:mode"), Some(&serde_json::json!("inspect")));
    assert_eq!(state.get("count"), Some(&serde_json::json!(2)));
}

#[tokio::test]
async fn test_run_sse_protocol_native_ag_ui_emits_partial_text_chunks_and_reasoning_chunks() {
    let agent = Arc::new(PartialReasoningStreamingTestAgent);
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "partial-reasoning-test-agent",
        "userId": "user1",
        "sessionId": "session-partial",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": {
            "threadId": "thread-partial",
            "runId": "run-partial",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "stream partial output" }
                    ]
                }
            ],
            "forwardedProps": {
                "source": "partial-suite"
            }
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
    assert!(body_str.contains("\"type\":\"REASONING_MESSAGE_CHUNK\""));
    assert!(body_str.contains("\"type\":\"TEXT_MESSAGE_CHUNK\""));
    assert!(!body_str.contains("\"type\":\"TEXT_MESSAGE_START\""));
    assert!(!body_str.contains("\"type\":\"TEXT_MESSAGE_END\""));

    let parsed_events = extract_sse_json_events(&body_str);
    let summary = summarize_ag_ui_native_events(&parsed_events);
    let expected: Value = serde_json::from_str(include_str!(
        "fixtures/ag_ui_protocol_native_partial_reasoning_summary.json"
    ))
    .expect("valid AG-UI partial reasoning fixture");
    assert_eq!(summary, expected);
}

#[tokio::test]
async fn test_run_sse_protocol_native_ag_ui_emits_tool_call_chunks_for_partial_string_deltas() {
    let agent = Arc::new(PartialToolCallStreamingTestAgent);
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "partial-tool-call-test-agent",
        "userId": "user1",
        "sessionId": "session-tool-chunk",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": {
            "threadId": "thread-tool-chunk",
            "runId": "run-tool-chunk",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "stream chunked tool args" }
                    ]
                }
            ],
            "forwardedProps": {
                "source": "tool-chunk-suite"
            }
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
    assert!(body_str.contains("\"type\":\"TOOL_CALL_CHUNK\""));
    assert!(!body_str.contains("\"type\":\"TOOL_CALL_ARGS\""));

    let parsed_events = extract_sse_json_events(&body_str);
    let summary = summarize_ag_ui_native_events(&parsed_events);
    let expected: Value = serde_json::from_str(include_str!(
        "fixtures/ag_ui_protocol_native_partial_tool_call_summary.json"
    ))
    .expect("valid AG-UI partial tool call fixture");
    assert_eq!(summary, expected);
}

#[tokio::test]
async fn test_run_sse_compat_rejects_protocol_native_for_mcp_apps() {
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
        "uiProtocol": "mcp_apps",
        "uiTransport": "protocol_native"
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
    assert!(body_str.contains("protocol_native transport is currently available only for ag_ui"));
    assert!(body_str.contains("bridge endpoints"));
}

#[tokio::test]
async fn test_run_sse_compat_applies_mcp_apps_runtime_bridge_payloads() {
    let agent = Arc::new(StreamingTestAgent);
    let config = adk_server::ServerConfig::new(
        Arc::new(adk_core::SingleAgentLoader::new(agent)),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "stream-test-agent",
        "userId": "user-bridge-runtime",
        "sessionId": "session-bridge-runtime",
        "newMessage": {
            "role": "user",
            "parts": [{ "text": "hello" }]
        },
        "uiProtocol": "mcp_apps",
        "mcpAppsInitialize": {
            "method": "ui/initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "appInfo": {
                    "name": "RuntimeBridgeApp",
                    "version": "0.1.0"
                }
            }
        },
        "mcpAppsRequest": {
            "method": "ui/message",
            "params": {
                "role": "user",
                "content": [
                    { "type": "text", "text": "approve from runtime" }
                ],
                "metadata": {
                    "source": "run_sse"
                }
            }
        },
        "mcpAppsInitialized": {
            "method": "ui/notifications/initialized"
        }
    });

    let response = app
        .clone()
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

    let inspect_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/ui/initialize")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "appName": "stream-test-agent",
                        "userId": "user-bridge-runtime",
                        "sessionId": "session-bridge-runtime"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(inspect_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(inspect_response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["initialized"], true);
    assert_eq!(json["protocolVersion"], "2025-11-25");
    assert_eq!(json["appInfo"]["name"], "RuntimeBridgeApp");
    assert_eq!(json["messageCount"], 1);
    assert_eq!(json["hostContext"]["sessionId"], "session-bridge-runtime");
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
                .header(
                    "access-control-request-headers",
                    "content-type,x-adk-ui-protocol,x-adk-ui-transport",
                )
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
    assert!(
        allow_headers.to_ascii_lowercase().contains("x-adk-ui-transport"),
        "cors allow headers missing x-adk-ui-transport: {}",
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
