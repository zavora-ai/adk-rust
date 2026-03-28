# Implementation Plan: adk-payments

## Overview

This plan introduces `adk-payments` in phases. The order intentionally follows protocol maturity and security dependency:

1. build the canonical commerce kernel and durable transaction journal
2. integrate auth, guardrails, and redaction
3. implement ACP stable support
4. add compaction-safe memory and evidence storage
5. add ACP experimental surfaces
6. add AP2 alpha support
7. add end-to-end integration harnesses and agentic example apps

The plan assumes `adk-payments` is a new publishable workspace crate and that the initial rollout keeps payments support opt-in.

## Tasks

- [x] 1. Create the `adk-payments` crate and workspace feature plumbing
  - [x] 1.1 Add `adk-payments` as a workspace member and workspace dependency in the root `Cargo.toml`.
    - Add an additive `payments` feature and optional dependency wiring in [adk-rust/Cargo.toml](/Users/jameskaranja/Developer/projects/adk-rust/adk-rust/Cargo.toml).
    - Keep `payments` opt-in and out of the default `standard` preset for the initial rollout.
    - _Requirements: 1.1, 1.2, 1.3, 1.6_
  - [x] 1.2 Scaffold crate modules for `domain`, `kernel`, `journal`, `auth`, `guardrail`, `protocol::acp`, `protocol::ap2`, `tools`, and `server`.
    - Add rustdoc module docs that state the ACP and AP2 baselines explicitly.
    - _Requirements: 1.1, 1.4, 1.5, 12.2_
  - [x] 1.3 Define additive crate feature flags for `acp`, `acp-experimental`, `ap2`, `ap2-a2a`, and `ap2-mcp`.
    - _Requirements: 1.4, 1.5, 5.1, 6.1_

- [x] 2. Implement the canonical commerce kernel
  - [x] 2.1 Add protocol-neutral domain types for money, actors, fulfillment, interventions, authorization mode, order state, receipt state, and merchant-of-record data.
    - Model Human Present and Human Not Present authorization as distinct modes.
    - Preserve protocol extension fields rather than discarding them.
    - _Requirements: 2.1, 2.2, 2.3, 2.4, 2.5, 2.6_
  - [x] 2.2 Add backend-facing service traits for checkout operations, payment execution, intervention handling, transaction storage, and evidence storage.
    - Design traits so ACP and AP2 adapters call the same kernel-facing surface.
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5_
  - [x] 2.3 Add unit tests for canonical state transitions and lossless protocol-extension retention.
    - _Requirements: 2.5, 2.6, 13.1_

- [x] 3. Implement the durable transaction journal and evidence store
  - [x] 3.1 Add `TransactionRecord`, `ProtocolRefs`, `SafeTransactionSummary`, and `ProtocolEnvelopeDigest` keyed by session identity plus internal transaction ID.
    - Correlate ACP checkout IDs, ACP order IDs, AP2 mandate IDs, AP2 receipt IDs, and local backend IDs.
    - _Requirements: 7.1, 7.2, 7.5, 10.1, 10.2_
  - [x] 3.2 Back structured transaction state with app-scoped session state and back raw protocol artifacts with `adk-artifact`.
    - Store safe memory summaries separately from raw evidence.
    - _Requirements: 9.4, 10.1, 10.6, 10.7_
  - [x] 3.3 Write only masked payment summaries into transcript-visible outputs and semantic memory.
    - Keep raw payloads, signatures, and authorizations out of transcript and memory.
    - _Requirements: 9.3, 9.4, 9.5, 10.4, 13.6_
  - [x] 3.4 Add regression tests proving transaction recall still works after runner compaction.
    - _Requirements: 10.3, 10.5, 10.6, 13.5, 15.4_

- [x] 4. Integrate payment auth scopes, actor binding, and audit sinks
  - [x] 4.1 Define payment-specific scope constants and helpers for scope-protected tools and endpoints.
    - Cover checkout mutation, delegated credential usage, intervention continuation, order update, and administrative operations.
    - _Requirements: 8.1, 8.2_
  - [x] 4.2 Add payment audit helpers using `adk-auth::AuditSink`.
    - Include actor identity, transaction ID, protocol, operation, and outcome metadata.
    - _Requirements: 8.3, 8.6_
  - [x] 4.3 Add identity-conflict checks so authenticated request identity cannot silently rebind a transaction or tenant.
    - _Requirements: 8.4, 8.6_
  - [x] 4.4 Add tests for scope denial, audit emission, and tenant isolation.
    - _Requirements: 8.1, 8.3, 8.4, 13.1_

- [x] 5. Integrate payment guardrails and sensitive-data redaction
  - [x] 5.1 Add payment-specific guardrails for amount thresholds, merchant allowlists, currency policy, intervention policy, and protocol-version policy.
    - _Requirements: 9.1, 9.2, 9.6_
  - [x] 5.2 Add masking and redaction helpers for CHD, billing PII, and signed authorization artifacts across transcript, memory, tool output, and telemetry.
    - _Requirements: 9.3, 9.4, 9.5, 13.6_
  - [x] 5.3 Add tests proving sensitive data stays out of transcript content, semantic memory, and telemetry payloads.
    - _Requirements: 9.3, 9.4, 13.1, 13.6_

- [x] 6. Implement the ACP stable adapter (`2026-01-30`)
  - [x] 6.1 Add ACP stable types and mappers for checkout sessions, totals, fulfillment options, interventions, payment handlers, affiliate attribution, delegated payment, and orders.
    - _Requirements: 4.1, 4.2, 4.3, 4.4_
  - [x] 6.2 Add ACP server builders or routers for create, update, retrieve, complete, cancel, and delegated payment flows.
    - _Requirements: 4.2, 4.3, 11.3_
  - [x] 6.3 Add ACP request verification for API version, optional detached signature or timestamp validation, and configurable idempotency replay behavior.
    - Support strict production mode that requires `Idempotency-Key` on all ACP POST operations.
    - _Requirements: 4.5, 4.6, 4.7, 4.8_
  - [x] 6.4 Add contract tests against ACP stable OpenAPI, JSON Schema, and example fixtures.
    - _Requirements: 4.1, 4.2, 4.3, 13.2_
  - [x] 6.5 Add an end-to-end ACP human-present integration test covering create, update, complete, and order-update synchronization.
    - Exercise the Commerce_Kernel, ACP adapter, journal, evidence storage, and masked tool-facing outputs together.
    - _Requirements: 14.1, 14.4, 15.1, 15.6_

- [x] 7. Implement ACP experimental surfaces behind `acp-experimental`
  - [x] 7.1 Add `/.well-known/acp.json` discovery document support.
    - _Requirements: 5.1, 5.2_
  - [x] 7.2 Add `Merchant-Signature` webhook verification and order event ingestion using the full order object.
    - _Requirements: 5.1, 5.3_
  - [x] 7.3 Add delegated authentication lifecycle models and routing for browser-based interventions such as 3DS2.
    - _Requirements: 5.1, 5.4, 9.6_
  - [x] 7.4 Add tests proving experimental ACP routes and types are absent when the feature flag is disabled.
    - _Requirements: 5.5, 5.6, 13.3_

- [x] 8. Implement the AP2 alpha adapter
  - [x] 8.1 Add typed AP2 role, mandate, payment request, payment response, and receipt models aligned to the alpha baseline.
    - _Requirements: 6.1, 6.2, 6.3_
  - [x] 8.2 Add pluggable verification for merchant authorization and user authorization artifacts.
    - Persist verified artifacts as evidence records.
    - _Requirements: 6.7, 9.4_
  - [x] 8.3 Add `ap2-a2a` support for AgentCard extension metadata and A2A message or artifact containers.
    - _Requirements: 6.4, 6.5_
  - [x] 8.4 Add `ap2-mcp` support for MCP-compatible wrappers that expose safe mandate and payment orchestration surfaces.
    - _Requirements: 6.6, 11.5_
  - [x] 8.5 Add tests using AP2 mandate and receipt fixtures and A2A extension examples.
    - _Requirements: 6.1, 6.2, 6.4, 6.5, 13.4_
  - [x] 8.6 Add end-to-end AP2 integration tests for:
    - a human-present shopper -> credentials-provider -> merchant -> payment-processor journey
    - a human-not-present intent journey with autonomous completion or forced user return
    - _Requirements: 14.2, 14.3, 14.5, 15.2, 15.3_

- [-] 9. Implement kernel-mediated cross-protocol correlation
  - [x] 9.1 Route ACP and AP2 adapters through the same canonical transaction ID and journal model.
    - _Requirements: 7.1, 7.2, 7.3_
  - [x] 9.2 Add best-effort canonical projections where ACP or AP2 data can be mapped safely without semantic loss.
    - _Requirements: 2.5, 7.3_
  - [x] 9.3 Return explicit unsupported or policy errors where direct ACP-to-AP2 or AP2-to-ACP conversion would be lossy.
    - _Requirements: 3.5, 7.4_
  - [x] 9.4 Add regression tests for a merchant backend serving both ACP and AP2 adapters in one deployment.
    - _Requirements: 7.3, 7.4, 13.1, 14.6, 15.5_

- [x] 10. Add tools, server integration, docs, and examples
  - [x] 10.1 Add tool builders or toolsets for checkout create, checkout update, completion, cancelation, status lookup, and intervention continuation.
    - Use masked structured outputs only.
    - _Requirements: 11.1, 11.2, 11.6_
  - [x] 10.2 Wire ACP and AP2 server helpers into `adk-server` and expose optional payment tool integration for `adk-tool`.
    - _Requirements: 11.3, 11.4, 11.5_
  - [x] 10.3 Add agentic end-to-end examples for:
    - an ACP merchant checkout backend with delegated payment and order updates
    - an AP2 shopper + merchant + credentials-provider human-present flow
    - an AP2 human-not-present intent flow with return-to-user intervention
    - a dual-protocol merchant backend exposing ACP and AP2 adapters against one canonical commerce kernel
    - a post-compaction transaction recall or order-follow-up flow
    - _Requirements: 12.3, 12.4, 14.1, 14.2, 14.3, 14.6, 15.5, 15.6_
  - [x] 10.4 Add an integration-test harness crate or test support module that can stand up mock shopper, merchant, credentials-provider, payment-processor, and webhook actors for end-to-end journeys.
    - Use the harness for ACP and AP2 multi-actor tests instead of isolated protocol mocks.
    - _Requirements: 14.1, 14.2, 14.3, 15.1, 15.2, 15.3, 15.7_
  - [x] 10.5 Update crate READMEs, official docs, and `CHANGELOG.md`.
    - Document the primary agentic commerce user journeys and point operators to the end-to-end examples and integration tests as the reference validation path.
    - Explain stable ACP vs experimental ACP vs AP2 alpha support explicitly.
    - _Requirements: 12.1, 12.2, 12.3, 12.5, 12.6, 15.7_

- [x] 11. Verification
  - [x] 11.1 Run `cargo fmt --all`.
  - [x] 11.2 Run targeted tests for `adk-payments` and touched integration crates such as `adk-auth`, `adk-guardrail`, `adk-memory`, `adk-session`, and `adk-server`.
  - [x] 11.3 Run contract, integration, and property tests for ACP replay safety, webhook signing, AP2 mandate handling, end-to-end shopper journeys, redaction, and compaction durability.
  - [x] 11.4 Run `cargo clippy --workspace --all-targets -- -D warnings`.
