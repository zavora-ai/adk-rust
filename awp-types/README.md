# awp-types

Shared protocol types for the [Agentic Web Protocol (AWP)](https://agenticwebprotocol.com).

`awp-types` provides pure protocol types with **zero `adk-*` dependencies**, making it importable by any Rust project that needs to work with AWP messages — gateways, agents, CLI tools, or third-party integrations.

## Overview

Use `awp-types` when you need to:

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
| `PaymentIntent` | Owner-policy-driven payment with HMAC signature |
| `PaymentIntentState` | Draft → PendingApproval → Approved → Executing → Settled/Rejected/Cancelled |
| `PaymentPolicy` | Auto-approve/require-approval thresholds |
| `PaymentPolicyDecision` | AutoApprove or RequireApproval |

## Quick Start

```toml
[dependencies]
awp-types = "0.7"
```

```rust
use awp_types::{TrustLevel, AwpVersion, CURRENT_VERSION, BusinessContext};

// Trust level ordering
assert!(TrustLevel::Anonymous < TrustLevel::Known);
assert!(TrustLevel::Known < TrustLevel::Partner);

// Version compatibility
let v = AwpVersion { major: 1, minor: 3 };
assert!(CURRENT_VERSION.is_compatible(&v)); // same major

// Business context from TOML
let ctx = BusinessContext::core("My Site", "A description", "example.com");
assert_eq!(ctx.site_name, "My Site");
```

## Serialization

- All wire types use `#[serde(rename_all = "camelCase")]` for JSON
- `BusinessContext` uses standard snake_case for TOML (`business.toml`)
- All types implement `Serialize` and `Deserialize`

## Design Principles

1. **Zero `adk-*` dependencies** — only `serde`, `uuid`, `chrono`, `thiserror`
2. **Backward-compatible** — all extended `BusinessContext` fields use `#[serde(default)]`
3. **Type-safe** — ordered enums, exhaustive error variants, `Display` impls
4. **Publishable** — suitable for crates.io (no git dependencies)

## Related Crates

- [`adk-awp`](https://crates.io/crates/adk-awp) — Full AWP implementation (routes, middleware, services)
- [`adk-core`](https://crates.io/crates/adk-core) — ADK foundational traits
- [`adk-server`](https://crates.io/crates/adk-server) — HTTP server with A2A protocol

## License

Apache-2.0
