# ServerBuilder Example

Demonstrates the `ServerBuilder` API from ADK-Rust v0.7.0 for registering custom Axum controllers alongside the built-in REST, A2A, and UI routes with shared middleware.

## What This Shows

- Creating a `ServerConfig` with a minimal no-op agent and in-memory session service
- Defining custom controller routers for domain-specific endpoints (projects, automations)
- Using `ServerBuilder::new(config).add_api_routes(...)` to compose them into the server
- Custom routes living under `/api` alongside built-in health, session, and runtime routes
- Graceful shutdown via `POST /api/shutdown` using `enable_shutdown_endpoint()`
- Verifying that both custom and built-in endpoints work together
- The built-in `/api/health` endpoint still functions normally

## Why ServerBuilder?

Before `ServerBuilder`, the only way to create an ADK server was via `create_app()` or `create_app_with_a2a()`. These functions return a fully assembled `Router` with no way to inject custom routes that share the same middleware stack (auth, CORS, tracing, timeout, security headers).

`ServerBuilder` solves this by letting you compose the server incrementally:

```rust
use adk_server::{ServerBuilder, ServerConfig};
use axum::{Router, routing::get};

let app = ServerBuilder::new(config)
    // Routes under /api — get auth middleware automatically
    .add_api_routes(
        Router::new()
            .route("/projects", get(list_projects))
            .route("/projects/{id}", get(get_project))
    )
    // More API routes — call add_api_routes() as many times as needed
    .add_api_routes(
        Router::new()
            .route("/automations", get(list_automations))
    )
    // Root-level routes — no auth middleware, but still get CORS/tracing/security headers
    // .add_root_routes(Router::new().route("/webhook", post(handle_webhook)))
    // Enable A2A protocol
    // .with_a2a("http://localhost:8080")
    .build();
```

## API

| Method | Description |
|--------|-------------|
| `ServerBuilder::new(config)` | Create a builder from a `ServerConfig` |
| `.add_api_routes(router)` | Add routes nested under `/api` with auth middleware |
| `.add_root_routes(router)` | Add routes at the root level (no auth middleware) |
| `.with_a2a(base_url)` | Enable A2A protocol endpoints |
| `.enable_shutdown_endpoint()` | Enable `POST /api/shutdown` for graceful shutdown |
| `.build()` | Build the final `axum::Router` |
| `.build_with_shutdown()` | Build and return a `ShutdownHandle` for graceful shutdown |

### Graceful Shutdown

```rust
let (app, shutdown_handle) = ServerBuilder::new(config)
    .enable_shutdown_endpoint()
    .build_with_shutdown();

axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_handle.signal())
    .await?;
```

`ShutdownHandle::signal()` resolves when any of these occur:
- `POST /api/shutdown` is called
- Ctrl+C is pressed
- SIGTERM is received
- `shutdown_handle.shutdown()` is called programmatically
| `.build()` | Build the final `axum::Router` with all middleware applied |

### Middleware Stack

All routes (built-in and custom) receive:
- CORS (configurable via `SecurityConfig`)
- Request tracing with `x-request-id` header
- Request timeout (default 30s)
- Body size limit (default 10MB)
- Security headers: `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `X-XSS-Protection: 1; mode=block`

Routes added via `add_api_routes()` additionally receive the auth middleware layer (when a `RequestContextExtractor` is configured on the `ServerConfig`).

## Prerequisites

- Rust 1.85+
- No LLM provider or API keys required

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `RUST_LOG` | No | Logging level (defaults to `info`) |

## Run

```bash
cargo run --manifest-path examples/server_builder/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  ServerBuilder — ADK-Rust v0.7.0         ║
╚══════════════════════════════════════════╝

📦 Created ServerConfig with no-op agent and in-memory sessions

🔧 Built server with custom project and automation routes

🚀 Server listening on http://127.0.0.1:<port>

── Built-in: Health Check ───────────────────────
GET /api/health → 200 OK
  Response: { "status": "healthy", ... }

── Custom: Projects ────────────────────────────
GET /api/projects → 200 OK
  Found 2 project(s):
    - Website Redesign (proj-1): active
    - API Migration (proj-2): planning

POST /api/projects → 201 Created
  Created: New Project (proj-3)

── Custom: Automations ─────────────────────────
GET /api/automations → 200 OK
  Found 2 automation(s):
    - Nightly Build (auto-1): cron: 0 2 * * *
    - PR Review Bot (auto-2): webhook: pull_request.opened

GET /api/automations/auto-1 → 200 OK
GET /api/automations/auto-999 → 404 Not Found (expected 404)

✅ ServerBuilder example completed successfully.
```
