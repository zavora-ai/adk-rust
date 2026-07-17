# adk-acp

[![crates.io](https://img.shields.io/crates/v/adk-acp.svg)](https://crates.io/crates/adk-acp)
[![docs.rs](https://docs.rs/adk-acp/badge.svg)](https://docs.rs/adk-acp)

Agent Client Protocol support for ADK-Rust. Use an external coding agent from an
ADK-Rust application, or expose an ADK-Rust agent to an ACP-compatible editor.

ACP standardizes the connection between a coding interface and a coding agent.
The interface owns the user experience and can provide files, terminals,
permissions, and MCP servers. The agent owns the reasoning and coding work. ACP
gives both sides typed sessions, prompts, streamed updates, tool calls, and
approval requests over JSON-RPC.

## Two directions

| ADK-Rust role | Use it when | Entry point |
|---|---|---|
| ACP client/host | An ADK-Rust coordinator should delegate work to a local ACP coding-agent process | `AcpAgentTool`, `AcpToolset`, `AcpSession`, `stream_prompt` |
| ACP agent/server | An editor or another ACP client should talk to an ADK-Rust agent | `AcpServer` with the `server` feature |

Both directions implement stable ACP protocol version 1 using
`agent-client-protocol` 1.2. Crate version 1.2 is the Rust SDK release; it does
not mean ACP protocol v2.

## Installation

```toml
[dependencies]
adk-acp = "2.0.0"

# Add the server feature only when exposing an ADK-Rust agent to a client.
adk-acp = { version = "2.0.0", features = ["server"] }
```

## Use an ACP agent as an ADK-Rust tool

`AcpAgentTool` starts one local process for each tool invocation, negotiates ACP
v1, creates a session rooted at the selected project, sends one prompt, and
returns the agent's text as normal ADK tool output.

```rust,ignore
use adk_acp::{AcpAgentTool, PermissionDecision, PermissionPolicy};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let coding_agent = AcpAgentTool::new(
        "npx -y @agentclientprotocol/codex-acp@latest"
    )
    .name("coding_agent")
    .description("Inspect and change code in the current project")
    .working_dir("/absolute/path/to/project")
    .permission_policy(PermissionPolicy::Custom(Box::new(|request| {
        if request.title.to_lowercase().contains("delete") {
            PermissionDecision::deny()
        } else {
            PermissionDecision::allow_once()
        }
    })));

let orchestrator = LlmAgentBuilder::new("orchestrator")
    .model(model)
    .instruction("Delegate repository changes to coding_agent.")
    .tool(Arc::new(coding_agent))
    .build()?;
```

Permissions are denied by default. `AutoApprove` selects an allow option by its
ACP permission kind rather than by menu order, but it should be reserved for a
trusted development environment. Custom policies receive the real session ID,
tool-call ID, title, kind, raw input, and every option offered by the agent.
`PermissionPolicy::async_custom` can await a desktop dialog, web approval UI,
or external policy service before returning the exact option to the agent.

### Choose the client shape

- `AcpAgentTool`: isolated one-shot delegation; easiest to add to an LLM agent.
- `AcpToolset`: several named ACP specialists exposed to one coordinator.
- `AcpSession`: keep one agent process and its conversation context across
  multiple prompts. Create an `AcpCancellationHandle` before a prompt when a UI,
  timeout, or shutdown task must cancel that turn concurrently.
- `stream_prompt`: receive text, thought, tool-call, permission, completion,
  and error chunks while one prompt runs.
- `prompt_agent_with_policy`: direct one-shot API without the ADK `Tool` adapter.

The current client transport is a local subprocess over stdio. Remote ACP
HTTP/WebSocket transport is still being standardized and is not advertised by
this crate.

### Give the coding agent editor services

AcpAgentConfig can attach the services that a normal editor supplies:

- filesystem with an AcpFileSystem advertises only the read and write
  operations implemented by the host;
- terminal with an AcpTerminal advertises the complete ACP terminal
  lifecycle: create, output, wait, kill, and release;
- mcp_server includes a typed MCP configuration in every new session.

These callbacks are opt-in and run outside the JSON-RPC dispatch loop, so a
long file operation or terminal wait does not freeze other ACP traffic. The
host remains responsible for workspace-root checks, symlink handling, process
policy, limits, and cleanup. AcpAgentConfig debug output lists environment
variable names and MCP server names without printing secret values.

The client verifies the negotiated protocol version before creating a session.
Stdio MCP is required by ACP v1. HTTP and SSE MCP configurations are rejected
unless the external agent advertises those optional transports.

## Expose an ADK-Rust agent through ACP

The server path uses the official SDK's `Agent.builder()` and `Stdio` transport.
It does not maintain a parallel wire format.

```rust,ignore
use adk_acp::server::{AcpServer, AcpServerConfigBuilder};
use adk_session::InMemorySessionService;
use std::sync::Arc;

let config = AcpServerConfigBuilder::new()
    .agent(Arc::new(agent))
    .session_service(Arc::new(InMemorySessionService::new()))
    .agent_name("repository-guide")
    .agent_description("Explains and improves this Rust workspace")
    .max_sessions(16)
    .build()?;

let handle = AcpServer::run(config).await?;
handle.wait().await?;
```

The server implements:

- `initialize` with implementation metadata and exact capability negotiation;
- `session/new` with absolute `cwd` and additional workspace roots;
- `session/prompt` with required text and resource-link content;
- live `session/update` notifications for text, thoughts, tool starts, and tool
  completion;
- `session/cancel` plus SDK-level `$/cancel_request` handling;
- `session/close`, `session/resume`, `session/list`, and `session/delete`;
- client-supplied stdio MCP servers, started per session and exposed to
  LlmAgent and CodeActAgent through invocation-scoped toolsets;
- one ACP session mapped to one persisted ADK-Rust session and Runner context;
- concurrent-session limits and one active prompt per session.

ACP v1 requires every agent to accept stdio MCP configuration, so no capability
flag is needed for it. The server does not advertise optional HTTP or SSE MCP,
image, audio, or embedded-context support. MCP startup has a bounded timeout,
session capacity is reserved before a process starts, duplicate names and
environment entries are rejected, and close, delete, and shutdown cancel the
session-owned MCP services.

## Verified protocol flow

The crate test suite connects the official SDK `Client` to the ADK-Rust SDK
`Agent` through an in-memory channel and executes:

```text
initialize
  → session/new
  → session/prompt
  ← session/update { agent_message_chunk }
  ← PromptResponse { stopReason: "end_turn" }
  → session/close
  → session/list
  → session/resume
  → session/close
  → session/delete
```

The test asserts the negotiated protocol, published lifecycle capabilities,
typed streamed update, stop reason, persistence, resume, deletion, and close
responses. A second official-SDK test holds an agent turn open, sends
`session/cancel`, and asserts `stopReason: "cancelled"` under a timeout. A third
test sends JSON-RPC `$/cancel_request`, then proves the same session accepts and
completes another prompt after cleanup.
Event-mapping tests cover thought, tool-start, and tool-completion updates.
Permission tests cover reject-first menus, arbitrary option IDs, and fabricated
selections. ADK agent tests also prove that a live allow-once decision applies
to one function-call ID and is requested again for a second call to the same
tool. MCP configuration tests reject ambiguous names and environment entries
before any process starts. The existing `adk-tool` live lifecycle gate also
starts a real stdio MCP child, completes its handshake, and discovers its tool
catalog through the same `McpToolset` used by ACP sessions.

Run the focused gates with:

```bash
cargo test -p adk-acp --all-features
cargo test -p adk-tool --features mcp \
  --test mcp_server_lifecycle_integration_tests \
  test_tool_aggregation -- --ignored --exact
cargo clippy -p adk-acp --all-features --all-targets -- -D warnings
cargo check --manifest-path examples/acp_server/Cargo.toml
```

## Design boundaries

- ACP is for the interactive coding-agent relationship. Use A2A for a remote
  business agent and MCP for tools and resources behind an agent.
- `cwd` and additional directories describe the project scope. They are not an
  operating-system sandbox. Apply `adk-sandbox` or another process boundary when
  the coding agent must not access the rest of the machine.
- A persistent `SessionService` preserves ACP conversations across process
  restarts. `InMemorySessionService` is suitable for local editor use and tests.
- ADK-Rust now has an async ToolConfirmationHandler that can resolve a tool
  call inside one invocation. The ACP server does not yet attach it to
  session/request_permission: the current official Rust SDK loses the outer
  prompt response after this nested bidirectional request in the in-memory
  interoperability test. The attempted bridge was removed rather than shipping
  a path that can hang an editor.

## Runnable examples

| Example | What it demonstrates |
|---|---|
| [`acp_client_host`](../examples/acp_client_host) | A vendor-neutral ACP client with streamed events, a read-only filesystem host, and an async permission policy |
| [`acp_kiro`](../examples/acp_kiro) | Direct, delegated, persistent-session, environment, and cancellation flows against a real external coding agent |
| [`acp_server`](../examples/acp_server) | A real ADK-Rust LLM agent exposed to editors and other ACP clients through the official server SDK |

Read the [official ADK-Rust ACP guide](../docs/official_docs/acp/index.md) for
architecture, client and server design choices, deployment boundaries, and the
focused verification matrix. The protocol specification is at
[agentclientprotocol.com](https://agentclientprotocol.com/).

## License

Apache-2.0
