# adk-awp

Agentic Web Protocol (AWP) implementation for [ADK-Rust](https://github.com/zavora-ai/adk-rust).

[![crates.io](https://img.shields.io/crates/v/adk-awp.svg)](https://crates.io/crates/adk-awp)
[![docs.rs](https://docs.rs/adk-awp/badge.svg)](https://docs.rs/adk-awp)
[![License](https://img.shields.io/crates/l/adk-awp.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)
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
- subscribe agents to events delivered as signed webhooks (Standard Webhooks), with internal callback URLs refused
- monitor service health with a validated state machine (Healthy → Degrading → Degraded)
- detect whether requests come from humans or AI agents
- negotiate AWP protocol versions automatically
- serve the AWP 2.0 edge: the `/awp/v2/` routes, typed capabilities as `adk_core::Tool`s, held and exactly-once calls, and edge stores

## Installation

```toml
[dependencies]
adk-awp = "3.0"
awp-types = "3.0"
async-trait = "0.1"
axum = "0.8"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net"] }
```

Or use the meta-crate, where the `awp` feature (included in `enterprise`) re-exports it:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["awp"] }
```

| Feature | Default | Enables |
|---------|---------|---------|
| `awp-v1` | yes | The AWP 1.x routes (`awp_routes`, `AwpState`), served beside 2.0 for one release line |
| `signing` | no | Detached JWS (EdDSA) over JCS, keys and delegations, the `_awp-root` DNS anchor, signed Agent Cards, the site log's tiles and checkpoint, `StaticBundle`; adds `adk-auth` |
| `verify` | no | `signing`, plus `Verifier` (the library behind `adk-rust awp verify`), the conformance kit and the test-vector generator; adds `reqwest` |

## AWP 2.0 edge

`adk_awp::v2` is the AWP 2.0 edge runtime (spec 3.5 §7.2–§7.4, §7.9). The wire types are
`awp_types::v2`.

| Route | Answers |
|-------|---------|
| `GET /awp/v2/manifest` | The capability manifest, filtered to the caller's tier |
| `GET /awp/v2/trust` | The published trust rules |
| `GET /awp/v2/status` | The signed site status |
| `POST /awp/v2/capabilities/{name}` | `200` with the output, `202` with a held call and `Location`, or an error envelope |
| `GET /awp/v2/calls/{id}` | A held call; from `known` up only to the requester that made it |

| Piece | Behaviour |
|-------|-----------|
| `TierAssigner` | Tiers come from verified credentials only; `PublicTierAssigner`, the default, makes every caller `public`. `claimed_agent` turns self-declared headers into an analytics label |
| `CapabilityTool` | One capability as an `adk_core::Tool`: `effect()` is the declared effect, `origin()` is `awp:{domain}`, input is validated against the declared schema, MCP hints and the held-call output schema come from the declaration |
| `CapabilityRegistry` | A site's capabilities and the manifest for a tier |
| `DirectExecutor` | The governed path every binding enters: visibility, sunset, schema, idempotency key, consent receipts, the idempotency record, then a held call or the capability. `decide` approves or rejects a held call; `expire_due` expires undecided ones |
| Edge stores | Subscriptions, consent receipts, status, held calls, leads and idempotency records, with in-memory implementations and `edge_store_conformance_tests!` |
| `AWP-Version` | A list such as `2.0, 1.0`; each route group answers in the highest common major, or `406` |
| `migrate_config` | `business.toml` to 2.0 site data, capabilities and a draft charter, with every inference listed |

```rust
use std::sync::Arc;

use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry};
use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
use awp_types::{ApprovalHint, CapabilityEffect, CapabilityV2, TrustTier};
use serde_json::{Value, json};

struct PlaceOrder;

#[async_trait::async_trait]
impl CapabilityHandler for PlaceOrder {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        // `call.idempotency_key` is stable across retries of this call.
        Ok(json!({ "orderId": call.idempotency_key, "sku": call.input["sku"] }))
    }
}

fn edge() -> axum::Router {
    let mut registry = CapabilityRegistry::new("example.com");
    let order = CapabilityV2::new("place_order", "Place an order", CapabilityEffect::NonIdempotent)
        .with_min_trust(TrustTier::Known)
        .with_approval(ApprovalHint::AboveAmount(100_000));
    registry.declare(order, Arc::new(PlaceOrder)).expect("a valid declaration");
    // Install a TierAssigner that verifies credentials; the default makes every caller public.
    awp_v2_routes(EdgeState::builder(registry).build())
}
```

## Signing and verification

| Module | Contents | AWP 2.0 |
|--------|----------|---------|
| `signing::jws` | `sign` and `verify`: a detached JWS over the JCS form of a document without `proof`; `DocumentSigner`, `Ed25519DocumentSigner` (seed zeroed on drop, never printed); `document_digest`; RFC 7638 `jwk_thumbprint` | §5.1, §5.4 |
| `signing::keys` | `delegate`, `revoke`, `endorse`; `verify_key_set`; `delegated_key` for a scope and a live or durable date | §5.2–§5.7 |
| `signing::anchor` | `root_record` (`v=awp2; kid=…; jkt=…`), `check_root_anchor`, `DnsTxtResolver`, `FixedTxtResolver` | §5.6 |
| `signing::card` | `sign_agent_card`, `verify_agent_card` by a `card` key | §7.4.2 |
| `signing::log` | Log entries, C2SP tiles and entry bundles, the signed checkpoint | §14.5 |
| `signing::bundle` | `StaticBundle`: the signed contract, `keys.json` and public terms, deterministic | §4, §13.2 |
| `verify` | `Verifier`: the fourteen steps of §15.2, each failure by name; pins per domain (`InMemoryPinStore`, `FilePinStore`); posture and readiness; `verify_preview` for the next release before it is logged, bound by its staged manifest digest; `HttpFetcher` (`GET` only, no redirects, optional `RequestSigner` for Web Bot Auth) and `RouterFetcher` | §15 |
| `conformance` | `run`: S1–S9 and E1–E13 by profile, passive, with `GET`-only edge checks behind `active_edge_checks`; `rules::check_document`; `fixture::FixtureSite`; `vectors` | §16 |

```toml
[dependencies]
adk-awp = { version = "3.0", features = ["verify"] }
```

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
    let a2a_token: Arc<str> = std::env::var("AWP_A2A_TOKEN")?.into();
    let state = AwpState::builder(loader.context_ref())
        .a2a_handler(Arc::new(ApplicationA2a { bearer_token: a2a_token }))
        .build();

    // Merge your own routes alongside the AWP routes
    let app = axum::Router::new().merge(awp_routes(state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3456").await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await?;
    Ok(())
}
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
| GET | `/awp/manifest` | JSON-LD capability manifest, with each capability's `accessLevel` and schemas |
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

## Event delivery

`InMemoryEventSubscriptionService` delivers every matching event over HTTP through
`adk_core::webhook_delivery::WebhookDelivery`: Standard Webhooks signatures keyed with the
subscription secret's bytes, the event id as the `webhook-id`, retries with exponential backoff
and jitter, dead-lettering after the last attempt, and an egress guard that refuses loopback,
private, and link-local destinations. `with_delivery` sets the retry policy, dead-letter sink,
or egress policy.

## Components

### AwpStateBuilder

Build `AwpState` with fail-closed defaults. Services default to in-memory and
the health state machine is wired to the event service. A2A dispatch returns
`503` until the application installs a handler:

```rust
use adk_awp::{AwpState, BusinessContextLoader, FileConsentService};
use std::sync::Arc;

fn state(loader: &BusinessContextLoader) -> Result<AwpState, Box<dyn std::error::Error>> {
    Ok(AwpState::builder(loader.context_ref())
        .consent_service(Arc::new(FileConsentService::new("data/consent.json")?))
        .build())
}
```

### BusinessContextLoader

Parse and validate `business.toml` with hot-reload support:

```rust
use adk_awp::BusinessContextLoader;

async fn load() -> Result<(), Box<dyn std::error::Error>> {
    let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
    loader.watch("business.toml".into()).await?; // hot-reload every 5s
    let ctx = loader.load();
    println!("Site: {}", ctx.site_name);
    Ok(())
}
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

fn limiter() -> InMemoryRateLimiter {
    let mut limits = HashMap::new();
    limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 10, window_secs: 60 });
    InMemoryRateLimiter::with_config(limits)
}
```

`DefaultTrustAssigner` classifies every request as anonymous. An authorization
header is not proof that a credential is valid. Install a verifier-backed
`TrustLevelAssigner` before assigning higher trust levels.

### Consent Service

Two implementations:

- **`InMemoryConsentService`** — ephemeral, for development
- **`FileConsentService`** — durable local JSON; the application controls file
  permissions, encryption, retention, and regulatory policy. Each write goes to a synced
  temporary file that is renamed over the target on a blocking thread, so readers and
  crashes never see a partial file

```rust
use adk_awp::{ConsentService, FileConsentService};

async fn consent() -> Result<(), Box<dyn std::error::Error>> {
    let consent = FileConsentService::new("data/consent.json")?;
    consent.capture_consent("visitor-123", "analytics").await?;
    assert!(consent.check_consent("visitor-123", "analytics").await?);
    consent.revoke_consent("visitor-123", "analytics").await?;
    Ok(())
}
```

### Health State Machine

Validated transitions with event emission:

```
Healthy → Degrading → Degraded → Healthy
```

Invalid transitions (e.g., Healthy → Degraded) return an error.

### Event Subscriptions

`EventSubscription::validate`, and with it `create` and `POST /awp/events/subscribe`, refuses a
callback URL that names a loopback, private, or link-local address; `validate_with` checks
against a given `EgressPolicy`. `deliver` returns at once and `deliver_and_wait` returns each
delivery report. Deliveries carry [Standard Webhooks](https://www.standardwebhooks.com/)
signatures; receivers verify them with `adk_core::webhook::WebhookVerifier`:

```rust
use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookVerifier};

fn verify(
    secret: &str,
    id: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
) -> Result<(), adk_core::webhook::WebhookError> {
    let verifier = WebhookVerifier::new(WebhookSecret::from_bytes(secret.as_bytes().to_vec())?);
    let headers = WebhookHeaders { id, timestamp: timestamp.into(), signature };
    verifier.verify(&headers, body)?;
    Ok(())
}
```

`sign_payload` and `verify_signature`, the earlier `sha256=<hex_digest>` HMAC pair, are
deprecated; the built-in service does not send that format.

### Requester Detection

Detect human vs agent from HTTP headers:

```rust
use adk_awp::detect_requester_type;
use awp_types::RequesterType;
use axum::http::HeaderMap;

fn main() {
    let mut headers = HeaderMap::new();
    headers.insert("X-AWP-Channel", "agent".parse().unwrap());
    assert_eq!(detect_requester_type(&headers), RequesterType::Agent);
}
```

## Testing

```bash
cargo nextest run -p adk-awp             # protocol and boundary tests
cargo clippy -p adk-awp -- -D warnings  # zero warnings
```

## Example

```bash
cp examples/awp_agent/.env.example examples/awp_agent/.env   # add your GOOGLE_API_KEY
cargo run --manifest-path examples/awp_agent/Cargo.toml
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

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
