# ACP server example

This example exposes an ADK-Rust coding assistant as a stable ACP v1 agent. An
ACP client—usually an editor—starts the binary and exchanges JSON-RPC messages
over stdin/stdout.

```text
Editor / ACP client
  │ initialize · session/new · session/prompt · session/cancel · session/close
  ▼
Official ACP SDK stdio transport
  ▼
AcpSessionHandler ── persisted ADK session
  ▼
ADK-Rust Runner
  ▼
coding-assistant ── read_file · list_directory
  │
  └── session/update notifications stream back to the editor
```

## Run it

```bash
cd examples/acp_server
cp .env.example .env
# Add GOOGLE_API_KEY to .env
cargo run
```

Protocol traffic is the only output written to stdout. Diagnostics and tracing
go to stderr so they cannot corrupt the ACP connection.

## Configure an ACP client

An editor or ACP test client should start this binary as a subprocess:

```json
{
  "name": "adk-rust-coding-assistant",
  "command": "cargo",
  "args": [
    "run",
    "--quiet",
    "--manifest-path",
    "/absolute/path/to/adk-rust/examples/acp_server/Cargo.toml"
  ],
  "env": {
    "GOOGLE_API_KEY": "set-this-in-your-client-secret-store"
  }
}
```

The exact configuration envelope differs by editor. The command, arguments,
environment, and stdio transport are the important parts.

## What the client and server exchange

ACP uses JSON-RPC 2.0 with camelCase fields and numeric protocol version `1`.
The following examples are formatted across lines for reading; the stdio
transport writes one JSON object per line.

### 1. Initialize

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientCapabilities": {},
    "clientInfo": { "name": "example-editor", "version": "1.0.0" }
  }
}
```

The response reports stable session list, delete, additional-directories,
resume, and close capabilities plus ADK-Rust implementation metadata.

### 2. Create a session

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/new",
  "params": {
    "cwd": "/absolute/path/to/project",
    "mcpServers": []
  }
}
```

`cwd` must be absolute. The returned `sessionId` identifies the persisted
ADK-Rust session used for every later turn.

### 3. Send a prompt

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/prompt",
  "params": {
    "sessionId": "<session-id>",
    "prompt": [
      { "type": "text", "text": "Explain the error handling in src/main.rs" }
    ]
  }
}
```

While the Runner works, the server sends separate `session/update`
notifications for message text, thoughts, tool calls, and tool completion. The
final response closes the turn:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": { "stopReason": "end_turn" }
}
```

### 4. Cancel or close

`session/cancel` is a notification. It cancels the active Runner invocation and
the outstanding prompt completes with `stopReason: "cancelled"`.

```json
{
  "jsonrpc": "2.0",
  "method": "session/cancel",
  "params": { "sessionId": "<session-id>" }
}
```

`session/close` is a request. It cancels active work and releases the active
session while leaving persisted history available for `session/resume`.

## Why manual `echo` testing is discouraged

ACP is bidirectional and a prompt can produce notifications before its final
response. A sequence of disconnected `echo | cargo run` commands starts a new
process each time and cannot preserve the connection or session. Use an ACP SDK
client or an editor integration instead.

The `adk-acp` unit suite already runs an official SDK client against the server
through an in-memory channel:

```bash
cargo test -p adk-acp --all-features \
  official_client_completes_initialize_session_prompt_and_close
```

## Production decisions

- Replace `InMemorySessionService` with a durable session backend when editor
  sessions must survive process restarts.
- The demonstration tools canonicalize every requested path against the server
  process's startup workspace. Start the binary in the project it should expose,
  keep that application check, and add an OS or `adk-sandbox` process boundary
  before accepting untrusted prompts.
- Keep secrets in the editor or process environment; never print them to
  protocol stdout or commit them to `.env`.
- Stdio is the supported stable transport. Remote ACP HTTP/WebSocket work is
  still evolving in the protocol and is not implemented by this example.
