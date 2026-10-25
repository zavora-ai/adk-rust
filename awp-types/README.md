# awp-types

Shared protocol types for the [Agentic Web Protocol (AWP)](https://agenticwebprotocol.com).

[![crates.io](https://img.shields.io/crates/v/awp-types.svg)](https://crates.io/crates/awp-types)
[![docs.rs](https://docs.rs/awp-types/badge.svg)](https://docs.rs/awp-types)
[![License](https://img.shields.io/crates/l/awp-types.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)
[![AWP](https://img.shields.io/badge/AWP-agenticwebprotocol.com-0F8A8A)](https://agenticwebprotocol.com)

`awp-types` provides pure protocol types with **zero `adk-*` dependencies**, making it importable by any Rust project that needs to work with AWP messages — gateways, agents, CLI tools, or third-party integrations.

## Overview

Use `awp-types` when you need to:

- parse or produce AWP 2.0 site contracts, release manifests, charters, capabilities, events and receipts
- parse or produce AWP discovery documents, capability manifests, or A2A messages
- work with AWP trust levels, requester types, or error codes
- define payment intents with owner-policy-driven lifecycle
- route typed AWP messages between agents
- share AWP types across crates without pulling in the full ADK tree

## Types

### Core Protocol

| Type | Description |
|------|-------------|
| `AwpVersion` | Protocol version with `is_compatible()` and `CURRENT_VERSION` (1.0) |
| `TrustLevel` | Anonymous < Known < Partner < Internal (ordered enum) |
| `RequesterType` | Human or Agent |
| `AwpError` | 8 error variants with HTTP status code mapping (400–503) |

### Wire Types

| Type | Description |
|------|-------------|
| `AwpRequest` | Protocol request envelope (id, trust_level, requester_type, version, payload) |
| `AwpResponse` | Protocol response envelope (id, version, status, payload) |
| `A2aMessage` | Generic agent-to-agent message |
| `A2aMessageType` | Request, Response, Notification, Error |
| `AwpTypedMessage` | AWP-specific typed message with `AwpMessageType` |
| `AwpMessageType` | 9 domain-specific variants for agent routing |

### Discovery & Capabilities

| Type | Description |
|------|-------------|
| `AwpDiscoveryDocument` | `/.well-known/awp.json` payload |
| `CapabilityManifest` | JSON-LD manifest with `@context` and `@type` |
| `CapabilityEntry` | Single capability with name, endpoint, method, schemas |

### Business Context

| Type | Description |
|------|-------------|
| `BusinessContext` | Full site configuration parsed from `business.toml` |
| `BusinessCapability` | Capability with access level |
| `BusinessPolicy` | Operational policy |
| `BusinessIdentity` | Country, languages, currency, timezone |
| `BrandVoice` | Tone, greeting, escalation message |
| `Product` | SKU, name, price, inventory, tags |
| `ChannelConfig` | WhatsApp, email, website, SMS |
| `PaymentConfig` | Providers, auto-approve thresholds |
| `SupportConfig` | Escalation contacts, hours, SLA |
| `ContentConfig` | Topics, auto-draft, publish delay |
| `ReviewConfig` | Platforms, auto-respond threshold |
| `OutreachConfig` | Follow-up delay, consent enforcement |

### Payments

| Type | Description |
|------|-------------|
| `PaymentIntent` | Owner-policy-driven payment with an HMAC-SHA256 `signature` field |
| `PaymentIntentState` | Draft → PendingApproval → Approved → Executing → Settled/Rejected/Cancelled |
| `PaymentPolicy` | Auto-approve/require-approval thresholds |
| `PaymentPolicyDecision` | AutoApprove or RequireApproval |

## AWP 2.0

AWP 2.0 is the signed contract between a website and the people and agents who use it (spec 3.5 §7.2–§7.8.3, §7.10). Its types live in `awp_types::v2` and are re-exported at the crate root beside the 1.0 types, whose names are unchanged. Cryptography and canonicalisation live in `adk-awp` and `adk-auth`; these types only carry and validate the documents.

| Area | Types | Spec |
|------|-------|------|
| Site contract (`/.well-known/awp.json`) | `SiteContract`, `SiteRef`, `Profile` (`static`, `static+edge`, `dynamic`), `SurfaceLinks`, `ContractRelease`, `ContractStatus`, `EventsLink`, `PaymentsLink`, `CharterLink`, `ContactLinks`, `VERSION_2_0` | §7.2 |
| Keys and proofs (`keys.json`) | `Proof` (detached JWS), `KeySet`, `RootKey`, `RootEndorsement`, `RetiredRoot`, `Revocation`, `Delegation`, `KeyScope`, `PublicKeyJwk`, `without_proof` | §7.2 |
| Identity | `IdentityPolicy`, `IdentityMethod`, `TrustTier` (`Public < Known < Verified < Partner`), `TrustRules`, `TrustRule`, `RateClass`, `RequesterId` | §7.3 |
| Capabilities | `CapabilityV2`, `CapabilityEffect`, `ConsentRequirement`, `Price`, `ApprovalHint`, `IdempotencySource`, `Deprecation`, `CapabilityManifestV2`, `MachineView` | §7.4 |
| Held calls | `HeldCall`, `HeldCallState` | §7.4 |
| Events | `EventEnvelope` (Standard Webhooks), `EventType`, `EventCatalog`, `EventSubscription`, `SubscriptionState`, `ReleasePublished`, `StatusChanged`, `CatalogChanged`, `CatalogChangesPage`, `OrderUpdated`, `ConsentRevoked`, `SubscriptionSuspended` | §7.5, §7.8.3 |
| Status | `SiteStatus`, `SiteState` | §7.5 |
| Consent and commerce | `ConsentReceipt`, `ConsentScope`, `LegalBasis`, `Receipt`, `QuoteDocument`, `LineItem` | §7.5 |
| Errors | `ErrorEnvelope` (with the status document on `read_only`), `ErrorBody`, `ErrorCode` | §7.5, §7.9 |
| Releases and the site log | `ReleaseManifest` (snapshot or delta), `FileEntry`, `Redirect`, `ReleaseSource`, `GeneratorRef`, `Digest`, `LogEntry`, `HostLimits`, `Atomicity` | §7.7, §7.8.3 |
| Charter | `Charter`, `SiteKind`, `Governance`, `Role`, `ApprovalChain`, `Goal`, `Kpi`, `Guardrail`, `JobAuthority`, `AutonomyLevel`, `Budget`, `Voice`, `RedLine`, `Reporting`, `Urgency`, `PublicTerms`, `OwnerSignature`, `PriceRule` | §7.6, §7.11, §7.16.1, §7.21 |
| Signals | `Signal`, `SignalKind`, `Bounded` | §7.10 |
| Content | `ContentItem`, `ContentKind`, `ContentState`, `Schedule`, `MediaRef`, `FactRef`, `LanguageTag`, `StreamEntry`, `BatchPolicy`, `FeedBounds`, `Bound`, `Rate`, `Trigger` | §7.8.2, §7.8.3 |

Validation helpers are pure:

| Helper | Rule |
|--------|------|
| `Charter::validate()` | No KPI target without a baseline (A-I14); at least one owner; no agent holds a role; every approval chain satisfiable by distinct principals (A-I23); unique KPI ids that goals reference |
| `ApprovalChain::is_satisfied_by()` | Each role of the chain filled by a different person who holds it (A-I23) |
| `CapabilityV2::requires_idempotency()` | `true` for `NonIdempotent` capabilities (A-I7) |
| `CapabilityManifestV2::visible_to()` | Lists only the capabilities a tier may see |
| `ReleaseManifest::is_snapshot()`, `apply_to()` | Snapshot or delta; a delta's full file set |
| `Bounded::new()` | Length-limited, always untrusted text (A-I3) |
| `PriceRule::allows()`, `QuoteDocument::is_open()` | Price changes within the rule; quote validity (A-I25) |
| `ConsentReceipt::covers()` | A live receipt for a scope and policy version (A-I8) |
| `KeySet::delegates_for()`, `is_revoked()` | Delegations valid for a scope and date, revocations applied |
| `TrustRules::tier_for()` | The highest tier a verified requester's matching rules grant, else `known` |

```rust
use awp_types::{
    CapabilityEffect, CapabilityManifestV2, CapabilityV2, Profile, SiteContract, SiteRef, TrustTier,
};
use uuid::Uuid;

fn main() {
    let mut contract = SiteContract::new(SiteRef {
        id: Uuid::nil(),
        name: "Example Solar".into(),
        domain: "example.com".into(),
        languages: vec!["en".parse().unwrap(), "sw".parse().unwrap()],
    });
    contract.profile = Some(Profile::StaticEdge);
    let json = serde_json::to_value(&contract).unwrap();
    assert_eq!(json["awp"], "2.0");
    assert_eq!(json["profile"], "static+edge");

    let manifest = CapabilityManifestV2::new(vec![
        CapabilityV2::new("search_site", "Search the site", CapabilityEffect::ReadOnly),
        CapabilityV2::new("place_order", "Place an order", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Known),
    ]);
    assert_eq!(manifest.visible_to(TrustTier::Public).capabilities.len(), 1);
    assert!(manifest.get("place_order").unwrap().requires_idempotency());
}
```

### JSON Schemas

The `json-schema` feature derives `schemars::JsonSchema` for every 2.0 type and adds `awp_types::v2::schema::wire_schemas()`, the JSON Schema (2020-12) of each wire document. The generated schemas are committed under [`schemas/2.0/`](schemas/2.0/): the site contract, `keys.json`, the trust rules, a capability, the capability manifest, a machine view, a held call, an event envelope, an event subscription, a `catalog.changed` page, the site status, a consent receipt, a quote, a receipt, the error envelope, a release manifest, a site log entry, a stream entry, the charter and its public terms. A test fails when they drift from the types.

```toml
[dependencies]
awp-types = { version = "3.0", features = ["json-schema"] }
```

## Quick Start

```toml
[dependencies]
awp-types = "3.0"
```

The only feature is `json-schema` (off by default).

```rust
use awp_types::{AwpError, AwpVersion, BusinessContext, CURRENT_VERSION, TrustLevel};

fn main() {
    // Trust level ordering
    assert!(TrustLevel::Anonymous < TrustLevel::Known);
    assert!(TrustLevel::Known < TrustLevel::Partner);

    // Version compatibility
    let v = AwpVersion { major: 1, minor: 3 };
    assert!(CURRENT_VERSION.is_compatible(&v)); // same major

    // Business context with only the required fields; `business.toml` deserializes into the same type
    let ctx = BusinessContext::core("My Site", "A description", "example.com");
    assert_eq!(ctx.site_name, "My Site");

    // Errors map to HTTP status codes
    assert_eq!(AwpError::NotFound("page".into()).status_code(), 404);
}
```

## Serialization

- Wire structs use `#[serde(rename_all = "camelCase")]` for JSON; enum values are lowercase or snake_case
- `BusinessContext` uses standard snake_case for TOML (`business.toml`)
- All types implement `Serialize` and `Deserialize`

## Design Principles

1. **Zero `adk-*` dependencies** — only `serde`, `serde_json`, `uuid`, `chrono`, `thiserror`, and `schemars` behind `json-schema`
2. **Backward-compatible** — all extended `BusinessContext` fields use `#[serde(default)]`
3. **Type-safe** — ordered enums, exhaustive error variants, `Display` impls
4. **Publishable** — suitable for crates.io (no git dependencies)

## Related Crates

- [`adk-awp`](https://crates.io/crates/adk-awp) — Full AWP implementation (routes, middleware, services)
- [`adk-core`](https://crates.io/crates/adk-core) — ADK foundational traits
- [`adk-server`](https://crates.io/crates/adk-server) — HTTP server with A2A protocol

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
