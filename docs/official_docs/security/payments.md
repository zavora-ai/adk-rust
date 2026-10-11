# Payments and Commerce

`adk-payments` adds protocol-neutral commerce orchestration to ADK-Rust. It is
designed for agentic commerce flows that need one durable transaction model
across multiple protocol adapters without leaking raw payment artifacts into
conversation history or semantic memory.

## Support Levels

- ACP stable `2026-01-30`: the production-oriented baseline for checkout, completion, cancelation, delegated payment, and order follow-up.
- ACP experimental: feature-gated discovery, delegate-authentication, and webhook extensions. Treat these routes as compatibility surfaces for evolving ACP drafts.
- AP2 `v0.1-alpha`: typed mandate, payment, receipt, intervention, A2A, and MCP-safe support for the current alpha protocol shape.

## Primary Journeys

- ACP human-present checkout with delegated payment followed by merchant or webhook order updates.
- AP2 human-present shopper, merchant, credentials-provider, and payment-processor coordination.
- AP2 human-not-present intent execution with either autonomous completion or explicit buyer reconfirmation.
- Dual-protocol merchant deployments where ACP and AP2 correlate into the same canonical transaction ID and journal.
- Post-compaction recall where the durable journal and masked memory survive transcript loss.

## Security and Durability

- Raw mandates, signatures, delegated credentials, and receipt payloads stay in evidence storage backed by `adk-artifact`.
- Durable transaction state lives in `adk-session` via the structured journal, not in fragile conversation-only context.
- Semantic recall uses masked summaries through `adk-memory`.
- `adk-auth` binds request identity, tenant scope, and audit metadata.
- `GovernedCheckoutService` evaluates a `PaymentPolicySet` (amount, merchant, currency, protocol-version, intervention, and spend policies) before every checkout creation and completion; the payment tools apply it automatically.
- `Ap2Adapter` evaluates the same policies, reserves spend, and records audit events before an AP2 cart mandate creates a checkout and before a payment mandate reaches the payment backend.
- Redaction guardrails mask card numbers and personal data in tool outputs and telemetry.

## Verification Defaults

Both protocol surfaces fail closed: a profile that claims verification without a
configured verifier rejects requests instead of accepting them.

| Surface | Default | Production setting | Development setting |
|---------|---------|--------------------|---------------------|
| AP2 merchant authorization | Rejected (`AuthorizationVerifierNotConfigured`) | `Ap2Adapter::with_merchant_authorization_verifier` | `Ap2Adapter::allow_unverified_authorizations()` |
| AP2 user authorization | Rejected (`AuthorizationVerifierNotConfigured`) | `Ap2Adapter::with_user_authorization_verifier` | `Ap2Adapter::allow_unverified_authorizations()` |
| AP2 payment mandate replay | Refused (`PaymentMandateReplayed`, `TransactionAlreadyPaid`) | `with_payment_mandate_ledger` over shared storage | `InMemoryPaymentMandateLedger` (default) |
| ACP request signatures | Not required (`permissive()`) | `AcpVerificationConfig::strict().with_signature_verifier(..)` | `AcpVerificationConfig::permissive()` |

- **AP2 verifiers** check each artifact cryptographically against the mandate
  contents and compare signatures in constant time. A merchant verifier that sets
  `VERIFIED_MERCHANT_NAME_CLAIM` makes the adapter reject carts whose
  `merchant_name` differs from the verified signer, so intent-mandate merchant
  allow-lists apply to the verified identity.
- **`allow_unverified_authorizations()`** accepts any non-empty artifact and logs
  a warning when enabled. It exists for local development and tests only.
- **Human-not-present intents** need merchant or SKU constraints; a refundability
  requirement alone does not bound the agent's authority.
- **ACP `strict()`** requires a verified `Signature`, a `Timestamp` within five
  minutes, and an `Idempotency-Key` on every POST. `AcpRouterBuilder::build`
  fails when a profile requires signatures but has no `DetachedSignatureVerifier`.

## Payment Policies

`GovernedCheckoutService` wraps any `MerchantCheckoutService` and evaluates its
`PaymentPolicySet` on every checkout creation and completion. Completion is evaluated
against the stored checkout, so a cart changed after creation is judged as it will be paid.

| Outcome | Effect | Error code |
|---------|--------|------------|
| Allow | The backend runs; spend holds are committed on success and released on failure | — |
| Escalate | The caller's `PaymentApprover` decides | `payments.policy.approval_required` (no decision), `payments.policy.approval_denied` |
| Deny | The backend is not reached | `payments.policy.denied` |

`PaymentToolsetBuilder` governs its create and complete tools with the policies passed to
`with_payment_policies`. An escalation goes through the run's tool confirmation flow: a
static decision for the call ID in `RunConfig::tool_confirmation_decisions` answers first,
then `RunConfig::tool_confirmation_handler`. Without either, the call fails with
`payments.policy.approval_required` and its event carries `actions.tool_confirmation`.

```rust
use adk_payments::guardrail::{
    AmountThresholdGuardrail, MerchantAllowlistGuardrail, PaymentPolicySet, SpendLimitGuardrail,
};
use adk_payments::tools::PaymentToolsetBuilder;

let toolset = PaymentToolsetBuilder::new(checkout_service, transaction_store)
    .with_payment_policies(
        PaymentPolicySet::new()
            // Review above 50 USD, refuse above 500 USD.
            .with(AmountThresholdGuardrail::new(Some(5_000), Some(50_000)).with_currency("USD", 2))
            .with(MerchantAllowlistGuardrail::new(["merchant-1", "merchant-2"]))
            // Refuse completion when no spend ledger is configured.
            .with(SpendLimitGuardrail::new()),
    )
    .build();
```

`SpendLimitGuardrail` reserves each checkout total in the [spend ledger](spend-ledger.md)
under `app / agent / merchant_id` when the checkout completes. The ledger is the one passed
to `PaymentToolsetBuilder::with_spend_ledger`, or else `RunConfig::spend_ledger`. A refused
reservation and an unreachable ledger both deny the payment. When the policy set has no
spend guardrail, the toolset adds `SpendLimitGuardrail::when_configured()`, which reserves
only when a ledger is configured. Only USD totals can be reserved; other currencies are
denied.

For ACP traffic, hand the ACP adapter a `GovernedCheckoutService`. The adapters supply
no approver, so an escalated protocol request is refused rather than completed.

`GovernedCheckoutService::with_auditor` records each allowed decision with a
`PaymentAuditor` before the backend is reached; when the audit sink fails, the
operation is refused with `payments.auth.audit_failed`. Refusals are recorded on a
best-effort basis.

## AP2 Payment Governance

`Ap2Adapter` routes AP2 money movement through the same governance as a governed
checkout.

| Entry point | Moves or authorizes money | Governance |
|-------------|---------------------------|------------|
| `submit_intent_mandate` | Authorizes a later human-not-present payment | User authorization verified when present (required human-not-present); no amount to evaluate |
| `submit_cart_mandate` (new checkout) | Sets the amount to be paid | Merchant authorization verified; payment policies (`CreateCheckout`) |
| `submit_payment_mandate`, `submit_payment_mandate_as` | Executes the payment | Verification, payment policies (`ExecutePayment`), spend reservation, approval, audit, single-use mandate, idempotency key |
| `apply_payment_receipt` | Records the processor's outcome | Bound to the executed payment mandate |
| `payments_ap2_payment_submit` tool | Executes the payment | As `submit_payment_mandate_as`, plus scope guard, `ToolEffect::NonIdempotent`, and the action ledger |

A payment mandate is processed in this order:

1. The user authorization is verified. A human-not-present payment without one
   requires an intent mandate whose authorization was verified.
2. The payment policies evaluate the stored transaction at the mandate total. A
   denial fails with `payments.policy.denied`; an escalation goes to the caller's
   `PaymentApprover` and fails with `payments.policy.approval_required` without one.
3. `SpendLimitGuardrail` reserves the total in the spend ledger — the adapter's
   (`with_spend_ledger`) or else the caller's — under `app / agent / merchant_id`.
4. With `with_payment_auditor`, the decision is recorded; a failed write releases the
   reservation and fails with `payments.auth.audit_failed`.
5. The mandate is consumed in the `PaymentMandateLedger`, so a refusal in steps 1 to 4
   leaves it available for resubmission.
6. The payment backend runs with an `idempotency_key` extension field: the caller's key
   (the tool's call key) or `ap2:payment_mandate:<payment_mandate_id>`.
7. The reservation is committed when the outcome is `Authorized` or `Completed` and
   released otherwise.

`PaymentToolsetBuilder::with_ap2_adapter` adds the `payments_ap2_payment_submit` tool
under the toolset's scope guard (`payments:payment:execute`). The tool declares
`ToolEffect::NonIdempotent`: an agent never retries it, and with
`RunConfig::action_ledger` a timed-out payment is answered `outcome_unknown` on retry
or resume instead of paying twice. An escalation goes through the run's tool
confirmation flow. With `ap2-mcp`, the tool response includes the
`Ap2McpPaymentContinuation` view. A2A carries mandates as data
(`Ap2A2aMessage::extract_payment_mandate`); submitting the extracted mandate through the
adapter applies the same governance.

```rust,ignore
use adk_payments::auth::PaymentAuditor;
use adk_payments::guardrail::{AmountThresholdGuardrail, PaymentPolicySet};
use adk_payments::protocol::ap2::Ap2Adapter;
use adk_payments::tools::PaymentToolsetBuilder;
use std::sync::Arc;

let adapter = Arc::new(
    Ap2Adapter::new(checkout_service, payment_service, transaction_store.clone(), evidence_store)
        .with_merchant_authorization_verifier(merchant_verifier)
        .with_user_authorization_verifier(user_verifier)
        .with_payment_policies(
            PaymentPolicySet::new()
                .with(AmountThresholdGuardrail::new(Some(5_000), Some(50_000)).with_currency("USD", 2)),
        )
        .with_spend_ledger(spend_ledger)
        .with_payment_auditor(PaymentAuditor::new(audit_sink)),
);
let toolset = PaymentToolsetBuilder::new(merchant_checkout, transaction_store)
    .with_ap2_adapter(adapter)
    .build();
```

## Amounts

Amounts are stored as `Money { currency, amount_minor, scale }` and converted
without rounding.

- AP2 amounts use the currency's ISO 4217 minor-unit scale (`JPY` 0, `USD` 2,
  `KWD` 3), raised to the finest scale present so that every amount in one cart
  shares one scale. Malformed values, mixed currencies, more than 18 fraction
  digits, and overflow return `Ap2Error::InvalidAmount`.
- ACP delegated-payment allowances use the currency's minor-unit scale.
- `AmountThresholdGuardrail` compares totals by value across scales. Without
  `with_currency`, its thresholds are minor units of the transaction currency;
  `with_currency(currency, scale)` binds them to one currency and denies the others.

## Validation Path

Use the integration tests as the authoritative end-to-end validation path:

```bash
cargo test -p adk-payments --features acp --test acp_integration_tests
cargo test -p adk-payments --features ap2,ap2-a2a,ap2-mcp --test ap2_integration_tests
cargo test -p adk-payments --features ap2,ap2-a2a,ap2-mcp --test ap2_governance_tests
cargo test -p adk-payments --test cross_protocol_correlation_tests
cargo test -p adk-payments --features acp-experimental --test acp_experimental_integration_tests
```

Reference files:

- `adk-payments/tests/acp_integration_tests.rs`
- `adk-payments/tests/ap2_integration_tests.rs`
- `adk-payments/tests/ap2_governance_tests.rs`
- `adk-payments/tests/cross_protocol_correlation_tests.rs`
- `examples/payments/README.md`

The example crate under `examples/payments/` is the scenario index for the
supported journeys, while the integration tests are the executable ground
truth for protocol, journal, and evidence behavior.
