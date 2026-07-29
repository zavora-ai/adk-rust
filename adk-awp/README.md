# adk-awp

Agentic Web Protocol (AWP) implementation for [ADK-Rust](https://github.com/zavora-ai/adk-rust).

[![crates.io](https://img.shields.io/crates/v/adk-awp.svg)](https://crates.io/crates/adk-awp)
[![docs.rs](https://docs.rs/adk-awp/badge.svg)](https://docs.rs/adk-awp)
[![AWP](https://img.shields.io/badge/AWP-agenticwebprotocol.com-0F8A8A)](https://agenticwebprotocol.com)

`adk-awp` provides AWP protocol types, route registration, middleware, rate
limiting, consent, events, health monitoring, and business context management.
Applications provide the agent-specific A2A dispatcher and protect management
routes with their authentication layer.

## Overview

Use `adk-awp` when you need to:

- serve AWP discovery documents and capability manifests from a `business.toml`
- apply per-trust-level rate limiting (Anonymous: 30/min, Known: 120/min, Partner: 600/min)
- manage consent records with in-memory or local file-backed storage
- subscribe agents to events with HMAC-SHA256 webhook signing
- monitor service health with a validated state machine (Healthy → Degrading → Degraded)
- detect whether requests come from humans or AI agents
- negotiate AWP protocol versions automatically

## Quick Start

### 1. Create a `business.toml`

```toml
site_name = "My Shop"
site_description = "An online store powered by AWP"
domain = "myshop.example.com"

[brand_voice]
tone = "friendly"
greeting = "Welcome! How can I help?"

[[capabilities]]
name = "browse_products"
description = "Browse the product catalog"
endpoint = "/api/products"
method = "GET"
access_level = "anonymous"

[[policies]]
name = "privacy"
description = "Minimal data collection."
policy_type = "privacy"
```

### 2. Serve AWP routes

```rust
use std::sync::Arc;

use adk_awp::{AwpA2aHandler, AwpState, BusinessContextLoader, awp_routes};
use async_trait::async_trait;
use awp_types::AwpError;
use axum::http::{HeaderMap, header};
use serde_json::{Value, json};

struct ApplicationA2a {
    bearer_token: Arc<str>,
}

#[async_trait]
impl AwpA2aHandler for ApplicationA2a {
    async fn handle(&self, headers: HeaderMap, message: Value) -> Result<Value, AwpError> {
        let expected = format!("Bearer {}", self.bearer_token);
        if !headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == expected)
        {
            return Err(AwpError::Unauthorized("invalid A2A credential".to_string()));
        }
        // Authorize the requested capability and dispatch to the application agent.
        Ok(json!({ "status": "processed", "messageId": message["id"] }))
    }
}

let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
let a2a_token: Arc<str> = std::env::var("AWP_A2A_TOKEN")?.into();
let state = AwpState::builder(loader.context_ref())
    .a2a_handler(Arc::new(ApplicationA2a { bearer_token: a2a_token }))
    .build();

let app = axum::Router::new()
    .merge(awp_routes(state))
    .merge(your_custom_routes);

let listener = tokio::net::TcpListener::bind("127.0.0.1:3456").await?;
axum::serve(
    listener,
    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
)
.await?;
```

This registers the four public endpoints with version negotiation and rate
limiting. `POST /awp/a2a` returns `503` until an `AwpA2aHandler` is installed.
Subscription management is registered separately behind application auth.
`ConnectInfo` supplies the peer address used to isolate anonymous rate-limit
buckets.

## Public endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/awp.json` | Discovery document |
| GET | `/awp/manifest` | JSON-LD capability manifest |
| GET | `/awp/health` | Health state |
| POST | `/awp/a2a` | Application-provided A2A dispatch |

## Authenticated management endpoints

`awp_management_routes()` returns these routes without an auth layer so the
application can apply its own:

| Method | Path | Description |
|--------|------|-------------|
| POST | `/awp/events/subscribe` | Create webhook subscription |
| GET | `/awp/events/subscriptions` | List subscriptions |
| DELETE | `/awp/events/subscriptions/{id}` | Delete subscription |

## Components

### AwpStateBuilder

Build `AwpState` with fail-closed defaults. Services default to in-memory and
the health state machine is wired to the event service. A2A dispatch returns
`503` until the application installs a handler:

```rust
use adk_awp::{AwpState, FileConsentService};
use std::sync::Arc;

let state = AwpState::builder(loader.context_ref())
    .consent_service(Arc::new(FileConsentService::new("data/consent.json")?))
    .build();
```

### BusinessContextLoader

Parse and validate `business.toml` with hot-reload support:

```rust
use adk_awp::BusinessContextLoader;

let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
loader.watch("business.toml".into()).await?; // hot-reload every 5s
let ctx = loader.load();
println!("Site: {}", ctx.site_name);
```

### Rate Limiting

Per-trust-level sliding window with configurable limits:

| Trust Level | Default Limit |
|-------------|--------------|
| Anonymous | 30 req/min |
| Known | 120 req/min |
| Partner | 600 req/min |
| Internal | Unlimited |

```rust
use adk_awp::{InMemoryRateLimiter, RateLimitConfig};
use awp_types::TrustLevel;
use std::collections::HashMap;

let mut limits = HashMap::new();
limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 10, window_secs: 60 });
let limiter = InMemoryRateLimiter::with_config(limits);
```

`DefaultTrustAssigner` classifies every request as anonymous. An authorization
header is not proof that a credential is valid. Install a verifier-backed
`TrustLevelAssigner` before assigning higher trust levels.

### Consent Service

Two implementations:

- **`InMemoryConsentService`** — ephemeral, for development
- **`FileConsentService`** — durable local JSON; the application controls file
  permissions, encryption, retention, and regulatory policy

```rust
use adk_awp::FileConsentService;

let consent = FileConsentService::new("data/consent.json")?;
consent.capture_consent("visitor-123", "analytics").await?;
assert!(consent.check_consent("visitor-123", "analytics").await?);
consent.revoke_consent("visitor-123", "analytics").await?;
```

### Health State Machine

Validated transitions with event emission:

```
Healthy → Degrading → Degraded → Healthy
```

Invalid transitions (e.g., Healthy → Degraded) return an error.

### Event Subscriptions

CRUD with HMAC-SHA256 webhook signing:

```rust
use adk_awp::{sign_payload, verify_signature};

let sig = sign_payload(b"event payload", "webhook-secret");
assert!(verify_signature(b"event payload", "webhook-secret", &sig));
```

### Requester Detection

Detect human vs agent from HTTP headers:

```rust
use adk_awp::detect_requester_type;
use axum::http::HeaderMap;

let mut headers = HeaderMap::new();
headers.insert("X-AWP-Channel", "agent".parse().unwrap());
let requester = detect_requester_type(&headers);
// RequesterType::Agent
```

`InMemoryEventSubscriptionService` signs and logs matching deliveries without
performing network I/O. Implement `EventSubscriptionService` for production
HTTP delivery so destination policy, retries, and durable storage remain under
application control.

## Testing

```bash
cargo nextest run -p adk-awp             # protocol and boundary tests
cargo clippy -p adk-awp -- -D warnings  # zero warnings
```

## Example

```bash
cd examples/awp_agent
cp .env.example .env   # add your GOOGLE_API_KEY
cargo run
```

The example loads `business.toml`, installs authenticated A2A dispatch to a real
LLM agent, protects management routes separately, and exercises each endpoint.

## Documentation

See [AWP Documentation](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/deployment/awp.md) for the full guide.

## Related Crates

- [`awp-types`](https://crates.io/crates/awp-types) — Pure protocol types (zero `adk-*` deps)
- [`adk-server`](https://crates.io/crates/adk-server) — HTTP server with A2A protocol
- [`adk-core`](https://crates.io/crates/adk-core) — ADK foundational traits

## License

Apache-2.0
