# Migrating from AWP 1.x to 2.0

AWP 2.0 replaces the 1.x wire (spec 3.5 §4.1). `adk-awp` keeps serving the 1.x routes behind
its default `awp-v1` feature for one release line, so a 1.x deployment runs unchanged while
you move it. This page maps each 1.x element to its 2.0 counterpart, converts a
`business.toml`, and keeps the 1.x reference for the deployments that still serve it.

> **Note:** for the Rust API changes to the 1.x types in 3.0.0 — access levels and schemas in
> the manifest, `AwpConfigError::Serialize`, event delivery over HTTP — see
> [Migrating 2.x → 3.0](2.x-to-3.0.md#adk-awp).

## What changes

| Area | AWP 1.x (`awp-v1`) | AWP 2.0 |
|------|--------------------|---------|
| Discovery | Unsigned `/.well-known/awp.json` with `version`, `siteName` and URLs built from `business.toml` | A root-signed `SiteContract` with a profile, keys, links to every surface, the latest release, the status and the charter digest |
| Trust | `Anonymous`, `Known`, `Partner`, `Internal`, assigned by the application's `TrustLevelAssigner`; the default makes every requester `Anonymous` | `public`, `known`, `verified`, `partner`, from verified credentials only: Web Bot Auth, OAuth, mTLS, owner-issued API keys |
| Requester detection | `detect_requester_type` from `X-AWP-Channel` and `User-Agent` | A label for analytics; it never changes a tier or a limit |
| Capabilities | Endpoints with `accessLevel` metadata the application enforces | Typed `CapabilityV2`s the edge enforces: effect, tier, schemas, consent, price, approval, idempotency; one declaration served over A2A, MCP and HTTP |
| Agent messages | `POST /awp/a2a` to an application handler; `AwpTypedMessage` categories | A2A v1.0 on the edge; typed signals into the back office |
| Events | Subscriptions on management routes the application authenticates | Subscriptions authenticated by Web Bot Auth or OAuth, scoped to their requester; replay within 72 hours; suspension after six failures |
| Health | `Healthy`, `Degrading`, `Degraded` | A signed `SiteStatus`: `operational`, `degraded`, `read_only`, `maintenance`, `incident` |
| Consent | `ConsentService` keyed by visitor and purpose | Signed `ConsentReceipt`s with a scope and a policy version; revocation, export and erasure |
| Payments | `PaymentIntent` and `PaymentPolicy`, which nothing in the workspace executes | Rails through `adk-payments` and signed receipts |
| Owner configuration | `business.toml`, hot-reloaded | Site data (`site.toml`, `capabilities.json`, `public-terms.json`) and a charter the owners sign |

## 1. Keep 1.x running

Nothing changes until you change it: `awp_routes(AwpState)` serves the 1.x routes at their 1.x
paths. With version lists, a client that asks for `AWP-Version: 2.0, 1.0` on a 1.x route is
answered in 1.0, and a request naming no common major gets `406`.

## 2. Convert `business.toml`

```bash
adk-rust awp migrate-config business.toml --out my-site
```

The command writes `site.toml`, `capabilities.json`, `public-terms.json`, `charter.draft.json`
and an unsigned `contract.draft.json`, lists every inference for the owner to confirm, and
never overwrites a file ([Sites and AWP from the command line](../deployment/site-cli.md#adk-rust-awp)).
It is `adk_awp::v2::migrate::migrate_config` underneath:

| 1.x | 2.0 |
|-----|-----|
| `site_name`, `domain`, `business.languages` | The site's `SiteRef` |
| `[[capabilities]]` | `CapabilityV2`: `GET`/`HEAD` → `read_only`, `PUT`/`DELETE` → `idempotent`, other methods → `non_idempotent` (listed for confirmation); the tier from `access_level`; names normalised to `^[a-z][a-z0-9_]{0,63}$`; a schema kept only when it is JSON Schema |
| `access_level = "internal"` | Not published; listed in the warnings |
| `[payments] auto_approve_threshold` | `above_amount` approval on `non_idempotent` capabilities; `always` without it |
| `site_description`, `brand_voice`, `business.currency`, `contact`, `outreach`, `payments` | The draft charter's mission, voice, budget currency, contact links and consent scopes |
| The domain, `[[products]]`, `[payments]` | The site kind: a public-sector domain is `government`, products or payments make `ecommerce`, anything else `business` |

The result is a draft: the charter is version 0 with no owner, the contract is unsigned, and
policies need a URL and a version before the public terms can name them.

```toml
[dependencies]
adk-awp = "3.0.0"
awp-types = "3.0.0"
uuid = "1"
```

```rust
use adk_awp::v2::migrate::migrate_config;
use awp_types::{BusinessCapability, BusinessContext, CapabilityEffect, SiteKind, TrustLevel};
use uuid::Uuid;

fn convert() {
    let mut ctx = BusinessContext::core("Example Solar", "Solar for Nairobi homes", "example.com");
    ctx.capabilities = vec![BusinessCapability {
        name: "place-order".into(),
        description: "Place an order".into(),
        endpoint: "/api/orders".into(),
        method: "POST".into(),
        access_level: TrustLevel::Known,
        input_schema: None,
        output_schema: None,
    }];
    let migration = migrate_config(&ctx, Uuid::nil());
    assert_eq!(migration.capabilities[0].name, "place_order");
    assert_eq!(migration.capabilities[0].effect, CapabilityEffect::NonIdempotent);
    assert_eq!(migration.charter.kind, SiteKind::Business);
    for warning in &migration.warnings {
        println!("confirm: {warning}");
    }
}
```

## 3. Serve 2.0

Build the site and its edge from the converted files: the content and releases with
[`adk-rust site`](../deployment/site-cli.md), the capabilities on [the agent
edge](../deployment/awp-edge.md) with their handlers ([Writing capability
packs](../deployment/capability-packs.md)), and the keys with `adk-rust awp keys rotate` and
`delegate`. Onboarding confirms the inferences and the owners sign the charter
([The charter](../deployment/charter.md)). `adk-rust awp verify <domain>` checks the result.

## 4. Turn off 1.x

Build `adk-awp` without default features once no client needs the 1.x routes:

```toml
[dependencies]
adk-awp = { version = "3.0.0", default-features = false, features = ["signing"] }
```

## AWP 1.x reference

What `awp-v1` serves, for the deployments that keep it.

### Serving the 1.x routes

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
        let authorized = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == expected);
        if !authorized {
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

    let app = axum::Router::new().merge(awp_routes(state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3456").await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
```

Without an `AwpA2aHandler`, `POST /awp/a2a` returns `503` and never acknowledges work that
was not dispatched. `ConnectInfo` supplies the peer address that isolates anonymous rate-limit
buckets; without it, unknown callers share one bucket.

`BusinessContextLoader` reloads `business.toml` through `ArcSwap`:

```rust
use adk_awp::BusinessContextLoader;

async fn hot_reload() -> Result<(), Box<dyn std::error::Error>> {
    let loader = BusinessContextLoader::from_file("business.toml".as_ref())?;
    loader.watch("business.toml".into()).await?;
    // Changes to business.toml are picked up every 5 seconds.
    Ok(())
}
```

Every 1.x route negotiates its version: a client may send `AWP-Version: 1.1`, or a list such as
`2.0, 1.0`. A request whose list holds major 1 proceeds, and its response carries
`AWP-Version: 1.0`; one naming no major 1 gets `406 version_mismatch`, and a malformed value
`400`.

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/awp.json` | Discovery document generated from `business.toml` |
| GET | `/awp/manifest` | JSON-LD capability manifest; each entry carries `accessLevel` and its optional schemas |
| GET | `/awp/health` | Health state |
| POST | `/awp/a2a` | Application-provided A2A dispatch, 64 KiB body limit |

`awp_management_routes()` returns subscription management without an authentication layer;
apply the application's auth middleware before merging it:

| Method | Path | Description |
|--------|------|-------------|
| POST | `/awp/events/subscribe` | Create a webhook subscription |
| GET | `/awp/events/subscriptions` | List subscriptions (the secret is never returned) |
| DELETE | `/awp/events/subscriptions/{id}` | Delete a subscription |

The discovery document at `/.well-known/awp.json` is generated from `business.toml`:

```json
{
  "version": { "major": 1, "minor": 0 },
  "siteName": "My Shop",
  "siteDescription": "An online store powered by AWP",
  "capabilityManifestUrl": "https://myshop.example.com/awp/manifest",
  "a2aEndpointUrl": "https://myshop.example.com/awp/a2a",
  "eventsEndpointUrl": "https://myshop.example.com/awp/events/subscribe",
  "healthEndpointUrl": "https://myshop.example.com/awp/health",
  "supportedTrustLevels": ["anonymous"]
}
```

The manifest at `/awp/manifest` is JSON-LD:

```json
{
  "@context": "https://schema.org",
  "@type": "WebAPI",
  "name": "My Shop",
  "description": "An online store powered by AWP",
  "capabilities": [
    {
      "name": "place_order",
      "description": "Place an order",
      "endpoint": "/api/orders",
      "method": "POST",
      "accessLevel": "known",
      "inputSchema": "OrderInput",
      "outputSchema": "OrderReceipt"
    }
  ]
}
```

### `business.toml`

| Section | Fields | Required |
|---------|--------|----------|
| (root) | `site_name`, `site_description`, `domain`, `contact` | Yes (except `contact`) |
| `[business]` | `name`, `country`, `languages`, `currency`, `timezone` | No |
| `[brand_voice]` | `tone`, `greeting`, `escalation_message` | No |
| `[[products]]` | `sku`, `name`, `price`, `inventory`, `tags`, `description` | No |
| `[[capabilities]]` | `name`, `description`, `endpoint`, `method`, `access_level`; optional `input_schema`, `output_schema` | Yes |
| `[[policies]]` | `name`, `description`, `policy_type` | Yes |
| `[channels]` | `whatsapp`, `email`, `website`, `sms` | No |
| `[payments]` | `providers`, `auto_approve_threshold`, `require_approval_threshold` | No |
| `[support]` | `escalation_contacts`, `hours`, `sla` | No |
| `[content]` | `topics`, `auto_draft`, `publish_delay` | No |
| `[reviews]` | `platforms`, `auto_respond_threshold` | No |
| `[outreach]` | `follow_up_delay`, `require_consent` | No |

### Trust levels and rate limits

| Level | How assigned | Default limit |
|-------|--------------|---------------|
| `Anonymous` | No credentials; every request under `DefaultTrustAssigner` | 30 requests/minute |
| `Known` | A custom `TrustLevelAssigner` that verified the caller | 120 requests/minute |
| `Partner` | A custom assigner | 600 requests/minute |
| `Internal` | A custom assigner | Unlimited |

The access level is manifest metadata; the application handler enforces it. Configure
`.supported_trust_levels(...)` so discovery advertises only levels the deployment can verify.
`InMemoryRateLimiter` keeps a sliding window per trust level; a rejected request receives
`429` with `Retry-After`.

```rust
use std::collections::HashMap;

use adk_awp::{InMemoryRateLimiter, RateLimitConfig};
use awp_types::TrustLevel;

fn limiter() -> InMemoryRateLimiter {
    let mut limits = HashMap::new();
    limits.insert(TrustLevel::Anonymous, RateLimitConfig { max_requests: 10, window_secs: 60 });
    limits.insert(TrustLevel::Known, RateLimitConfig { max_requests: 100, window_secs: 60 });
    InMemoryRateLimiter::with_config(limits)
}
```

```rust
use std::sync::Arc;

use adk_awp::TrustLevelAssigner;
use async_trait::async_trait;
use awp_types::TrustLevel;
use axum::http::{HeaderMap, header};

struct MyTrustAssigner {
    bearer_token: Arc<str>,
}

#[async_trait]
impl TrustLevelAssigner for MyTrustAssigner {
    async fn assign(&self, headers: &HeaderMap) -> TrustLevel {
        let expected = format!("Bearer {}", self.bearer_token);
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == expected)
        {
            TrustLevel::Known
        } else {
            TrustLevel::Anonymous
        }
    }
}
```

### Events, health and consent

Subscription management is a privileged surface: mount `awp_management_routes()` behind
authentication. Callback URLs must be absolute HTTPS URLs that do not name a loopback, private
or link-local address, and signing secrets must contain 32 to 4096 bytes:

```bash
# Subscribe — answers 201 with {"id": "..."}
curl -X POST http://localhost:3456/awp/events/subscribe \
  -H "Authorization: Bearer $AWP_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "subscriber": "my-agent",
    "callbackUrl": "https://my-agent.example/webhook",
    "eventTypes": ["health.changed"],
    "secret": "replace-with-at-least-32-random-bytes"
  }'
```

`InMemoryEventSubscriptionService`, the default, keeps subscriptions in memory and delivers each
matching event through `adk_core::webhook_delivery::WebhookDelivery`:

- Each delivery carries Standard Webhooks headers (`webhook-id`, `webhook-timestamp`,
  `webhook-signature`), signed with the subscription secret's bytes as the key; the event ID is
  the `webhook-id`.
- Failed deliveries are retried with exponential backoff and jitter, and dead-lettered after
  the last attempt. Redirects are not followed.
- The egress policy refuses plain-HTTP, loopback, private and link-local destinations at
  subscription time and again when the host name resolves.
- `deliver` starts the deliveries and returns at once; `deliver_and_wait` returns a
  `DeliveryReport` per subscription. `with_delivery` sets the retry policy, dead-letter sink
  or egress policy.

Implement `EventSubscriptionService` for durable storage. Receivers verify a delivery with
`adk_core::webhook::WebhookVerifier` (feature `webhooks`):

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

`adk_awp::sign_payload` and `verify_signature`, the earlier `sha256=<hex_digest>` HMAC pair,
are deprecated; the built-in service no longer sends that format.

`HealthStateMachine` tracks `/awp/health` with validated transitions — Healthy → Degrading,
Degrading → Degraded, and Degrading or Degraded → Healthy — and emits `health.changed` to
matching subscribers; any other transition returns `AwpError::InvalidRequest`.

```rust
use std::sync::Arc;

use adk_awp::{HealthStateMachine, InMemoryEventSubscriptionService};

async fn health() -> Result<(), awp_types::AwpError> {
    let health = HealthStateMachine::new(Arc::new(InMemoryEventSubscriptionService::new()));
    health.report_degrading("database latency high").await?;
    health.report_degraded("database unreachable").await?;
    health.report_healthy().await?;
    Ok(())
}
```

`ConsentService` has in-memory and file-backed (`FileConsentService`) implementations.
`FileConsentService` writes each change to a synced temporary file and renames it over the
target on a blocking thread, so a reader or a crash never sees a partial file. Choosing a
storage implementation does not establish regulatory compliance.

```rust
use adk_awp::{ConsentService, InMemoryConsentService};

async fn consent() -> Result<(), awp_types::AwpError> {
    let consent = InMemoryConsentService::new();
    consent.capture_consent("visitor-123", "analytics").await?;
    let granted = consent.check_consent("visitor-123", "analytics").await?;
    assert!(granted);
    consent.revoke_consent("visitor-123", "analytics").await?;
    Ok(())
}
```

### Requester detection, messages and payment intents

`detect_requester_type` returns `Agent` for `X-AWP-Channel: agent`, or for an `Accept` of
`application/json`, `application/ld+json` or `application/awp+json` together with an agent
User-Agent pattern (`bot`, `crawler`, `spider`, `agent`, `gpt`, `claude`, `gemini`,
`perplexity`, `anthropic`, `openai`); otherwise `Human`. These are claims, which AWP 2.0
uses only as an analytics label.

`AwpTypedMessage` carries one of `VisitorIntentSignal`, `ContentGapSignal`, `PaymentIntent`,
`SupportEscalation`, `ReviewSignal`, `OperationsProposal`, `InvokeCapability`, `RenderUi` or
`OutboundTrigger`.

`PaymentIntent` moves Draft → PendingApproval → Approved → Executing → Settled, Rejected or
Cancelled. `PaymentPolicy::evaluate` auto-approves an amount at or below
`auto_approve_threshold` (default 5000 cents) from a caller at or above
`min_trust_for_auto_approve` (default `Known`) and requires approval for everything else;
`require_approval_threshold` (default 50,000) does not change the decision. No crate in this
workspace executes a `PaymentIntent`; `adk-gateway`, a separate repository, uses it.

### The 1.x example

`examples/awp_agent` loads a `business.toml`, builds an agent from it, installs authenticated
A2A dispatch, mounts the management routes behind a separate credential and exercises every
endpoint:

```bash
cd examples/awp_agent
cp .env.example .env   # add your GOOGLE_API_KEY
cargo run
```

## Related

- [Agentic Web Protocol (AWP) 2.0](../deployment/awp.md)
- [Sites and AWP from the command line](../deployment/site-cli.md)
- [Migrating 2.x → 3.0](2.x-to-3.0.md)
