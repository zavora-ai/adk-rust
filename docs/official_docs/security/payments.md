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
- `adk-guardrail` applies amount, merchant, protocol-version, intervention, and redaction policy before state is persisted.

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
cargo test -p adk-payments --test cross_protocol_correlation_tests
cargo test -p adk-payments --features acp-experimental --test acp_experimental_integration_tests
```

Reference files:

- `adk-payments/tests/acp_integration_tests.rs`
- `adk-payments/tests/ap2_integration_tests.rs`
- `adk-payments/tests/cross_protocol_correlation_tests.rs`
- `examples/payments/README.md`

The example crate under `examples/payments/` is the scenario index for the
supported journeys, while the integration tests are the executable ground
truth for protocol, journal, and evidence behavior.
