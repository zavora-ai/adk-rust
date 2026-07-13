# ACP coding agents

Agent Client Protocol connects a coding interface to a coding agent. The
interface may be an editor, desktop application, CLI, or another ADK-Rust
agent. The coding agent is a separate process that reasons about a project and
reports its work through a shared session.

ADK-Rust implements both sides of stable ACP protocol version 1:

- use an external ACP coding agent from an ADK-Rust application;
- expose an ADK-Rust agent as an ACP process an editor can start.

The implementation uses the official `agent-client-protocol` Rust SDK. The SDK
crate version and ACP wire version are separate: ADK-Rust currently uses SDK
1.2 while negotiating protocol version 1 during `initialize`.

## Read this section

1. [Architecture and concepts](../acp/index.md) explains the two roles and the
   full request flow.
2. [Build an ACP client or host](../acp/client.md) covers one-shot delegation,
   persistent sessions, streaming, cancellation, permissions, files,
   terminals, and session MCP servers.
3. [Expose an ADK-Rust ACP agent](../acp/server.md) covers server construction,
   lifecycle mapping, event streaming, session persistence, and deployment.
4. [Testing and support matrix](../acp/testing.md) lists verified behavior and
   the features ADK-Rust deliberately does not advertise.

## Which protocol should I use?

| Boundary | Use it for |
|---|---|
| ACP | An interactive coding interface working with a coding agent inside a project session |
| A2A | Independently deployed business agents exchanging remote tasks and artifacts |
| MCP | Tools, prompts, and resources made available behind an agent |

ACP may carry MCP server configuration when a coding session opens. That does
not make ACP and MCP interchangeable: ACP owns the coding-agent conversation;
MCP supplies tools and resources used during that conversation.

## Examples

| Example | What it proves |
|---|---|
| [`examples/acp_client_host`](../../../examples/acp_client_host) | Streamed client UI, asynchronous permissions, and a workspace-bounded read-only filesystem |
| [`examples/acp_kiro`](../../../examples/acp_kiro) | Direct delegation, an ACP agent as an ADK tool, persistent sessions, environment configuration, and concurrent cancellation |
| [`examples/acp_server`](../../../examples/acp_server) | A tool-using ADK-Rust LLM agent exposed to editors through ACP v1 |

---

**Previous**: [← Benchmarking](benchmarking.md) | **Next**: [Retry & Reflect →](retry-reflect.md)
