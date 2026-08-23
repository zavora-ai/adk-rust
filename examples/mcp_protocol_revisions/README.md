# MCP protocol revisions

Demonstrates MCP protocol revision `2026-07-28` and the SEP-2663 tasks
extension, ending with a Gemini-backed `LlmAgent` calling the tools.

The crate ships its own MCP server. That is not a convenience: at the time of
writing no public MCP server advertises `2026-07-28` on the wire. The official
reference server answers `server/discover` with `Method not found`, so the
revision cannot be shown against a third party.

## Run

```bash
cp .env.example .env   # add GOOGLE_API_KEY
cargo run --manifest-path examples/mcp_protocol_revisions/Cargo.toml --bin revisions-agent
```

The agent section is skipped when `GOOGLE_API_KEY` is unset; everything before
it still runs.

The model defaults to `gemini-3.7-flash`. Override it with
`GEMINI_MODEL`, for example `GEMINI_MODEL=gemini-3.1-pro-preview`.

## What it shows

**1. The default handshake.** Connects with `ServiceExt::serve` and negotiates
`2025-11-25`. Every MCP server has understood this handshake since `2024-11-05`,
which is why ADK-Rust keeps it as the default.

**2. Probing with `server/discover`.** Connects with
`ClientLifecycleMode::Auto` and negotiates `2026-07-28` against the same server.
`Auto` returns to the default handshake when a server refuses the probe with
`METHOD_NOT_FOUND` — and only that code, which is why the probe is opt-in.

**3. Which tools may run as tasks.** SEP-2663 removed the per-tool task
contract, so `Tool::is_long_running` answers per connection: true when the
client declared the extension and the server negotiated it.

**4. A slow call answered as a task.** `restock_warehouse` takes two seconds.
The server returns a task handle; `McpToolset` polls `tasks/get` until it
completes. The result carries `(completed as a task)`, which the server writes
only on its task branch.

**5. An `LlmAgent` calling both tools.** The agent sees ordinary tool results.
The task lifecycle stays inside `McpToolset`.

## The rule worth remembering

A server must not return a task to a client that did not declare the extension.
Two settings are therefore needed, and they do different jobs:

| Call | Job |
|------|-----|
| `AdkClientHandler::with_tasks()` | Declares the extension during the handshake. Without it the server answers inline. |
| `McpToolset::with_task_support(config)` | Sets how the client polls: interval, timeout, attempt cap. |

`with_task_support` alone configures a path no server will take.

## Files

| File | Contents |
|------|----------|
| `src/server.rs` | An MCP server on `rmcp 3.1` supporting `2026-07-28`, with one immediate tool and one that becomes a task |
| `src/main.rs` | The client, the five demonstrations, and the agent |
