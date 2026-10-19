# ACP testing and support matrix

ACP is bidirectional. A useful interoperability test must keep one connection
open while notifications and nested requests arrive; a series of disconnected
JSON lines cannot prove session behavior.

## Verified tests

The interoperability tests in `adk-acp` (`src/server/transport/stdio.rs`)
connect the official SDK `Client` to the ADK-Rust SDK `Agent` through an
in-memory transport and exercise:

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

Further tests cover:

| Area | What is checked |
|------|-----------------|
| Cancellation | Session cancellation, JSON-RPC request cancellation, and recovery |
| Permissions (client) | Reject-first menus, opaque option IDs, fabricated selections, awaited human decisions, exact-call allow-once |
| Permissions (server) | The bridge approves, denies, and maps a cancellation to deny |
| `session/load` | Replayed `session/update`s follow the stored events' order |
| Prompt content | Image and audio accepted; content the server does not advertise rejected; `ContentBlock` ↔ `Part` round trips (`tests/content_mapping_property_tests.rs`) |
| Client fidelity | `ToolCallUpdate` and `UsageUpdate` surfaces alongside agent text |
| Session controls | `set_mode` and `set_config_option` record advertised values, reject unknown ones, and persist across `session/load` |
| `session/fork` | The fork's history equals the source's, and the source is unchanged |
| Activation updates | Available commands and session info emitted only when declared or recorded |
| Capabilities | The advertised capabilities match the registered handlers and content mappings; modes and options only with a `SessionControls` provider |
| Process lifecycle | Every client entry point returns, rather than hangs, when the agent process exits (`tests/process_lifecycle_tests.rs`) |
| Configuration | MCP configuration validation and secret-redacted debug output |

## Run the gates

```bash
cargo nextest run -p adk-acp --all-features
cargo nextest run -p adk-agent --test tool_confirmation_tests
cargo nextest run -p adk-tool --features mcp --test mcp_server_lifecycle_integration_tests --run-ignored only -E 'test(test_tool_aggregation)'

cargo test --manifest-path examples/acp_client_host/Cargo.toml
cargo check --manifest-path examples/acp_kiro/Cargo.toml
cargo check --manifest-path examples/acp_server/Cargo.toml
cargo test --manifest-path examples/acp_full_protocol/Cargo.toml
```

`adk-acp`'s only feature is `server`. The `mcp_server_lifecycle_integration_tests`
gate starts real stdio MCP servers through `npx`, so it needs Node.js.

`acp_full_protocol` is a no-API-key, `Runner`-backed `AcpServer` driven through
the official SDK over an in-process channel. Its test checks embedded-resource and
image/audio prompts, the permission bridge, `session/load` replay ordering, and
`UsageUpdate` / `ToolCallUpdate` surfacing without a subprocess or model
credentials.

`acp_openai_e2e` runs both directions against a live OpenAI model: one binary
spawns itself as an ACP server and drives it with `prompt_agent_with_policy`, an
`AcpSession`, and an `LlmAgent` coordinator, checking the permission bridge and
that each approved call runs once. It needs `OPENAI_API_KEY`; `OPENAI_MODEL`
selects the model.

## Current support

| Area | Status | Notes |
|---|---|---|
| Stable wire protocol v1 | Implemented | `agent-client-protocol` SDK 3.3 (ACP schema 1.11), re-exported as `adk_acp::agent_client_protocol`; the wire protocol version is negotiated separately |
| Local stdio client transport | Implemented | One-shot, streaming, and persistent sessions |
| Client permissions | Implemented | Deny by default, semantic matching, opaque IDs, sync or async policy |
| Client filesystem callbacks | Implemented API | Read and write advertised independently |
| Client terminal callbacks | Implemented API | Complete create/output/wait/kill/release trait |
| Client-supplied MCP | Implemented | stdio required; HTTP/SSE capability-gated |
| ADK-Rust ACP server | Implemented | New, prompt, load, cancel, close, list, resume, fork, set_mode, set_config_option, delete |
| Server session load + replay | Implemented | Reactivates a persisted session and replays stored events in chronological order; `load_session` advertised |
| Server session fork | Implemented | Copies history and relevant state into a new session id, leaving the source unchanged; `fork` advertised |
| Server session modes + config options | Implemented | Provider-gated via `SessionControls`; `set_mode` / `set_config_option` validated and persisted across load/resume/fork; advertised only when declared |
| Server available-commands + session-info | Implemented | Emitted on activation when the agent declares commands or records a title; none otherwise |
| Server plan updates | Dormant | `Plan` `SessionUpdate` mapping exists but is inert until an ADK plan primitive surfaces plan entries |
| Server session MCP | Implemented | stdio, per session, at most 16 servers, 15-second startup bound, cleanup on close; two servers sharing a tool name fail session setup |
| Text and resource-link prompts | Implemented | Mapped through the shared content module |
| Multimodal prompts (image, audio) | Implemented | Mapped to `Part::InlineData`; `image`/`audio` advertised; unadvertised content rejected |
| Embedded-resource prompts | Implemented | Mapped to `Part::EmbeddedResource`; `embedded_context` advertised |
| Server ADK tool approval to ACP | Implemented | `ToolConfirmationRequest` bridged to `session/request_permission`; allow → approve, deny/cancel → deny; each decision bound to the call's ID and fingerprint |
| Client tool-update and usage fidelity | Implemented | `OutputChunk::ToolUpdate` and `OutputChunk::Usage` surface the external agent's `ToolCallUpdate`/`UsageUpdate` |
| Client rich prompt content | Implemented | `prompt_agent_content_with_policy` transmits non-text ADK content as the matching ACP block |
| Remote ACP HTTP/WebSocket | Not advertised | The stable implementation is local stdio |
| Experimental protocol features | Not advertised | Add only after implementation and interoperability tests |

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
- [`acp_full_protocol`](../../../examples/acp_full_protocol) — no-API-key, `Runner`-backed server-direction reference with an end-to-end validating test
- [`acp_openai_e2e`](../../../examples/acp_openai_e2e) — both directions against a live OpenAI model
