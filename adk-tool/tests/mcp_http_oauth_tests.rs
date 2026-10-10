//! An OAuth2-authenticated HTTP toolset keeps working after its first access
//! token expires or is rejected: each request carries the current token, and a
//! 401 fetches a new one before the request is resent.
#![cfg(feature = "http-transport")]

use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{McpAuth, McpHttpClientBuilder, OAuth2Config};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Mock {
    /// `expires_in` declared on every issued token.
    expires_in: u64,
    /// Whether a 401 carries `WWW-Authenticate`, which changes how rmcp reports it.
    challenge: bool,
    issued: Arc<AtomicUsize>,
    revoked: Arc<Mutex<HashSet<String>>>,
    rejections: Arc<AtomicUsize>,
    /// The bearer token on each accepted JSON-RPC request, by method.
    accepted: Arc<Mutex<Vec<(String, String)>>>,
}

impl Mock {
    fn new(expires_in: u64, challenge: bool) -> Self {
        Self {
            expires_in,
            challenge,
            issued: Arc::default(),
            revoked: Arc::default(),
            rejections: Arc::default(),
            accepted: Arc::default(),
        }
    }

    fn tokens_for(&self, method: &str) -> Vec<String> {
        let accepted = self.accepted.lock().unwrap();
        accepted.iter().filter(|(m, _)| m == method).map(|(_, token)| token.clone()).collect()
    }
}

async fn issue_token(State(mock): State<Mock>) -> Json<Value> {
    let serial = mock.issued.fetch_add(1, Ordering::SeqCst) + 1;
    Json(json!({
        "access_token": format!("token-{serial}"),
        "token_type": "Bearer",
        "expires_in": mock.expires_in,
    }))
}

async fn mcp(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    if token.is_empty() || mock.revoked.lock().unwrap().contains(&token) {
        mock.rejections.fetch_add(1, Ordering::SeqCst);
        return if mock.challenge {
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer error=\"invalid_token\"")],
            )
                .into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        };
    }

    let method = body["method"].as_str().unwrap_or_default().to_string();
    mock.accepted.lock().unwrap().push((method.clone(), token));
    let Some(id) = body.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let result = match method.as_str() {
        "initialize" => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "oauth-mock", "version": "1.0.0" },
        }),
        "tools/list" => json!({
            "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }],
        }),
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

async fn start(mock: Mock) -> String {
    let app = Router::new()
        .route("/token", post(issue_token))
        .route("/mcp", post(mcp).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-oauth-test"))
}

#[tokio::test]
async fn a_rejected_access_token_is_replaced_and_the_request_resent() {
    for challenge in [true, false] {
        let mock = Mock::new(3600, challenge);
        let base = start(mock.clone()).await;
        let oauth = OAuth2Config::new("client", format!("{base}/token")).with_secret("secret");
        let toolset = McpHttpClientBuilder::new(format!("{base}/mcp"))
            .with_auth(McpAuth::oauth2(oauth))
            .connect()
            .await
            .unwrap();
        assert_eq!(mock.tokens_for("initialize"), vec!["token-1"]);

        // The server revokes the token while the connection is open.
        mock.revoked.lock().unwrap().insert("token-1".to_string());
        let tools = toolset.tools(ctx()).await.unwrap_or_else(|error| {
            panic!("challenge={challenge}: discovery failed after the token was revoked: {error}")
        });

        assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["echo"]);
        assert_eq!(mock.tokens_for("tools/list"), vec!["token-2"], "challenge={challenge}");
        assert_eq!(mock.rejections.load(Ordering::SeqCst), 1, "challenge={challenge}");
        assert_eq!(mock.issued.load(Ordering::SeqCst), 2, "challenge={challenge}");
    }
}

#[tokio::test]
async fn an_access_token_is_refreshed_before_it_expires() {
    // A one-second token is refreshed once its full second has elapsed.
    let mock = Mock::new(1, true);
    let base = start(mock.clone()).await;
    let oauth = OAuth2Config::new("client", format!("{base}/token"));
    let toolset = McpHttpClientBuilder::new(format!("{base}/mcp"))
        .with_auth(McpAuth::oauth2(oauth))
        .connect()
        .await
        .unwrap();
    assert_eq!(mock.tokens_for("initialize"), vec!["token-1"]);

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    toolset.tools(ctx()).await.unwrap();

    assert_eq!(mock.tokens_for("tools/list"), vec!["token-2"]);
    assert_eq!(mock.rejections.load(Ordering::SeqCst), 0, "the refresh waited for a 401");
}
