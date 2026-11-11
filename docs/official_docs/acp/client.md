# Build an ACP client or host

Use the client direction when an ADK-Rust application needs to delegate coding
work to an external ACP process. The application remains the host: it owns the
project selection, user experience, approval rules, and any local services
offered to the coding agent.

## Install

```toml
[dependencies]
adk-acp = "3.0.0"
```

The default feature set is the client implementation. The `server` feature is
needed only when exposing an ADK-Rust agent.

## Choose the client shape

| Product shape | API |
|---|---|
| One isolated task with a fresh process | `prompt_agent_with_policy` |
| One isolated task with non-text (image, audio, resource) content | `prompt_agent_content_with_policy` |
| A coding specialist available to an LLM agent | `AcpAgentTool` |
| Several named coding specialists | `AcpToolset` |
| A continuing project conversation | `AcpSession` |
| Several sessions on one agent, or the agent's stored sessions | `AcpConnection` |
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

The command string is split into words with POSIX shell rules, so a backslash
escapes the next character; single-quote an argument that contains backslashes,
such as a Windows path: `python3 'C:\agents\agent.py'`.

## Send rich prompt content

`prompt_agent_content_with_policy` transmits a full `adk_core::Content` value —
not just a string — so a prompt can carry non-text content. Embedded-resource,
image, and audio parts are mapped to the matching ACP content block through the
shared content module rather than being dropped; text is always preserved. Parts
that have no transmittable ACP representation are skipped, and a prompt that maps
to no blocks at all is rejected.

```rust,ignore
use adk_acp::{AcpAgentConfig, PermissionPolicy};
use adk_acp::connection::prompt_agent_content_with_policy;
use adk_core::{Content, Part};
use std::sync::Arc;

let mut content = Content::new("user");
content.parts.push(Part::Text { text: "What is in this image?".into() });
content.parts.push(Part::inline_data("image/png", png_bytes));

let config = AcpAgentConfig::new("my-coding-agent --acp")
    .working_dir("/absolute/path/to/project");

let answer = prompt_agent_content_with_policy(
    &config,
    &content,
    Arc::new(PermissionPolicy::DenyAll),
).await?;
```

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

| Property | Value |
|----------|-------|
| Name | `name(...)`, else the first word of the command |
| Parameters | `prompt`: string (required) |
| Returns | `{"response": "<the agent's final text>"}` |
| Effect | `ToolEffect::NonIdempotent` (the default), so a default-deny `DeclarativePolicy` must allow it by name |
| Failure | A tool error; `LlmAgent` sends the model its category and code, not the agent's error text, unless `expose_tool_error_messages(true)` is set |

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

Each `PromptResult` carries the turn's `stop_reason` and, when the agent reports
it, its token `usage`. `AcpSession::start` returns once the agent has created the
session, so a refused handshake or session fails there rather than at the first
prompt.

## Work with the agent's sessions

`AcpConnection` keeps one agent process and opens any number of sessions on it.
Its `agent_info()` reports the negotiated protocol version, the agent's name and
version, its capabilities and its authentication methods. Each method below is
checked against those capabilities; one the agent did not advertise returns
`AcpError::Unsupported` without being sent.

```rust,ignore
use adk_acp::{AcpAgentConfig, AcpConnection, PermissionPolicy};
use std::sync::Arc;

let connection = AcpConnection::connect(
    AcpAgentConfig::new("my-coding-agent --acp")
        .working_dir("/absolute/path/to/project")
        .append_system_prompt("Follow the repository's CONTRIBUTING.md.")
        .initial_config_option("verbose", true)
        .auth_method("api-key"),
    Arc::new(PermissionPolicy::DenyAll),
)
.await?;

// Stored sessions: list (archived ones only on request), load with replay,
// resume without it, fork, archive, delete.
let stored = connection.list_sessions(None).await?;
let mut loaded = connection.load_session(stored[0].session_id.clone()).await?;
let replay = loaded.pending_updates();

// A new session, its controls, and a branch of it.
let mut session = connection.new_session().await?;
session.set_mode("code").await?;
session.set_title("Release notes").await?;
let branch = session.fork().await?;
println!("{:?}", session.status().await?);

connection.logout().await?;
connection.close().await?;
```

| Configuration | Sent as | Sent only when the agent advertises |
|---|---|---|
| `append_system_prompt` / `override_system_prompt` | `systemPrompt` on new, load, resume and fork | `sessionCapabilities.systemPrompt` (and its `override` mode) |
| `additional_directory` | `additionalDirectories` | `sessionCapabilities.additionalDirectories` |
| `initial_config_option` | `configOptions` on new, load and resume | `sessionCapabilities.options` |
| `auth_method` | `authenticate` right after `initialize` | the method in `authMethods` |
| `elicitation` | the `elicitation` client capability | (always; the agent then asks) |
| `client_info` | `clientInfo` (defaults to `adk-rust`) | (always) |

An `AcpElicitation` answers the agent's `elicitation/create` questions with
`accept` and the form's content, `decline` or `cancel`; `DeclineElicitation`
declines them all. Without a handler the capability is not advertised and the
agent does not ask.

## Stream a turn into a UI

`stream_prompt(config, prompt, policy, status_tracker)` yields `OutputChunk`
values for agent text, thoughts, tool starts (`ToolCall`, carrying the tool's
title), permission decisions, completion, and errors. It also surfaces two
richer views of the external agent's turn:

- `OutputChunk::ToolUpdate` — the agent's `ToolCallUpdate`: the tool-call `id`,
  reported status, kind, updated title, extracted content text, and affected
  file locations. This lets a UI render tool progress, diffs, and affected-file
  lists rather than only the final text. `ToolCall` carries no id, so group
  updates by their `id`.
- `OutputChunk::Usage` — the agent's `UsageUpdate`, carrying tokens `used` and
  context-window `size`, plus the cumulative `cost` and `currency` when the
  agent reports them, so a UI can display context-window consumption.
- `OutputChunk::Plan`, `PlanUpdate`, `PlanRemoved`, `Notice`, `Compaction`,
  `CompactionSummary`, `ModeChanged`, `ConfigOptions`, `AvailableCommands` and
  `SessionInfo` — the other v1 session updates. The client advertises notices,
  compactions and plan operations, so agents send them.
- `OutputChunk::Subagent` and `SubagentOutput` — a child session the agent
  announced, and that child's own updates, which arrive under its session id.
- `OutputChunk::Done { stop_reason, usage }` — the end of the turn.

`AcpSession::prompt_blocks(blocks, on_chunk)` reports the same chunks for a turn
of a persistent session.

The application may hide thought chunks, render tool activity separately, and
expose the shared `StatusTracker` in its interface.

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
use adk_acp::agent_client_protocol::v1::schema::{
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

## Serve ADK-Rust tools over the connection

An agent that advertises `mcpCapabilities.acp` (the unstable MCP-over-ACP
surface) can call tools that run in your application, with no MCP process or
port. An `AcpToolServer` is a named set of ADK-Rust tools:

```rust,ignore
use adk_acp::{AcpAgentConfig, AcpToolServer};

let config = AcpAgentConfig::new("my-coding-agent --acp")
    .tool_server(AcpToolServer::new("app-tools").tool(lookup_order_tool));
```

`AcpConnection` declares each tool server as an `acp` MCP server, with a fresh
`serverId`, on `session/new`, `session/load`, `session/resume` and
`session/fork`. It answers the agent's `mcp/message` requests (`tools/list`,
`tools/call`, `server/discover`, MCP `2026-07-28`) over the same connection.
Starting a session fails with `AcpError::Unsupported` when the agent does not
advertise `mcp.acp`. Tool server names must be unique among the session's MCP
servers. A tool that fails comes back to the agent as an `isError` result, so
its model can read the error. Tools run with a minimal `ToolContext`: no
session state, artifacts or memory.

`AcpToolServer` is also an SDK `McpService`. Code that drives the SDK directly
can attach one with
`McpServer::new_service(name, tools, NullRun)` and
`SessionBuilder::with_mcp_server`.

## Configure the agent's model providers

When the agent advertises `providers` (the unstable configurable-provider
surface), the client can see which model providers it routes to and point them
elsewhere, for example at a company gateway. Do this before creating sessions:

```rust,ignore
use adk_acp::agent_client_protocol::v1::schema::{LlmProtocol, SetProviderRequest};

for provider in connection.list_providers().await? {
    println!("{} required={} {:?}", provider.provider_id, provider.required, provider.current);
}
connection
    .set_provider(
        SetProviderRequest::new("main", LlmProtocol::OpenAi, "https://gateway.example/v1")
            .headers([("authorization".into(), format!("Bearer {token}"))].into()),
    )
    .await?;
connection.disable_provider("helper").await?;
```

`providers/list` never returns headers. SDK trace logging of JSON-RPC bodies
would record them, so keep body-level logging off when headers carry
credentials.

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
