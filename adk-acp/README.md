# adk-acp

[![crates.io](https://img.shields.io/crates/v/adk-acp.svg)](https://crates.io/crates/adk-acp)
[![docs.rs](https://docs.rs/adk-acp/badge.svg)](https://docs.rs/adk-acp)
[![License](https://img.shields.io/crates/l/adk-acp.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

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
| ACP client/host | An ADK-Rust coordinator should delegate work to a local ACP coding-agent process | `AcpAgentTool`, `AcpToolset`, `AcpConnection`, `AcpSession`, `stream_prompt` |
| ACP agent/server | An editor or another ACP client should talk to an ADK-Rust agent | `AcpServer` with the `server` feature |

Both directions implement ACP protocol version 1 using the official
`agent-client-protocol` Rust SDK 5.2 (ACP schema 1.17), re-exported as
`adk_acp::agent_client_protocol`. SDK version 5.2 is the Rust SDK release; it does
not mean ACP protocol v5. The SDK's v1 connection guard negotiates the protocol
version during `initialize` on both sides: the server answers with the negotiated
version, and the client requests the highest version in
`SUPPORTED_PROTOCOL_VERSIONS` and refuses an agent that selects another. Code that
uses the re-export follows the SDK's
[v5 migration guide](https://github.com/agentclientprotocol/rust-sdk/blob/main/md/migration-v5.md):
protocol v1 messages are in `agent_client_protocol::v1::schema`, and connections
start from `Client.v1()` and `Agent.v1()`.

Besides the stable v1 surface, both directions implement these unstable ACP
surfaces: session fork, archive, status, options and title, `auth/status`,
end-of-turn token usage, plan operations and subagent updates. The SDK may change
unstable surfaces in a minor release, so adk-acp depends on `~5.2` and takes patch
releases only. The draft protocol v2, MCP-over-ACP and LLM provider methods are not
implemented yet.

## Installation

```toml
[dependencies]
adk-acp = "3.0"

# Or, to also expose an ADK-Rust agent to a client:
# adk-acp = { version = "3.0", features = ["server"] }
```

| Feature | Description |
|---------|-------------|
| (default) | Client and host APIs for connecting to ACP agents |
| `server` | `AcpServer` for exposing an ADK-Rust agent to ACP clients |
| `unstable-protocol-v2` | Draft ACP protocol v2 on both sides, through the SDK's `unstable_protocol_v2`; see [Draft protocol v2](#draft-protocol-v2) |

Or use the meta-crate's `acp` feature, which is not part of any tier:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["acp"] }
```

## Use an ACP agent as an ADK-Rust tool

`AcpAgentTool` starts one local process for each tool invocation, negotiates ACP
v1, creates a session rooted at the selected project, sends one prompt, and
returns the agent's text as normal ADK tool output.

```rust
use adk_acp::{AcpAgentTool, PermissionDecision, PermissionPolicy};
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::Llm;
use std::sync::Arc;

fn orchestrator(model: Arc<dyn Llm>) -> adk_core::Result<LlmAgent> {
    let coding_agent = AcpAgentTool::new("npx -y @agentclientprotocol/codex-acp@latest")
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

    LlmAgentBuilder::new("orchestrator")
        .model(model)
        .instruction("Delegate repository changes to coding_agent.")
        .tool(Arc::new(coding_agent))
        .build()
}
```

Permissions are denied by default (`PermissionPolicy::DenyAll`). `AutoApprove` selects an allow option by its
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
  timeout, or shutdown task must cancel that turn concurrently. `prompt_blocks`
  reports every update of a turn as an `OutputChunk`, and each `PromptResult`
  carries the stop reason and the turn's token usage.
- `AcpConnection`: one agent process shared by several sessions, with the
  connection-level methods: list, load, resume, fork, archive and delete
  sessions, session options and status, and authentication.
- `stream_prompt`: receive text, thought, tool-call, plan, notice, permission,
  completion, and error chunks while one prompt runs.
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
- mcp_server includes a typed MCP configuration in every new session;
- tool_server serves an `AcpToolServer`, a named set of ADK-Rust tools, to the
  agent over the ACP connection itself (unstable MCP over ACP). The tools run
  in your application, and no MCP process is started.

These callbacks are opt-in and run outside the JSON-RPC dispatch loop, so a
long file operation or terminal wait does not freeze other ACP traffic. The
host remains responsible for workspace-root checks, symlink handling, process
policy, limits, and cleanup. AcpAgentConfig debug output lists environment
variable names and MCP server names without printing secret values.

The client verifies the negotiated protocol version before creating a session.
Stdio MCP is required by ACP v1. HTTP, SSE and ACP MCP configurations, tool
servers included, are rejected unless the external agent advertises those
optional transports. Tool servers are declared on new, loaded, resumed and
forked sessions, and the client answers the agent's `mcp/message` requests
(`tools/list`, `tools/call`) for them.

### Work with the agent's sessions

`AcpConnection` exposes the rest of the protocol. Every request is checked against
the capabilities the agent advertised; one it did not advertise returns
`AcpError::Unsupported` without being sent.

```rust,ignore
use adk_acp::{AcpAgentConfig, AcpConnection, PermissionPolicy};
use std::sync::Arc;

let connection = AcpConnection::connect(
    AcpAgentConfig::new("my-coding-agent --acp")
        .working_dir("/absolute/path/to/project")
        // Sent only when the agent advertises `sessionCapabilities.systemPrompt`.
        .append_system_prompt("Follow the repository's CONTRIBUTING.md.")
        // Authenticate during the handshake with one of the agent's `authMethods`.
        .auth_method("api-key"),
    Arc::new(PermissionPolicy::DenyAll),
)
.await?;
println!("ACP v{}", connection.agent_info().protocol_version);

for info in connection.list_sessions(None).await? {
    println!("{} {:?}", info.session_id, info.title);
}
let mut session = connection.new_session().await?;
session.set_title("Release notes").await?;
let reply = session.prompt("Draft the release notes").await?;
println!("{} ({:?}, {:?})", reply.text, reply.stop_reason, reply.usage);
let mut branch = session.fork().await?;
session.close().await?;
branch.close().await?;
connection.close().await?;
```

The client advertises the session updates it surfaces (config options, notices,
compactions, plan operations and subagents), so agents send them. A subagent's
own updates arrive under the child's session id; the client routes them to the
session that announced it as `OutputChunk::SubagentOutput`. An `AcpElicitation`
set with `AcpAgentConfig::elicitation` answers the agent's `elicitation/create`
questions; without one, elicitation is not advertised.

When the agent advertises `providers` (unstable), `list_providers`,
`set_provider` and `disable_provider` show and change the model providers it
routes to, for example to send its requests through a company gateway. Call them
before creating sessions.

## Expose an ADK-Rust agent through ACP

The server path uses the official SDK's `Agent.v1()` builder and stdio transport.
It does not maintain a parallel wire format. `AcpServer::serve` serves one
connection over any other SDK transport, such as an in-process `Channel`.

Requires the `server` feature.

```rust
use adk_acp::server::{AcpServer, AcpServerConfigBuilder};
use adk_core::Agent;
use adk_session::InMemorySessionService;
use std::sync::Arc;

async fn serve(agent: Arc<dyn Agent>) -> Result<(), Box<dyn std::error::Error>> {
    let config = AcpServerConfigBuilder::new()
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        .agent_name("repository-guide")
        .agent_description("Explains and improves this Rust workspace")
        .max_sessions(16)
        .build()?;

    let handle = AcpServer::run(config).await?;
    handle.wait().await?;
    Ok(())
}
```

| `AcpServerConfigBuilder` option | Default |
|---------------------------------|---------|
| `agent`, `session_service` | Required; `build()` fails without them |
| `agent_name` | `"adk-agent"` |
| `agent_description` | Empty |
| `user_id` | `"acp-client"`, the ADK user that owns every session |
| `max_sessions` | 16; `build()` fails on 0 |
| `shutdown_timeout` | 30 seconds; `build()` fails on 0 |
| `session_controls` | None |
| `transport` | `TransportConfig::Stdio`, the only transport |

Protocol traffic is the only data written to stdout; send tracing and diagnostics to stderr.

The server implements:

- `initialize` with implementation metadata and exact capability negotiation;
- `session/new` with absolute `cwd` and additional workspace roots;
- `session/prompt` with text, resource-link, embedded-resource, image, and
  audio content;
- `session/load` with chronological history replay: a persisted session is
  reactivated (validating `cwd`) and its stored user, agent, thought, and tool
  events are replayed as ordered `session/update` notifications before the
  request completes;
- live `session/update` notifications for text, thoughts, tool starts, tool
  completion, embedded-resource content, and token-usage updates;
- `session/cancel` plus SDK-level `$/cancel_request` handling;
- `session/request_permission` bridging: an agent tool-confirmation pause is
  turned into a native ACP permission request and the client's outcome resumes
  the turn (allow → approve, deny/cancel → deny), bound to the call's fingerprint;
- `session/fork`: branch a persisted session into a new session id whose history
  is a copy of the source's, leaving the source unchanged;
- `session/set_mode` and `session/set_config_option`: change the current session
  mode or a configuration value when the agent declares them through a
  `SessionControls` provider, validated against the advertised set and echoed as
  a `CurrentModeUpdate` / `ConfigOptionUpdate`; the selection persists across
  load / resume / fork;
- `session/close`, `session/resume`, `session/list`, and `session/delete`;
- `systemPrompt` on `session/new`, `session/load`, `session/resume` and
  `session/fork`, applied to every model request of the session's turns through a
  run invocation hook: `append` after the agent's own instructions, `override`
  instead of them (`AcpServerConfigBuilder::system_prompts` narrows or disables
  this with `SystemPromptSupport`);
- the token `usage` of each turn in its `PromptResponse`;
- the unstable session methods: `session/set_title`, `session/status`,
  `session/archive` and `session/unarchive` (an `archived` filter on
  `session/list`, which also reports titles), and `session/options`, plus
  initial `configOptions` on `session/new`, `session/load` and `session/resume`;
- `authenticate`, `logout` and `auth/status` through an `AcpAuthenticator`
  (`AcpServerConfigBuilder::authenticator`), whose methods are advertised as
  `authMethods`. While it requires authentication, session requests get
  `auth_required` (`-32000`) until the client authenticates, and the server sends
  `auth/status_update` when the state changes;
- client-supplied stdio MCP servers in `session/new`, `session/load`,
  `session/resume`, or `session/fork`, started per session and exposed to
  LlmAgent and CodeActAgent through invocation-scoped toolsets named
  `acp:{server name}`;
- client-supplied `acp` MCP servers (unstable MCP over ACP) in the same
  requests, whose tools the client serves: the server reaches them by sending
  `mcp/message` back over the connection, as the same kind of toolset;
- `providers/list`, `providers/set` and `providers/disable` (unstable) through
  an `AcpProviders` hook (`AcpServerConfigBuilder::providers`). `ModelProviders`
  switches `SwitchableModel`s, `Llm`s whose backing model a client can replace;
- one ACP session mapped to one persisted ADK-Rust session and Runner context;
- concurrent-session limits and one active prompt per session.

ACP v1 requires every agent to accept stdio MCP configuration, so no capability
flag is needed for it. The server advertises `load_session`, the
`embedded_context`, `image`, and `audio` prompt capabilities (because it accepts
embedded-resource, image, and audio prompt content), and the session lifecycle
capabilities (`list`, `delete`, `resume`, `close`, `fork`,
`additional_directories`, `archive`, `status`, `options`, `title` and
`systemPrompt`), `auth.status`, `mcp.acp`, `auth.logout` when the authenticator
supports it, and `providers` when a providers hook is configured. A request naming a missing session gets ACP's `session_not_found` error
(`-32002` with the session id). Session modes and configuration options are advertised
only when the agent supplies a `SessionControls` provider — an agent without one
advertises no modes and no options, so advertised capabilities always match what
the server implements. It does not advertise optional HTTP or SSE MCP support.
Before starting a process the server rejects more than 16 servers, a server that
is neither stdio nor `acp`, an empty or repeated name or environment variable name, and a command
that is not an absolute path. Each startup handshake is bounded to 15 seconds,
session capacity is reserved before a process starts, servers that publish the
same tool name are rejected at session setup with an error naming both servers
and the tool, and close, delete, and shutdown cancel the session-owned MCP
services.

### Session modes, configuration options, and commands

An agent opts into interactive session controls by supplying a `SessionControls`
provider (`AcpServerConfigBuilder::session_controls`). The provider declares:

- **modes** — a `SessionModeState` (available modes plus the current one), such
  as "ask" versus "code". `session/set_mode` validates the id against the
  advertised set, records it, and emits a `CurrentModeUpdate`; an unknown id is
  rejected and leaves the mode unchanged.
- **configuration options** — selects and toggles. `session/set_config_option`
  validates the value against the option's declared choices, records it, and
  emits a `ConfigOptionUpdate`; an unknown option or invalid value is rejected.
- **commands** — ACP slash-commands surfaced as an `AvailableCommandsUpdate` when
  a session becomes active (create / load / resume / fork).

Selections persist in ADK session state (`acp:mode`, `acp:config:<id>`), so they
survive load / resume / fork. A recorded session title (`acp:title`, set via
`set_session_title`) is surfaced as a `SessionInfoUpdate` on activation and
whenever it changes. A `Plan` `SessionUpdate` mapping exists but stays dormant
until an ADK plan primitive surfaces plan entries.

### Content mapping and streaming fidelity

A shared `content` module owns the `ContentBlock` ↔ `adk_core::Part` mapping in
both directions, so the server prompt parser, the server streamer, and the
client share one implementation:

- embedded-resource prompt content maps to `Part::EmbeddedResource`, preserving
  the source URI, optional MIME type, and text or binary contents. Text
  resources are preserved verbatim; binary resources are base64-encoded on the
  wire and decoded to raw bytes internally. ADK embedded-resource content
  streams back to the client as an ACP embedded-resource block.
- image and audio prompt content maps to `Part::InlineData`, preserving the MIME
  type and decoded bytes. The client prompt path also transmits non-text ADK
  content (embedded-resource, image, audio) as the matching ACP block instead of
  dropping it. Stored user content replays as `UserMessageChunk`; model and
  agent content replays as `AgentMessageChunk`. `Part::FileData` replays as a
  resource link, while function-response inline/file parts appear in tool-call
  result content.
- Inbound image, audio, and binary-resource payloads are rejected before Base64
  decoding when their encoded size cannot fit the core 10 MiB inline-data
  limit, and the decoded size is checked again. Outbound inline data maps only
  for `image/*` and `audio/*` MIME types; ACP has no lossless block for other
  URI-less binary MIME types, so those parts are not emitted.
- ACP image `uri` and content annotations have no corresponding fields in the
  current `adk_core::Part` shapes, so an inbound image preserves its MIME type
  and bytes but not those optional display hints.
- `UsageUpdate` notifications are emitted from ADK usage metadata: `used` is the
  turn's total token count, plus cost in USD when reported. ADK does not know the
  model's context-window size, so `size` is `0`. Nothing is emitted when an event
  carries no usage metadata.
- A tool call starts as an `in_progress` `ToolCall` with a `kind` inferred from
  the tool name (`read`, `edit`, `execute`, and so on, else `other`). Its result
  arrives as a `completed` `ToolCallUpdate` with the same identifier, carrying the
  result content and the file locations the result names in `path`, `paths`, or
  `locations`.

### Client streaming surface

`stream_prompt` yields `OutputChunk` values as they arrive from an
External_Agent. Beyond agent text (`Text`), reasoning (`Thought`), tool starts
(`ToolCall`), permission decisions, completion, and errors, the client surfaces:

- `OutputChunk::ToolUpdate` — an External_Agent's `ToolCallUpdate`, correlated
  by tool-call `id`, carrying the reported status, kind, updated title,
  extracted content text, and affected file locations.
- `OutputChunk::Usage` — an External_Agent's `UsageUpdate`, carrying tokens
  `used`/`size` and, when reported, the cumulative `cost` and `currency`.
- `OutputChunk::Plan`, `PlanUpdate` and `PlanRemoved` — the agent's plan and,
  with plan operations, changes to individual plans.
- `OutputChunk::Notice`, `Compaction` and `CompactionSummary` — notices and
  context compactions, which agents send only to clients that advertise them.
- `OutputChunk::ModeChanged`, `ConfigOptions`, `AvailableCommands` and
  `SessionInfo` — session control and metadata changes.
- `OutputChunk::Subagent`, `SessionMessage`, `SessionMessageChunk` and
  `SubagentOutput` — child sessions the agent creates and what they produce.
- `OutputChunk::Done { stop_reason, usage }` — the end of the turn, with the
  token usage the agent reported.

## Verified protocol flow

`tests/protocol_v5_tests.rs` connects adk-acp's client to adk-acp's server
through an in-process channel and covers version negotiation, system prompts
(append and override, checked against the model request an `LlmAgent` sends),
end-of-turn usage, titles, status, archiving, options, modes and initial config
values, fork, load replay, resume, delete and authentication. A scripted SDK
agent covers the agent-initiated surfaces: updates sent before `session/new`
answers, notices, compactions, subagent output and elicitation.

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
- The ACP server bridges ADK tool confirmations to native ACP permission
  requests. When the agent pauses on a `ToolConfirmationRequest`, the server
  sends a `session/request_permission` titled `Run tool '{name}'` with the
  options **Allow once** (`acp-allow-once`) and **Deny** (`acp-reject-once`),
  awaits the outcome, and resumes the turn with the mapped decision:
  `acp-allow-once` approves; any other selection, and a cancellation, deny.
  With `AcpServerConfigBuilder::standing_permissions`, a call that may be
  remembered also offers `acp-allow-always` and `acp-reject-always`, which decide
  the call and remember the answer as a standing rule (spec 3.3 §10 A1), and a
  call the implementation already decides — such as one a rule remembered for an
  earlier call of the same turn covers — is not asked about.
  `adk-harness`'s `acp-server` feature implements it with the harness's standing
  rules, and never offers "always" for a payment, leaving plan mode, a memory
  write, a desktop action, a protected edit, or an unsandboxed retry. Each
  outcome is recorded under the call's
  function-call id and bound to the call's fingerprint — the acting agent, the
  tool's origin, the tool name, and the arguments — so a reused call id on
  another agent's call, or on a tool another MCP server publishes, is not
  authorized. A confirmation without a call id fails the prompt, and one prompt
  resolves at most 64 rounds of confirmations. The resumed run dispatches the
  held call under its original id before the next model call. The nested
  request is issued from the spawned prompt task, so the outer prompt response
  still completes.
- `AcpServerConfigBuilder` takes no `ToolPolicy` or run budget. In-process
  controls belong on the agent: tool confirmation, callbacks, guardrails, and
  `adk-auth` wrappers.

## Draft protocol v2

The `unstable-protocol-v2` feature adds the draft ACP protocol v2. The SDK may
change v2 in any 5.x minor release, and no shipping agent or editor speaks it
yet, so treat it as experimental.

- **Server.** `AcpServer` serves both versions through the SDK's agent protocol
  router: a client that initializes with v1 sees the v1 server unchanged, and a
  v2 client gets the baseline v2 session surface (`session/new`, `list`,
  `resume` with replay from the start, `close`, `prompt`, `cancel`, `update`)
  plus `session/delete` and `session/set_config_option`, over the same session
  handler. A v2 prompt is answered when its user message is inserted, with that
  message's id; a prompt sent during a turn is held and runs in order; each turn
  reports `running`, then `idle` with the stop reason and token usage, or the
  `error` stop reason when it fails after insertion. `tool_call/cancel` and
  `terminal/cancel` are answered with `-32602`, since the server owns no
  cancellable tool calls or terminals. A server with an `AcpAuthenticator`
  serves v1 only until v2 `auth/login` is implemented.
- **Client.** `AcpConnection::connect` asks for v2 through the SDK's client
  protocol connector and falls back to v1 when the agent selects it.
  `AcpConnection::connect_with_factory` does the same over a transport the
  caller can open again. The v1 fallback advertises capabilities the v2 request
  cannot carry, so against a v1-only agent process the connector starts the
  process a second time. `AcpConnection::connect_to` takes a single transport,
  which cannot be reopened, and speaks v1. Over v2 the API stays v1-typed:
  `agent_info()` reports version 2 with the agent's capabilities in their v1
  form, and sessions are created, resumed, listed, deleted, prompted,
  cancelled, configured and closed as over v1. The operations v2 lacks or
  adk-acp does not yet map (`load_session`, modes, fork, archive, status,
  title, options, authentication) fail as unsupported. A v2 connection offers
  no file system or terminal, since v2 has no client-side terminals, and no
  elicitation. A connection configured with an `auth_method` speaks v1.

## Runnable examples

| Example | What it demonstrates |
|---|---|
| [`acp_client_host`](https://github.com/zavora-ai/adk-rust/tree/main/examples/acp_client_host) | A vendor-neutral ACP client with streamed events, a read-only filesystem host, and an async permission policy |
| [`acp_kiro`](https://github.com/zavora-ai/adk-rust/tree/main/examples/acp_kiro) | Direct, delegated, persistent-session, environment, and cancellation flows against a real external coding agent |
| [`acp_server`](https://github.com/zavora-ai/adk-rust/tree/main/examples/acp_server) | A real ADK-Rust LLM agent exposed to editors and other ACP clients through the official server SDK |
| [`acp_full_protocol`](https://github.com/zavora-ai/adk-rust/tree/main/examples/acp_full_protocol) | In-process, no-API-key `Runner`-backed reference exercising the server surface (embedded-resource and image/audio prompts, permission bridge, `session/load` replay, usage and tool-call updates) with a validating test |

Read the [official ADK-Rust ACP guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/acp/index.md) for
architecture, client and server design choices, deployment boundaries, and the
focused verification matrix. The protocol specification is at
[agentclientprotocol.com](https://agentclientprotocol.com/).

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
