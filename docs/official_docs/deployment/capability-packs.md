# Writing capability packs

A capability is one typed action a site offers people and agents: enquire, quote, book, order,
search. One declaration, `awp_types::CapabilityV2`, and one handler,
`adk_awp::v2::capability::CapabilityHandler`, give an A2A skill, an MCP tool and
`POST /awp/v2/capabilities/{name}`, all through one governed path (spec 3.5 §7.4, D-4). A pack is
a set of declarations with their handlers; `adk-website` ships six, and a site adds its own
(spec 3.5 §7.16 `packs`, §7.23).

> **Note:** a pack in 3.0.0 is code: declarations and handlers registered on the edge. The
> signed, installable bundle of declarations, handlers, skills and driver manifests that spec
> 3.5 §7.23 describes is not built
> ([Remaining work](../../design/spec-3.5-agentic-web.md#remaining-work)).

```toml
[dependencies]
adk-awp = { version = "3.0.0", features = ["signing"] }
adk-core = "3.0.0"
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
| Orders | `place_order`, `order_status`, `request_return` | Orders against open quotes |
| Payments | `pay_order`, `donate`, `subscribe`, `buy_ticket` | Payment through the owner's `PaymentDesk`; refused without one |
| Bookings | `check_availability`, `book`, `book_appointment` | Slots of the owner's `Calendar`; a taken slot answers `inventory_unavailable` |
| Search | `search_site` | The release's index |
| Listings | `list_products`, `list_notices`, `list_events`, `get_schedule`, `practice_information` | The published items of the matching kind |
| Status | `lookup_status` | An application's status by its reference |

`SiteEdge::build` serves only the capabilities a handler covers; any other name in the kind's
declarations is logged and left unserved. A capability of your own runs on an edge you build
with `CapabilityRegistry`, as above.

## Checking a pack

| Check | How |
|-------|-----|
| The declarations are visible to the right tiers | `CapabilityRegistry::manifest_for(tier)`, or `GET /awp/v2/manifest` with and without a credential |
| The bindings agree | The same call over `POST /awp/v2/capabilities/{name}`, A2A `SendMessage` and MCP `tools/call` returns the same outcome, and a repeated key returns the first one |
| The deployment conforms | `adk-rust awp conformance http://127.0.0.1:8080 --active` runs the edge's `GET` checks against a deployment on loopback (A-I9) |
| The scenarios | S6 (identity, not claims) and S9 (agent-to-agent business) in `examples/website`. S9 reports a gap on `main`: no default pack capability carries an amount threshold, so a large order is never held |

```bash
cargo run --manifest-path examples/website/Cargo.toml -- S6 S9
```

## Related

- [The AWP agent edge](awp-edge.md)
- [Agentic Web Protocol (AWP) 2.0](awp.md)
- [The AWP website product](adk-website.md)
- [MCP Server](../mcp/server.md)
