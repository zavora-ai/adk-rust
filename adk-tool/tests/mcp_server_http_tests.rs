//! The MCP server's HTTP edge (spec 3.5 §7.9), driven in process: body limits, malformed
//! requests, caller refusals, sessions, host checks and the discovery documents' headers.

#[path = "common/mcp_server.rs"]
mod fixture;

use std::sync::Arc;

use adk_core::Url;
use adk_tool::mcp::server::{AiCatalogHost, McpRouterBuilder, McpSessions};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use fixture::*;
use serde_json::{Value, json};
use tower::ServiceExt;

const ACCEPT: &str = "application/json, text/event-stream";

fn builder() -> McpRouterBuilder {
    McpRouterBuilder::new(server(GateExecutor::new()))
        .public_url(Url::parse("https://agent.example.com/mcp").unwrap())
}

fn post() -> axum::http::request::Builder {
    Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::ACCEPT, ACCEPT)
        .header(header::CONTENT_TYPE, "application/json")
}

fn json_request(body: &Value) -> Request<Body> {
    post().body(Body::from(body.to_string())).unwrap()
}

async fn send(router: &Router, request: Request<Body>) -> Response {
    router.clone().oneshot(request).await.unwrap()
}

async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_body_over_the_limit_answers_413() {
    let router = builder().max_body_bytes(64).build().unwrap();
    let big = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": { "pad": "x".repeat(200) } });

    let declared = send(&router, json_request(&big)).await;
    assert_eq!(declared.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(declared).await;
    assert_eq!(body["error"]["code"], -32600);
    assert_eq!(body["error"]["message"], "the request body exceeds 64 bytes");

    let chunks =
        big.to_string().into_bytes().chunks(16).map(Bytes::copy_from_slice).collect::<Vec<_>>();
    let streamed = Body::from_stream(futures::stream::iter(
        chunks.into_iter().map(Ok::<_, std::convert::Infallible>),
    ));
    let streamed = send(&router, post().body(streamed).unwrap()).await;
    assert_eq!(
        streamed.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "no Content-Length, still bounded"
    );
}

#[tokio::test]
async fn malformed_requests_answer_json_rpc_errors() {
    let router = builder().build().unwrap();
    let cases = [
        (Body::from("{\"jsonrpc\": \"2.0\", "), -32700, Value::Null),
        (Body::from(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#), -32600, Value::Null),
        (Body::from(r#"{"hello":"world"}"#), -32600, Value::Null),
        (
            Body::from(r#"{"jsonrpc":"2.0","id":"c-9","method":"tools/call","params":{"name":3}}"#),
            -32602,
            json!("c-9"),
        ),
    ];
    for (body, code, id) in cases {
        let response = send(&router, post().body(body).unwrap()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = body_json(response).await;
        assert_eq!((body["error"]["code"].as_i64(), &body["id"]), (Some(code), &id), "{body}");
    }
}

#[tokio::test]
async fn a_refused_caller_gets_401_with_a_challenge_and_no_detail() {
    let router = builder().caller_resolver(Arc::new(Tokens::new(true))).build().unwrap();
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let response = send(&router, json_request(&ping)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(
        body_json(response).await,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {
                "code": -32603,
                "message": "auth.unauthorized (solar.caller.token)",
                "data": { "category": "unauthorized", "code": "solar.caller.token", "retryable": false },
            },
        })
    );

    let get = Request::get("/mcp")
        .header(header::HOST, "localhost")
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&router, get).await.status(),
        StatusCode::UNAUTHORIZED,
        "every method is guarded"
    );
}

#[tokio::test]
async fn an_unknown_session_answers_404() {
    let router = builder().build().unwrap();
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let mut request = json_request(&list);
    request.headers_mut().insert("mcp-session-id", "no-such-session".parse().unwrap());
    assert_eq!(send(&router, request).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_allowed_hosts_reach_the_transport() {
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let stateless = || builder().sessions(McpSessions::Stateless);

    let loopback_only = stateless().build().unwrap();
    let mut request = json_request(&ping);
    request.headers_mut().insert(header::HOST, "agent.example.com".parse().unwrap());
    assert_eq!(send(&loopback_only, request).await.status(), StatusCode::FORBIDDEN);

    let public = stateless().allowed_hosts(["agent.example.com"]).build().unwrap();
    let mut request = json_request(&ping);
    request.headers_mut().insert(header::HOST, "agent.example.com".parse().unwrap());
    assert_eq!(send(&public, request).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_stateless_endpoint_answers_in_json() {
    let router = builder().sessions(McpSessions::Stateless).build().unwrap();

    let list =
        send(&router, json_request(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })))
            .await;
    assert_eq!(list.status(), StatusCode::OK);
    assert!(list.headers().get("mcp-session-id").is_none());
    let list = body_json(list).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["get_quote", "set_reminder", "book_install", "lookup_order", "cancel_order"]
    );
    assert_eq!(
        list["result"]["tools"][0]["outputSchema"]["properties"]["price"]["type"], "integer",
        "an object response schema is published as the output schema"
    );

    let call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "get_quote", "arguments": { "kw": 7 } },
    });
    let call = body_json(send(&router, json_request(&call)).await).await;
    assert_eq!(call["result"]["structuredContent"], json!({ "kw": 7, "price": 7000 }));
    assert_eq!(call["result"]["isError"], false);

    let unknown = json!({ "jsonrpc": "2.0", "id": 3, "method": "vendor/extra", "params": {} });
    let unknown = body_json(send(&router, json_request(&unknown)).await).await;
    assert_eq!(unknown["error"]["code"], -32601, "an unknown method is the transport's to answer");

    let get = Request::get("/mcp")
        .header(header::HOST, "localhost")
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, get).await.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn the_card_is_served_with_its_media_type_cors_and_cache_headers() {
    let router = builder().ai_catalog(AiCatalogHost::new("Example Solar")).build().unwrap();
    let get = |path: &str| {
        Request::get(path).header(header::HOST, "agent.example.com").body(Body::empty()).unwrap()
    };

    let card = send(&router, get("/.well-known/mcp/server-card.json")).await;
    assert_eq!(card.status(), StatusCode::OK);
    let headers = card.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "application/mcp-server-card+json");
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_METHODS], "GET");
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_HEADERS], "Content-Type, If-None-Match");
    assert_eq!(headers[header::ACCESS_CONTROL_EXPOSE_HEADERS], "ETag");
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=3600");
    let etag = headers[header::ETAG].clone();
    let body = body_json(card).await;
    assert_eq!(body["name"], "com.example/solar");
    assert_eq!(body["remotes"][0]["url"], "https://agent.example.com/mcp");

    let reserved = send(&router, get("/mcp/server-card")).await;
    assert_eq!(reserved.headers()[header::ETAG], etag, "one card at both locations");

    let mut revalidate = get("/.well-known/mcp/server-card.json");
    revalidate.headers_mut().insert(header::IF_NONE_MATCH, etag.clone());
    let unchanged = send(&router, revalidate).await;
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.headers()[header::ETAG], etag);

    let mut stale = get("/.well-known/mcp/server-card.json");
    stale.headers_mut().insert(header::IF_NONE_MATCH, "\"other\"".parse().unwrap());
    assert_eq!(send(&router, stale).await.status(), StatusCode::OK);

    let preflight = Request::options("/.well-known/mcp/server-card.json")
        .header(header::HOST, "agent.example.com")
        .body(Body::empty())
        .unwrap();
    let preflight = send(&router, preflight).await;
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

    let catalog = send(&router, get("/.well-known/ai-catalog.json")).await;
    assert_eq!(catalog.headers()[header::CONTENT_TYPE], "application/ai-catalog+json");
    assert_eq!(
        body_json(catalog).await,
        json!({
            "specVersion": "1.0",
            "host": { "displayName": "Example Solar" },
            "entries": [{
                "identifier": "urn:air:example.com:mcp:solar",
                "type": "application/mcp-server-card+json",
                "url": "https://agent.example.com/mcp/server-card",
            }],
        })
    );
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).header(header::HOST, "localhost").body(Body::empty()).unwrap()
}

#[tokio::test]
async fn without_a_public_url_no_discovery_document_is_served() {
    let router = McpRouterBuilder::new(server(GateExecutor::new())).build().unwrap();
    for path in
        ["/mcp/server-card", "/.well-known/mcp/server-card.json", "/.well-known/ai-catalog.json"]
    {
        assert_eq!(send(&router, get(path)).await.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn the_catalog_discovers_the_card_by_default_and_can_be_left_to_the_host() {
    let by_default = builder().build().unwrap();
    let catalog = body_json(send(&by_default, get("/.well-known/ai-catalog.json")).await).await;
    assert_eq!(
        catalog,
        json!({
            "specVersion": "1.0",
            "entries": [{
                "identifier": "urn:air:example.com:mcp:solar",
                "type": "application/mcp-server-card+json",
                "url": "https://agent.example.com/mcp/server-card",
            }],
        }),
        "the entry points at the location SEP-2127 reserves"
    );

    let left_to_host = builder().without_ai_catalog().build().unwrap();
    let catalog = send(&left_to_host, get("/.well-known/ai-catalog.json")).await;
    assert_eq!(catalog.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(&left_to_host, get("/mcp/server-card")).await.status(), StatusCode::OK);
}

#[test]
fn the_builder_refuses_routes_it_cannot_serve() {
    let new = || McpRouterBuilder::new(server(GateExecutor::new()));
    assert_eq!(new().path("mcp").build().unwrap_err().code, "mcp.server.path");
    assert_eq!(
        new().ai_catalog(AiCatalogHost::new("x")).build().unwrap_err().code,
        "mcp.server.ai_catalog"
    );
    let with_query = Url::parse("https://agent.example.com/mcp?tenant=1").unwrap();
    assert_eq!(new().public_url(with_query).build().unwrap_err().code, "mcp.server.public_url");
    let ftp = Url::parse("ftp://agent.example.com/mcp").unwrap();
    assert_eq!(new().public_url(ftp).build().unwrap_err().code, "mcp.server.public_url");

    let builder = new().public_url(Url::parse("https://agent.example.com/v1/mcp/").unwrap());
    assert_eq!(
        builder.server_card_url().unwrap().as_str(),
        "https://agent.example.com/v1/mcp/server-card"
    );
    assert_eq!(
        builder.well_known_card_url().unwrap().as_str(),
        "https://agent.example.com/.well-known/mcp/server-card.json"
    );
}
