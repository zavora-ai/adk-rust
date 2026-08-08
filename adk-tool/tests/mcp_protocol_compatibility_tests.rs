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
use adk_tool::SimpleToolContext;
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

// ---------------------------------------------------------------------------
// Live tests against real external MCP servers.
//
// Ignored by default because each needs a server this repository does not ship.
// Run them with:
//
//     cargo nextest run -p adk-tool --features mcp \
//         -E 'binary(mcp_protocol_compatibility_tests)' --run-ignored all
// ---------------------------------------------------------------------------

/// Connects to a real MCP server built on rmcp 1.7, two major versions behind
/// the SDK this crate now uses.
///
/// Set `ADK_TEST_MCP_SERVER` to the server binary and `ADK_TEST_MCP_SERVER_DIR`
/// to the directory it must run from.
#[tokio::test]
#[ignore] // requires an external MCP server binary; see ADK_TEST_MCP_SERVER
async fn a_real_rmcp_1_7_server_over_stdio_still_works() {
    use rmcp::transport::TokioChildProcess;

    let Ok(binary) = std::env::var("ADK_TEST_MCP_SERVER") else {
        panic!("set ADK_TEST_MCP_SERVER to an MCP server binary");
    };
    let mut command = tokio::process::Command::new(&binary);
    if let Ok(dir) = std::env::var("ADK_TEST_MCP_SERVER_DIR") {
        command.current_dir(dir);
    }

    let client =
        ().serve(TokioChildProcess::new(command).expect("spawn the MCP server"))
            .await
            .expect("a server two rmcp majors behind must still connect");

    let info = client.peer_info().expect("peer info after the handshake").clone();
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::V_2025_11_25,
        "the negotiated revision must be the one this client advertises"
    );

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let toolset = McpToolset::new(client);
    let tools = toolset.tools(ctx).await.expect("list tools from a real server");
    assert!(!tools.is_empty(), "the server published no tools");

    let server = info
        .server_info
        .as_ref()
        .map(|i| format!("{} {}", i.name, i.version))
        .unwrap_or_else(|| "unnamed".to_string());
    println!(
        "server {}, protocol {}, {} tools: {}",
        server,
        info.protocol_version.as_str(),
        tools.len(),
        tools.iter().map(|tool| tool.name().to_string()).collect::<Vec<_>>().join(", ")
    );
}

/// Connects to the Playwright MCP server, an implementation written in
/// TypeScript rather than against rmcp at all.
///
/// Proves the client is not merely compatible with servers that share its SDK.
#[tokio::test]
#[ignore] // requires npx (Node.js)
async fn a_server_from_another_implementation_still_works() {
    use rmcp::transport::TokioChildProcess;

    let mut command = tokio::process::Command::new("npx");
    command.arg("-y").arg("@playwright/mcp@latest");

    let client =
        ().serve(TokioChildProcess::new(command).expect("spawn the Playwright MCP server"))
            .await
            .expect("a server outside the rmcp family must still connect");

    let info = client.peer_info().expect("peer info after the handshake").clone();
    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let toolset = McpToolset::new(client);
    let tools = toolset.tools(ctx).await.expect("list tools from the Playwright server");
    assert!(!tools.is_empty(), "the server published no tools");

    let server = info
        .server_info
        .as_ref()
        .map(|i| format!("{} {}", i.name, i.version))
        .unwrap_or_else(|| "unnamed".to_string());
    println!(
        "server {}, protocol {}, {} tools",
        server,
        info.protocol_version.as_str(),
        tools.len()
    );
}

// ---------------------------------------------------------------------------
// Tests against a real rmcp server, rather than a hand-written peer.
//
// `ServerHandler`'s default `supported_protocol_versions()` returns every known
// revision including `2026-07-28`, so a plain implementation is a dual-stack
// server. These tests therefore exercise the SDK's own `server/discover`
// handling and version selection, not a fixture of our own making.
// ---------------------------------------------------------------------------

/// A dual-stack server publishing one tool.
#[derive(Clone, Default)]
struct DualStackServer;

impl rmcp::ServerHandler for DualStackServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("dual-stack-server", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _params: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![test_tool()]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text("pong")]).into())
    }
}

/// Spawns [`DualStackServer`] and returns the client end of the pipe.
fn spawn_dual_stack_server() -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = DualStackServer.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

/// The default handshake against a real dual-stack server settles on
/// `2025-11-25`, and tools still load.
///
/// This is the case that matters most for existing deployments: a server that
/// has moved on must not require the client to move with it.
#[tokio::test]
async fn the_default_handshake_against_a_dual_stack_server_negotiates_2025_11_25() {
    let client = ().serve(spawn_dual_stack_server()).await.expect("connect");
    let negotiated = client.peer_info().expect("peer info").protocol_version.clone();
    assert_eq!(negotiated, ProtocolVersion::V_2025_11_25);

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client).tools(ctx).await.expect("list tools");
    assert_eq!(tools.len(), 1);
}

/// `Discover` reaches `2026-07-28` against a server that supports it.
#[tokio::test]
async fn discover_negotiates_2026_07_28_against_a_real_server() {
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let client = ()
        .serve_with_lifecycle(
            spawn_dual_stack_server(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("a 2026-07-28 server must accept the discover handshake");

    let negotiated = client.peer_info().expect("peer info").protocol_version.clone();
    assert_eq!(
        negotiated,
        ProtocolVersion::V_2026_07_28,
        "Discover must settle on the new revision, not fall back"
    );

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client).tools(ctx).await.expect("list tools over discover");
    assert_eq!(tools.len(), 1, "tools must load over the stateless path too");
}

/// `Auto` prefers the new handshake when the server answers the probe.
#[tokio::test]
async fn auto_uses_discover_when_the_server_supports_it() {
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let client = ()
        .serve_with_lifecycle(
            spawn_dual_stack_server(),
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await
        .expect("Auto must succeed against a server that answers discover");

    let negotiated = client.peer_info().expect("peer info").protocol_version.clone();
    assert_eq!(
        negotiated,
        ProtocolVersion::V_2026_07_28,
        "Auto must prefer the new revision when the server answers the probe"
    );

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client).tools(ctx).await.expect("list tools");
    assert_eq!(tools.len(), 1);
}

/// A tool call over a `2026-07-28` connection returns its result inline.
///
/// Guards the `execute` inversion on the new revision, where the server is the
/// party that decides whether a call becomes a task.
#[tokio::test]
async fn a_tool_call_over_2026_07_28_returns_its_result() {
    use rmcp::{ClientLifecycleMode, ClientServiceExt};

    let client = ()
        .serve_with_lifecycle(
            spawn_dual_stack_server(),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("connect over discover");

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client).tools(ctx).await.expect("list tools");
    let echo = tools.first().expect("one tool");
    let output = echo
        .execute(
            Arc::new(SimpleToolContext::new("protocol-test")) as Arc<dyn adk_core::ToolContext>,
            json!({ "text": "ping" }),
        )
        .await
        .expect("the call must succeed over the new revision");

    assert!(
        output.to_string().contains("pong"),
        "expected the server's content back, got {output}"
    );
}

/// A server pinned to the oldest MCP revision, `2024-11-05`.
///
/// The dual-stack servers above accept the revision we ask for. This one cannot,
/// so it exercises the other direction: the client must accept what an old
/// server offers.
#[derive(Clone, Default)]
struct AncientServer;

impl rmcp::ServerHandler for AncientServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info =
            rmcp::model::ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new("ancient-server", "0.1.0"));
        info.protocol_version = ProtocolVersion::V_2024_11_05;
        info
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Owned(vec![ProtocolVersion::V_2024_11_05])
    }

    async fn list_tools(
        &self,
        _params: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![test_tool()]))
    }
}

/// The client connects to a server that only knows `2024-11-05` and still
/// loads its tools.
#[tokio::test]
async fn a_server_pinned_to_the_oldest_revision_still_works() {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = AncientServer.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });

    let client = ().serve(client_io).await.expect("a 2024-11-05 server must still be reachable");
    let negotiated = client.peer_info().expect("peer info").protocol_version.clone();
    assert_eq!(negotiated, ProtocolVersion::V_2024_11_05);

    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client).tools(ctx).await.expect("list tools");
    assert_eq!(tools.len(), 1);
}

// ---------------------------------------------------------------------------
// SEP-2663 tasks.
//
// A server must not return a task to a client that did not declare the
// extension, so the declaration is what makes the whole task path reachable.
// ---------------------------------------------------------------------------

/// A server that answers `slow_tool` with a task, but only for a client that
/// declared the extension. Mirrors the rule in the specification.
#[derive(Clone, Default)]
struct TaskServer {
    tasks: Arc<rmcp::task_manager::TaskManager>,
}

impl rmcp::ServerHandler for TaskServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            ServerCapabilities::builder().enable_tools().enable_tasks().build(),
        )
        .with_server_info(Implementation::new("task-server", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _params: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![test_tool()]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let client_declared_tasks =
            context.client_capabilities().is_some_and(|caps| caps.supports_tasks());
        if !client_declared_tasks {
            return Ok(CallToolResult::success(vec![ContentBlock::text("inline")]).into());
        }

        let task = self.tasks.spawn(rmcp::task_manager::TaskOptions::default(), |_ctx| {
            Box::pin(
                async move { Ok(CallToolResult::success(vec![ContentBlock::text("from a task")])) },
            )
        });
        Ok(rmcp::model::CallToolResponse::Task(rmcp::model::CreateTaskResult::new(task)))
    }

    async fn get_task(
        &self,
        params: rmcp::model::GetTaskParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetTaskResult, rmcp::ErrorData> {
        Ok(rmcp::model::GetTaskResult::new(self.tasks.get_task(&params.task_id)?))
    }
}

fn spawn_task_server() -> tokio::io::DuplexStream {
    let (server_io, client_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(running) = TaskServer::default().serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    client_io
}

/// Without the declaration the server answers inline, so the task path is
/// unreachable however the toolset is configured.
#[tokio::test]
async fn a_client_that_does_not_declare_tasks_gets_an_inline_result() {
    let client = ().serve(spawn_task_server()).await.expect("connect");
    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let tools = McpToolset::new(client)
        .with_task_support(adk_tool::mcp::McpTaskConfig::enabled())
        .tools(ctx)
        .await
        .expect("list tools");

    let output = tools[0]
        .execute(
            Arc::new(SimpleToolContext::new("task-test")) as Arc<dyn adk_core::ToolContext>,
            json!({}),
        )
        .await
        .expect("call the tool");
    assert!(output.to_string().contains("inline"), "got {output}");
}

/// With the declaration the server materializes a task, and the toolset polls
/// it to completion.
#[tokio::test]
async fn a_declared_client_polls_a_server_materialized_task() {
    let handler = adk_tool::mcp::AdkClientHandler::new(Arc::new(
        adk_tool::mcp::AutoDeclineElicitationHandler,
    ))
    .with_tasks();

    let client = handler.serve(spawn_task_server()).await.expect("connect");
    let ctx = Arc::new(TestContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>;
    let toolset =
        McpToolset::new(client).with_task_support(adk_tool::mcp::McpTaskConfig::enabled());
    let tools = toolset.tools(ctx).await.expect("list tools");

    assert!(
        tools[0].is_long_running(),
        "with tasks declared on both sides the tool must report as long-running"
    );

    let output = tools[0]
        .execute(
            Arc::new(SimpleToolContext::new("task-test")) as Arc<dyn adk_core::ToolContext>,
            json!({}),
        )
        .await
        .expect("the task must be polled to completion");
    assert!(output.to_string().contains("from a task"), "got {output}");
}
