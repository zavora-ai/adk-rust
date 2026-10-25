# The AWP Agent Edge

`adk-server` (feature `awp`) serves one site's AWP 2.0 capabilities to people and agents. The
edge assigns every caller a trust tier from verified credentials, exposes each capability over
A2A, MCP and HTTP from one declaration, runs every call through one governed path, holds calls
the owner must decide, answers repeated calls from their record, and hosts the site's public
agent. It has no owner route: the owner's control plane is the kernel's.

> **Note:** The edge follows AWP 2.0, which stays Beta until its normative text is published
> (spec 3.5 §13 Q1).

| Part | Type |
|------|------|
| The edge and its routes | `AwpEdge`, `AwpEdgeBuilder`, `ServerBuilder::awp` |
| Trust tiers | `EdgeTiers`, `WebBotAuthTrustAssigner`, `OAuthClientCredentials`, `MtlsCredentials`, `ApiKeyCredentials`, `EdgeCaller` |
| Rate limits | `EdgeLimits` |
| The governed path | `GovernedExecutor`, `CapabilityPolicy` |
| The public partition | `PublicTools`, `PublicPartitionPolicy`, `SiteSearch`, `ThreadSink`, `SignalEmitter` |
| Modes and status | `EdgeMode`, `EdgeStatusControl` |

## Setup

```toml
[dependencies]
adk-server = { version = "3.0.0", features = ["awp"] }
adk-awp = { version = "3.0.0", features = ["signing"] }
adk-auth = { version = "3.0.0", features = ["web-bot-auth"] }
adk-core = "3.0.0"
adk-session = "3.0.0"
awp-types = "3.0.0"
async-trait = "0.1"
axum = "0.8"
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net"] }
```

## Serve a site's capabilities

```rust
use std::sync::Arc;

use adk_auth::web_bot_auth::{HttpKeyDirectorySource, HttpSignatureVerifier};
use adk_awp::signing::jws::Ed25519DocumentSigner;
use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_core::{
    Agent, ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, SingleAgentLoader,
    StoreApprovalService, Url,
};
use adk_server::awp::{AwpEdge, EdgeKeys, EdgeTiers, WebBotAuthTrustAssigner};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use awp_types::{ApprovalHint, CapabilityEffect, CapabilityV2, TrustTier};
use serde_json::{Value, json};

struct Book;

#[async_trait::async_trait]
impl CapabilityHandler for Book {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        Ok(json!({ "booking": "bk-1", "date": call.input["date"] }))
    }
}

/// Serves `agent` on the server's API and the site's edge beside it.
async fn serve(agent: Arc<dyn Agent>) -> Result<(), Box<dyn std::error::Error>> {
    // One declaration per capability: the A2A skill, the MCP tool and the HTTP endpoint.
    let mut registry = CapabilityRegistry::new("example.com");
    registry.declare(
        CapabilityV2::new("book_visit", "Book a site visit", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Known)
            .with_approval(ApprovalHint::AboveAmount(50_000)),
        Arc::new(Book),
    )?;

    // Delegated keys: `card` signs the Agent Card, `receipt` signs the status.
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::generate("edge-card-2026-10")?),
        receipt: Arc::new(Ed25519DocumentSigner::generate("edge-receipt-2026-10")?),
    };
    let tiers = EdgeTiers::builder()
        .verifier(WebBotAuthTrustAssigner::new(
            HttpSignatureVerifier::builder(Arc::new(HttpKeyDirectorySource::new()?)).build(),
        ))
        .build();
    // Held calls wait in the owner's inbox; the owner decides them on any surface.
    let approvals = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(ApprovalPolicy::new([ApproverSelector::human("owner")])),
    );
    let edge = AwpEdge::builder(registry, keys, Url::parse("https://agent.example.com")?)
        .tiers(tiers)
        .approval_service(approvals)
        .build()?;
    let _resolver = edge.spawn_resolver(std::time::Duration::from_secs(5));

    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent)),
        Arc::new(InMemorySessionService::new()),
    )
    .with_allowed_hosts(["agent.example.com"]);
    let app = ServerBuilder::new(config).awp(edge).build();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

`ServerBuilder::awp` mounts the edge through `add_root_routes`, so the server's host guard,
CORS, timeouts and security headers apply, and attaches the server's kill switch. With an edge
mounted, `with_a2a` keeps its JSON-RPC routes but the edge serves
`/.well-known/agent-card.json`.

## Routes

| Route | Answers |
|-------|---------|
| `GET /.well-known/agent-card.json` | The A2A v1.0 Agent Card, one skill per capability the caller's tier sees, signed by the delegated `card` key |
| `POST /awp/v2/a2a` | A2A JSON-RPC: `SendMessage`, `GetTask`, `CancelTask` |
| `POST /awp/v2/a2a/v1/message:send`, `GET /awp/v2/a2a/v1/tasks/{id}` | The same over A2A HTTP+JSON |
| `POST /mcp`, `GET /mcp/server-card`, `GET /.well-known/mcp/server-card.json` | The capabilities as MCP tools over Streamable HTTP |
| `GET /awp/v2/manifest` | The capability manifest for the caller's tier |
| `GET /awp/v2/trust` | The published trust rules |
| `GET /awp/v2/status` | The signed site status |
| `POST /awp/v2/capabilities/{name}` | A call: `200` output, `202` held call, or an error envelope; the body is JSON or form-encoded |
| `GET /awp/v2/calls/{id}` | A held call; from `known` up only to the requester that made it |
| `GET /awp/v2/chat` | The on-site assistant over a WebSocket, when a public agent is hosted |
| `POST /awp/v2/events`; `GET`, `PATCH`, `DELETE /awp/v2/events/{id}`; `POST /awp/v2/events/replay` | Event subscriptions |
| `GET /awp/v2/events/changes/{seq}/{page}` | Further item ids of a release's `catalog.changed`, from page 2 |
| `POST /awp/v2/consent`, `POST /awp/v2/consent/revoke`, `GET /awp/v2/consent/export`, `POST /awp/v2/consent/erase` | Consent receipts, authenticated to the subject |
| `POST /awp/v2/beacon` | Cookieless measurement |

## Trust tiers

| Credential | Verifier | Requester id | Tier |
|------------|----------|--------------|------|
| Web Bot Auth signature | `WebBotAuthTrustAssigner` | `wba:{directory origin}#{thumbprint}` | `known`; `verified` when the directory is on the owner's verified list |
| OAuth client-credentials token | `OAuthClientCredentials` | `oauth:{issuer}#{client id}` | `known`, or the tier `scope_tier` maps a scope to |
| Client certificate the listener verified | `MtlsCredentials` (reads `ClientCertificate` from the request extensions) | `mtls:{base64url SHA-256 of the DER}` | `known`; `verified` for an issuer on the verified list |
| Owner-issued API key, `Authorization: ApiKey {id}:{secret}` | `ApiKeyCredentials` | `key:{id}` | `known` |
| Any of these on an owner rule | `EdgeTiersBuilder::rule`, `private_rule` | — | The rule's tier, such as `partner` |

`User-Agent`, `X-AWP-Channel` and every other self-declared header are claims: they never
change a tier, a limit or what a caller sees. A presented credential that fails a check is
refused with `401 unauthorized`; one that cannot be checked, such as a signature whose key
directory is unreachable, is served as `public`. A capability above the caller's tier answers
`404 capability_not_found`, exactly as one that does not exist.

Limits key on the verified requester id, else a digest of the presented credential, else the
peer address. A request over its per-minute limit, or a call over its daily limit, answers
`429 rate_limit_exceeded` with `Retry-After`; every response carries `RateLimit-Policy` and
`RateLimit`.

## One governed path

HTTP, A2A and MCP calls, and the public agent's capability calls, enter `GovernedExecutor`:

1. Visibility by tier and deprecation.
2. Read-only mode and the kill switch.
3. The caller's daily call limit.
4. The input schema; the idempotency key a `non_idempotent` capability requires; a live consent
   receipt for a consent-bound capability.
5. The idempotency record: a repeat with the same key and input returns the first outcome —
   output, error or held call — without executing; the same key with another input answers
   `409 idempotency_conflict`.
6. `adk_core::authorize_tool_call` under `CapabilityPolicy`, compiled from the manifest, with
   the action ledger and the audit trail. The acting agent is the site's principal; the
   verified requester is the call's counterparty on every approval request and `awp.requester`
   on every `awp.capability.*` audit record.

| Binding | Idempotency key | Held call | Error |
|---------|-----------------|-----------|-------|
| HTTP | `Idempotency-Key` | `202` with `Location: /awp/v2/calls/{id}` | The error envelope |
| A2A | The `messageId`, or the metadata key `https://agenticwebprotocol.com/ns/2.0/idempotencyKey` | The task stays `TASK_STATE_WORKING` with the held-call document | The task ends `TASK_STATE_REJECTED` or `TASK_STATE_FAILED` with the envelope |
| MCP | `params._meta["agenticwebprotocol.com/idempotency-key"]`, else the JSON-RPC id within its session | `structuredContent` is the held-call document | `isError: true`, the envelope in the text block |

A held call waits in the owner's approval store for as long as the owner takes, or until its
`hold_expiry`. Once the owner decides through the `ApprovalService`, `GET /awp/v2/calls/{id}`,
`GetTask` or the resolver started by `AwpEdge::spawn_resolver` runs it through the same path:
an approved call completes with its output, a rejected one ends `rejected`, an expired one
`expired`. A call decided while the kill switch is engaged stays pending until it lifts.

## The public partition

`AwpEdgeBuilder::public_agent` hosts the site's public agent on A2A messages that name no
capability and on `GET /awp/v2/chat`. The factory receives `PublicTools` — each declared
capability, called with the conversation's own tier, plus `search_site`, `reply_in_thread` and
`emit_signal` — and the agent runs under `PublicPartitionPolicy`, which allows only those tools,
by name and origin. A tool the agent was built with beyond them is denied and the refusal is
audited.

| Tool | Behaviour |
|------|-----------|
| `search_site` | Searches the release through a `SiteSearch`; `StaticSiteIndex` is an in-memory index |
| `reply_in_thread` | Sends text to the conversation's own thread, inside `ReplyPolicy`'s template and rate; the thread comes from the conversation, never from the model, and replies carry no attachments |
| `emit_signal` | Files one typed `awp_types::Signal` through a `SignalEmitter`; a signal naming a requester names the verified caller |

## Modes and the incident handle

| Mode | Capabilities | Status |
|------|--------------|--------|
| `EdgeMode::Serving` | Every visible capability | `operational` |
| `EdgeMode::ReadOnly { reason }` | `read_only` capabilities only; the rest answer `503 read_only` with the signed status | `read_only` with `reasonCode` |
| `EdgeMode::Off` | None: every edge route answers `404` and only the static content remains | — |

The owner's kill switch (`GovernanceControl`) puts the edge in read-only mode with
`reasonCode: "kill_switch"` while it is frozen. `AwpEdge::status_control()` returns an
`EdgeStatusControl` a back-office tool holds to switch the edge to read-only and back:

```rust
use adk_server::awp::{AwpEdge, StatusError};

async fn contain(edge: &AwpEdge) -> Result<(), StatusError> {
    let control = edge.status_control();
    control.read_only("drift_detected").await?;
    // ... roll back to the last signed release ...
    control.resume().await?;
    Ok(())
}
```

## Events

Subscriptions need tier `known` or above through Web Bot Auth or OAuth, and belong to the
requester that created them. `POST /awp/v2/events` with `{"types": [...], "url": "https://…"}`
answers `201` with the subscription and its Standard Webhooks secret, shown once. A URL that is
not `https`, or that names or resolves to a loopback, private or link-local address, is refused.

| Event | Published by |
|-------|--------------|
| `site.status.changed` | The edge, on every status transition |
| `consent.revoked` | The edge, to the requesters whose calls referenced the receipt |
| `site.release.published`, `catalog.changed` | The site's release pipeline, through `AwpEdge::events()` |
| `order.updated` | The site's commerce capabilities, to the requester that placed the order |
| `subscription.suspended` | The edge, once, to a subscription it suspends |

Each delivery carries `webhook-id` (the event id), `webhook-timestamp`, `webhook-signature` and
`AWP-Version: 2.0`. A failed delivery is retried after 1 s, 5 s, 30 s, 5 min, 30 min and 2 h with
jitter; six consecutive failures suspend the subscription, and `PATCH` with
`{"state": "active"}` reactivates it. Events are retained 72 hours; `POST /awp/v2/events/replay`
redelivers them under their original ids. `EdgeEvents::announce_catalog` publishes the
`catalog.changed` of an `adk-site` announcement and serves its further pages; give the
pipeline's `changes_url` the edge's `/awp/v2/events/changes` base.

## Consent

`ConsentConfig` names the scopes the site asks for, the policy versions with their digests, and
the key subject sessions are authenticated with. `POST /awp/v2/consent` answers `201` with a
receipt signed by the edge's `receipt` key and the subject's session token in `AWP-Subject`;
the subject is a pseudonymous `subj_…` reference. The other consent routes authenticate the
subject by that token, in `AWP-Subject` or a signed link's `subject` parameter, never by the
requester's tier.

A revocation takes effect at once: pending held calls that rely on the receipt end `rejected`
with `consent_required`, later calls cannot use it, and `consent.revoked` reaches the
requesters that referenced it. `GET /awp/v2/consent/export` returns the receipts and what the
`SubjectData` holds; `POST /awp/v2/consent/erase` revokes, erases the subject's data through
`SubjectData` and erases the receipts. The default `LeadsData` exports the leads that reference
the subject's receipts and erases none, since a `LeadStore` cannot delete.

## The beacon

`POST /awp/v2/beacon` with `{"kind": "pageview", "path": "/pricing"}` or `{"kind": "event",
"name": "signup"}` counts per day and answers `204`. It sets no cookie, stores no address, cuts
query strings and fragments from paths, counts nothing for a request with `Sec-GPC: 1`, and
counts a visitor only under a live `persistent_profile` receipt the beacon names.
`AwpEdge::beacon()` reads the counts — `pageviews`, `pageviews:{path}`, `events:{name}`,
`visitors` — in the shape of the kernel's `KpiSource::read`. The content origin posts to the
edge origin, so list it in the server's allowed origins.

## Durable stores

| Backend | Feature | Type |
|---------|---------|------|
| SQLite | `awp-sqlite` | `adk_server::awp::stores::SqliteEdgeStores` |
| PostgreSQL | `awp-postgres` | `adk_server::awp::stores::PostgresEdgeStores` |
| Redis | `awp-redis` | `adk_server::awp::stores::RedisEdgeStores` |

Each implements the six edge store traits, passes `adk_awp::edge_store_conformance_tests!`, and
keeps one site's records under its `with_site` namespace. Call `migrate` once, then pass
`edge_stores()` to `AwpEdgeBuilder::stores`.

## Related

- [Agentic Web Protocol](awp.md)
- [Web Bot Auth](../security/web-bot-auth.md)
- [Approval Service](../security/approval-service.md)
- [MCP Server](../mcp/server.md)
