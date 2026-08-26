//! Contract tests for the Agent Registry v1 client and discovery tool.
//!
//! A mock Axum server captures every request (method, path, query, body) and
//! returns fixture JSON, so the tests pin both directions of the wire
//! contract: the client sends exactly the documented request shapes and
//! parses the documented responses. Live tests against the real service are
//! `#[ignore]` and gated on `GOOGLE_CLOUD_PROJECT`/`GOOGLE_CLOUD_LOCATION`.

#![cfg(feature = "vertex-agent-registry")]

use adk_core::ToolContext;
use adk_tool::vertex::agent_registry::{
    Agent, AgentCard, AgentProtocol, AgentRegistryClient, AgentRegistryConfig, AgentSearchTool,
    AgentSkill, AgentSpec, AgentSpecType, Interface, ListEndpointsRequest, McpServer,
    McpServerTool, McpToolAnnotations, ProtocolBinding, SearchComponent, SearchRequest, Service,
    ServiceRegistration,
};
use adk_tool::{SimpleToolContext, Tool};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{Method, StatusCode, Uri},
};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Option<Value>,
}

#[derive(Clone, Default)]
struct MockRegistryState {
    /// Every request in arrival order.
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    /// Fixture responses keyed by `"{METHOD} {path}"`.
    responses: Arc<Mutex<HashMap<String, (StatusCode, Value)>>>,
}

async fn handle(
    State(state): State<MockRegistryState>,
    method: Method,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let path = uri.path().to_string();
    let parsed = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice(&body) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": { "message": error.to_string() } })),
                );
            }
        }
    };
    state.requests.lock().await.push(CapturedRequest {
        method: method.to_string(),
        path: path.clone(),
        query,
        body: parsed,
    });
    match state.responses.lock().await.get(&format!("{method} {path}")) {
        Some((status, value)) => (*status, Json(value.clone())),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "no fixture registered" }))),
    }
}

async fn test_client() -> (MockRegistryState, AgentRegistryClient, tokio::task::JoinHandle<()>) {
    let state = MockRegistryState::default();
    let app = Router::new().fallback(handle).with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mock agent registry server should run");
    });

    let config =
        AgentRegistryConfig::new("test-project", "global").with_endpoint(format!("http://{addr}"));
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    let client = AgentRegistryClient::with_credentials(config, credentials)
        .expect("build test agent registry client");

    (state, client, server)
}

async fn register_fixture(
    state: &MockRegistryState,
    method: &str,
    path: &str,
    status: StatusCode,
    body: Value,
) {
    state.responses.lock().await.insert(format!("{method} {path}"), (status, body));
}

async fn captured(state: &MockRegistryState) -> Vec<CapturedRequest> {
    state.requests.lock().await.clone()
}

fn invoicer_agent_json() -> Value {
    json!({
        "name": "projects/test-project/locations/global/agents/invoicer",
        "agentId": "urn:agent:acme:billing:invoicer",
        "uid": "0000-1111",
        "displayName": "Invoicer",
        "description": "Creates and sends invoices.",
        "version": "1.2.0",
        "skills": [
            {
                "id": "create-invoice",
                "name": "Create invoice",
                "description": "Creates an invoice from a purchase order.",
                "tags": ["billing"],
                "examples": ["Create an invoice for PO-42"],
            },
        ],
        "protocols": [
            {
                "type": "A2A_AGENT",
                "protocolVersion": "0.3.0",
                "interfaces": [
                    { "url": "https://invoicer.example.com/a2a", "protocolBinding": "JSONRPC" },
                ],
            },
        ],
        "card": { "type": "A2A_AGENT_CARD", "content": { "name": "Invoicer" } },
        "attributes": { "team": "billing" },
        "createTime": "2026-01-01T00:00:00Z",
        "updateTime": "2026-01-02T00:00:00Z",
    })
}

fn invoicer_agent() -> Agent {
    Agent {
        name: "projects/test-project/locations/global/agents/invoicer".into(),
        agent_id: Some("urn:agent:acme:billing:invoicer".into()),
        uid: Some("0000-1111".into()),
        display_name: Some("Invoicer".into()),
        description: Some("Creates and sends invoices.".into()),
        version: Some("1.2.0".into()),
        skills: vec![AgentSkill {
            id: Some("create-invoice".into()),
            name: Some("Create invoice".into()),
            description: Some("Creates an invoice from a purchase order.".into()),
            tags: vec!["billing".into()],
            examples: vec!["Create an invoice for PO-42".into()],
        }],
        protocols: vec![AgentProtocol {
            protocol_type: Some("A2A_AGENT".into()),
            protocol_version: Some("0.3.0".into()),
            interfaces: vec![
                Interface::new("https://invoicer.example.com/a2a")
                    .with_protocol_binding(ProtocolBinding::Jsonrpc),
            ],
        }],
        card: Some(AgentCard {
            card_type: Some("A2A_AGENT_CARD".into()),
            content: Some(json!({ "name": "Invoicer" })),
        }),
        attributes: Some(json!({ "team": "billing" })),
        create_time: Some("2026-01-01T00:00:00Z".into()),
        update_time: Some("2026-01-02T00:00:00Z".into()),
    }
}

const SEARCH_AGENTS_PATH: &str = "/v1/projects/test-project/locations/global/agents:search";
const SEARCH_MCP_PATH: &str = "/v1/projects/test-project/locations/global/mcpServers:search";
const SERVICES_PATH: &str = "/v1/projects/test-project/locations/global/services";
const ENDPOINTS_PATH: &str = "/v1/projects/test-project/locations/global/endpoints";
const GET_AGENT_PATH: &str = "/v1/projects/test-project/locations/global/agents/invoicer";

#[tokio::test]
async fn test_search_agents_sends_documented_body_and_parses_the_response() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "POST",
        SEARCH_AGENTS_PATH,
        StatusCode::OK,
        json!({ "agents": [invoicer_agent_json()], "nextPageToken": "page-2" }),
    )
    .await;

    let response = client
        .search(
            SearchComponent::Agents,
            SearchRequest::new("billing AND invoice*").with_page_size(10),
        )
        .await
        .expect("search should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, SEARCH_AGENTS_PATH);
    assert_eq!(
        requests[0].body,
        Some(json!({ "searchString": "billing AND invoice*", "pageSize": 10 })),
    );

    assert_eq!(response.agents, vec![invoicer_agent()]);
    assert!(response.mcp_servers.is_empty());
    assert_eq!(response.next_page_token.as_deref(), Some("page-2"));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_search_mcp_servers_parses_tools_without_input_schemas() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "POST",
        SEARCH_MCP_PATH,
        StatusCode::OK,
        json!({
            "mcpServers": [
                {
                    "name": "projects/test-project/locations/global/mcpServers/ledger",
                    "mcpServerId": "ledger",
                    "displayName": "Ledger",
                    "description": "Bookkeeping tools.",
                    "interfaces": [
                        { "url": "https://ledger.example.com/mcp", "protocolBinding": "HTTP_JSON" },
                    ],
                    "tools": [
                        {
                            "name": "post_entry",
                            "description": "Posts a ledger entry.",
                            "annotations": {
                                "title": "Post entry",
                                "readOnlyHint": false,
                                "destructiveHint": false,
                                "idempotentHint": true,
                                "openWorldHint": false,
                            },
                        },
                    ],
                },
            ],
        }),
    )
    .await;

    let response = client
        .search(SearchComponent::McpServers, SearchRequest::new("ledger"))
        .await
        .expect("search should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, SEARCH_MCP_PATH);
    assert_eq!(requests[0].body, Some(json!({ "searchString": "ledger" })));

    assert_eq!(
        response.mcp_servers,
        vec![McpServer {
            name: "projects/test-project/locations/global/mcpServers/ledger".into(),
            mcp_server_id: Some("ledger".into()),
            display_name: Some("Ledger".into()),
            description: Some("Bookkeeping tools.".into()),
            interfaces: vec![
                Interface::new("https://ledger.example.com/mcp")
                    .with_protocol_binding(ProtocolBinding::HttpJson),
            ],
            tools: vec![McpServerTool {
                name: "post_entry".into(),
                description: Some("Posts a ledger entry.".into()),
                annotations: Some(McpToolAnnotations {
                    title: Some("Post entry".into()),
                    read_only_hint: Some(false),
                    destructive_hint: Some(false),
                    idempotent_hint: Some(true),
                    open_world_hint: Some(false),
                }),
            }],
            ..McpServer::default()
        }],
    );
    assert!(response.agents.is_empty());
    assert!(response.next_page_token.is_none());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_register_agent_creates_the_service_and_polls_the_lro() {
    let (state, client, server) = test_client().await;
    let operation_path = "/v1/projects/test-project/locations/global/operations/op-1";
    register_fixture(
        &state,
        "POST",
        SERVICES_PATH,
        StatusCode::OK,
        json!({
            "name": "projects/test-project/locations/global/operations/op-1",
            "done": false,
        }),
    )
    .await;
    register_fixture(
        &state,
        "GET",
        operation_path,
        StatusCode::OK,
        json!({
            "name": "projects/test-project/locations/global/operations/op-1",
            "done": true,
            "response": {
                "@type": "type.googleapis.com/google.cloud.agentregistry.v1.Service",
                "name": "projects/test-project/locations/global/services/invoicer-svc",
                "displayName": "Invoicer",
                "description": "Creates and sends invoices.",
                "agentSpec": { "type": "A2A_AGENT_CARD", "content": { "name": "Invoicer" } },
                "registryResource": "projects/test-project/locations/global/agents/invoicer",
                "createTime": "2026-01-01T00:00:00Z",
                "updateTime": "2026-01-01T00:00:00Z",
            },
        }),
    )
    .await;

    let service = client
        .register_agent(
            ServiceRegistration::new(
                "invoicer-svc",
                "Invoicer",
                AgentSpec::a2a_agent_card(json!({ "name": "Invoicer" })),
            )
            .with_description("Creates and sends invoices."),
        )
        .await
        .expect("register should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 2, "expected create + one poll, got {requests:?}");

    let create = &requests[0];
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, SERVICES_PATH);
    assert_eq!(
        create.body,
        Some(json!({
            "displayName": "Invoicer",
            "description": "Creates and sends invoices.",
            "agentSpec": { "type": "A2A_AGENT_CARD", "content": { "name": "Invoicer" } },
        })),
    );
    assert_eq!(create.query.get("serviceId").map(String::as_str), Some("invoicer-svc"));
    let request_id = create.query.get("requestId").expect("requestId query param");
    uuid::Uuid::parse_str(request_id).expect("requestId must be a UUID");

    let poll = &requests[1];
    assert_eq!(poll.method, "GET");
    assert_eq!(poll.path, operation_path);

    assert_eq!(
        service,
        Service {
            name: "projects/test-project/locations/global/services/invoicer-svc".into(),
            display_name: Some("Invoicer".into()),
            description: Some("Creates and sends invoices.".into()),
            agent_spec: Some(AgentSpec {
                spec_type: AgentSpecType::A2aAgentCard,
                content: Some(json!({ "name": "Invoicer" })),
            }),
            registry_resource: Some(
                "projects/test-project/locations/global/agents/invoicer".into()
            ),
            create_time: Some("2026-01-01T00:00:00Z".into()),
            update_time: Some("2026-01-01T00:00:00Z".into()),
            ..Service::default()
        },
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_register_agent_rejects_interfaces_alongside_an_a2a_card() {
    let (state, client, server) = test_client().await;

    let error = client
        .register_agent(
            ServiceRegistration::new(
                "invoicer-svc",
                "Invoicer",
                AgentSpec::a2a_agent_card(json!({ "name": "Invoicer" })),
            )
            .with_interfaces(vec![Interface::new("https://invoicer.example.com/a2a")]),
        )
        .await
        .expect_err("card registrations with interfaces must be rejected");
    assert!(
        error.message.contains("interfaces must be empty"),
        "unexpected error: {}",
        error.message,
    );
    assert!(captured(&state).await.is_empty(), "no request may be sent");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_get_agent_resolves_a_urn_via_search_then_get() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "POST",
        SEARCH_AGENTS_PATH,
        StatusCode::OK,
        // The search projection may be partial; the client re-fetches by name.
        json!({
            "agents": [
                {
                    "name": "projects/test-project/locations/global/agents/invoicer",
                    "agentId": "urn:agent:acme:billing:invoicer",
                },
            ],
        }),
    )
    .await;
    register_fixture(&state, "GET", GET_AGENT_PATH, StatusCode::OK, invoicer_agent_json()).await;

    let agent =
        client.get_agent("urn:agent:acme:billing:invoicer").await.expect("URN get should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 2, "expected search + get, got {requests:?}");
    assert_eq!(requests[0].path, SEARCH_AGENTS_PATH);
    assert_eq!(
        requests[0].body,
        Some(json!({
            "searchString": "agentId=\"urn:agent:acme:billing:invoicer\"",
            "pageSize": 1,
        })),
    );
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, GET_AGENT_PATH);
    assert!(requests[1].body.is_none());

    assert_eq!(agent, invoicer_agent());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_get_agent_maps_an_unknown_urn_to_not_found() {
    let (state, client, server) = test_client().await;
    register_fixture(&state, "POST", SEARCH_AGENTS_PATH, StatusCode::OK, json!({})).await;

    let error = client
        .get_agent("urn:agent:acme:billing:missing")
        .await
        .expect_err("unknown URN must be an error");
    assert!(error.is_not_found(), "unexpected error: {error:?}");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_resolve_endpoint_returns_the_first_interface_url() {
    let (state, client, server) = test_client().await;
    register_fixture(&state, "GET", GET_AGENT_PATH, StatusCode::OK, invoicer_agent_json()).await;

    let url = client
        .resolve_endpoint("projects/test-project/locations/global/agents/invoicer")
        .await
        .expect("resolve should succeed");
    assert_eq!(url, "https://invoicer.example.com/a2a");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_upstream_error_statuses_map_to_adk_error_categories() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "POST",
        SEARCH_AGENTS_PATH,
        StatusCode::NOT_FOUND,
        json!({ "error": { "code": 404, "message": "parent not found" } }),
    )
    .await;

    let error = client
        .search(SearchComponent::Agents, SearchRequest::new("q"))
        .await
        .expect_err("404 must surface as an error");
    assert!(error.is_not_found(), "unexpected error: {error:?}");
    assert_eq!(error.details.upstream_status_code, Some(404));

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_agent_search_tool_returns_discovery_entries() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "POST",
        SEARCH_AGENTS_PATH,
        StatusCode::OK,
        json!({ "agents": [invoicer_agent_json()] }),
    )
    .await;

    let tool = AgentSearchTool::new(Arc::new(client));
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());

    let ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("test"));
    let output =
        tool.execute(ctx, json!({ "query": "invoice" })).await.expect("tool should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, SEARCH_AGENTS_PATH);
    assert_eq!(requests[0].body, Some(json!({ "searchString": "invoice" })));

    assert_eq!(
        output,
        json!([
            {
                "urn": "urn:agent:acme:billing:invoicer",
                "displayName": "Invoicer",
                "description": "Creates and sends invoices.",
                "skills": [
                    {
                        "id": "create-invoice",
                        "name": "Create invoice",
                        "description": "Creates an invoice from a purchase order.",
                        "tags": ["billing"],
                        "examples": ["Create an invoice for PO-42"],
                    },
                ],
                "endpoint": "https://invoicer.example.com/a2a",
            },
        ]),
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_agent_search_tool_lists_endpoints_with_the_query_as_filter() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "GET",
        ENDPOINTS_PATH,
        StatusCode::OK,
        json!({
            "endpoints": [
                {
                    "name": "projects/test-project/locations/global/endpoints/billing-api",
                    "endpointId": "billing-api",
                    "displayName": "Billing API",
                    "description": "REST billing endpoint.",
                    "interfaces": [
                        { "url": "https://billing.example.com/v2", "protocolBinding": "HTTP_JSON" },
                    ],
                },
            ],
        }),
    )
    .await;

    let tool = AgentSearchTool::new(Arc::new(client));
    let ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("test"));
    let output = tool
        .execute(
            ctx,
            json!({ "query": "displayName=\"Billing API\"", "component_type": "endpoint" }),
        )
        .await
        .expect("tool should succeed");

    // Endpoints have no :search — the query travels as an AIP-160 filter.
    let requests = captured(&state).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, ENDPOINTS_PATH);
    assert_eq!(
        requests[0].query.get("filter").map(String::as_str),
        Some("displayName=\"Billing API\""),
    );

    assert_eq!(
        output,
        json!([
            {
                "urn": "billing-api",
                "displayName": "Billing API",
                "description": "REST billing endpoint.",
                "skills": [],
                "endpoint": "https://billing.example.com/v2",
            },
        ]),
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_agent_search_tool_rejects_unknown_component_types() {
    let (_state, client, server) = test_client().await;

    let tool = AgentSearchTool::new(Arc::new(client));
    let ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("test"));
    let error = tool
        .execute(ctx, json!({ "query": "q", "component_type": "workflow" }))
        .await
        .expect_err("unknown component_type must be rejected");
    assert!(error.message.contains("component_type"), "unexpected error: {}", error.message);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn test_list_endpoints_paginates_with_filter() {
    let (state, client, server) = test_client().await;
    register_fixture(
        &state,
        "GET",
        ENDPOINTS_PATH,
        StatusCode::OK,
        json!({ "endpoints": [], "nextPageToken": "page-2" }),
    )
    .await;

    let response = client
        .list_endpoints(
            ListEndpointsRequest::new()
                .with_filter("displayName=\"Billing API\"")
                .with_page_size(5)
                .with_page_token("page-1"),
        )
        .await
        .expect("list should succeed");

    let requests = captured(&state).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].query,
        HashMap::from([
            ("filter".to_string(), "displayName=\"Billing API\"".to_string()),
            ("pageSize".to_string(), "5".to_string()),
            ("pageToken".to_string(), "page-1".to_string()),
        ]),
    );
    assert!(response.endpoints.is_empty());
    assert_eq!(response.next_page_token.as_deref(), Some("page-2"));

    server.abort();
    let _ = server.await;
}

// ===== Live tests (require ADC and a real project) =====

#[tokio::test]
#[ignore = "requires ADC credentials and Agent Registry access (GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION)"]
async fn agent_registry_live_search_agents() {
    let config = AgentRegistryConfig::from_env().expect("agent registry env vars must be set");
    let client = AgentRegistryClient::new_with_adc(config).expect("build ADC client");

    // Any result set is valid; the call exercises auth and the wire shape.
    let response = client
        .search(SearchComponent::Agents, SearchRequest::new("agent").with_page_size(5))
        .await
        .expect("live search should succeed");
    for agent in &response.agents {
        assert!(!agent.name.is_empty(), "live agents must carry resource names");
    }
}

#[tokio::test]
#[ignore = "requires ADC credentials and Agent Registry access (GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION)"]
async fn agent_registry_live_tool_end_to_end() {
    let config = AgentRegistryConfig::from_env().expect("agent registry env vars must be set");
    let client = AgentRegistryClient::new_with_adc(config).expect("build ADC client");

    let tool = AgentSearchTool::new(Arc::new(client));
    let ctx: Arc<dyn ToolContext> = Arc::new(SimpleToolContext::new("live-test"));
    let output =
        tool.execute(ctx, json!({ "query": "agent" })).await.expect("live tool should succeed");
    assert!(output.is_array(), "tool output must be a JSON array, got: {output}");
}
