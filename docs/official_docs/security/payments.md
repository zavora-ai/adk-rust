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
