# Requirements Document: adk-payments

## Introduction

This feature introduces `adk-payments`, a new ADK-Rust crate for serious agentic commerce and payments workflows. The immediate goal is not to invent a third protocol. The immediate goal is to give ADK-Rust one secure, protocol-neutral commerce kernel that can support:

- the OpenAI / Stripe Agentic Commerce Protocol (ACP) merchant and PSP flows
- the Agent Payments Protocol (AP2) mandate and role model over A2A and MCP

As of March 22, 2026, the protocol baselines verified for this specification are:

- **ACP stable baseline:** `2026-01-30`
- **ACP experimental baseline:** unreleased additions for discovery, stricter idempotency, webhook signing, and delegated authentication
- **AP2 baseline:** repository and docs describing `v0.1-alpha`, with an A2A extension URI rooted at `https://github.com/google-agentic-commerce/ap2/tree/v0.1`

The design must treat those baselines honestly. ACP is the more mature merchant-facing HTTP contract. AP2 is earlier-stage and defines a trust and authorization layer built around roles, signed mandates, and A2A or MCP transport bindings. The implementation therefore must not force a lossy, fake wire-level unification. Instead, the implementation must define one canonical commerce kernel and expose ACP and AP2 as adapters on top of that kernel.

This scope is intentionally broader than transport parsing. The feature must integrate with:

- `adk-auth` for scopes, actor binding, and auditability
- `adk-guardrail` for payment policy, redaction, and intervention gating
- `adk-session`, `adk-artifact`, and `adk-memory` for durable transaction memory that survives context compaction

## Glossary

- **ACP**: The Agentic Commerce Protocol maintained by OpenAI and Stripe, centered on merchant-facing REST contracts such as checkout sessions, delegated payment, and order webhooks.
- **ACP_Stable_Baseline**: The ACP snapshot dated `2026-01-30`.
- **ACP_Experimental_Baseline**: Unreleased ACP additions for discovery, stricter idempotency, Stripe-style webhook signing, and delegated authentication.
- **AP2**: The Agent Payments Protocol, an open payments protocol built as an extension for A2A and MCP.
- **AP2_Alpha_Baseline**: The AP2 documentation and repository state describing `v0.1-alpha` as of March 22, 2026.
- **Commerce_Kernel**: The protocol-neutral `adk-payments` core that owns transaction state, evidence references, policy enforcement, and backend-facing service traits.
- **Protocol_Adapter**: A protocol-specific edge layer that translates ACP or AP2 messages into Commerce_Kernel commands and translates kernel results back into protocol-native responses.
- **Merchant_Backend**: The business logic and storage that determine cart contents, totals, fulfillment, order completion, refunds, and payment execution.
- **Transaction_Journal**: Durable structured payment state stored separately from conversation history so payment continuity does not depend on replaying chat events.
- **Evidence_Record**: A durable reference to raw protocol material such as ACP request bodies, webhook payloads, detached signatures, CartMandates, PaymentMandates, or receipts.
- **Safe_Transaction_Summary**: A redacted transaction summary suitable for session history, semantic memory, and user-facing tool results.
- **Sensitive_Payment_Data**: Cardholder data, CVC values, full PAN values, cryptographic authorization artifacts, unredacted billing PII, or any raw payload that would expand PCI or privacy exposure if stored in transcript or memory.
- **Human_Present_Transaction**: A payment flow where the user is available for final authorization or intervention.
- **Human_Not_Present_Transaction**: A payment flow where the user has pre-authorized constraints and the agent may act later within those constraints.
- **Intervention**: A user challenge or additional verification step such as 3DS, biometric confirmation, or address verification.
- **Merchant_Of_Record**: The merchant that sells the goods or services, appears on the payment statement, and owns refunds, disputes, and compliance obligations.

## Primary Agentic Commerce User Stories

These user stories describe how agentic commerce should happen with `adk-payments` at the product level, not only at the protocol-plumbing level.

1. **Human-present ACP checkout**
   - As a user shopping through an agent surface, I want the agent to build a merchant-backed cart, collect shipping and payment details, complete the transaction through the merchant's own PSP, and keep the order synchronized afterward without making the agent surface the merchant of record.
2. **Human-present AP2 mandate flow**
   - As a user shopping through a multi-agent surface, I want the shopping agent, credentials provider, merchant, and payment processor to exchange signed mandates and complete a purchase with verifiable proof of my authorization.
3. **Human-not-present AP2 intent flow**
   - As a user delegating future buying authority, I want to authorize intent with clear constraints such as merchant, amount, product class, and expiry so that the agent can act later without exceeding my authority.
4. **Shared backend dual-protocol support**
   - As a commerce platform operator, I want one merchant backend to support both ACP and AP2 entry points so that the same catalog, pricing, fulfillment, payment, and order systems serve multiple agent ecosystems.
5. **Compaction-safe transaction continuity**
   - As a returning user or support agent, I want the system to recall active and completed transactions correctly after long sessions and compaction, so that payment continuity does not depend on replaying old chat turns.
6. **Auditable and policy-controlled payments**
   - As a security or compliance operator, I want every sensitive payment step to be authorized, audited, redacted, and policy-checked so that serious commerce use does not leak sensitive payment material into general agent state.

## Requirements

### Requirement 1: New Workspace Crate and Feature Gating

**User Story:** As an ADK-Rust maintainer, I want payments support to be a dedicated publishable crate with explicit feature flags, so that commerce support is reusable without bloating minimal builds.

#### Acceptance Criteria

1. THE workspace SHALL add a publishable crate named `adk-payments`.
2. THE `adk-rust` umbrella crate SHALL add an additive `payments` feature that re-exports `adk-payments`.
3. WHERE the `payments` feature is not enabled, THE workspace SHALL continue to compile without requiring `adk-payments`.
4. THE `adk-payments` crate SHALL expose additive feature flags for at least `acp`, `acp-experimental`, `ap2`, `ap2-a2a`, and `ap2-mcp`.
5. WHERE protocol-specific feature flags are disabled, THE `adk-payments` crate SHALL omit the corresponding protocol adapter modules and dependencies.
6. THE initial rollout SHALL keep `payments` opt-in and SHALL NOT silently add payments support to the default `adk-rust` feature preset.

### Requirement 2: Canonical Commerce Kernel

**User Story:** As a backend engineer, I want one protocol-neutral commerce kernel, so that the same merchant backend can serve both ACP and AP2 without duplicating business logic.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL provide a protocol-neutral Commerce_Kernel for checkout, authorization, intervention, completion, cancelation, and order-state tracking.
2. THE Commerce_Kernel SHALL provide protocol-neutral domain types for money, cart contents, fulfillment selections, interventions, authorization mode, order state, receipt state, merchant of record, and payment processor identity.
3. THE Commerce_Kernel SHALL distinguish Human_Present_Transaction and Human_Not_Present_Transaction as separate authorization modes.
4. THE Commerce_Kernel SHALL represent Merchant_Of_Record explicitly and SHALL NOT infer that an agent surface or protocol adapter is the merchant of record.
5. THE Commerce_Kernel SHALL allow protocol-specific extension fields and evidence references to be preserved without flattening those fields into a lowest-common-denominator schema.
6. IF a protocol field cannot be represented losslessly by a canonical field, THEN THE Commerce_Kernel SHALL preserve the field in a protocol extension envelope rather than discard the field.

### Requirement 3: Shared Backend Service Traits

**User Story:** As a merchant platform developer, I want ACP and AP2 adapters to call the same backend-facing traits, so that dual-protocol support is obtained by adapter reuse rather than protocol-to-protocol transcoding.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL define backend-facing service traits for at least cart or checkout operations, payment execution, intervention handling, transaction storage, and evidence storage.
2. THE ACP adapter SHALL call the shared backend-facing service traits for create, update, complete, cancel, retrieve, and order-update flows.
3. THE AP2 adapter SHALL call the shared backend-facing service traits for mandate-driven authorization and payment execution flows.
4. THE system SHALL NOT require direct ACP-to-AP2 or AP2-to-ACP wire-level transcoding to achieve dual-protocol compatibility.
5. IF a protocol-specific action has no safe canonical mapping, THEN THE corresponding adapter SHALL return an explicit unsupported or policy error instead of silently approximating the action.

### Requirement 4: ACP Stable Compatibility

**User Story:** As a merchant or PSP integrator, I want `adk-payments` to support the ACP stable baseline, so that ADK-Rust can expose or consume the current mainstream agentic commerce HTTP contracts.

#### Acceptance Criteria

1. THE `acp` feature SHALL target ACP_Stable_Baseline `2026-01-30`.
2. THE `acp` adapter SHALL support the stable checkout session lifecycle endpoints for create, update, retrieve, complete, and cancel.
3. THE `acp` adapter SHALL support the stable delegated payment endpoint at `/agentic_commerce/delegate_payment`.
4. THE `acp` adapter SHALL support ACP stable data concepts including payment handlers, interventions, fulfillment options, totals, order objects, and affiliate attribution.
5. WHEN an ACP request includes an `API-Version` header, THE `acp` adapter SHALL validate that the requested version is supported by the configured ACP profile.
6. WHEN an ACP POST request includes an `Idempotency-Key`, THEN THE `acp` adapter SHALL store and replay the resulting response deterministically for safe retries.
7. WHERE an ACP deployment enables a strict production profile, THE `acp` adapter SHALL provide a configuration that requires `Idempotency-Key` on all ACP POST requests.
8. WHERE detached request signatures and timestamps are configured for ACP inbound traffic, THE `acp` adapter SHALL verify the signature and timestamp before the Commerce_Kernel is invoked.

### Requirement 5: ACP Experimental Compatibility

**User Story:** As a platform engineer, I want unreleased ACP additions to be available behind an explicit feature gate, so that serious adopters can prepare for emerging contracts without pretending the contracts are already stable.

#### Acceptance Criteria

1. THE `acp-experimental` feature SHALL gate support for ACP_Experimental_Baseline additions.
2. WHERE `acp-experimental` is enabled, THE `acp` adapter SHALL support `/.well-known/acp.json` discovery documents for seller capability bootstrapping.
3. WHERE `acp-experimental` is enabled, THE `acp` adapter SHALL support Stripe-style `Merchant-Signature` webhook verification using `t=<unix_seconds>,v1=<64_hex>` and HMAC-SHA256 over `timestamp + "." + raw_body`.
4. WHERE `acp-experimental` is enabled, THE `acp` adapter SHALL support delegated authentication lifecycle modeling for browser-based interventions such as 3DS2.
5. WHERE `acp-experimental` is disabled, THE `adk-payments` crate SHALL NOT expose experimental ACP routes, types, or claims as stable behavior.
6. THE documentation for `acp-experimental` SHALL state that the corresponding ACP contracts are based on unreleased protocol material rather than a dated stable snapshot.

### Requirement 6: AP2 Alpha Compatibility

**User Story:** As an agent platform engineer, I want `adk-payments` to support AP2 mandates and roles, so that ADK-Rust can participate in mandate-based agentic payment flows over A2A or MCP.

#### Acceptance Criteria

1. THE `ap2` feature SHALL target AP2_Alpha_Baseline as of March 22, 2026.
2. THE `ap2` adapter SHALL provide typed support for IntentMandate, CartMandate, PaymentMandate, PaymentReceipt, payment request, payment response, and AP2 role metadata.
3. THE `ap2` adapter SHALL distinguish the AP2 roles of shopper, merchant, credentials-provider, and payment-processor.
4. WHERE `ap2-a2a` is enabled, THE `ap2` adapter SHALL advertise the AP2 A2A extension URI `https://github.com/google-agentic-commerce/ap2/tree/v0.1` and SHALL validate AP2 role parameters in AgentCard extension metadata.
5. WHERE `ap2-a2a` is enabled, THE `ap2` adapter SHALL support AP2 mandate exchange using A2A message or artifact containers for IntentMandate, CartMandate, and PaymentMandate.
6. WHERE `ap2-mcp` is enabled, THE `ap2` adapter SHALL expose MCP-compatible wrappers for mandate exchange and payment orchestration without leaking Sensitive_Payment_Data into MCP transcript content.
7. THE `ap2` adapter SHALL preserve merchant authorization and user authorization artifacts as Evidence_Records rather than compressing those artifacts into plain strings without provenance.
8. WHERE the AP2 flow is Human_Not_Present_Transaction, THE `ap2` adapter SHALL require explicit expiration and authority constraints before a payment can progress to execution.

### Requirement 7: Cross-Protocol Correlation Without Lossy Translation

**User Story:** As a serious commerce operator, I want one durable internal transaction identity across protocols, so that ACP sessions, AP2 mandates, and local order state can be correlated without pretending the protocols are equivalent.

#### Acceptance Criteria

1. THE Commerce_Kernel SHALL assign one internal transaction identifier for each canonical transaction.
2. THE Transaction_Journal SHALL correlate ACP checkout session identifiers, ACP order identifiers, AP2 mandate identifiers, AP2 receipt identifiers, and local backend identifiers under the internal transaction identifier.
3. WHEN both ACP and AP2 are enabled for the same Merchant_Backend, THE system SHALL allow both adapters to operate against the same canonical cart, authorization, and order state.
4. IF direct protocol-to-protocol conversion would lose semantics or accountability evidence, THEN THE system SHALL refuse the direct conversion and SHALL require kernel-mediated continuation instead.
5. THE system SHALL store the originating protocol name and protocol version for every Evidence_Record written to the Transaction_Journal.

### Requirement 8: Auth Integration, Actor Binding, and Auditability

**User Story:** As a security engineer, I want payment operations to use `adk-auth` scopes and audit sinks, so that sensitive commerce actions are explicitly authorized and attributable.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL integrate with `adk-auth` scope enforcement for sensitive operations.
2. THE `adk-payments` crate SHALL define payment-specific scopes for at least checkout mutation, delegated credential usage, intervention completion, order update, and administrative or settlement operations.
3. WHEN a sensitive payment tool or endpoint is invoked, THE system SHALL emit an audit event through `adk-auth::AuditSink` containing the actor identity, the transaction identifier, the operation name, and the outcome.
4. WHEN an authenticated request identity conflicts with the transaction identity or tenant identity, THEN THE system SHALL reject the operation instead of rebinding the transaction implicitly.
5. WHERE discovery endpoints are exposed, THE discovery endpoints SHALL remain readable without authentication only when the underlying protocol requires public discovery.
6. THE system SHALL preserve the separation between authenticated request identity, session identity, and protocol actor roles.

### Requirement 9: Payment Guardrails and Sensitive Data Protection

**User Story:** As a platform owner, I want payment-specific guardrails and redaction rules, so that ADK-Rust can enforce policy without leaking PCI or privacy-sensitive material into general agent state.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL provide payment-specific guardrails that can enforce amount limits, merchant allowlists, currency policy, intervention policy, and protocol-version policy.
2. WHEN a transaction exceeds a configured policy threshold, THEN THE guardrail layer SHALL return an explicit failure or escalation result before the payment is executed.
3. THE system SHALL NOT write Sensitive_Payment_Data into conversation history, semantic memory entries, or telemetry spans.
4. WHERE raw protocol payloads or signed authorization artifacts must be retained, THE system SHALL store the raw material in an Evidence_Store and SHALL expose only masked values, hashes, digests, or artifact references outside that store.
5. WHEN user-facing summaries are produced, THEN THE summaries SHALL mask card data, redact cryptographic authorization blobs, and reduce billing details to the minimum needed for operator or user comprehension.
6. THE guardrail design SHALL support human confirmation or intervention escalation when a policy allows continuation only after explicit user approval.

### Requirement 10: Durable Transaction Memory That Survives Compaction

**User Story:** As an agent runtime maintainer, I want payment continuity to survive context compaction, so that completed or active transactions remain recoverable even when old conversation events are summarized away.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL persist a Transaction_Journal separately from conversation history.
2. THE Transaction_Journal SHALL be keyed by session-scoped identity and internal transaction identifier.
3. WHEN runner context compaction occurs, THEN THE Transaction_Journal SHALL remain queryable without replaying pre-compaction events.
4. THE system SHALL store Safe_Transaction_Summary records that can be surfaced through history or memory after compaction without reconstructing raw payment payloads.
5. IF a transaction remains unresolved at compaction time, THEN THE compaction-visible summary SHALL retain the unresolved transaction identifier and current state.
6. THE system SHALL treat the Transaction_Journal, not compacted chat history, as the source of truth for payment state.
7. WHERE semantic memory is enabled, THE `adk-payments` crate SHALL write searchable non-sensitive payment summaries into `adk-memory` while keeping raw evidence elsewhere.

### Requirement 11: Tooling and Server Integration

**User Story:** As an ADK-Rust user, I want first-class tools and server builders for commerce flows, so that dual-protocol payments can be integrated into agents and services without rebuilding the plumbing.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL expose tool builders or toolsets for at least checkout creation, checkout update, completion, cancelation, transaction status lookup, and intervention continuation.
2. THE payment tool outputs SHALL use structured JSON responses with masked data rather than raw sensitive payloads.
3. WHERE ACP routes are enabled, THE `adk-payments` crate SHALL provide route builders or router integration suitable for `adk-server`.
4. WHERE AP2 A2A support is enabled, THE `adk-payments` crate SHALL provide helpers that integrate with the A2A surfaces already present in `adk-server`.
5. WHERE AP2 MCP support is enabled, THE `adk-payments` crate SHALL provide MCP-friendly wrappers suitable for `adk-tool` or server-hosted MCP surfaces.
6. THE tooling layer SHALL make long-running or asynchronous payment continuation explicit rather than hiding continuation identifiers inside opaque strings.

### Requirement 12: Documentation, Examples, and Operator Guidance

**User Story:** As an ADK-Rust adopter, I want clear docs and examples for secure commerce usage, so that the crate can be used safely without reading protocol repositories line by line.

#### Acceptance Criteria

1. THE official documentation SHALL explain the difference between ACP and AP2 and SHALL explain why the implementation uses a Commerce_Kernel instead of direct protocol transcoding.
2. THE official documentation SHALL state the verified protocol baselines with explicit dates or version strings.
3. THE official documentation SHALL explain how payment auth scopes, audit sinks, guardrails, and transaction memory work together.
4. THE official documentation SHALL include at least one ACP example, one AP2 example, and one example showing transaction continuity after context compaction.
5. THE documentation SHALL warn operators that AP2 support is alpha-baseline support and ACP experimental support is unreleased-baseline support.
6. THE `CHANGELOG.md` file SHALL include user-facing notes when `adk-payments` is introduced.

### Requirement 13: Test Coverage and Contract Conformance

**User Story:** As a maintainer, I want strong regression and contract tests, so that a sensitive commerce implementation is verified against protocol material and not only against hand-written mocks.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL include unit tests for canonical state transitions, protocol correlation, redaction, and policy enforcement.
2. THE `adk-payments` crate SHALL include contract tests against ACP_Stable_Baseline schemas or examples for checkout, delegated payment, and order update flows.
3. THE `adk-payments` crate SHALL include tests for ACP experimental discovery and webhook signing behavior when the `acp-experimental` feature is enabled.
4. THE `adk-payments` crate SHALL include tests against AP2 example payloads for IntentMandate, CartMandate, PaymentMandate, and PaymentReceipt handling.
5. THE payment implementation SHALL include regression tests proving that transaction recall still works after session compaction.
6. THE payment implementation SHALL include tests proving that Sensitive_Payment_Data is absent from session transcript content, semantic memory, and telemetry payloads.
7. THE documentation examples introduced for `adk-payments` SHALL compile or be exercised as part of CI-compatible example validation.

### Requirement 14: End-to-End Agentic Commerce Journeys

**User Story:** As a platform builder, I want `adk-payments` to model complete agentic commerce journeys rather than isolated protocol messages, so that the crate can actually drive real purchases, interventions, and order follow-up.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL support an end-to-end Human_Present_Transaction journey for ACP where an agent creates a checkout session, updates shipping or fulfillment details, completes payment, and receives order lifecycle updates.
2. THE `adk-payments` crate SHALL support an end-to-end Human_Present_Transaction journey for AP2 where the shopper role, credentials-provider role, merchant role, and payment-processor role exchange mandates and complete a payment with durable evidence preservation.
3. THE `adk-payments` crate SHALL support an end-to-end Human_Not_Present_Transaction journey for AP2 where a signed intent with explicit constraints can later produce either a successful autonomous payment or a return-to-user intervention request.
4. WHEN a supported end-to-end journey reaches a terminal or recoverable state, THEN THE Transaction_Journal SHALL expose enough canonical state for a later status lookup or order follow-up without replaying full protocol traffic.
5. WHEN an end-to-end journey requires intervention such as 3DS or explicit buyer re-confirmation, THEN THE system SHALL preserve the continuation state, actor identity, and transaction correlation needed to resume safely.
6. WHERE one merchant backend exposes both ACP and AP2 adapters, THE same canonical cart, order, and transaction state SHALL be reusable across those journeys without making the protocols appear wire-equivalent.

### Requirement 15: Integration Testing and End-to-End Agentic Examples

**User Story:** As a maintainer or adopter, I want integration tests and runnable agentic examples that cover complete commerce journeys, so that the design is validated as a real system and not only as a collection of typed payloads.

#### Acceptance Criteria

1. THE `adk-payments` crate SHALL include integration tests for at least one ACP Human_Present_Transaction journey from checkout creation through order update synchronization.
2. THE `adk-payments` crate SHALL include integration tests for at least one AP2 Human_Present_Transaction journey involving shopper, credentials-provider, merchant, and payment-processor roles.
3. THE `adk-payments` crate SHALL include integration tests for at least one AP2 Human_Not_Present_Transaction journey with explicit authority constraints and either autonomous completion or forced user return.
4. THE `adk-payments` crate SHALL include integration tests proving transaction continuity after runner context compaction for unresolved and completed transactions.
5. THE workspace SHALL provide end-to-end agentic examples that show:
   - an ACP merchant checkout backend
   - an AP2 human-present shopper, merchant, credentials-provider, and payment-processor flow
   - an AP2 human-not-present intent flow
   - a dual-protocol merchant backend
   - a post-compaction transaction recall or order-follow-up flow
6. THE end-to-end examples SHALL demonstrate safe summaries, auditability, and sensitive-data redaction rather than exposing raw payment credentials in normal agent output.
7. THE integration tests and end-to-end examples SHALL be documented as the primary reference for serious adopter validation of `adk-payments`.
