//! Protocol-compatibility tests for the MCP client.
//!
//! These tests answer one question: does `adk-tool` still work against a server
//! that only speaks the legacy `initialize` handshake? The MCP `2026-07-28`
//! revision adds a stateless `server/discover` path, and rmcp 3.x can use it,
//! but an MCP server in the wild is under no obligation to have moved.
//!
//! Each test drives a hand-written server over an in-process duplex pipe, so it
//! needs no child process, no network, and no credentials.

#![cfg(feature = "mcp")]

use adk_core::{Content, ReadonlyContext, Toolset as _};
use adk_tool::mcp::McpToolset;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientJsonRpcMessage, ClientRequest, ContentBlock,
    ErrorCode, ErrorData, Implementation, InitializeResult, ListToolsResult, ProtocolVersion,
    ServerCapabilities, ServerJsonRpcMessage, ServerResult, Tool,
};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::{RoleServer, ServiceExt};
use serde_json::json;
use std::sync::Arc;

/// Reads one request, asserting it is `initialize`, and answers it with the
/// given protocol version. Also consumes the `notifications/initialized` that
/// the client sends to complete the legacy handshake.
///
/// Returns the protocol version the client advertised.
async fn complete_legacy_handshake<T>(
    server: &mut T,
    reply_with: ProtocolVersion,
) -> ProtocolVersion
where
    T: Transport<RoleServer> + Send,
{
    let Some(ClientJsonRpcMessage::Request(request)) = server.receive().await else {
        panic!("expected a request, and the legacy handshake sends one first");
    };
    let ClientRequest::InitializeRequest(initialize) = request.request else {
        panic!(
            "expected initialize: a client that probes with server/discover here would break every legacy server"
        );
    };
    let advertised = initialize.params.protocol_version.clone();

    let mut result = InitializeResult::new(ServerCapabilities::default());
    result.protocol_version = reply_with;
    result.server_info = Implementation::new("legacy-test-server", "1.0.0");
    server
        .send(ServerJsonRpcMessage::response(ServerResult::InitializeResult(result), request.id))
        .await
        .expect("send the initialize response");

    let Some(ClientJsonRpcMessage::Notification(_)) = server.receive().await else {
        panic!("expected notifications/initialized to close the handshake");
    };
    advertised
}

/// The smallest `ReadonlyContext` that satisfies `Toolset::tools`.
struct TestContext {
    content: Content,
}

#[async_trait::async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str {
        "protocol-compatibility-test"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

/// One tool, so `tools/list` has something to return.
fn test_tool() -> Tool {
    Tool::new(
        "echo",
        "Echoes its input.",
        Arc::new(
            json!({ "type": "object", "properties": { "text": { "type": "string" } } })
                .as_object()
                .expect("schema is an object")
                .clone(),
        ),
    )
}

/// The default `serve` path must keep sending `initialize`.
///
/// This is the backward-compatibility contract: all connection sites in this
/// crate use `ServiceExt::serve`, so if that ever starts probing with
/// `server/discover`, every server that predates `2026-07-28` breaks.
#[tokio::test]
async fn the_default_handshake_is_still_legacy_initialize() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        // Panics unless the first request is `initialize`.
        complete_legacy_handshake(&mut server, ProtocolVersion::V_2025_11_25).await
    });

    let client = ().serve(client_io).await.expect("a legacy-only server must still connect");
    let advertised = server_task.await.expect("server task");

    // The upgrade to rmcp 3.x must not move the version we put on the wire.
    assert_eq!(advertised, ProtocolVersion::V_2025_11_25);
    client.cancel().await.expect("cancel");
}

/// A server that has moved to `2026-07-28` still answers the legacy handshake,
/// so the dual-stack design means one client reaches both generations.
#[tokio::test]
async fn a_2026_07_28_server_accepts_the_legacy_handshake() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        complete_legacy_handshake(&mut server, ProtocolVersion::V_2026_07_28).await
    });

    let client = ().serve(client_io).await.expect("connect to a 2026-07-28 server");
    let negotiated =
        client.peer_info().expect("peer info is set after the handshake").protocol_version.clone();
    server_task.await.expect("server task");

    assert_eq!(negotiated, ProtocolVersion::V_2026_07_28);
    client.cancel().await.expect("cancel");
}

/// `McpToolset` lists tools from a legacy-only server.
///
/// Exercises this crate's own code, not just rmcp's: the handshake, `tools/list`,
/// and the conversion into `adk_core::Tool`.
#[tokio::test]
async fn the_toolset_lists_tools_from_a_legacy_only_server() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        complete_legacy_handshake(&mut server, ProtocolVersion::V_2025_11_25).await;

        let Some(ClientJsonRpcMessage::Request(request)) = server.receive().await else {
            panic!("expected tools/list");
        };
        assert!(matches!(request.request, ClientRequest::ListToolsRequest(_)));
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::ListToolsResult(ListToolsResult::with_all_items(vec![test_tool()])),
                request.id,
            ))
            .await
            .expect("send tools/list response");
    });

    let client = ().serve(client_io).await.expect("connect");
    let toolset = McpToolset::new(client);
    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = toolset.tools(ctx).await.expect("load tools from a legacy server");
    server_task.await.expect("server task");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "echo");
}

/// A plain `tools/call` still returns its result inline.
///
/// SEP-2663 lets a server answer with a task instead, and this crate now reads
/// the response to find out which happened. A server that never uses tasks must
/// be unaffected by that.
#[tokio::test]
async fn a_tool_call_against_a_legacy_server_returns_its_result_inline() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        complete_legacy_handshake(&mut server, ProtocolVersion::V_2025_11_25).await;

        let Some(ClientJsonRpcMessage::Request(request)) = server.receive().await else {
            panic!("expected tools/call");
        };
        let ClientRequest::CallToolRequest(call) = request.request else {
            panic!("expected tools/call");
        };
        assert_eq!(call.params.name, "echo");
        server
            .send(ServerJsonRpcMessage::response(
                ServerResult::CallToolResult(CallToolResult::success(vec![ContentBlock::text(
                    "pong",
                )])),
                request.id,
            ))
            .await
            .expect("send tools/call response");
    });

    let client = ().serve(client_io).await.expect("connect");
    let result = client
        .call_tool_once(CallToolRequestParams::new("echo").with_arguments(
            json!({ "text": "ping" }).as_object().expect("arguments are an object").clone(),
        ))
        .await
        .expect("the call must succeed against a legacy server");
    server_task.await.expect("server task");

    match result {
        rmcp::model::CallToolResponse::Complete(result) => {
            assert_eq!(result.is_error, Some(false));
            assert_eq!(
                result.content.first().and_then(|block| block.as_text()).map(|t| t.text.as_str()),
                Some("pong")
            );
        }
        other => panic!("a legacy server cannot produce anything but a complete result: {other:?}"),
    }
}

/// `Auto` recovers when the server rejects `server/discover` with
/// `METHOD_NOT_FOUND`, which is the only error it treats as proof of a legacy
/// peer.
#[tokio::test]
async fn auto_falls_back_to_legacy_on_method_not_found() {
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        let Some(ClientJsonRpcMessage::Request(discover)) = server.receive().await else {
            panic!("expected server/discover");
        };
        assert!(matches!(discover.request, ClientRequest::DiscoverRequest(_)));
        server
            .send(ServerJsonRpcMessage::error(
                ErrorData::new(ErrorCode::METHOD_NOT_FOUND, "Method not found", None),
                Some(discover.id),
            ))
            .await
            .expect("reject discover");

        complete_legacy_handshake(&mut server, ProtocolVersion::V_2025_11_25).await
    });

    let client = ()
        .serve_with_lifecycle(
            client_io,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await
        .expect("Auto must fall back to the legacy handshake");
    server_task.await.expect("server task");
    client.cancel().await.expect("cancel");
}

/// `Auto` gives up when the probe fails with anything else.
///
/// This is why the default stays `Initialize`: a legacy server is free to answer
/// an unknown method with `INVALID_REQUEST`, and `Auto` then fails a connection
/// that the legacy handshake would have completed.
#[tokio::test]
async fn auto_fails_when_discover_is_refused_with_another_error() {
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let (server_io, client_io) = tokio::io::duplex(4096);
    let mut server = IntoTransport::<RoleServer, _, _>::into_transport(server_io);

    let server_task = tokio::spawn(async move {
        let Some(ClientJsonRpcMessage::Request(discover)) = server.receive().await else {
            panic!("expected server/discover");
        };
        server
            .send(ServerJsonRpcMessage::error(
                ErrorData::new(ErrorCode::INVALID_REQUEST, "Invalid request", None),
                Some(discover.id),
            ))
            .await
            .expect("refuse discover");
    });

    let outcome = ()
        .serve_with_lifecycle(
            client_io,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await;
    server_task.await.expect("server task");

    assert!(
        outcome.is_err(),
        "Auto only falls back on METHOD_NOT_FOUND, so this must fail: that is the reason Initialize stays the default"
    );
}
