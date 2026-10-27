# Writing capability packs

A capability is one typed action a site offers people and agents: enquire, quote, book, order,
search. One declaration, `awp_types::CapabilityV2`, and one handler,
`adk_awp::v2::capability::CapabilityHandler`, give an A2A skill, an MCP tool and
`POST /awp/v2/capabilities/{name}`, all through one governed path (spec 3.5 §7.4, D-4). A pack is
a set of declarations with their handlers; `adk-website` ships six, and a site adds its own
(spec 3.5 §7.16 `packs`, §7.23).

A pack ships as a signed bundle: its declarations, the configuration of the built-in handlers
that serve them, its playbooks and its driver manifests, sealed with a content digest and its
publisher's AWP 2.0 signature, and verified against the site's trust list when it is installed
and every time the site starts ([Signed bundles](#signed-bundles)). A capability whose handler
is your own code runs on an edge you build with `CapabilityRegistry`
([Registering a pack](#registering-a-pack)).

```toml
[dependencies]
adk-awp = { version = "3.0.0", features = ["signing"] }
adk-core = "3.0.0"
adk-payments = { version = "3.0.0", features = ["ap2"] }
adk-server = { version = "3.0.0", features = ["awp"] }
adk-website = "3.0.0"
awp-types = "3.0.0"
async-trait = "0.1"
serde_json = "1"
tracing = "0.1"
```

## The declaration

| Member | Values | Effect at the edge |
|--------|--------|--------------------|
| `name` | `^[a-z][a-z0-9_]{0,63}$` | The A2A skill id, the MCP tool name and the HTTP path segment |
| `description` | Text | Shown in the manifest, the Agent Card and `tools/list` |
| `effect` | `read_only`, `idempotent`, `non_idempotent` | The tool's `ToolEffect`; `non_idempotent` requires an idempotency key and runs at most once per key (A-I7) |
| `minTrust` | `public`, `known`, `verified`, `partner` | Callers below it neither see nor call the capability; to them it does not exist |
| `inputSchema`, `outputSchema` | JSON Schema | The input is validated before the handler runs; a call whose input fails answers `schema_validation_failed` |
| `consent` | `{scope, policyVersion}` | A call without a live receipt for the scope under that policy version is refused with `consent_required` (A-I8) |
| `price` | `{amount, currency, rails}` | The price in minor units; the call's amount when the handler reads none |
| `approval` | `{"mode": "never"}`, `{"mode": "above_amount", "amount": n}`, `{"mode": "always"}` | A call the owner must decide answers `202` and waits in the approval store |
| `idempotency` | `binding` (default), `a2a_message_id`, `mcp_request_id`, `idempotency_key` | Where the call's key comes from |
| `rateClass` | Text | The rate-limit class |
| `deprecated` | `{successor, sunset}` | The capability's replacement and the time it stops answering |

A site lists its declarations in `capabilities.json`:

```json
[
  {
    "name": "book_visit",
    "description": "Book a site survey for a solar installation",
    "effect": "non_idempotent",
    "minTrust": "known",
    "inputSchema": {
      "type": "object",
      "properties": {
        "date": { "type": "string", "format": "date" },
        "address": { "type": "string", "maxLength": 300 },
        "amount": { "type": "integer", "minimum": 0 }
      },
      "required": ["date", "address"],
      "additionalProperties": false
    },
    "consent": { "scope": "contact", "policyVersion": "1" },
    "approval": { "mode": "above_amount", "amount": 50000 },
    "idempotency": "binding"
  }
]
```

Every field of the input is text from an untrusted caller: bound its lengths, close the object
with `additionalProperties: false`, and declare formats.

## The handler

```rust
use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
use awp_types::ErrorCode;
use serde_json::{Value, json};

struct BookVisit;

#[async_trait::async_trait]
impl CapabilityHandler for BookVisit {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let date = call.input["date"].as_str().unwrap_or_default();
        if date < "2026-11-01" {
            return Err(CapabilityError::refused(
                ErrorCode::InventoryUnavailable,
                "no survey slots before November",
            ));
        }
        // `call.idempotency_key` travels to anything downstream that creates the booking.
        Ok(json!({ "booking": format!("bk-{}", call.idempotency_key), "date": date }))
    }
}
```

| `CapabilityCall` member | Holds |
|-------------------------|-------|
| `capability` | The capability's name |
| `input` | The input object, already validated against `inputSchema` |
| `tier` | The caller's verified tier |
| `requester` | The verified requester id, from `known` up, such as `wba:{directory origin}#{thumbprint}` |
| `idempotency_key` | The call's key; a `non_idempotent` handler passes it downstream |
| `consent` | The consent receipt ids the call references |

`CapabilityError::refused(code, message)` answers the caller with an AWP error code —
`inventory_unavailable`, `quote_expired`, `payment_policy_rejected` and the rest of the
registry — and a message that carries no internal detail. `CapabilityError::Failed` answers
`internal_error` and logs the detail. `call_amount` gives the amount an `above_amount` approval
compares; the default reads a non-negative integer `amount` member of the input, and a
capability with neither that nor a `price` is never held by an amount.

## What runs before the handler

The edge's governed path decides every call, from every binding, before the handler sees it
([The AWP agent edge](awp-edge.md#one-governed-path)):

1. Visibility by tier, and deprecation.
2. Read-only mode and the kill switch: only `read_only` capabilities answer while either holds.
3. The caller's daily call limit.
4. The input schema; the idempotency key a `non_idempotent` capability requires; the consent
   receipt a consent-bound capability requires.
5. The idempotency record: a repeat with the same key and input returns the first outcome
   without executing; the same key with other input answers `409 idempotency_conflict`.
6. `adk_core::authorize_tool_call` under the policy compiled from the manifest, with the action
   ledger and the audit trail; a call above its approval threshold is held for the owner.

A handler therefore needs no tier, consent or idempotency checks of its own. The acting agent is the site's own
principal, and the verified requester is the counterparty on every approval request and
`awp.requester` on every `awp.capability.*` audit record.

## Registering a pack

```rust
use std::sync::Arc;

use adk_awp::signing::jws::Ed25519DocumentSigner;
use adk_awp::v2::capability::CapabilityRegistry;
use adk_core::Url;
use adk_server::awp::{AwpEdge, EdgeKeys};
use awp_types::CapabilityV2;

fn edge(declarations: Vec<CapabilityV2>) -> Result<AwpEdge, Box<dyn std::error::Error>> {
    let mut registry = CapabilityRegistry::new("example.com");
    for declaration in declarations {
        match declaration.name.as_str() {
            "book_visit" => {
                registry.declare(declaration, Arc::new(BookVisit))?;
            }
            other => tracing::warn!(capability = other, "no handler; not served"),
        }
    }
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::generate("edge-card-2026-10")?),
        receipt: Arc::new(Ed25519DocumentSigner::generate("edge-receipt-2026-10")?),
    };
    Ok(AwpEdge::builder(registry, keys, Url::parse("https://agent.example.com")?).build()?)
}
```

`declare` refuses a name outside `^[a-z][a-z0-9_]{0,63}$`, an input schema that does not
compile, and a name declared twice. Mount the edge with `ServerBuilder::awp`, or serve
`edge.router()` beside the content; give it tiers, an approval service, stores and consent as
[The AWP agent edge](awp-edge.md) shows. In a project from `cargo adk new my-site --template
website`, `src/main.rs` reads `capabilities.json` and registers the handlers it knows — add a
match arm there for each capability you write.

## The default packs

`adk_website::packs::Pack` holds the declarations `adk-website` ships. Their names match the
ones migration drafts from a legacy site's features, so a migrated contact form and a
template's contact form are one capability ([Move a WordPress site to AWP](wordpress-to-awp.md#what-becomes-what)).

| Pack | Capabilities |
|------|--------------|
| `contact` | `submit_inquiry`, `subscribe_newsletter`, `request_prospectus`, `licensing_enquiry`, `volunteer`, `submit_service_form` |
| `quote` | `get_quote` |
| `booking` | `check_availability`, `book`, `book_appointment`, `register` |
| `order` | `list_products`, `place_order`, `pay_order`, `order_status`, `request_return`, `donate`, `subscribe`, `buy_ticket` |
| `search` | `search_site` |
| `components` | `practice_information`, `lookup_status`, `list_notices`, `list_events`, `get_schedule` |

```rust
use adk_website::packs::Pack;

fn quote_pack() {
    let names: Vec<String> = Pack::Quote.capabilities().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["get_quote"]);
    assert_eq!(Pack::of("book"), Some(Pack::Booking));
}
```

Each site kind's template picks the capabilities it offers ([Site kinds](adk-website.md#site-kinds)),
and `edge::SiteEdge::build` serves them with the packs' handlers:

| Handler | Serves | Keeps |
|---------|--------|-------|
| Forms | The `contact` pack and `register` | A lead, the contact, and a `lead_captured` signal into the back office |
| Quotes | `get_quote` | Quotes at the published prices, each keeping its price until it expires (A-I25) |
| Orders | `place_order`, `order_status`, `request_return` | Orders against open quotes, each with what the payment desk needs to take its payment; an order is its buyer's, and another requester learns nothing of it |
| Payments | `pay_order`, `donate`, `subscribe`, `buy_ticket` | Payment through the owner's `PaymentDesk`; refused without one; an order is paid once |
| Bookings | `check_availability`, `book`, `book_appointment` | Slots of the owner's `Calendar`, each with an optional `price`; a taken slot answers `inventory_unavailable` |
| Search | `search_site` | The release's index |
| Listings | `list_products`, `list_notices`, `list_events`, `get_schedule`, `practice_information` | The published items of the matching kind |
| Status | `lookup_status` | An application's status by its reference |

`SiteEdge::build` serves only the capabilities a handler covers; any other name in the kind's
declarations is logged and left unserved. A capability of your own runs on an edge you build
with `CapabilityRegistry`, as above.

## Amount thresholds

The owner holds large orders, bookings and payments for a decision with
`adk_website::packs::ApprovalThresholds` (spec 3.5 §7.4, S9): `SiteEdgeParts::thresholds`, or
`approvals.json` in a served site's project, such as `{"place_order": 20000, "book": 15000}` in
minor units of the site's currency. A call above its threshold answers `202` with
`Location: /awp/v2/calls/{id}` and runs once the owner approves.

| Capability | The amount the edge compares |
|------------|------------------------------|
| `place_order` | The quote's total |
| `pay_order` | The order's total |
| `book`, `book_appointment` | The slot's `price` |
| `donate` | The `amount` the donor gives |

`ApprovalThresholds::commerce(amount)` holds orders, bookings, appointments and donations above
`amount`; a threshold on `pay_order` as well would ask the owner a second time for an order they
approved. A threshold tightens a declaration and never loosens one, and a threshold on a
capability whose calls carry no amount is refused when the edge is built.

## Payments

`pay_order` takes the order, the rail, and either the buyer's AP2 payment mandate (`mandate`)
or a single-use payment token from the buyer's provider (`credential`). The owner's
`PaymentDesk` takes the payment: the payment handler finds the order, refuses one that is not
awaiting payment, holds it `paying` while the desk works, so a second payment under another
key is refused, and marks it paid when the desk answers `status: "paid"`; a desk that settles
later marks it with `OrderBook::set_status`.

| Desk | Takes |
|------|-------|
| `deploy::PaymentLinks` | Nothing: answers with the owner's payment instructions from `payments.json` |
| `edge::ap2::Ap2Desk` | AP2: `place_order` returns the site's cart signed by the edge's `receipt` key as `payment.cartMandate`; `pay_order` with the buyer's mandate for that cart runs through `adk-payments`' merchant-side `Ap2Adapter` — the cart, total and expiry checked, the buyer's authorisation verified, the mandate used once — and the owner's payment processor; the answer carries a receipt signed by the `receipt` key |
| `edge::token::TokenDesk` | Token rails, such as `card`: `pay_order` with the single-use token the buyer's payment provider issued (`credential`) hands it to the owner's payment processor with the order's total and the call's idempotency key, and answers with a receipt signed by the `receipt` key. The token reaches the processor only — never the journal, the answer or a log; a call on another rail, without a token, or with a token the processor declines pays nothing |
| `edge::token::PaymentRails` | Several desks by rail: `pay_order`'s `rail` picks the desk, so an AP2 desk and token rails serve one site; a call naming no rail goes to the first desk |

```rust
use std::sync::Arc;

use adk_awp::signing::jws::DocumentSigner;
use adk_core::identity::AdkIdentity;
use adk_payments::kernel::PaymentExecutionService;
use adk_payments::protocol::ap2::UserAuthorizationVerifier;
use adk_website::edge::ap2::{Ap2Desk, in_memory_journal};

async fn desk(
    receipt_key: Arc<dyn DocumentSigner>,
    processor: Arc<dyn PaymentExecutionService>,
    buyers: Arc<dyn UserAuthorizationVerifier>,
    identity: AdkIdentity,
) -> adk_core::Result<Ap2Desk> {
    let (transactions, evidence) = in_memory_journal(&identity).await?;
    Ap2Desk::builder("Juniper Bakery", receipt_key)
        .processor(processor)
        .buyer_authorization(buyers)
        .journal(transactions, evidence, identity)
        .build()
}
```

The desk picks no payment processor: give it the owner's `PaymentExecutionService`, and keep
its journal in the site's session and artifact services in production;
`edge::ap2::SandboxProcessor` moves no money and is for tests and sandboxes. A served site
takes the desk as `SiteOptions::payments`.

A token rail beside AP2:

```rust
use std::sync::Arc;

use adk_awp::signing::jws::DocumentSigner;
use adk_core::identity::AdkIdentity;
use adk_payments::kernel::{PaymentExecutionService, TransactionStore};
use adk_website::edge::ap2::{AP2_RAIL, Ap2Desk};
use adk_website::edge::handlers::PaymentDesk;
use adk_website::edge::token::{PaymentRails, TokenDesk};

fn rails(
    ap2: Ap2Desk,
    receipt_key: Arc<dyn DocumentSigner>,
    processor: Arc<dyn PaymentExecutionService>,
    transactions: Arc<dyn TransactionStore>,
    identity: AdkIdentity,
) -> adk_core::Result<PaymentRails> {
    let cards = TokenDesk::builder("Juniper Bakery", receipt_key)
        .rail("card")
        .processor(processor)
        .journal(transactions, identity)
        .build()?;
    Ok(PaymentRails::new()
        .rail(AP2_RAIL, Arc::new(ap2) as Arc<dyn PaymentDesk>)
        .rail("card", Arc::new(cards) as Arc<dyn PaymentDesk>))
}
```

The processor redeems the token with the provider that issued it: the command's
`selected_payment_method` is `single_use_token` with the token as its `reference`, and its
`awp-token` extension carries the rail and the idempotency key.

## Bookings

`check_availability`, `book` and `book_appointment` read and take the slots of the owner's
`Calendar`; a taken slot answers `inventory_unavailable`.

| Calendar | Slots |
|----------|-------|
| `edge::handlers::InMemoryCalendar` | A fixed list: `calendar.json` in a served site, a list of `{id, startsAt, minutes, price}` |
| `edge::caldav::CalDavCalendar` | The owner's CalDAV calendar (RFC 4791) on Nextcloud, Fastmail, iCloud, Radicale, Baïkal, Google or any CalDAV server: `caldav.json` in a served site, or `SiteOptions::calendar` |

A CalDAV calendar reads two collections. Every event of the **slots** collection is a bookable
slot, and a recurring event one slot per occurrence, which the server expands. Bookings go to
the **bookings** collection — the slots' own by default, or the owner's everyday calendar,
whose busy events then also block the slots they overlap; transparent and cancelled events
block nothing. Each booking is one event under a name derived from its slot, written with
`If-None-Match: *`, so a slot is booked once whichever process, site or caller asks first; a
retry with the same booking key answers as booked. A booking event carries the slot and the
booking key and no personal data — the contact stays in the site's contact book. A slot's
`X-ADK-PRICE` property (minor units) prices it for the owner's amount thresholds, else the
calendar's price.

```json
{
  "slots": "https://dav.example.com/calendars/juniper/open-slots/",
  "bookings": "https://dav.example.com/calendars/juniper/personal/",
  "username": "juniper",
  "password": "env:JUNIPER_CALDAV_PASSWORD",
  "price": 1500,
  "timeZone": "Africa/Nairobi"
}
```

Secrets in `caldav.json` are references — `env:VAR` or `file:PATH` relative to the project —
never the secret itself; `token` takes an OAuth bearer token in place of `username` and
`password`. The server must be `https`, or plain `http` on loopback, since the credentials
travel with every request.

## Signed bundles

A bundle is one JSON document (spec 3.5 §7.23), format `adk-website.pack/1`:

```json
{
  "format": "adk-website.pack/1",
  "name": "waitlist",
  "version": "1.0.0",
  "publisher": { "id": "tables.example", "name": "Tables" },
  "description": "A waiting list, the menu and table reservations for restaurants",
  "capabilities": [ { "name": "join_waitlist", "effect": "non_idempotent", "…": "…" } ],
  "handlers": {
    "join_waitlist": { "handler": "form" },
    "list_menu": { "handler": "listing", "content": "product" },
    "table_availability": { "handler": "booking", "operation": "availability" }
  },
  "playbooks": [ { "id": "waitlist-capture", "job": "capture", "agents": ["concierge"], "text": "---\nname: waitlist-capture\n…" } ],
  "drivers": [ { "id": "tables-pos", "manifest": "id = \"tables-pos\"\n…" } ],
  "digest": "sha256:…",
  "proof": { "alg": "EdDSA", "kid": "tables-2026", "jws": "<detached JWS>" }
}
```

| Member | Holds |
|--------|-------|
| `name`, `version` | Lowercase letters, digits and `-`; a Semantic Version without build metadata |
| `publisher` | The `id` a trust list names, and a display `name` |
| `capabilities` | Declarations, as [above](#the-declaration) |
| `handlers` | The built-in handler serving each capability ([below](#handler-configuration)) |
| `playbooks` | Skill files for a job of spec 3.5 §7.20, each for agents that run the job; the front matter's `name` is the playbook's `id`, which is not one of the product's playbooks |
| `drivers` | Kernel driver manifests (TOML), each sandboxed (`os` or `container`), serving the `public` or `back-office` partition, every tool with an effect class other than `other` |
| `digest` | SHA-256 of the JCS form of the bundle without `digest` and `proof` |
| `proof` | The publisher's detached JWS (EdDSA) over the JCS form of the bundle without `proof`, so it covers the digest |

A pack runs no code in the site's process: its capabilities run on the built-in handlers, and
its drivers are MCP servers the kernel runs sandboxed once the owner mounts them.

### Handler configuration

| `handler` | Serves | Effect at least | Input members it reads |
|-----------|--------|-----------------|------------------------|
| `form` | The contact, a lead and a `lead_captured` signal | `idempotent` | — |
| `quote` | Quotes at the release's prices | `idempotent` | `items` |
| `order` with `operation` `place`, `status` or `return` | Orders against quotes | `non_idempotent`; `read_only` for `status` | `quoteId`; `orderId` |
| `payment` | Payments through the owner's payment desk | `non_idempotent` | — |
| `booking` with `operation` `availability` or `book` | The owner's calendar | `read_only`; `non_idempotent` for `book` | —; `slot` |
| `search` | The release's index | `read_only` | `query` |
| `listing` with `content` | Published items of one content kind | `read_only` | — |
| `status` | An application's status by its reference | `read_only` | `reference` |

A capability declared weaker than its handler, or whose input schema lacks a member the handler
reads, is refused. A capability that takes an `email` must ask for consent (A-I8), under policy
version `1`, the site's public terms; its scope joins the edge's consent scopes.

### Publishing a pack

The publisher writes the bundle without `digest` and `proof` and seals it with their key file —
a private Ed25519 JWK, the format `adk-rust awp keys` writes:

```text
$ adk-rust site packs sign waitlist.json --key tables-2026.jwk
Signed waitlist 1.0.0 as waitlist-1.0.0.pack.json; digest sha256:….
Owners trust this key with `adk-rust site packs trust tables.example KEY_FILE`; its thumbprint is ….
{"crv":"Ed25519","kid":"tables-2026","kty":"OKP","x":"…"}
```

`sign` checks the content first. In Rust, `PackBundle::check` and `PackBundle::seal` do the same
with any `DocumentSigner`, such as a KMS-backed one.

### Trusting and installing

| Command | Does |
|---------|------|
| `site packs trust PUBLISHER KEY [--kid ID] [--note TEXT]` | Adds the publisher's public JWK to `packs/trust.json`, after showing its RFC 7638 thumbprint to compare with the publisher's; a private key file is refused |
| `site packs install FILE` | Verifies the bundle, shows what it adds and installs it: `packs/{name}.pack.json` byte for byte, its driver manifests in `packs/drivers/{name}/` |
| `site packs list [--json]` | The built-in packs, the installed ones, and the installed files the site does not load, with why |
| `site packs remove NAME` | Removes an installed pack and its driver manifests |

Each asks at the terminal; `--yes` answers for a script. A pack takes effect when the site next
starts: `deploy::serve` verifies every installed bundle again, serves its capabilities on the
edge with the kind's, and writes its playbooks into each agent's workspace under
`.skills/packs/{name}/`. A file that no longer verifies is not loaded and is logged. Driver
manifests are never mounted by installation: mount each with `adk-rust drivers mount
packs/drivers/{name}/{id}.toml`, which asks the owner again.

| Refusal | When |
|---------|------|
| `unsigned` | The bundle has no `proof` |
| `tampered` | The content does not match `digest`, or `proof` does not verify with the publisher's trusted key |
| `unknown_publisher` | `packs/trust.json` holds no key for the bundle's publisher and key id |
| `downgrade` | The installed version is newer |
| `republished` | The installed version has other content |
| `conflict` | Another pack holds the name, a capability, a playbook or a driver; a built-in pack's name belongs to its publisher |
| `invalid` | The content breaks a rule above |
| `malformed`, `format` | Not a bundle, or a later format |

The conformance vectors in `adk-website/tests/fixtures/packs/` — a valid pack, its upgrade and a
bundle for each refusal, with `cases.json` naming each outcome — are checked by
`tests/packs.rs`; another implementation can run the same cases.

### The built-in packs as bundles

The six default packs ship as bundles in `adk-website/packs/`, generated from
`packs::capability` and `Pack::handler` and checked by a test that regenerates them. They are
signed with a test key whose seed is public in `packs::builtin`, so the signature proves only
that the compiled-in copy is the generated one. Its trust is explicit: `builtin_trust()` names it
for the built-in bundles alone, and `site packs trust` refuses it, so a pack anyone signs with it
never installs.

```rust
use adk_website::packs::Pack;
use adk_website::packs::installed::PackDir;

fn packs(project: &std::path::Path) -> adk_core::Result<()> {
    let booking = Pack::Booking.bundle()?;
    assert_eq!(booking.bundle.publisher.id, "adk-website");
    let loaded = PackDir::new(project).load()?;
    for refused in loaded.refused() {
        tracing::warn!(file = %refused.file.display(), error = %refused.error, "pack not loaded");
    }
    Ok(())
}
```

## Checking a pack

| Check | How |
|-------|-----|
| The declarations are visible to the right tiers | `CapabilityRegistry::manifest_for(tier)`, or `GET /awp/v2/manifest` with and without a credential |
| The bindings agree | The same call over `POST /awp/v2/capabilities/{name}`, A2A `SendMessage` and MCP `tools/call` returns the same outcome, and a repeated key returns the first one |
| The deployment conforms | `adk-rust awp conformance http://127.0.0.1:8080 --active` runs the edge's `GET` checks against a deployment on loopback (A-I9) |
| The scenarios | S6 (identity, not claims) and S9 (agent-to-agent business) in `examples/website`: S9's buyer pays from its own wallet through `adk-payments`' AP2 rail into the shop's AP2 desk, and its large booking waits for the owner |

```bash
cargo run --manifest-path examples/website/Cargo.toml -- S6 S9
```

## Related

- [The AWP agent edge](awp-edge.md)
- [Agentic Web Protocol (AWP) 2.0](awp.md)
- [The AWP website product](adk-website.md)
- [MCP Server](../mcp/server.md)
