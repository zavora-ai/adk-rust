# adk-payments

Protocol-neutral agentic commerce and payment orchestration for ADK-Rust.

`adk-payments` gives you one canonical commerce kernel and durable transaction
journal that can sit behind multiple protocol adapters. It is built for cases
where an agent needs to create, update, authorize, complete, and follow up on
real commerce transactions without leaking raw payment artifacts into normal
conversation state.

## Overview

Use `adk-payments` when you need to:

- expose merchant checkout or delegated-payment APIs over ACP
- handle AP2 mandate-based shopper flows with explicit authorization artifacts
- keep one canonical transaction record even when ACP and AP2 are both enabled
- store raw mandates, signatures, tokens, and receipts as evidence instead of transcript text
- preserve transaction continuity after context compaction through durable journal state plus masked memory

The crate combines:

- a **canonical commerce domain model** in `domain`
- backend-facing **kernel service traits** in `kernel`
- a durable **journal + evidence layer** in `journal`
- protocol adapters in `protocol::acp` and `protocol::ap2`
- agent-facing payment tools in `tools`
- server wiring helpers in `server`

## Which Protocol Should I Use?

- Start with **ACP stable** if you want merchant-hosted HTTP checkout routes and delegated payment support.
- Use **ACP experimental** only when you need the evolving discovery, delegate-authentication, or webhook extensions and can tolerate draft churn.
- Use **AP2** if your flow is mandate-based and involves multiple commerce actors such as shopper, merchant, credentials-provider, and payment-processor.
- Run **both ACP and AP2** if you need standards compatibility on multiple fronts but want one backend transaction model.

## Protocol Support

| Feature | Status | What it enables |
|---------|--------|-----------------|
| `acp` | Stable baseline | ACP `2026-01-30` checkout routes, delegated payment, completion, cancelation, status |
| `acp-experimental` | Draft / feature-gated | ACP discovery, delegate-authentication, signed webhooks |
| `ap2` | Alpha baseline | AP2 `v0.1-alpha` mandates, payment execution, receipts, interventions |
| `ap2-a2a` | Additive | AP2 A2A-oriented helpers and AgentCard metadata |
| `ap2-mcp` | Additive | AP2 MCP-safe continuation, intervention, and receipt views |

ACP stable is the conservative production baseline. AP2 support tracks the
current alpha research draft. The experimental ACP surface is intentionally
feature-gated because the wire format is still moving.

## Installation

Choose only the features you need:

```toml
[dependencies]
adk-payments = { version = "0.6.0", features = ["acp"] }
```

```toml
[dependencies]
adk-payments = { version = "0.6.0", features = ["ap2", "ap2-a2a", "ap2-mcp"] }
```

```toml
[dependencies]
adk-payments = { version = "0.6.0", features = ["acp", "acp-experimental", "ap2"] }
```

## Core Concepts

### Canonical Transaction

ACP sessions, ACP orders, AP2 mandates, and AP2 receipts all correlate to one
internal `TransactionId`. Protocol IDs are preserved in `ProtocolRefs`; they
are not treated as interchangeable.

### Durable Journal

The transaction journal stores canonical commerce state in `adk-session`. Safe,
masked transaction summaries are indexed into `adk-memory`. Raw sensitive
payloads live in `adk-artifact` through the evidence store.

### Evidence First

Raw payment credentials, merchant signatures, user authorizations, mandate
payloads, and receipt bodies are preserved as immutable evidence artifacts.
Only safe summary metadata is kept in transcript-visible and memory-visible
surfaces.

### Kernel-Mediated Compatibility

ACP and AP2 can share a backend, but `adk-payments` does not pretend their
artifacts are semantically equivalent. When a direct ACP-to-AP2 or AP2-to-ACP
conversion would lose provenance or authorization semantics, the kernel returns
an explicit unsupported error instead of fabricating a translation.

## Typical Architecture

```text
Agent / Tool / Server surface
        |
        v
   ACP router or AP2 adapter
        |
        v
Canonical commerce kernel traits
        |
        +--> transaction journal (adk-session)
        +--> evidence store (adk-artifact)
        +--> masked recall index (adk-memory)
        +--> auth + guardrail enforcement
```

## Quick Start

### 1. Expose ACP Checkout Routes

```rust,ignore
use std::sync::Arc;

use adk_payments::domain::{CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolExtensions};
use adk_payments::protocol::acp::{AcpContextTemplate, AcpRouterBuilder, AcpVerificationConfig};

let router = AcpRouterBuilder::new(AcpContextTemplate {
    session_identity: Some(identity),
    actor: CommerceActor {
        actor_id: "shopper-agent".to_string(),
        role: CommerceActorRole::AgentSurface,
        display_name: Some("shopper".to_string()),
        tenant_id: Some("tenant-1".to_string()),
        extensions: ProtocolExtensions::default(),
    },
    merchant_of_record: MerchantRef {
        merchant_id: "merchant-123".to_string(),
        legal_name: "Merchant Example LLC".to_string(),
        display_name: Some("Merchant Example".to_string()),
        statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
        country_code: Some("US".to_string()),
        website: Some("https://merchant.example".to_string()),
        extensions: ProtocolExtensions::default(),
    },
    payment_processor: None,
    mode: CommerceMode::HumanPresent,
})
.with_merchant_checkout_service(checkout_service)
.with_delegated_payment_service(delegated_payment_service)
.with_verification(AcpVerificationConfig::strict())
.build()?;
```

Use this route surface when you want HTTP endpoints for checkout session create,
update, completion, cancelation, status lookup, and delegated payment.

### 2. Handle AP2 Mandate Flows

```rust,ignore
use adk_payments::protocol::ap2::Ap2Adapter;

let adapter = Ap2Adapter::new(
    checkout_service,
    payment_execution_service,
    transaction_store,
    evidence_store,
)
.with_intervention_service(intervention_service);

let record = adapter.submit_cart_mandate(context, cart_mandate).await?;
let payment = adapter.submit_payment_mandate(context, payment_mandate).await?;
let final_record = adapter.apply_payment_receipt(context, payment_receipt).await?;
```

Use AP2 when your flow is mandate-based and multiple specialized actors
participate in authorization and settlement.

### 3. Register Agent-Facing Payment Tools

```rust,ignore
use adk_payments::tools::PaymentToolsetBuilder;

let toolset = PaymentToolsetBuilder::new(checkout_service, transaction_store)
    .with_intervention_service(intervention_service)
    .build();

let tools = toolset.tools();
```

These tools return masked transaction summaries only. Raw card data, signatures,
and authorization blobs do not appear in tool outputs.

## Primary Journeys

The crate is currently shaped around five first-class journeys:

- ACP human-present checkout with delegated payment and post-order updates
- AP2 human-present shopper, merchant, credentials-provider, and payment-processor flow
- AP2 human-not-present intent flow with either autonomous completion or forced return-to-user intervention
- dual-protocol backends serving ACP and AP2 against one canonical transaction journal
- post-compaction recall where durable transaction state survives loss of conversational detail

## Security Model

- Raw payment credentials, merchant signatures, user authorizations, and receipt artifacts are stored as evidence.
- Session state and semantic memory retain only masked transaction summaries.
- `adk-auth` binds requests to session identity and audit metadata.
- `adk-guardrail` enforces amount, merchant, intervention, and protocol-policy checks before persistence.
- The kernel refuses lossy protocol-to-protocol conversions rather than approximating semantics.

## Validation and Examples

The integration tests are the authoritative executable reference path:

```bash
cargo test -p adk-payments --features acp --test acp_integration_tests
cargo test -p adk-payments --features ap2,ap2-a2a,ap2-mcp --test ap2_integration_tests
cargo test -p adk-payments --test cross_protocol_correlation_tests
cargo test -p adk-payments --features acp-experimental --test acp_experimental_integration_tests
```

Use these as the starting point for real implementations:

- `adk-payments/tests/acp_integration_tests.rs`
- `adk-payments/tests/ap2_integration_tests.rs`
- `adk-payments/tests/cross_protocol_correlation_tests.rs`
- `examples/payments/`
- `docs/official_docs/security/payments.md`

## Related Modules

- `domain` - canonical commerce types
- `kernel` - protocol-neutral commands, service traits, correlation, and errors
- `journal` - durable transaction and evidence persistence
- `protocol::acp` - ACP server and verification surfaces
- `protocol::ap2` - AP2 adapter, verification, A2A, and MCP surfaces
- `tools` - agent-facing checkout, status, and intervention tools
- `server` - ACP router re-exports and AP2 integration notes
