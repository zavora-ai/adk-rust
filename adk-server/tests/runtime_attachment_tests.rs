//! Integration tests for runtime endpoints with attachment support

use adk_server::create_app;
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use async_stream::stream;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

// Mock implementations
struct MockAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for MockAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Ok(Arc::new(MockAgent))
    }

    fn list_agents(&self) -> Vec<String> {
        vec!["test-app".to_string()]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        Arc::new(MockAgent)
    }
}

// Mock agent that captures content parts
struct MockAgent;

#[async_trait]
impl adk_core::Agent for MockAgent {
    fn name(&self) -> &str {
        "test-agent"
    }

    fn description(&self) -> &str {
        "Test agent with attachment support"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            yield Ok(adk_core::Event::new("test-invocation"));
        };
        Ok(Box::pin(s))
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

fn create_test_app() -> axum::Router {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    create_app(config)
}

// Helper to decode a small PNG for testing
fn test_png_base64() -> &'static str {
    // 1x1 transparent PNG
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
}

#[tokio::test]
async fn test_run_sse_with_text_only() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Hello, world!",
        "ui_protocol": null
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_with_single_attachment() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Describe this image",
        "attachments": [
            {
                "name": "test.png",
                "type": "image/png",
                "base64": test_png_base64()
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_with_multiple_attachments() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Compare these images",
        "attachments": [
            {
                "name": "image1.png",
                "type": "image/png",
                "base64": test_png_base64()
            },
            {
                "name": "image2.png",
                "type": "image/png",
                "base64": test_png_base64()
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_with_invalid_base64() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Test",
        "attachments": [
            {
                "name": "invalid.png",
                "type": "image/png",
                "base64": "not-valid-base64!!!"
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_run_sse_compat_with_text_only() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "Hello, world!"}
            ]
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_compat_with_inline_data() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "Describe this image"},
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": test_png_base64()
                    }
                }
            ]
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_compat_with_multiple_inline_data() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "Compare these images"},
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": test_png_base64()
                    }
                },
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": test_png_base64()
                    }
                }
            ]
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_compat_with_invalid_inline_data_base64() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "Test"},
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": "not-valid-base64!!!"
                    }
                }
            ]
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_run_sse_compat_with_mixed_parts() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "First text"},
                {"text": "Second text"},
                {
                    "inlineData": {
                        "mimeType": "image/jpeg",
                        "data": test_png_base64()
                    }
                },
                {"text": "After image"}
            ]
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_with_empty_attachments() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Test",
        "attachments": []
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_compat_with_empty_parts() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": []
        },
        "streaming": true
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run_sse")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_run_sse_with_different_mime_types() {
    let app = create_test_app();

    let body = serde_json::json!({
        "new_message": "Multiple file types",
        "attachments": [
            {
                "name": "image.png",
                "type": "image/png",
                "base64": test_png_base64()
            },
            {
                "name": "document.pdf",
                "type": "application/pdf",
                "base64": "JVBERi0xLjQKJdPr6eEKMSAwIG9iago8PAovVHlwZSAvQ2F0YWxvZwovUGFnZXMgMiAwIFIKPj4KZW5kb2JqCjIgMCBvYmoKPDwKL1R5cGUgL1BhZ2VzCi9LaWRzIFszIDAgUl0KL0NvdW50IDEKPj4KZW5kb2JqCjMgMCBvYmoKPDwKL1R5cGUgL1BhZ2UKL1BhcmVudCAyIDAgUgovTWVkaWFCb3ggWzAgMCA2MTIgNzkyXQo+PgplbmRvYmoKeHJlZgowIDQKMDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAKMDAwMDAwMDExNSAwMDAwMCBuIAp0cmFpbGVyCjw8Ci9TaXplIDQKL1Jvb3QgMSAwIFIKPj4Kc3RhcnR4cmVmCjE2NQolJUVPRg=="
            },
            {
                "name": "text.txt",
                "type": "text/plain",
                "base64": "SGVsbG8gV29ybGQh"
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/run/test-app/user123/session456")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
