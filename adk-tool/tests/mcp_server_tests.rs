//! The MCP server round trip (spec 3.5 §7.4, §7.9): adk-tool's own client (`McpToolset` over
//! Streamable HTTP) against a live in-process `McpServer` — listing, schemas, annotations,
//! calls, error mapping, the executor hook, caller identity and sessions.

#[path = "common/mcp_server.rs"]
mod fixture;

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{Tool, ToolEffect, Toolset};
use adk_tool::mcp::rmcp::ServiceExt;
use adk_tool::mcp::rmcp::model::{CallToolRequestParams, ErrorCode};
use adk_tool::mcp::rmcp::transport::StreamableHttpClientTransport;
use adk_tool::mcp::server::{McpCaller, McpSessions};
use adk_tool::mcp::{McpAuth, McpHttpClientBuilder};
use fixture::*;
use serde_json::{Value, json};

async fn listed(running: &Running, auth: McpAuth) -> HashMap<String, Arc<dyn Tool>> {
    let toolset =
        McpHttpClientBuilder::new(running.endpoint()).with_auth(auth).connect().await.unwrap();
    toolset
        .tools(ctx())
        .await
        .unwrap()
        .into_iter()
        .map(|tool| (tool.name().to_string(), tool))
        .collect()
}

#[tokio::test]
async fn the_client_lists_each_tool_from_its_declaration() {
    let executor = GateExecutor::new();
    let server = server(executor);
    let declared: HashMap<String, Value> = server
        .tool_definitions()
        .into_iter()
        .map(|tool| (tool.name.to_string(), Value::Object((*tool.input_schema).clone())))
        .collect();
    let running = start(server, |router| router).await;
    let tools = listed(&running, McpAuth::None).await;

    let mut names: Vec<&str> = tools.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["book_install", "cancel_order", "get_quote", "lookup_order", "set_reminder"],
        "admin_report is hidden from anonymous callers"
    );
    for (name, source) in tools_by_name() {
        let Some(client) = tools.get(&name) else { continue };
        assert_eq!(client.description(), source.description(), "{name}");
        assert_eq!(client.parameters_schema().as_ref(), Some(&declared[&name]), "{name}");
        assert_eq!(client.effect(), source.effect(), "{name}: the effect survives the round trip");
    }
    assert_eq!(
        declared["get_quote"]["properties"]["kw"]["type"], "integer",
        "the schemars schema is published as is"
    );

    let hints = |name: &str| tools[name].annotations().cloned().unwrap();
    assert_eq!(
        hints("get_quote"),
        json!({
            "readOnlyHint": true,
            "idempotentHint": true,
            "destructiveHint": false,
            "openWorldHint": false,
        })
    );
    assert_eq!(
        hints("set_reminder"),
        json!({
            "readOnlyHint": false,
            "idempotentHint": true,
            "destructiveHint": true,
            "openWorldHint": false,
        })
    );
    assert_eq!(
        hints("book_install"),
        json!({
            "title": "Book an installation",
            "readOnlyHint": false,
            "idempotentHint": false,
            "destructiveHint": false,
            "openWorldHint": false,
        }),
        "the tool's own destructive and open-world hints and title are kept"
    );
}

fn tools_by_name() -> HashMap<String, Arc<dyn Tool>> {
    tools().into_iter().map(|tool| (tool.name().to_string(), tool)).collect()
}

#[tokio::test]
async fn a_call_returns_the_value_and_its_structured_content() {
    let running = start(server(GateExecutor::new()), |router| router).await;
    let tools = listed(&running, McpAuth::None).await;

    let quote = tools["get_quote"].execute(tool_ctx(), json!({ "kw": 5 })).await.unwrap();
    assert_eq!(quote["output"], json!({ "kw": 5, "price": 5000 }));

    let reminder = tools["set_reminder"].execute(tool_ctx(), json!({ "day": "friday" })).await;
    assert_eq!(reminder.unwrap(), json!({ "output": "reminder set for \"friday\"" }));
}

#[tokio::test]
async fn arguments_that_break_the_schema_are_a_tool_error_the_model_reads() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| router).await;
    let tools = listed(&running, McpAuth::None).await;

    let answer = tools["get_quote"].execute(tool_ctx(), json!({ "kw": "five" })).await.unwrap();
    let text = answer["error"].as_str().unwrap();
    assert!(text.contains("invalid arguments for 'get_quote'") && text.contains("/kw"), "{text}");
    assert_eq!(executor.seen(), vec![], "a call whose arguments break the schema never runs");
}

#[tokio::test]
async fn a_failing_tool_keeps_internal_detail_back_and_a_rejection_explains_itself() {
    let running = start(server(GateExecutor::new()), |router| router).await;
    let tools = listed(&running, McpAuth::None).await;

    let failed = tools["lookup_order"].execute(tool_ctx(), json!({})).await.unwrap();
    assert_eq!(
        failed,
        json!({ "error": "tool 'lookup_order' failed: tool.unavailable (orders.db)" }),
        "the host and credential in the error stay on the server"
    );

    let rejected = tools["cancel_order"].execute(tool_ctx(), json!({})).await.unwrap();
    assert_eq!(
        rejected,
        json!({
            "error": "tool 'cancel_order' failed: tool.conflict (orders.shipped): order A-1 has \
                      shipped and can no longer be cancelled"
        })
    );
}

#[tokio::test]
async fn the_executor_sees_every_call_with_its_caller() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(false)))
    })
    .await;
    let anonymous = listed(&running, McpAuth::None).await;
    let acme = listed(&running, McpAuth::bearer("token-acme")).await;

    anonymous["get_quote"].execute(tool_ctx(), json!({ "kw": 3 })).await.unwrap();
    acme["get_quote"].execute(tool_ctx(), json!({ "kw": 4 })).await.unwrap();
    acme["book_install"].execute(tool_ctx(), json!({ "date": "2026-11-02" })).await.unwrap();
    anonymous["lookup_order"].execute(tool_ctx(), json!({})).await.unwrap();

    let seen = executor.seen();
    let summary: Vec<(&str, &str, &Value)> = seen
        .iter()
        .map(|call| (call.tool.as_str(), call.caller.as_str(), &call.arguments))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("get_quote", McpCaller::ANONYMOUS_ID, &json!({ "kw": 3 })),
            ("get_quote", "partner:acme", &json!({ "kw": 4 })),
            ("book_install", "partner:acme", &json!({ "date": "2026-11-02" })),
            ("lookup_order", McpCaller::ANONYMOUS_ID, &json!({})),
        ]
    );
    assert!(seen.iter().all(|call| call.session_id.is_some()), "stateful calls carry the session");
    assert!(seen.iter().all(|call| !call.request_id.is_empty()));
}

#[tokio::test]
async fn a_refusal_reaches_the_client_as_a_json_rpc_error() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| router).await;
    let tools = listed(&running, McpAuth::None).await;

    let error = tools["book_install"]
        .execute(tool_ctx(), json!({ "date": "2026-11-02" }))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains(
            "tool.forbidden (solar.booking.known_tier): booking needs a caller the site knows"
        ),
        "{text}"
    );
    assert_eq!(executor.seen().len(), 1, "the executor saw the call it refused");

    let client =
        ().serve(StreamableHttpClientTransport::from_uri(running.endpoint())).await.unwrap();
    let refused = client
        .call_tool(
            CallToolRequestParams::new("book_install")
                .with_arguments(json!({ "date": "2026-11-02" }).as_object().unwrap().clone()),
        )
        .await
        .unwrap_err();
    let adk_tool::mcp::rmcp::ServiceError::McpError(data) = refused else {
        panic!("a JSON-RPC error: {refused:?}");
    };
    assert_eq!(data.code, ErrorCode::INTERNAL_ERROR);
    assert_eq!(
        data.data,
        Some(json!({
            "category": "forbidden",
            "code": "solar.booking.known_tier",
            "retryable": false,
        }))
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn a_hidden_tool_is_an_unknown_tool() {
    let executor = GateExecutor::new();
    let running = start(server(executor.clone()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(false)))
    })
    .await;
    let client =
        ().serve(StreamableHttpClientTransport::from_uri(running.endpoint())).await.unwrap();
    let hidden = client.call_tool(CallToolRequestParams::new("admin_report")).await.unwrap_err();
    let missing = client.call_tool(CallToolRequestParams::new("no_such_tool")).await.unwrap_err();
    for (error, name) in [(hidden, "admin_report"), (missing, "no_such_tool")] {
        let adk_tool::mcp::rmcp::ServiceError::McpError(data) = error else {
            panic!("a JSON-RPC error: {error:?}");
        };
        assert_eq!(data.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(data.message, format!("unknown tool '{name}'"));
    }
    assert_eq!(executor.seen(), vec![], "neither call reaches the executor");
    client.cancel().await.unwrap();

    let partner = listed(&running, McpAuth::bearer("token-acme")).await;
    let report = partner["admin_report"].execute(tool_ctx(), json!({})).await.unwrap();
    assert_eq!(report["output"], json!({ "revenue": 1 }));
}

#[tokio::test]
async fn a_caller_the_resolver_refuses_cannot_connect() {
    let running = start(server(GateExecutor::new()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(true)))
    })
    .await;
    for auth in [McpAuth::None, McpAuth::bearer("token-forged")] {
        let error = McpHttpClientBuilder::new(running.endpoint())
            .with_auth(auth)
            .connect()
            .await
            .err()
            .expect("the connection is refused");
        assert!(error.to_string().contains("connect"), "{error}");
    }
    assert_eq!(listed(&running, McpAuth::bearer("token-globex")).await.len(), 6);
}

#[tokio::test]
async fn a_session_belongs_to_the_caller_that_opened_it() {
    let running = start(server(GateExecutor::new()), |router| {
        router.caller_resolver(Arc::new(Tokens::new(true)))
    })
    .await;
    let http = reqwest::Client::new();
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "probe", "version": "1.0.0" },
        },
    });
    let post = |token: &str, body: &Value, session: Option<&str>| {
        let mut request = http
            .post(running.endpoint())
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .json(body);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request.send()
    };
    let opened = post("token-acme", &initialize, None).await.unwrap();
    assert_eq!(opened.status(), 200);
    let session = opened.headers()["mcp-session-id"].to_str().unwrap().to_string();
    drop(opened);

    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let other = post("token-globex", &list, Some(&session)).await.unwrap();
    assert_eq!(other.status(), 404, "another caller cannot use the session");

    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let owner = post("token-acme", &initialized, Some(&session)).await.unwrap();
    assert_eq!(owner.status(), 202, "the caller that opened it can");
}

#[tokio::test]
async fn the_session_limit_refuses_a_new_session_until_one_closes() {
    let running = start(server(GateExecutor::new()), |router| {
        router.sessions(McpSessions::stateful(std::time::Duration::from_secs(60), 1))
    })
    .await;
    let first = McpHttpClientBuilder::new(running.endpoint()).connect().await.unwrap();
    assert_eq!(first.tools(ctx()).await.unwrap().len(), 5);
    let second = McpHttpClientBuilder::new(running.endpoint()).connect().await;
    let error = second.err().expect("a second session is refused");
    assert!(error.to_string().contains("session limit"), "{error}");

    let http = reqwest::Client::new();
    let refused = http
        .post(running.endpoint())
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "probe", "version": "1.0.0" },
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503);

    first.close().await;
    let third = McpHttpClientBuilder::new(running.endpoint()).connect().await;
    assert!(third.is_ok(), "a closed session frees its place: {:?}", third.err());
}

#[tokio::test]
async fn a_stateless_server_round_trips_without_sessions() {
    let executor = GateExecutor::new();
    let running =
        start(server(executor.clone()), |router| router.sessions(McpSessions::Stateless)).await;
    let tools = listed(&running, McpAuth::None).await;
    assert_eq!(tools.len(), 5);
    let quote = tools["get_quote"].execute(tool_ctx(), json!({ "kw": 2 })).await.unwrap();
    assert_eq!(quote["output"], json!({ "kw": 2, "price": 2000 }));
    let seen = executor.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].session_id, None, "no session is issued");
}

#[tokio::test]
async fn the_card_and_catalog_are_served_beside_the_endpoint() {
    let running = start(server(GateExecutor::new()), |router| {
        router.ai_catalog(adk_tool::mcp::server::AiCatalogHost::new("Example Solar"))
    })
    .await;
    let http = reqwest::Client::new();
    let well_known = http
        .get(format!("{}/.well-known/mcp/server-card.json", running.base))
        .header("Accept", "application/mcp-server-card+json")
        .send()
        .await
        .unwrap();
    assert_eq!(well_known.status(), 200);
    assert_eq!(well_known.headers()["content-type"], "application/mcp-server-card+json");
    let card: Value = well_known.json().await.unwrap();
    assert_eq!(card["remotes"][0]["url"], running.endpoint());

    let reserved: Value = http
        .get(format!("{}/server-card", running.endpoint()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reserved, card, "both locations serve one card");

    let catalog: Value = http
        .get(format!("{}/.well-known/ai-catalog.json", running.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(catalog["entries"][0]["url"], format!("{}/server-card", running.endpoint()));
    let effect_of = |name: &str| tools_by_name()[name].effect();
    assert_eq!(effect_of("book_install"), ToolEffect::NonIdempotent);
}
