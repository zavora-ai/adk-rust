# adk-awp

Agentic Web Protocol (AWP) implementation for [ADK-Rust](https://github.com/zavora-ai/adk-rust).

[![crates.io](https://img.shields.io/crates/v/adk-awp.svg)](https://crates.io/crates/adk-awp)
[![docs.rs](https://docs.rs/adk-awp/badge.svg)](https://docs.rs/adk-awp)
[![AWP](https://img.shields.io/badge/AWP-agenticwebprotocol.com-0F8A8A)](https://agenticwebprotocol.com)

`adk-awp` provides the full AWP protocol implementation — route registration,
middleware, rate limiting, consent, events, health monitoring, and business
context management. Plug `awp_routes()` into any Axum app to make it
AWP-compliant.

## Overview

Use `adk-awp` when you need to:

- serve AWP discovery documents and capability manifests from a `business.toml`
- apply per-trust-level rate limiting (Anonymous: 30/min, Known: 120/min, Partner: 600/min)
- manage consent records with durable file-backed storage (GDPR/KPA compliance)
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
use adk_awp::{AwpState, BusinessContextLoader, awp_routes};

let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
let state = AwpState::builder(loader.context_ref()).build();

let app = axum::Router::new()
    .merge(awp_routes(state))
    .merge(your_custom_routes);

let listener = tokio::net::TcpListener::bind("0.0.0.0:3456").await?;
axum::serve(listener, app).await?;
```

This registers all 7 AWP endpoints with version negotiation middleware.

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/awp.json` | Discovery document |
| GET | `/awp/manifest` | JSON-LD capability manifest |
| GET | `/awp/health` | Health state |
| POST | `/awp/events/subscribe` | Create webhook subscription |
| GET | `/awp/events/subscriptions` | List subscriptions |
| DELETE | `/awp/events/subscriptions/{id}` | Delete subscription |
| POST | `/awp/a2a` | A2A message handler |

## Components

### AwpStateBuilder

Build `AwpState` with sensible defaults — all services default to in-memory,
health state machine auto-wired to event service:

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
use std::time::Duration;

let mut limits = HashMap::new();
limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 10, window_secs: 60 });
let limiter = InMemoryRateLimiter::with_config(limits, Duration::from_secs(60));
```

### Consent Service

Two implementations:

- **`InMemoryConsentService`** — ephemeral, for development
- **`FileConsentService`** — JSON file-backed, for production (GDPR/KPA)

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

## Feature Flags

```toml
[features]
default = []
webhook-delivery = ["dep:reqwest"]  # Enable real HTTP webhook delivery
```

## Testing

```bash
cargo test -p adk-awp                    # 124 tests
cargo clippy -p adk-awp -- -D warnings  # zero warnings
```

## Example

```bash
cd examples/awp_agent
cp .env.example .env   # add your GOOGLE_API_KEY
cargo run
```

The example loads `business.toml`, creates an LLM agent with business context
instructions, serves all AWP endpoints, and exercises each one.

## Documentation

See [AWP Documentation](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/deployment/awp.md) for the full guide.

## Related Crates

- [`awp-types`](https://crates.io/crates/awp-types) — Pure protocol types (zero `adk-*` deps)
- [`adk-server`](https://crates.io/crates/adk-server) — HTTP server with A2A protocol
- [`adk-core`](https://crates.io/crates/adk-core) — ADK foundational traits

## License

Apache-2.0
