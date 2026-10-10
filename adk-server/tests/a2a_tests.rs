use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult};
use adk_server::a2a::A2aClient;
use adk_server::{ServerBuilder, ServerConfig, create_app_with_a2a};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

// Simple test agent for A2A testing
struct TestAgent {
    name: String,
    description: String,
}

impl TestAgent {
    fn new(name: &str, description: &str) -> Self {
        Self { name: name.to_string(), description: description.to_string() }
    }
}

#[async_trait]
impl Agent for TestAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let invocation_id = ctx.invocation_id().to_string();
        let agent_name = self.name.clone();

        let stream = async_stream::stream! {
            let mut event = adk_core::Event::new(invocation_id);
            event.author = agent_name;
            event.llm_response.content = Some(adk_core::Content {
                role: "model".to_string(),
                parts: vec![adk_core::Part::Text {
                    text: "Hello from test agent!".to_string(),
                }],
            });
            event.llm_response.turn_complete = true;
            yield Ok(event);
        };

        Ok(Box::pin(stream))
    }
}

struct TestAgentLoader {
    agent: Arc<dyn Agent>,
}

#[async_trait]
impl adk_core::AgentLoader for TestAgentLoader {
    fn root_agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    async fn load_agent(&self, name: &str) -> AdkResult<Arc<dyn Agent>> {
        if name == self.agent.name() {
            Ok(self.agent.clone())
        } else {
            Err(adk_core::AdkError::agent(format!("Agent not found: {}", name)))
        }
    }

    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }
}

fn create_test_config() -> ServerConfig {
    let agent = Arc::new(TestAgent::new("test_agent", "A test agent for A2A"));
    let agent_loader = Arc::new(TestAgentLoader { agent });
    let session_service = Arc::new(InMemorySessionService::new());

    ServerConfig::new(agent_loader, session_service)
}

#[tokio::test]
async fn test_agent_card_endpoint() {
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request = Request::builder()
        .method("GET")
        .uri("/.well-known/agent.json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["name"], "test_agent");
    assert!(json["url"].as_str().unwrap().contains("localhost:8080"));
    assert!(json["skills"].is_array());
}

/// Discovery paths: the A2A 0.3+ well-known path and the earlier one.
const AGENT_CARD_PATHS: [&str; 2] = ["/.well-known/agent-card.json", "/.well-known/agent.json"];

async fn get_json(app: axum::Router, path: &str) -> serde_json::Value {
    let request = Request::builder().method("GET").uri(path).body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn agent_card_is_served_at_both_well_known_paths() {
    let routers = [
        (
            "create_app_with_a2a",
            create_app_with_a2a(create_test_config(), Some("http://localhost:8080")),
        ),
        (
            "ServerBuilder",
            ServerBuilder::new(create_test_config()).with_a2a("http://localhost:8080").build(),
        ),
    ];

    for (label, app) in routers {
        let [card, legacy] = AGENT_CARD_PATHS;
        let card_json = get_json(app.clone(), card).await;
        assert_eq!(card_json["name"], "test_agent", "{label}");
        assert_eq!(card_json, get_json(app, legacy).await, "{label}: both paths serve one card");
    }
}

/// Serves `app` on an ephemeral local port and returns its base URL.
async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{address}")
}

/// A router that serves one agent card, named `name`, at `path` only.
fn card_router(path: &str, name: &str) -> axum::Router {
    let card = serde_json::json!({
        "name": name,
        "description": "card fixture",
        "url": "http://127.0.0.1/a2a",
        "version": "1.0.0"
    });
    axum::Router::new().route(path, axum::routing::get(move || async move { axum::Json(card) }))
}

#[tokio::test]
async fn client_resolves_the_card_this_server_serves() {
    let base_url =
        serve(ServerBuilder::new(create_test_config()).with_a2a("http://localhost:8080").build())
            .await;

    let card = A2aClient::resolve_agent_card(&base_url).await.expect("resolve agent card");
    assert_eq!(card.name, "test_agent");
}

#[tokio::test]
async fn client_prefers_the_agent_card_json_path() {
    let base_url = serve(
        card_router("/.well-known/agent-card.json", "current")
            .merge(card_router("/.well-known/agent.json", "legacy")),
    )
    .await;

    let card = A2aClient::resolve_agent_card(&base_url).await.expect("resolve agent card");
    assert_eq!(card.name, "current");
}

#[tokio::test]
async fn client_falls_back_to_agent_json_on_404() {
    let base_url = serve(card_router("/.well-known/agent.json", "legacy")).await;

    let card = A2aClient::resolve_agent_card(&base_url).await.expect("resolve agent card");
    assert_eq!(card.name, "legacy");
}

#[tokio::test]
async fn client_does_not_fall_back_on_other_errors() {
    let app = card_router("/.well-known/agent.json", "legacy").route(
        "/.well-known/agent-card.json",
        axum::routing::get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
    );
    let base_url = serve(app).await;

    let error = A2aClient::resolve_agent_card(&base_url).await.expect_err("500 is not a 404");
    assert!(error.to_string().contains("HTTP 500"), "{error}");
}

/// The bundled v1 client fetches and parses the served card, but the card
/// advertises no `supportedInterfaces`, so the client has no endpoint to call.
#[cfg(feature = "a2a-v1")]
#[tokio::test]
async fn bundled_v1_client_finds_no_v1_interface_on_the_served_card() {
    use adk_server::a2a::client::v1_client::{A2aV1Client, V1ClientError};

    let base_url =
        serve(ServerBuilder::new(create_test_config()).with_a2a("http://localhost:8080").build())
            .await;

    let card = A2aV1Client::resolve_agent_card(&base_url).await.expect("resolve agent card");
    assert_eq!(card.name, "test_agent");
    assert!(card.supported_interfaces.is_empty());

    let error = A2aV1Client::new(card).cancel_task("task-1").await.expect_err("no endpoint");
    assert!(
        matches!(
            &error,
            V1ClientError::UnexpectedStatus { status: 0, body }
                if body == "no JSONRPC interface in agent card"
        ),
        "{error}"
    );
}

#[tokio::test]
async fn test_a2a_jsonrpc_invalid_version() {
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request_body = serde_json::json!({
        "jsonrpc": "1.0",  // Invalid version
        "method": "message/send",
        "params": {},
        "id": 1
    });

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("Content-Type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["error"].is_object());
    assert_eq!(json["error"]["code"], -32600); // Invalid request
}

#[tokio::test]
async fn test_a2a_jsonrpc_method_not_found() {
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "unknown/method",
        "params": {},
        "id": 1
    });

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("Content-Type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["error"].is_object());
    assert_eq!(json["error"]["code"], -32601); // Method not found
}

#[tokio::test]
async fn test_a2a_jsonrpc_invalid_params() {
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message/send",
        // Missing required params
        "id": 1
    });

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("Content-Type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(json["error"].is_object());
    assert_eq!(json["error"]["code"], -32602); // Invalid params
}

#[tokio::test]
async fn test_a2a_message_send() {
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message/send",
        "params": {
            "message": {
                "role": "user",
                "messageId": "msg-123",
                "parts": [{"text": "Hello agent!"}]
            }
        },
        "id": 1
    });

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("Content-Type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Should have a result (task object)
    assert!(json["result"].is_object());
    assert!(json["result"]["id"].is_string());
    assert!(json["result"]["status"].is_object());
}

#[tokio::test]
async fn test_a2a_tasks_cancel_unknown_task_is_not_found() {
    // An unknown task answers exactly like another caller's task, so cancel cannot be
    // used to probe which task IDs exist.
    let config = create_test_config();
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    let request_body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tasks/cancel",
        "params": {
            "taskId": "task-123"
        },
        "id": 1
    });

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("Content-Type", "application/json")
        .body(Body::from(request_body.to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        json,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32603, "message": "Task not found: task-123" }
        })
    );
}
