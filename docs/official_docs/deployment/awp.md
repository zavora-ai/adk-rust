# Agentic Web Protocol (AWP) 2.0

AWP 2.0 is the signed contract between a website and the people and agents that use it
(spec 3.5 Part A). A site publishes a root-signed contract at `/.well-known/awp.json` that
links every surface it offers — the A2A Agent Card, the MCP server, the capability manifest,
Markdown and machine views — with its keys, its release log, its status and the public part of
its charter. AWP 2.0 uses Web Bot Auth for identity, A2A for conversation, MCP for tools and
IETF AI preferences for usage, and defines signed releases, a site status, typed capabilities
under owner policy, held and exactly-once calls, consent receipts and the charter. Payment
rails come through `adk-payments`; on `main` no rail reaches the edge yet
([Remaining work](../../design/spec-3.5-agentic-web.md#remaining-work)).

> **Note:** `awp-types` and `adk-awp` are **Beta**, and AWP 2.0 stays Beta until its normative
> text is published (spec 3.5 §13 Q1). The site pipeline (`adk-site`) and the product
> (`adk-website`) are **Experimental**.

| You want to | Read |
|-------------|------|
| Run a site that builds and runs itself | [Spin up an AWP website](website-quickstart.md) |
| Move an existing WordPress site | [Move a WordPress site to AWP](wordpress-to-awp.md) |
| Serve capabilities to agents | [The AWP agent edge](awp-edge.md), [Writing capability packs](capability-packs.md) |
| Write the site's goals, authority and budget | [The charter](charter.md) |
| Check any site from the command line | [`adk-rust awp verify`](site-cli.md#adk-rust-awp) |
| Keep an AWP 1.x deployment running, or convert it | [Migrating from AWP 1.x to 2.0](../migration/awp-1-to-2.md) |

## Profiles

The contract's `profile` says what runs behind the site.

| Profile | What runs | What a client finds |
|---------|-----------|---------------------|
| `static` | Nothing: the site is files on a static host | The contract, `keys.json`, the release log, the charter's public terms, and every page with its Markdown and machine-view twins |
| `static+edge` | The agent edge beside the static content | All of `static`, plus capabilities over A2A, MCP and HTTP, held and exactly-once calls, the signed status, events, consent and the beacon. `adk-website`'s default |
| `dynamic` | An application that serves AWP itself | Whatever the application serves; the verifier reports it as server code |

No code runs when a page is requested on the first two profiles. If the edge stops, the content
keeps serving its last signed release.

## What a site publishes

`awp-types` holds every AWP 2.0 document in `awp_types::v2`, re-exported at the crate root
beside the 1.0 types. They are plain data with pure validation helpers and no `adk-*`
dependency.

| Document | Type | Served at |
|----------|------|-----------|
| Site contract | `SiteContract` | `/.well-known/awp.json` (`application/awp+json`) |
| Keys, delegations and revocations | `KeySet` | `/.well-known/awp/keys.json` |
| Charter public terms | `PublicTerms` (part of `Charter`) | `/.well-known/awp/charter.json` |
| Release manifest | `ReleaseManifest` | `/.well-known/awp/releases/{seq}.json` |
| Site log entry | `LogEntry` | Leaf `seq − 1` of the log under `/.well-known/awp/log/` |
| Machine view of a page | `MachineView` | `<path>/index.awp.json`, beside the Markdown twin `<path>/index.md` |
| Trust rules | `TrustRules` | The contract's `identity.trustRules` |
| Capability manifest | `CapabilityManifestV2` of `CapabilityV2` | `GET /awp/v2/manifest` |
| Held call | `HeldCall` | `GET /awp/v2/calls/{id}` |
| Site status | `SiteStatus` | `GET /awp/v2/status` |
| Event, subscription | `EventEnvelope`, `EventSubscription` | Standard Webhooks delivery; `POST /awp/v2/events` |
| Consent receipt, receipt, quote | `ConsentReceipt`, `Receipt`, `QuoteDocument` | Capability results |
| Stream entry | `StreamEntry` | The edge's stream feed for a live page |
| Error | `ErrorEnvelope` | Every 2.0 error response; `read_only` carries the status document |

The `json-schema` feature derives JSON Schema (2020-12) for every 2.0 type; the generated
schemas are committed in `awp-types/schemas/2.0/`.

```toml
[dependencies]
awp-types = { version = "3.0.0", features = ["json-schema"] }
```

## Crates and features

| Crate | Feature | Provides |
|-------|---------|----------|
| `awp-types` | — | The AWP 1.0 and 2.0 wire types; `json-schema` adds the schemas |
| `adk-awp` | — | `adk_awp::v2`: the `/awp/v2/` routes, `AWP-Version` negotiation, `TierAssigner`, `CapabilityTool`, `CapabilityRegistry`, `DirectExecutor`, the edge store traits with `edge_store_conformance_tests!`, and `business.toml` migration |
| `adk-awp` | `signing` | Detached JWS (EdDSA) over JCS, `KeySet` delegations, revocations and root rotation, the `_awp-root` DNS record, signed A2A Agent Cards, the site log's tiles and checkpoint, `StaticBundle` |
| `adk-awp` | `verify` | `Verifier`, the conformance kit (`adk_awp::conformance::run`) and the test-vector generator |
| `adk-awp` | `awp-v1` (default) | The AWP 1.x routes, for one release line |
| `adk-auth` | `web-bot-auth` | HTTP Message Signatures: signer, verifier and key directory ([Web Bot Auth](../security/web-bot-auth.md)) |
| `adk-tool` | `mcp-server` | An MCP server over Streamable HTTP with a Server Card ([MCP Server](../mcp/server.md)) |
| `adk-server` | `awp`, `awp-sqlite`, `awp-postgres`, `awp-redis` | The agent edge and its durable stores ([The AWP agent edge](awp-edge.md)) |
| `adk-site` | — | Content, the built-in generator, the launch gate, signed releases, the site log and hosts |
| `adk-website` | — | The prebuilt website ([The AWP website product](adk-website.md)) |
| `adk-rust` | `website` | The umbrella over `adk-website`, `adk-site`, the edge and AWP signing; in `full` |

## The static profile

Every published state of a site is a signed release. `adk-site`'s `ReleaseBuilder` hashes a
build into a `ReleaseManifest`, refuses server-executable files, writes the manifest under
`/.well-known/awp/releases/`, and appends its digest to the site log. The host receives the
files, then the log's tiles, then a checkpoint signed by a delegated `release` key, so a reader
never sees a checkpoint for files that are not there. The built-in generator writes each page's
`index.md` and `index.awp.json` twins, `llms.txt`, `robots.txt` with `Content-Usage` from the
charter's public terms, `/.well-known/security.txt`, sitemaps, feeds and, for a site with
products, a product feed at `/products/feed.xml` ([`adk-site`](../../../adk-site/README.md)).
`adk-awp`'s `signing::log` is the one C2SP tile writer: `adk-site`'s site log publishes the
tiles each release changes with it.

### Keys

| Key | Signs | Held by |
|-----|-------|---------|
| Root | The contract and the delegations | The owner, off the server; anchored by the `_awp-root.<domain>` TXT record |
| `release` | The site log's checkpoints | The site's release builder |
| `card` | The A2A Agent Card | The edge |
| `receipt` | The site status, consent receipts and stream entries | The edge |
| `payments` | Quotes and payment mandates | No component on `main` yet; delegated with `adk-rust awp keys delegate --scope payments` |

Verification walks root, delegation, signature. A new root is endorsed by the old one; when the
old root is lost, a person publishes the new root in DNS, so a stolen host account alone cannot
rotate it. No key appears in a model context, tool result, log or audit record (A-I5). Root
operations run only at a terminal: `adk-rust awp keys rotate` and `adk-rust awp keys delegate`
([Sites and AWP from the command line](site-cli.md#making-a-site-anyone-can-verify)).

### The contract, keys and public terms

`StaticBundle` (feature `signing`) builds the three static-profile documents: it links the
contract to `keys.json` and to the public terms with their digest, signs the contract with the
root, and serialises all three. The output is deterministic, so a release that changes nothing
here uploads nothing.

```toml
[dependencies]
adk-awp = { version = "3.0.0", features = ["signing"] }
awp-types = "3.0.0"
uuid = "1"
```

```rust
use adk_awp::signing::bundle::StaticBundle;
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use awp_types::{KeySet, Profile, PublicTerms, RootKey, SiteContract, SiteRef};

async fn bundle() -> Result<(), Box<dyn std::error::Error>> {
    let root = Ed25519DocumentSigner::generate("site-root-2026")?;
    let keys = KeySet {
        root: RootKey { kid: "site-root-2026".into(), public_key: root.public_key(), endorsement: None },
        delegations: vec![],
        retired_roots: vec![],
        revocations: vec![],
    };
    let mut contract = SiteContract::new(SiteRef {
        id: uuid::Uuid::nil(),
        name: "Example".into(),
        domain: "example.com".into(),
        languages: vec![],
    });
    contract.profile = Some(Profile::Static);
    let bundle = StaticBundle::build(contract, &keys, &PublicTerms::default(), &root).await?;
    // /.well-known/awp.json, /.well-known/awp/keys.json, /.well-known/awp/charter.json
    bundle.write_to(std::path::Path::new("well-known-root"))?;
    Ok(())
}
```

## The edge

The `static+edge` profile adds the agent edge, `adk-server`'s `awp` feature: one site's
capabilities to people and agents, with no owner route on its listener.

| Element | Behaviour |
|---------|-----------|
| Trust tiers | `public` by default; `known` from a valid Web Bot Auth signature, an OAuth client-credentials token, a client certificate or an owner-issued API key; `verified` from a directory or issuer on the owner's verified list; `partner` from an owner rule. `User-Agent`, `X-AWP-Channel` and every other self-declared header are claims and never change a tier (A-I6) |
| One declaration, three bindings | Each `CapabilityV2` is one `adk_core::Tool` with origin `awp:{domain}`, served as an A2A skill, an MCP tool and `POST /awp/v2/capabilities/{name}`, all through one governed path |
| Visibility | The manifest, the Agent Card and `tools/list` show only what the caller's tier may call; a capability above the tier answers `404 capability_not_found` |
| Held calls | A call the owner must decide answers `202` with `Location: /awp/v2/calls/{id}` and waits in the approval store |
| Exactly once | A `non_idempotent` capability requires an idempotency key; a repeat returns the first outcome without executing (A-I7) |
| Status, events, consent, beacon | The signed status, Standard Webhooks subscriptions, consent receipts with export and erasure, cookieless measurement |

See [The AWP agent edge](awp-edge.md) for the full edge and [Writing capability
packs](capability-packs.md) for capabilities.

An application not built on `adk-server` mounts the core routes from `adk-awp` itself:
`awp_v2_routes` serves `GET /awp/v2/manifest`, `GET /awp/v2/trust`, `GET /awp/v2/status`,
`POST /awp/v2/capabilities/{name}` and `GET /awp/v2/calls/{id}`, with `AWP-Version: 2.0` on
every response and the error envelope on every failure.

| Piece | Behaviour |
|-------|-----------|
| `TierAssigner` | Assigns `public`, `known`, `verified` or `partner` from a verified credential; the default makes every caller `public` |
| `CapabilityRegistry` | The site's capabilities and the manifest for a tier |
| `DirectExecutor` | Refuses a capability above the caller's tier as `capability_not_found`, validates input, requires an idempotency key for `non_idempotent` capabilities, checks consent receipts, answers a repeated key from its record, and holds a call above its approval threshold with `202` |
| Edge stores | Subscriptions, consent receipts, status, held calls, leads and idempotency records; backends run `edge_store_conformance_tests!` |

```rust
use adk_awp::v2::capability::CapabilityRegistry;
use adk_awp::v2::routes::{EdgeState, awp_v2_routes};

fn routes() -> axum::Router {
    let state = EdgeState::builder(CapabilityRegistry::new("example.com")).build();
    awp_v2_routes(state)
}
```

## Verify

Anyone can check a site from what it publishes. `Verifier` (feature `verify`) sends only `GET`
requests and names the step that fails:

| Step | Check | Failure |
|------|-------|---------|
| 1–2 | The contract and the key set are served and valid | `contract.unavailable`, `contract.invalid`, `keys.unavailable`, `keys.invalid` |
| 3 | The `_awp-root` DNS record names the root; a root other than the pinned one passes only when DNS anchors it, and is reported as a recovery unless the pinned root endorsed it | `root.unanchored` |
| 4–5 | The contract's proof by the root; every delegation, revocation and endorsement | `contract.signature`, `delegation.signature` |
| 6 | The A2A Agent Card's signature by a `card` key | `card.signature` |
| 7–8 | The checkpoint's signature by a `release` key; the log's tiles match it and extend the pinned one | `checkpoint.signature`, `log.consistency` |
| 9–12 | The release sequence, the latest manifest's inclusion, the delta chain, no server-executable path | `contract.release_ahead`, `release.inclusion`, `release.manifest`, `release.executable` |
| 13 | Sampled files (default 20) match their release digests | `page.digest` |
| 14 | The public terms match the contract's digest | `charter.digest` |

A pass pins the root and the checkpoint, so the next run fails when the log is rewritten. The
report ends with a posture and readiness summary: the profile, signs of server code, the
identity methods, A2A and MCP, usage preferences, `security.txt`, HSTS and a Content Security
Policy.

```toml
[dependencies]
adk-awp = { version = "3.0.0", features = ["verify"] }
```

```rust
use std::sync::Arc;

use adk_awp::signing::anchor::FixedTxtResolver;
use adk_awp::verify::Verifier;
use adk_awp::verify::fetch::HttpFetcher;
use adk_awp::verify::pins::FilePinStore;

async fn verify() -> Result<(), Box<dyn std::error::Error>> {
    let verifier = Verifier::new(
        Arc::new(HttpFetcher::new()?),
        Arc::new(FixedTxtResolver::default()),
        Arc::new(FilePinStore::new("awp-pins.json")),
    );
    let report = verifier.verify("example.com").await;
    println!("{}", report.failure.map_or("pass".to_owned(), |f| f.to_string()));
    Ok(())
}
```

`adk-awp` has no network DNS resolver of its own: pass a `DnsTxtResolver`, as
`adk-rust awp verify` does, or `FixedTxtResolver` with the record you expect. The conformance
kit, `adk_awp::conformance::run`, checks any AWP 2.0 implementation; `adk-rust awp verify
<domain>` and `adk-rust awp conformance <url>` run both from the command line
([Sites and AWP from the command line](site-cli.md#adk-rust-awp)).

## Version negotiation

`AWP-Version` takes a list, highest first. A 2.0 route answers in 2.0, and a 1.x route answers
`2.0, 1.0` in 1.0; a request naming no common major gets `406`. Every 2.0 response carries
`AWP-Version: 2.0`.

## The charter

The charter is part of the protocol: a site's kind, governance, mission, goals, KPIs,
guardrails, authority, budget, voice, red lines and reporting, signed by its owners. Its public
terms are published at `/.well-known/awp/charter.json` with their digest in the contract; the
rest stays in the charter store. See [The charter](charter.md).

```rust
use awp_types::{CapabilityEffect, CapabilityManifestV2, CapabilityV2, Charter, CharterViolation, TrustTier};

fn check(mut charter: Charter) {
    charter.kpis[0].baseline = None; // a target with no baseline
    let error = charter.validate().unwrap_err();
    assert!(matches!(error.violations[0], CharterViolation::TargetWithoutBaseline { .. }));

    let manifest = CapabilityManifestV2::new(vec![
        CapabilityV2::new("search_site", "Search the site", CapabilityEffect::ReadOnly),
        CapabilityV2::new("place_order", "Place an order", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Known),
    ]);
    // An unverified requester sees only what `public` may call.
    assert_eq!(manifest.visible_to(TrustTier::Public).capabilities.len(), 1);
}
```

## Migrating from AWP 1.x

AWP 1.x — `business.toml`, the unsigned discovery document, `/awp/manifest`, `/awp/a2a` and
trust levels assigned by the application — keeps working behind the default `awp-v1` feature
for one release line. `adk-rust awp migrate-config business.toml` converts its configuration
into 2.0 site data and a draft charter. [Migrating from AWP 1.x to 2.0](../migration/awp-1-to-2.md)
maps every 1.x element to its 2.0 counterpart and keeps the 1.x reference.

## Related

- [The AWP agent edge](awp-edge.md)
- [Sites and AWP from the command line](site-cli.md)
- [The AWP website product](adk-website.md)
- [Web Bot Auth](../security/web-bot-auth.md)
- [A2A Protocol](a2a.md)

---

**Previous**: [← A2A Protocol](a2a.md) | **Next**: [The AWP Agent Edge →](awp-edge.md)
