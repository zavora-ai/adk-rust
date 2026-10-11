//! An MCP HTTP toolset authenticated through a `CredentialBroker` sends the brokered
//! token only to the resource it was minted for.
#![cfg(feature = "http-transport")]

use adk_core::{
    ActingContext, AgentId, AgentPrincipal, CredentialBroker, ReadonlyContext, Result,
    RevocationScope, ScopedToken, Toolset, Url, async_trait,
};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::McpHttpClientBuilder;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// Every JSON-RPC request the server received, as (method, Authorization header).
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, String)>>>);

async fn mcp(State(seen): State<Seen>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let method = body["method"].as_str().unwrap_or_default().to_string();
    seen.0.lock().unwrap().push((method.clone(), auth));
    let Some(id) = body.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "broker-mock", "version": "1.0.0" },
        }),
        "tools/list" => {
            json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] })
        }
        other => {
            let error = json!({ "code": -32601, "message": format!("unknown method {other}") });
            return Json(json!({ "jsonrpc": "2.0", "id": id, "error": error })).into_response();
        }
    };
    (
        [("mcp-session-id", "session-1")],
        Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
    )
        .into_response()
}

async fn start(seen: Seen) -> String {
    let app = Router::new()
        .route("/mcp", post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .with_state(seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// Issues `token-for:<resource>`, with the requested resource as the audience unless
/// `audience` overrides it.
#[derive(Debug, Default)]
struct MockBroker {
    audience: Option<Url>,
    requests: Mutex<Vec<(String, String, Vec<String>)>>,
}

#[async_trait]
impl CredentialBroker for MockBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> Result<ScopedToken> {
        self.requests.lock().unwrap().push((
            acting.agent.id.to_string(),
            resource.to_string(),
            scopes.to_vec(),
        ));
        let audience = self.audience.clone().unwrap_or_else(|| resource.clone());
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            format!("token-for:{audience}"),
            audience,
            scopes.to_vec(),
        ))
    }

    async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
        Ok(0)
    }
}

fn acting() -> ActingContext {
    ActingContext::new(
        AgentPrincipal::new(AgentId::new("spiffe://example.test/agent/researcher").unwrap()),
        "mcp-connection",
    )
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-broker-test"))
}

#[tokio::test]
async fn the_brokered_token_is_sent_to_its_own_resource() {
    let seen = Seen::default();
    let endpoint = format!("{}/mcp", start(seen.clone()).await);
    let broker = Arc::new(MockBroker::default());

    let toolset = McpHttpClientBuilder::new(&endpoint)
        .with_credential_broker(broker.clone(), acting(), vec!["tools.read".to_string()])
        .connect()
        .await
        .unwrap();
    let tools = toolset.tools(ctx()).await.unwrap();
    assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["echo"]);

    let expected = format!("Bearer token-for:{endpoint}");
    let seen = seen.0.lock().unwrap().clone();
    assert!(seen.iter().any(|(method, _)| method == "tools/list"), "{seen:?}");
    assert!(seen.iter().all(|(_, auth)| *auth == expected), "{seen:?}");
    let requests = broker.requests.lock().unwrap().clone();
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|request| *request
        == (
            "spiffe://example.test/agent/researcher".to_string(),
            endpoint.clone(),
            vec!["tools.read".to_string()],
        )));
}

#[tokio::test]
async fn a_token_minted_for_another_resource_is_never_sent() {
    let seen = Seen::default();
    let endpoint = format!("{}/mcp", start(seen.clone()).await);
    let broker = Arc::new(MockBroker {
        audience: Some(Url::parse("https://other.example.test/mcp").unwrap()),
        ..MockBroker::default()
    });

    let error = McpHttpClientBuilder::new(&endpoint)
        .with_credential_broker(broker, acting(), vec![])
        .connect()
        .await
        .err()
        .expect("a token for another resource must fail the connection");

    assert_eq!(error.code, "credential.audience_mismatch");
    assert!(seen.0.lock().unwrap().is_empty(), "no request reaches the server");
}
