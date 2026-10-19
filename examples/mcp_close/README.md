# MCP close

`Runner::close` on an `LlmAgent` whose tools come from MCP servers. Every scenario
drives the real stack — `LlmAgentBuilder`, `Runner` over the in-memory session
service, `McpToolset`, `McpHttpClientBuilder`, `McpServerManager`, and the composed
toolsets — and checks what happens to the server when the runner closes.

The stdio MCP server is this binary, started as a child process with
`--mcp-server <mode>`; its PID is read from the `TokioChildProcess` transport, or
from its `server_pid` tool for the manager. The Streamable HTTP MCP server is a
local axum server. The agent's model is the real `GeminiModel` client pointed at a
local scripted Gemini endpoint, except in `live_runner_close_stops_stdio_server`,
which runs on `gemini-3.5-flash-lite`.

## Scenarios

| Scenario | Model | Passes when |
|----------|-------|-------------|
| `runner_close_stops_stdio_server` | Scripted | The agent calls `lookup_order`, then the stdio server's process has exited when `Runner::close` returns |
| `live_runner_close_stops_stdio_server` | Gemini | The same on `gemini-3.5-flash-lite` |
| `closed_toolset_does_not_reconnect` | Scripted | After `Runner::close`, a run fails with the toolset's closed error before any model call, its connection factory starts no server, and a second `Runner::close` returns at once |
| `composed_toolsets_close_members` | Scripted | Both servers behind `PrefixedToolset(MergedToolset[McpToolset, FilteredToolset(McpToolset)])` have exited when `Runner::close` returns |
| `close_is_bounded` | Scripted | A server that ignores the end of its input holds neither `Runner::close` call past the 500 ms close timeout, and is killed within 6 seconds |
| `http_session_ends` | Scripted | The Streamable HTTP server receives `DELETE` for the session when `Runner::close` returns, and a later run opens no new session |
| `manager_servers_outlive_runner_close` | Scripted | A shared `McpServerManager`'s server is still `Running` after `Runner::close`, and its process has exited when `shutdown` returns |

## Running

```bash
cargo run --manifest-path examples/mcp_close/Cargo.toml
cargo run --manifest-path examples/mcp_close/Cargo.toml -- --no-live
cargo run --manifest-path examples/mcp_close/Cargo.toml -- --scenario close_is_bounded,http_session_ends
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence, followed by a summary
table; the process exits non-zero on any `FAIL`. Keys are loaded from the dotenv file
named by `ADK_ENV_FILE`, else from the repository's `.env`; either overrides the process
environment. The live scenario runs when `GOOGLE_API_KEY` (or `GEMINI_API_KEY`) is set,
and makes two model calls. `--no-live` skips it. The PID checks use `kill -0`, so the
example runs on Linux and macOS.

The `main-baseline` feature builds against a release without
`McpToolset::with_close_timeout`; `close_is_bounded` then uses the default close.
