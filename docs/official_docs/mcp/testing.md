# MCP testing and verification

MCP integrations cross process, transport, schema, and authorization
boundaries. Unit tests alone are insufficient; keep at least one deterministic
client/server test that completes a real handshake and tool call.

## Focused framework gates

```bash
cargo nextest run -p adk-tool --features mcp
cargo nextest run -p adk-tool --features mcp,http-transport
cargo nextest run -p adk-tool --features mcp,mcp-sampling
cargo clippy -p adk-tool --features mcp,http-transport,mcp-sampling --all-targets -- -D warnings
```

The `adk-tool` integration tests for MCP:

| Test target | Feature | Covers |
|-------------|---------|--------|
| `mcp_roundtrip_property_tests` | `mcp` | JSON-RPC message serialization round trips |
| `mcp_discovery_bounds_tests` | `mcp` | Page caps, repeated cursors, and timeouts; tool-list cache invalidation; an HTTP 5xx never reconnecting |
| `mcp_reconnect_tests` | `mcp` | Crashed-server reconnects, concurrent `SimpleClient` and `ConnectionRefresher` requests, no replay of an interrupted call by default |
| `mcp_toolset_concurrency_tests` | `mcp` | Parallel calls and discovery, dropped-call cancellation, the `tools/list` cache, one reconnect for concurrent failures |
| `mcp_tool_origin_tests` | `mcp` | `mcp:{toolset name}` origins and approval binding |
| `mcp_lifecycle_regression_tests` | `mcp` | Direct calls keeping the connection's input policy, subscriptions, and remote task lifecycle |
| `mcp_protocol_compatibility_tests` | `mcp` | Servers that speak only the legacy `initialize` handshake, and the `server/discover` path |
| `mcp_close_tests` | `mcp`, Unix hosts only | `Toolset::close` on stdio and Streamable HTTP toolsets, a shared `McpServerManager`, and `Runner::close` |
| `mcp_http_oauth_tests` | `http-transport` | OAuth 2.0 token refresh and 401 retry |
| `mcp_http_broker_tests` | `http-transport` | Credential-broker tokens sent only to their resource |
| `mcp_sampling_property_tests`, `mcp_sampling_metering_tests` | `mcp-sampling` | Sampling conversion and metering |
| `mcp_server_lifecycle_integration_tests` | `mcp` | `McpServerManager` against `npx` servers; `#[ignore]`, run with `-- --ignored` on a host with Node.js |

The sampling gates are compatibility coverage for an upstream-deprecated
capability, not a recommendation for new architecture. CI's `feature-coverage`
job builds `adk-tool` with `mcp-sampling` (`scripts/feature-coverage-pairs.txt`).

## Compile integration consumers

```bash
cargo check -p adk-acp --all-features
cargo check -p adk-computer-use
```

These gates catch mismatched public `rmcp` types across crates that embed or
carry MCP configurations.

## Standalone examples

Each example is a standalone crate run with
`cargo run --manifest-path examples/<name>/Cargo.toml`.

| Example | What it runs | Needs |
|---------|--------------|-------|
| `mcp_manager` | A real Rust stdio server started from its own binary in fixture mode: discovery, a tool call, runtime add/enable/update/disable/remove, persistence, shutdown | Nothing |
| `mcp_close` | `Runner::close` against stdio, Streamable HTTP, and manager-backed toolsets | Nothing; one live scenario uses `GOOGLE_API_KEY` |
| `mcp_hardening` | `McpToolset` and `McpServerManager` against hostile servers, through `LlmAgent` and `Runner` | Nothing; one live scenario uses Gemini |
| `mcp_protocol_revisions` | The default handshake, `server/discover`, and SEP-2663 tasks against a bundled server | `GOOGLE_API_KEY` for the agent section only |
| `approval_scoping` | Approvals bound to the agent and the MCP server that held the call | Nothing; one live scenario uses OpenAI |
| `tools_hardening` | Live-model scenarios for the tool, MCP, auth, and code-execution fixes | A model API key; the code scenarios use the `docker` CLI |
| `mcp_resources` | Resources, prompts, and resource-update notifications (`--bin resources-client`) | `GOOGLE_API_KEY` |
| `mcp_elicitation`, `mcp_sampling` | Elicitation and sampling client/server pairs | `GOOGLE_API_KEY` |

`examples/mcp_manager` is the deterministic live gate: it does not download a
package or contact a remote endpoint.

## What the live example proves

- child-process startup;
- MCP initialization and capability negotiation;
- tool discovery;
- a real tool request and result;
- runtime add, enable, update, disable, and remove;
- registry serialization and file persistence; and
- session shutdown.

## What requires deployment-specific testing

- remote HTTP identity and token policy;
- full MCP OAuth discovery and user authorization;
- external API and database health;
- process sandbox enforcement;
- tenant isolation;
- approval UI and audit storage;
- retry and idempotency behavior for consequential tools; and
- task recovery after an application restart.

## Test server contracts

For every server, keep fixtures for:

- initialization capability accuracy;
- tool and resource schemas;
- invalid and unauthorized inputs;
- maximum result sizes;
- cancellation and timeout behavior;
- idempotent retry of write operations; and
- redaction of credentials and sensitive fields.
