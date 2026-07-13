# ACP coding-agent client examples

This crate demonstrates four ways an ADK-Rust application can use an external
ACP coding agent. The binaries currently use Kiro CLI, but the `adk-acp` APIs
are vendor-neutral and accept any compatible stdio command.

## Prerequisites

- `kiro-cli` installed, authenticated, and able to start in ACP mode;
- `GOOGLE_API_KEY` only for the orchestrator example.

The examples deliberately keep approval policy in ADK-Rust. They do not pass a
command-line flag that bypasses the ACP permission conversation.

## Direct one-shot prompt

Starts a fresh coding-agent process, negotiates stable ACP v1, opens one
session, sends one prompt, and returns the collected text:

```bash
cargo run --manifest-path examples/acp_kiro/Cargo.toml \
  --bin acp-kiro-direct
```

This binary uses semantic auto-approval and should only be used in a trusted
local workspace.

## ADK agent delegation

Makes the coding agent a named tool of an ADK-Rust LLM coordinator:

```bash
export GOOGLE_API_KEY=your-key
cargo run --manifest-path examples/acp_kiro/Cargo.toml \
  --bin acp-kiro-delegate
```

The coordinator decides whether a request requires repository work. The custom
permission policy allows ordinary operations once and rejects titles containing
destructive terms such as delete, drop, or sudo.

## Persistent multi-turn session

Keeps one process and ACP session alive while later prompts build on earlier
context:

```bash
cargo run --manifest-path examples/acp_kiro/Cargo.toml \
  --bin acp-kiro-session
```

Use `AcpSession` for a chat, editor pane, or workflow where several turns refer
to the same project and prior discussion.

## Environment and concurrent cancellation

Passes named environment values to the child process without logging their
contents, starts a long turn, and uses `AcpCancellationHandle` to send
`session/cancel` while the prompt is still being awaited:

```bash
cargo run --manifest-path examples/acp_kiro/Cargo.toml \
  --bin acp-kiro-env-cancel
```

After the cancelled turn closes, the binary sends another prompt through the
same session to prove the connection remains usable.

## Which example should I start with?

| Product shape | API | Binary |
|---|---|---|
| One isolated repository task | `prompt_agent` or `AcpAgentTool` | `acp-kiro-direct` |
| An LLM coordinator that delegates coding | `AcpAgentTool` | `acp-kiro-delegate` |
| A continuing project conversation | `AcpSession` | `acp-kiro-session` |
| UI timeout, shutdown, or stop button | `AcpCancellationHandle` | `acp-kiro-env-cancel` |

For streamed UI events and a client-controlled filesystem, see
[`examples/acp_client_host`](../acp_client_host). To make an ADK-Rust agent
available to an editor, see [`examples/acp_server`](../acp_server).
