# Build an ACP client or host

Use the client direction when an ADK-Rust application needs to delegate coding
work to an external ACP process. The application remains the host: it owns the
project selection, user experience, approval rules, and any local services
offered to the coding agent.

## Install

```toml
[dependencies]
adk-acp = "2.0.0"
```

The default feature set is the client implementation. The `server` feature is
needed only when exposing an ADK-Rust agent.

## Choose the client shape

| Product shape | API |
|---|---|
| One isolated task with a fresh process | `prompt_agent_with_policy` |
| A coding specialist available to an LLM agent | `AcpAgentTool` |
| Several named coding specialists | `AcpToolset` |
| A continuing project conversation | `AcpSession` |
| Text and tool progress rendered while the turn runs | `stream_prompt` |

## One-shot prompt

```rust,ignore
use adk_acp::{
    AcpAgentConfig, PermissionPolicy, prompt_agent_with_policy,
};
use std::sync::Arc;

let config = AcpAgentConfig::new("my-coding-agent --acp")
    .working_dir("/absolute/path/to/project");

let answer = prompt_agent_with_policy(
    &config,
    "Inspect the failing test and explain the cause.",
    Arc::new(PermissionPolicy::DenyAll),
).await?;
```

`DenyAll` is the default because a spawned coding agent can request operations
with real side effects. Use `AutoApprove` only inside a trusted local workflow.

## Delegate from an ADK agent

```rust,ignore
use adk_acp::{AcpAgentTool, PermissionDecision, PermissionPolicy};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let policy = PermissionPolicy::Custom(Box::new(|request| {
    if request.title.to_ascii_lowercase().contains("delete") {
        PermissionDecision::deny()
    } else {
        PermissionDecision::allow_once()
    }
}));

let coding_agent = AcpAgentTool::new("my-coding-agent --acp")
    .name("repository_specialist")
    .description("Inspect and improve the current Rust repository")
    .working_dir("/absolute/path/to/project")
    .permission_policy(policy);

let coordinator = LlmAgentBuilder::new("coordinator")
    .model(model)
    .instruction("Delegate repository changes to repository_specialist.")
    .tool(Arc::new(coding_agent))
    .build()?;
```

Each `AcpAgentTool` call starts a fresh process and session. Choose this shape
when the delegated task is self-contained and the coordinator needs only the
final text as its tool result.

## Persistent sessions and cancellation

```rust,ignore
use adk_acp::{AcpAgentConfig, AcpSession, PermissionPolicy};
use std::sync::Arc;

let config = AcpAgentConfig::new("my-coding-agent --acp")
    .working_dir("/absolute/path/to/project");
let mut session = AcpSession::start(
    config,
    Arc::new(PermissionPolicy::DenyAll),
).await?;

let first = session.prompt("Map the error-handling modules.").await?;
let second = session.prompt("Now inspect the most central one.").await?;

let cancel = session.cancellation_handle()?;
// Move `cancel` into a stop-button, timeout, or shutdown task while another
// task awaits `session.prompt(...)`.

session.close().await?;
```

The cancellation handle sends the official `session/cancel` notification. The
prompt should remain awaited until the cancelled stop reason arrives; this lets
the same session accept another prompt without a stale response in its queue.

## Stream a turn into a UI

`stream_prompt` yields `OutputChunk` values for agent text, thoughts, tool
starts, permission decisions, completion, and errors. The application may hide
thought chunks, render tool activity separately, and expose the shared
`StatusTracker` in its interface.

See the runnable [`acp_client_host`](../../../examples/acp_client_host) crate for
the complete loop.

## Let the agent request files

Implement `AcpFileSystem` and attach it with `AcpAgentConfig::filesystem`. Read
and write capabilities are advertised independently through `supports_read`
and `supports_write`.

The callback receives absolute paths. A production host should:

1. canonicalize the approved workspace and the requested path;
2. reject paths outside approved roots, including symlink escapes;
3. decide whether unsaved editor buffers override disk content;
4. apply file-size and line-range limits;
5. advertise writes only when the application implements and authorizes them.

The working directory is context, not a sandbox. Filesystem validation and an
OS process boundary solve different problems.

## Let the agent run commands

Implement `AcpTerminal` and attach it with `AcpAgentConfig::terminal`. ACP
advertises the terminal as one capability, so the host must implement the full
create, output, wait, kill, and release lifecycle.

The host chooses command allowlists, working-directory rules, environment
variables, output limits, process isolation, and cleanup behavior. Terminal
callbacks execute outside the JSON-RPC dispatch loop so a long wait does not
freeze permission or cancellation traffic.

## Supply an MCP server to the session

```rust,ignore
use adk_acp::AcpAgentConfig;
use adk_acp::agent_client_protocol::schema::v1::{
    McpServer, McpServerStdio,
};

let tools = McpServer::Stdio(
    McpServerStdio::new("project-tools", "/absolute/path/to/mcp-server")
        .args(vec!["--read-only".into()]),
);

let config = AcpAgentConfig::new("my-coding-agent --acp")
    .working_dir("/absolute/path/to/project")
    .mcp_server(tools);
```

Stable ACP v1 requires agents to accept stdio MCP configuration. HTTP and SSE
entries are sent only when the external agent advertises those optional
transports. `AcpAgentConfig` debug output lists names and environment keys
without printing secret values.

## Permission policies

Every permission request includes the session ID, exact tool-call ID, tool
kind, raw input, and all options offered by the agent. Option IDs are opaque.
ADK-Rust matches allow and reject semantics, then returns the original ID; a
fabricated selection becomes cancellation.

`PermissionPolicy::async_custom` can await a desktop dialog, web approval UI,
or organisation policy service. Keep the dispatch loop responsive by awaiting
human interaction through this API instead of blocking a thread.

## Next

- [Expose an ADK-Rust ACP agent](server.md)
- [Testing and support matrix](testing.md)
