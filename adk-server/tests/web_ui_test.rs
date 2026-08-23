use adk_core::{
    Agent, AgentInteractionMode, AgentRelationshipKind, AgentTopology, AgentTopologyMember,
    AgentTopologyRelationship, Event, InvocationContext, Result, SingleAgentLoader,
};
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;
use adk_telemetry::SpanSink;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::stream;
use std::pin::Pin;
use std::sync::Arc;
use tower::ServiceExt;

struct MockAgent;

struct WorkflowAgent;

struct RealtimeMockAgent;

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        "mock-agent"
    }

    fn description(&self) -> &str {
        "Mock Agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    fn topology(&self) -> Option<AgentTopology> {
        Some(AgentTopology {
            root: "mock-agent".to_string(),
            coordinator: "supervisor".to_string(),
            members: vec![
                AgentTopologyMember {
                    name: "supervisor".to_string(),
                    description: "Coordinates work".to_string(),
                    coordinator: true,
                    capabilities: self.capabilities(),
                },
                AgentTopologyMember {
                    name: "researcher".to_string(),
                    description: "Researches facts".to_string(),
                    coordinator: false,
                    capabilities: self.capabilities(),
                },
            ],
            relationships: vec![AgentTopologyRelationship {
                from: "supervisor".to_string(),
                to: "researcher".to_string(),
                kind: AgentRelationshipKind::Delegate,
            }],
        })
    }

    async fn run(
        &self,
        _context: Arc<dyn InvocationContext>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<Event>> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[async_trait]
impl Agent for WorkflowAgent {
    fn name(&self) -> &str {
        "workflow"
    }

    fn description(&self) -> &str {
        "Mock Workflow"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    fn topology(&self) -> Option<AgentTopology> {
        Some(AgentTopology {
            root: "workflow".to_string(),
            coordinator: "workflow".to_string(),
            members: vec![
                AgentTopologyMember {
                    name: "workflow".to_string(),
                    description: "Workflow root".to_string(),
                    coordinator: true,
                    capabilities: self.capabilities(),
                },
                AgentTopologyMember {
                    name: "plan".to_string(),
                    description: "Planning node".to_string(),
                    coordinator: false,
                    capabilities: self.capabilities(),
                },
            ],
            relationships: vec![AgentTopologyRelationship {
                from: "workflow".to_string(),
                to: "plan".to_string(),
                kind: AgentRelationshipKind::Flow,
            }],
        })
    }

    async fn run(
        &self,
        _context: Arc<dyn InvocationContext>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<Event>> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[async_trait]
impl Agent for RealtimeMockAgent {
    fn name(&self) -> &str {
        "realtime"
    }

    fn description(&self) -> &str {
        "Realtime mock"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    fn interaction_mode(&self) -> AgentInteractionMode {
        AgentInteractionMode::Realtime
    }

    async fn run(
        &self,
        _context: Arc<dyn InvocationContext>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<Event>> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[tokio::test]
async fn test_web_ui_redirect() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    let response =
        app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get("location").unwrap(), "/ui/");
}

#[tokio::test]
async fn test_web_ui_assets() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    // Test index.html
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/ui/index.html").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-type").unwrap(), "text/html");
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-cache");
    assert!(response.headers().get("content-security-policy").is_some());

    // Test runtime-config.json
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ui/assets/config/runtime-config.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("backendUrl"));
}

#[tokio::test]
async fn test_agent_details_exposes_exact_portable_topology() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let app = create_app(ServerConfig::new(agent_loader, session_service));

    let response = app
        .oneshot(Request::builder().uri("/api/ui/agents/mock-agent").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["kind"], "team");
    assert_eq!(value["interactionMode"], "requestResponse");
    assert_eq!(value["services"]["telemetry"], false);
    assert_eq!(value["services"]["telemetryStatus"], "disabled");
    assert_eq!(value["services"]["artifacts"], false);
    assert_eq!(value["services"]["memory"], false);
    assert_eq!(value["topology"]["coordinator"], "supervisor");
    assert_eq!(value["topology"]["relationships"][0]["kind"], "delegate");
    assert_eq!(value["topology"]["relationships"][0]["to"], "researcher");
}

#[tokio::test]
async fn test_agent_details_reports_actual_telemetry_collector_state() {
    let exporter = Arc::new(adk_telemetry::AdkSpanExporter::new());
    let configured_app = create_app(
        ServerConfig::new(
            Arc::new(SingleAgentLoader::new(Arc::new(MockAgent))),
            Arc::new(InMemorySessionService::new()),
        )
        .with_span_exporter(exporter.clone()),
    );
    let response = configured_app
        .oneshot(Request::builder().uri("/api/ui/agents/mock-agent").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["services"]["telemetry"], true);
    assert_eq!(value["services"]["telemetryStatus"], "configured");

    SpanSink::export_span(
        exporter.as_ref(),
        "agent.execute",
        [
            ("gcp.vertex.agent.event_id".to_string(), "evt-1".to_string()),
            ("span_id".to_string(), "span-1".to_string()),
        ]
        .into_iter()
        .collect(),
    );
    let collecting_app = create_app(
        ServerConfig::new(
            Arc::new(SingleAgentLoader::new(Arc::new(MockAgent))),
            Arc::new(InMemorySessionService::new()),
        )
        .with_span_exporter(exporter),
    );
    let response = collecting_app
        .oneshot(Request::builder().uri("/api/ui/agents/mock-agent").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["services"]["telemetryStatus"], "collecting");
}

#[tokio::test]
async fn test_agent_details_identifies_realtime_interaction() {
    let agent = Arc::new(RealtimeMockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let app = create_app(ServerConfig::new(agent_loader, session_service));

    let response = app
        .oneshot(Request::builder().uri("/api/ui/agents/realtime").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["kind"], "realtime");
    assert_eq!(value["interactionMode"], "realtime");
}

#[tokio::test]
async fn test_agent_details_classifies_flow_topology_as_workflow() {
    let agent = Arc::new(WorkflowAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let app = create_app(ServerConfig::new(agent_loader, session_service));

    let response = app
        .oneshot(Request::builder().uri("/api/ui/agents/workflow").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["kind"], "workflow");
    assert_eq!(value["topology"]["relationships"][0]["kind"], "flow");
}

#[tokio::test]
async fn test_web_ui_index_route() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    // Test /ui/ serves index.html
    let response =
        app.oneshot(Request::builder().uri("/ui/").body(Body::empty()).unwrap()).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("content-type").unwrap().to_str().unwrap().contains("text/html")
    );
}

#[tokio::test]
async fn test_api_apps() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/api/apps").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("mock-agent"));
}

#[tokio::test]
async fn test_api_list_apps_compat() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    // Test /api/list-apps (adk-go compatible endpoint)
    let response = app
        .oneshot(
            Request::builder().uri("/api/list-apps?relative_path=./").body(Body::empty()).unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    // Should return array of agent names (strings)
    assert!(body_str.contains("mock-agent"));
}
