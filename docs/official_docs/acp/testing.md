# ACP testing and support matrix

ACP is bidirectional. A useful interoperability test must keep one connection
open while notifications and nested requests arrive; a series of disconnected
JSON lines cannot prove session behavior.

## Verified tests

The `adk-acp` suite connects the official SDK `Client` to the ADK-Rust SDK
`Agent` through an in-memory transport and exercises:

```text
initialize
  → session/new
  → session/prompt
  ← session/update
  ← PromptResponse(end_turn)
  → session/close
  → session/list
  → session/resume
  → session/close
  → session/delete
```

Separate tests cover session cancellation, JSON-RPC request cancellation and
recovery, event mapping, reject-first permission menus, opaque option IDs,
fabricated selections, awaited human decisions, exact-call allow-once behavior,
MCP configuration validation, and secret-redacted debug output.

The live MCP lifecycle gate starts a real stdio MCP child, completes the
handshake, and discovers its tools through the same `McpToolset` used by ACP
sessions.

## Run the gates

```bash
cargo test -p adk-acp --all-features
cargo test -p adk-agent --test tool_confirmation_tests
cargo test -p adk-tool --features mcp --test mcp_server_lifecycle_integration_tests test_tool_aggregation -- --ignored --exact

cargo test --manifest-path examples/acp_client_host/Cargo.toml
cargo check --manifest-path examples/acp_kiro/Cargo.toml
cargo check --manifest-path examples/acp_server/Cargo.toml
```

## Current support

| Area | Status | Notes |
|---|---|---|
| Stable wire protocol v1 | Implemented | Official Rust SDK 1.2; protocol version negotiated separately |
| Local stdio client transport | Implemented | One-shot, streaming, and persistent sessions |
| Client permissions | Implemented | Deny by default, semantic matching, opaque IDs, sync or async policy |
| Client filesystem callbacks | Implemented API | Read and write advertised independently |
| Client terminal callbacks | Implemented API | Complete create/output/wait/kill/release trait |
| Client-supplied MCP | Implemented | stdio required; HTTP/SSE capability-gated |
| ADK-Rust ACP server | Implemented | New, prompt, update, cancel, close, list, resume, delete |
| Server session MCP | Implemented | stdio, per session, bounded startup and cleanup |
| Text and resource-link prompts | Implemented | Unsupported media rejected and unadvertised |
| Server ADK tool approval to ACP | Held back | Official SDK nested-request response issue described in the server guide |
| Remote ACP HTTP/WebSocket | Not advertised | The stable implementation is local stdio |
| Optional media and experimental protocol features | Not advertised | Add only after implementation and interoperability tests |

## Manual editor test

Build `examples/acp_server`, then configure an ACP client to start that binary
with an absolute manifest path and model credentials. Verify:

1. the initialization response reports protocol version 1;
2. a new session accepts the intended absolute project directory;
3. text appears as live updates before the final response;
4. read tool starts and completions appear in the client;
5. cancellation closes the turn without closing the connection;
6. a later prompt succeeds in the same session;
7. close and resume preserve history when the session service is durable.

Do not use `echo | cargo run` for this test. Each pipe starts a different
process and cannot preserve the connection or session.

## Related examples

- [`acp_client_host`](../../../examples/acp_client_host)
- [`acp_kiro`](../../../examples/acp_kiro)
- [`acp_server`](../../../examples/acp_server)
