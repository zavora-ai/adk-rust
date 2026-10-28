# streaming_bash

Live tool output in the browser — an `LlmAgent` whose tool activity (calls,
streaming stdout/stderr, and final results) is rendered from a **single
`EventStream`**.

This example demonstrates two `adk-core` capabilities:

1. **Streaming tool progress** — `ToolContext::emit_progress` lets a tool push
   stdout/stderr to the UI *while it is still running*. `BashTool`
   (`adk-devtools`) streams each line; the framework forwards it as a partial
   `Event`.
2. **First-class tool results** — `Event::tool_calls()` and
   `Event::tool_results()` surface every tool's call and output generically, so
   the UI renders **any** tool — streaming (`bash`) or one-shot (`read_file`,
   `grep`, `glob`) — not just shell tools.

## Architecture

```text
  browser ──prompt (JSON over WS)──▶ Rust /ws handler
  browser ◀──model text + tool ─────  LlmAgent + DevTools (Gemini)
           calls/progress/results      ├─ bash      → streams stdout/stderr
           (events)                     ├─ read_file → one-shot result
                                        ├─ grep      → one-shot result
                                        └─ glob      → one-shot result
```

The server owns the agent. For each `Event` it forwards a typed frame derived
from first-class accessors:

| Accessor | Frame | UI effect |
|----------|-------|-----------|
| `event.tool_calls()` | `tool_call` | open a card (tool name + args) |
| `event.tool_progress_stream()` | `terminal` | append a live stdout/stderr line |
| `event.tool_results()` | `tool_result` | finalize the card with the output |
| text parts | `token` | append to the assistant's reply |

Cards are correlated by `call_id`, so streaming and one-shot tools both render
correctly even across multiple tool calls in one turn.

## Run

```bash
# Web UI (default) — open http://127.0.0.1:3000
cargo run --manifest-path examples/streaming_bash/Cargo.toml

# Console demo
cargo run --manifest-path examples/streaming_bash/Cargo.toml -- cli
```

Set `PORT` to change the web port (e.g. `PORT=3717 cargo run ...`).

## Configuration

`.env.example` lists these; `.env` is read from the directory you run in or any parent.

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | yes | Gemini API key (model: `gemini-3.7-flash`). |
| `PORT` | no | Web server port (default `3000`). |
| `BIND_HOST` | no | Address to listen on (default `127.0.0.1`). `0.0.0.0` listens on every interface and logs a warning. |

## Network exposure

The agent runs shell commands in the directory you start it from, and the server has no
authentication. Two guards limit who can drive it:

| Guard | Effect |
|-------|--------|
| Loopback bind | The server listens on `127.0.0.1` unless `BIND_HOST` names another address, and prints the address it bound. With `BIND_HOST=0.0.0.0`, anyone who can reach the port can run commands as you. |
| `Origin` check | A request whose `Origin` is not this server's page gets `403 Forbidden`, the `/ws` upgrade included. Browsers apply no CORS to WebSockets, so without it any site you visit could open the socket. |

The page's origin is `http://` and the bound address, where any loopback address and
`localhost` count as one host. On `BIND_HOST=0.0.0.0` it is the IP address the browser
connected to, so open the page by IP address rather than by host name; host names other than
`localhost` are refused, which also stops DNS rebinding.

A request without an `Origin` header is accepted. Browsers send `Origin` on every WebSocket
handshake, so such an upgrade comes from a non-browser client such as `websocat`, which could
send any `Origin` it liked; the bind address is what limits those clients. The check lives in
`src/origin.rs`.

CORS stays permissive: the server's only plain HTTP route is the static page.

## Try

- `Run exactly: for i in $(seq 1 10); do echo "tick $i"; sleep 0.4; done` — watch
  lines stream in live (one every 0.4s) via `bash`.
- `Read the root Cargo.toml` — `read_file` renders its result with no streaming.
- `Search for "tokio" in the root Cargo.toml` — `grep` result card.
- `Find all README.md files in examples` — `glob` result card.

## Key code

- `src/server.rs` — `run_turn` translates each `Event` into a typed WS frame
  using `tool_calls()` / `tool_progress_stream()` / `tool_results()`.
- `assets/index.html` — renders one card per tool call (keyed by `call_id`),
  streaming `bash` output live and finalizing every tool with its result.
- The streaming `bash` tool itself lives in `adk-devtools`
  (`src/tools/bash.rs`), and `emit_progress` is defined on `ToolContext` in
  `adk-core`.
