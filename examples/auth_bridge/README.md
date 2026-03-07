# Auth Bridge Example

Demonstrates the auth middleware bridge that flows authenticated identity from HTTP requests into agent tool execution.

## What it shows

1. A custom `RequestContextExtractor` that validates Bearer tokens and extracts user identity + scopes
2. Scope-aware tools that check `ctx.user_scopes()` before returning data
3. The server rejecting unauthenticated requests with 401
4. `user_id()` in the invocation context being overridden by the authenticated identity

## Running

```bash
# Set your API key
export GOOGLE_API_KEY=your-key

# Start the server
cargo run -p adk-examples --example auth_bridge
```

## Testing with curl

```bash
# Admin user (alice) — has "admin" + "read" scopes
curl -N -X POST http://localhost:8080/api/run_sse \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer admin-token-abc' \
  -d '{"appName":"auth_demo","userId":"ignored","sessionId":"s1","newMessage":{"role":"user","parts":[{"text":"Show me the secret data"}]}}'

# Read-only user (bob) — has only "read" scope
curl -N -X POST http://localhost:8080/api/run_sse \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer reader-token-xyz' \
  -d '{"appName":"auth_demo","userId":"ignored","sessionId":"s2","newMessage":{"role":"user","parts":[{"text":"Show me the secret data"}]}}'

# No token — returns 401
curl -X POST http://localhost:8080/api/run_sse \
  -H 'Content-Type: application/json' \
  -d '{"appName":"auth_demo","userId":"u1","sessionId":"s3","newMessage":{"role":"user","parts":[{"text":"hello"}]}}'
```

## Token map

| Token | User | Scopes |
|-------|------|--------|
| `admin-token-abc` | alice | admin, read |
| `reader-token-xyz` | bob | read |

## Architecture

```
HTTP Request
  │
  ├─ Authorization: Bearer admin-token-abc
  │
  ▼
DemoTokenExtractor (implements RequestContextExtractor)
  │
  ├─ Extracts: user_id="alice", scopes=["admin","read"]
  │
  ▼
RequestContext → RunnerConfig → InvocationContext
  │
  ├─ ctx.user_id() returns "alice" (overrides request body)
  ├─ ctx.user_scopes() returns ["admin","read"]
  │
  ▼
SecretDataTool checks scopes → allows/denies
```
