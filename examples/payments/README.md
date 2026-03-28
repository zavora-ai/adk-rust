# Payments Example Index

This crate is the local scenario index for the `adk-payments` end-to-end
journeys.

The executable ground truth lives in the integration tests:

- `adk-payments/tests/acp_integration_tests.rs`
- `adk-payments/tests/ap2_integration_tests.rs`
- `adk-payments/tests/cross_protocol_correlation_tests.rs`
- `adk-payments/tests/acp_experimental_integration_tests.rs`

Available scenarios:

- `acp-human-present` — ACP checkout, delegated payment, completion, and order follow-up.
- `ap2-human-present` — shopper, merchant, credentials-provider, and payment-processor journey.
- `ap2-human-not-present` — intent mandate with autonomous completion or buyer reconfirmation.
- `dual-protocol` — ACP and AP2 adapters sharing one canonical transaction model.
- `post-compaction-recall` — durable transaction recall after transcript compaction.

List scenarios:

```bash
cargo run --manifest-path examples/payments/Cargo.toml -- list
```

Show one scenario summary:

```bash
cargo run --manifest-path examples/payments/Cargo.toml -- show ap2-human-present
```
