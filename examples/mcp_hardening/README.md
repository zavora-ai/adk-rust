# MCP hardening

An `LlmAgent` with `McpToolset` and `McpServerManager`, run through a `Runner`
against MCP servers that misbehave. Each scenario prints `PASS` or `FAIL`, and
the process exits non-zero on any failure.

The MCP servers are scripted in this binary: in-process over a pipe, as stdio
child processes of the binary itself (for `McpServerManager`), or as a local
Streamable HTTP server. The agent's model is the real `GeminiModel` client
pointed at a local scripted Gemini endpoint, except in `live_gemini_turn`.

```bash
cargo run --manifest-path examples/mcp_hardening/Cargo.toml
```

`live_gemini_turn` runs only when `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) is set
in the repository's `.env` (or a `.env` in the working directory or a parent); it
makes two or four `gemini-3.5-flash-lite` calls (`GEMINI_MODEL` overrides the model).

## Scenarios

| Scenario | Server behaviour | Passes when |
|----------|------------------|-------------|
| `endless_cursor` | Returns a fresh `nextCursor` on every `tools/list` page | A `strict_toolsets` turn fails promptly after at most 64 pages, before any model call |
| `cyclic_cursor` | Cursors cycle `a -> b -> a` with a one-minute `ttlMs` | The turn fails after at most 3 `tools/list` requests |
| `duplicate_names_beside_healthy_server` | Publishes `dup` twice, a name with control characters, and two 71-byte names sharing a 64-byte prefix, beside a healthy order server | The turn answers from `lookup_order`; every declared name is unique, at most 64 bytes, and free of control characters |
| `live_gemini_turn` | The same pair, with live Gemini | Gemini calls `lookup_order` and answers with its tracking code |
| `manager_hung_server` | One managed server never answers `tools/list` | The manager's healthy server still answers within the discovery timeout |
| `manager_prefix_collision` | Managed servers `a` (`b__x`), `a__b` (`x`), and `z` (`b__x`, `x`) | The manager declares `a__b__x` once, and `z__x` reaches server `z` |
| `annotations` | Publishes `idempotentHint`, `readOnlyHint`, and `destructiveHint` tools | Effects are `Idempotent`, `ReadOnly`, `NonIdempotent`; `Tool::annotations` returns the hints, also behind `PrefixedToolset` |
| `http_5xx_geoffrey` | Answers one `tools/list` with HTTP 500, body `upstream geoffrey is restarting` | The error is returned, the session is not replaced, and the next `tools/list` succeeds on it |
| `invalidation` | Changes its catalog during a `tools/list`, and with a `ttlMs` | `invalidate_tool_list_cache` makes the next listing return the new catalog |
| `call_tool_value_filter` | Publishes `public` and `secret`; the toolset admits `public` only | `call_tool_value("secret")` fails without a server call |

Set `RUST_LOG=adk_tool=warn` to see the warnings for skipped tools.
