# Stability

This document defines the stability contract for every public `adk-*` crate in the ADK-Rust workspace. It helps consumers assess upgrade risk before adopting a crate.

## Tiers

ADK-Rust uses three stability tiers. Every public crate is assigned exactly one tier.

| Tier | Contract |
|------|----------|
| **Stable** | Follows [semantic versioning](https://semver.org/). No unannounced breaking changes. Public API removals go through the deprecation lifecycle below. |
| **Beta** | Breaking changes may occur in minor releases. Each breaking change is accompanied by a migration guide in the release notes. |
| **Experimental** | API may change without notice. These crates are under active exploration and should not be relied upon for production workloads without pinning an exact version. |

## Crate Tiers

The table below assigns one stability tier to every public `adk-*` crate in the workspace.

| Crate | Tier | Notes |
|-------|------|-------|
| `adk-core` | **Stable** | Core traits: Agent, Tool, Llm, Session, Event, Content, State |
| `adk-agent` | **Stable** | Agent implementations: LlmAgent, workflows |
| `adk-model` | **Stable** | LLM provider facade (Gemini, OpenAI, Anthropic, etc.) |
| `adk-gemini` | **Stable** | Dedicated Gemini client with Studio + Vertex AI backends |
| `adk-tool` | **Stable** | Tool system: FunctionTool, MCP integration |
| `adk-runner` | **Stable** | Agent execution runtime with event streaming |
| `adk-session` | **Stable** | Session management and state persistence |
| `adk-rust` | **Stable** | Umbrella crate re-exporting the workspace |
| `adk-server` | **Stable** | HTTP server (Axum) and A2A protocol |
| `adk-graph` | **Stable** | Graph-based workflow orchestration with checkpoints |
| `adk-memory` | **Stable** | Semantic memory and RAG search |
| `adk-anthropic` | **Stable** | Dedicated Anthropic client and tool search |
| `adk-artifact` | **Stable** | Binary artifact storage for agents |
| `adk-auth` | **Stable** | Authentication: API keys, JWT, OAuth2, OIDC, SSO |
| `adk-telemetry` | **Stable** | OpenTelemetry integration for agent observability |
| `adk-guardrail` | **Stable** | Input/output guardrails: validation, content filtering, PII redaction |
| `adk-plugin` | **Stable** | Plugin system for agent lifecycle hooks |
| `adk-skill` | **Stable** | Skill discovery, parsing, and convention-based agent capabilities |
| `adk-cli` | **Stable** | Command-line launcher for agents |
| `adk-rag` | **Stable** | Retrieval-augmented generation pipelines |
| `adk-action` | **Stable** | Action node execution for deterministic workflow operations |
| `adk-deploy` | **Stable** | Deployment utilities |
| `adk-payments` | **Stable** | Payment integration for agent services |
| `adk-bench` | **Stable** | Benchmarking framework for framework performance measurement |
| `adk-rust-macros` | **Stable** | Procedural macros for ADK-Rust |
| `cargo-adk` | **Stable** | Cargo subcommand for ADK project management |
| `adk-realtime` | **Stable** | Real-time bidirectional audio/video streaming |
| `adk-retry-reflect` | **Stable** | Retry & Reflect plugin — failure interception, reflection prompts, circuit breaker |
| `adk-browser` | **Stable** | Browser automation tools via WebDriver |
| `adk-eval` | **Stable** | Evaluation framework: trajectory, semantic, rubric, LLM-judge |
| `adk-code` | **Stable** | Code generation and execution |
| `adk-sandbox` | **Stable** | Sandboxed execution environments |
| `adk-audio` | **Stable** | Audio processing and STT/TTS providers |
| `adk-mistralrs` | **Stable** | Native local LLM inference via mistral.rs |
| `adk-acp` | **Beta** | Agent Client Protocol client and server (tracks the upstream ACP spec) |
| `adk-awp` | **Beta** | Agentic Web Protocol implementation (tracks the upstream AWP spec) |
| `awp-types` | **Beta** | AWP protocol types, zero `adk-*` dependencies |
| `adk-devtools` | **Beta** | Coding-agent dev tools scoped to a `Workspace` (new in 2.0.0) |
| `adk-computer-use` | **Beta** | Governed orchestration for the `computer-use-mcp` server (new in 2.0.0) |
| `adk-gcp` | **Beta** | Shared Google Cloud REST plumbing for Vertex AI backends (new in 2.1.0) |
| `adk-harness` | **Beta** | Governed harness agent: modes, standing approvals, shell, todos, instructions, skills (new in 3.0.0) |
| `adk-kernel` | **Beta** | Agent kernel: supervisor, process model, wake sources, event bus, control plane (spec 3.2) |
| `adk-enterprise` | **Experimental** | Enterprise client SDK for ADK-Rust Managed Agent Service |
| `adk-managed` | **Experimental** | Managed agent runtime engine |
| `adk-codeact-monty` | **Experimental** | Python CodeRuntime for the CodeActAgent via the Monty interpreter |

### Beta Crate Rationale

The crates listed as Beta in the table above are fully functional but track
external specifications that are still moving, so their public API may change in a
minor release. Every other crate was promoted to Stable in 1.0.0.

| Crate | Reason for Beta | Path to Stable |
|-------|----------------|----------------|
| `adk-acp` | Agent Client Protocol is evolving upstream; capability negotiation follows the SDK | Stabilize once the ACP wire spec settles past v1 |
| `adk-awp` | Agentic Web Protocol is a young specification | Stabilize when the AWP spec reaches a versioned release |
| `awp-types` | Mirrors the AWP wire format, so it moves with the spec | Stabilize alongside `adk-awp` |
| `adk-devtools` | New in 2.0.0; the coding-agent tool surface is still being shaped by use | Promote after 1-2 release cycles without breaking changes |
| `adk-computer-use` | New in 2.0.0; tracks the `computer-use-mcp` wire contracts | Stabilize when the upstream contracts are versioned |
| `adk-gcp` | New in 2.1.0; the shared ADC/LRO surface is still absorbing the per-crate GCP clients | Promote once the remaining Vertex backends consume it without API changes |
| `adk-harness` | New in 3.0.0 (spec 3.1); the team, plan-graph, goal and pack layers are implemented and still taking shape in use | Review for Stable in 3.2.0 (spec 3.1 §12 phase 6) |
| `adk-kernel` | New with spec 3.2; the agenda and the consolidation tick are in, and drivers, partitions, `/proc` and service installation land in later phases on top of the phase-1 API | Review for Stable once spec 3.2 phases 1–3 ship without breaking changes |

### Alpha Backends

A backend inside a Beta or Stable crate can be **Alpha**: it implements its store's whole
contract and carries the store's conformance suite, but that suite has not yet passed
against a live server in CI, so its storage layout may change in a minor release without a
migration.

| Crate | Features | Backends | Leaves Alpha when |
|-------|----------|----------|-------------------|
| `adk-kernel` | `mongodb`, `firestore`, `neo4j` | `ProcessStore`, `AgendaStore`, `DriverStore` and `LedgerStore` on MongoDB, Firestore and Neo4j (`Mongo*`, `Firestore*`, `Neo4j*`) | Each store's conformance suite passes on the backend in the nightly `durable-stores` job (spec 3.2 §13 Q8) |

### Excluded from Workspace

The following crates are not part of the main workspace and are not covered by the tier system above:

| Crate | Reason |
|-------|--------|
| `adk-studio` | Extracted to [standalone repo](https://github.com/zavora-ai/adk-studio). |
| `adk-ui` | Extracted to [standalone repo](https://github.com/zavora-ai/adk-ui). |

## Deprecation Policy

ADK-Rust follows a predictable deprecation lifecycle so that consumers have time to migrate away from removed APIs.

1. **Announcement.** When a public item is deprecated, the crate annotates it with `#[deprecated(since = "X.Y.Z", note = "Use <replacement> instead. Will be removed in X.(Y+2).0.")]`. The `since` field records the version that introduced the deprecation. The `note` field provides migration instructions.

2. **Grace period.** Deprecated public items remain available and functional for at least **N+2 minor releases** after the release in which the deprecation is announced. For example, an item deprecated in `3.1.0` will not be removed before `3.3.0`.

3. **Removal.** After the grace period, the item may be removed in the next minor (pre-1.0) or major (post-1.0) release. Removal is documented in the `CHANGELOG.md` with a reference to the original deprecation notice.

### Example

```rust
/// Replaced by [`new_function`].
#[deprecated(since = "3.1.0", note = "Use `new_function` instead. Will be removed in 4.0.0.")]
pub fn old_function() {
    new_function()
}

/// The replacement.
pub fn new_function() {}
```

## Non-Breaking Field Addition Policy

Public structs in Stable-tier crates that are constructed by downstream consumers (e.g., `RunnerConfig`, `RunConfig`, session service request structs) follow strict rules to prevent compilation failures when new fields are introduced.

1. **Optional fields only.** New fields added to public structs in Stable-tier crates MUST be `Option<T>` with a default value, or the struct MUST use a builder pattern that assigns defaults for all new fields. Existing call sites — whether struct literals using `..Default::default()` or builder chains — MUST continue to compile without modification.

2. **Required fields are breaking.** Adding a required field (one that has no default and must be explicitly provided) to a Stable-tier public struct is a **breaking change**. It follows the same deprecation lifecycle defined above: announce in version N, remove the old API no earlier than version N+2.

3. **Applies to.** This policy applies to all public structs in Stable-tier crates that downstream consumers construct directly, including but not limited to:
   - `RunnerConfig` and `RunConfig` in `adk-runner` / `adk-core`
   - Session service request structs in `adk-session`
   - Any future configuration or request structs added to Stable-tier crates

4. **Struct literal construction.** Public configuration and request structs that downstream code constructs, such as `RunConfig`, `EventActions`, and `LlmResponse`, implement `Default`, so a struct literal that ends with `..Default::default()` keeps compiling when a field is added. A few types are built only through constructors — `ToolApproval` (`approve()` / `deny()`) and `adk_graph::node::ExecutionConfig` (`ExecutionConfig::new`); `RunnerConfig` is best built with `Runner::builder()`.

## Public Dependency Policy

A dependency whose types appear in a public signature is part of the crate's API: callers construct those types with their own copy of the dependency, and two semver-incompatible versions are distinct Rust types.

1. **SQLx is a public dependency** of the crates below, through their SQL backends.

   | Crate | Features | SQLx types in the public API |
   |-------|----------|------------------------------|
   | `adk-session` | `sqlite`, `postgres` | `from_pool` on every SQL store and session service, `SqliteSessionService::pool`, the `migration` runners (`run_sql_migrations`, `run_sql_migrations_on_connection`, `sql_schema_version`) |
   | `adk-memory` | `sqlite-memory`, `graph-memory`, `database-memory` | `from_pool` on `SqliteMemoryService`, `GraphMemoryService`, and `PostgresMemoryService`, the `migration` runners |
   | `adk-graph` | `sqlite`, `postgres` | `SqliteCheckpointer::from_pool`, `PostgresCheckpointer::from_pool` |
   | `adk-auth` | `postgres-audit` | `PostgresAuditSink::from_pool`, `PostgresAuditSink::pool` |
   | `adk-rag` | `pgvector` | `PgVectorStore::from_pool` |

2. **Breaking SQLx upgrades are breaking releases.** Moving these crates to a semver-incompatible SQLx release (0.8 → 0.9, 0.9 → 0.10) is a breaking change and ships only in a major ADK-Rust release, with a migration guide. A semver-compatible SQLx update is not breaking.

3. **CI coverage.** The `semver` gate checks these crates with the features above enabled, in addition to their default features (`SEMVER_EXTRA_FEATURES` in `.github/workflows/semver.yml`).

## 1.0 Milestone

ADK-Rust 1.0.0 was released on June 7, 2026. All Stable-tier crates commit to long-term API stability under semantic versioning.

### Criteria Status

| Criterion | Status | Notes |
|-----------|--------|-------|
| **Semver compliance** | ✅ Met | `cargo semver-checks` passes for Stable-tier crates. The 0.10.0 → 1.0.0 bump is a major version change under pre-1.0 semver rules. |
| **Test coverage** | ✅ Met | All Stable-tier crates have unit tests, integration tests, and/or property-based tests. CI runs 3000+ tests. |
| **Deprecation cleanup** | ✅ Met | Only active deprecations remain (Gemini model variants with shutdown dates). No legacy items pending removal. |
| **Beta documentation** | ✅ Met | All 7 Beta crates have documented rationale for remaining Beta with path-to-Stable criteria. |
| **CI enforcement** | ✅ Met | `cargo fmt`, `cargo clippy -D warnings`, `cargo nextest run`, and doc-example validation run on every PR. `cargo semver-checks` to be added as a CI gate. |
| **Documentation coverage** | 🔲 In progress | Public API coverage is high; formal 90% audit pending tooling automation. |

### Post-1.0 Contract

- Breaking changes to Stable-tier crates require a 2.0.0 release
- Beta crates may have breaking changes in 1.x minor releases (with migration guides)
- Experimental crates may change without notice

## 2.0 Milestone

ADK-Rust 2.0.0 is the second major release. It keeps the Stable-tier commitment and
spends the major version on the breakage that had accumulated since 1.0.0.

### What the major version bought

| Area | Change |
|---|---|
| MCP | Official `rmcp 3.1` SDK, advertises protocol `2025-11-25` |
| ACP | Official `agent-client-protocol` 1.2; `PermissionDecision::Allow` split into `AllowOnce`/`AllowAlways`/`Select` |
| Tool authorization | `RunConfig` gained an async `ToolConfirmationHandler`; decisions key on function-call ID |
| Realtime | `ClientEvent`/`ServerEvent` became `#[non_exhaustive]` so future protocol events are additive |
| New crates | `adk-devtools`, `adk-computer-use` (both Beta) |

The complete breaking-change list, generated with `cargo semver-checks`, is in
[`CHANGELOG.md`](CHANGELOG.md) and the
[1.0 → 2.0 migration guide](docs/official_docs/migration/1.0-to-2.0.md).

### Criteria Status

| Criterion | Status | Notes |
|-----------|--------|-------|
| **Semver compliance** | ✅ Met | `cargo semver-checks check-release` accepts the 1.0.0 → 2.0.0 bump for every published crate. Because a major bump permits any change, the breaking-change inventory is generated separately with `--release-type minor` and recorded in the changelog. |
| **Migration guide** | ✅ Met | [docs/official_docs/migration/1.0-to-2.0.md](docs/official_docs/migration/1.0-to-2.0.md) covers every breakage with before/after code. |
| **Test coverage** | ✅ Met | 3700+ workspace tests plus 300+ doctests, all gated in CI. |
| **CI enforcement** | ✅ Met | `fmt`, `clippy --all-targets -D warnings`, `nextest --workspace`, doctests, doc build, template scaffolding, and semver run per PR; supply-chain (`cargo audit` + `cargo deny`) and standalone-example compilation run nightly. |
| **Supply chain** | ✅ Met | `cargo audit` and `cargo deny check` both pass. Accepted advisories and non-OSI dependency licenses are documented in [docs/security/DEPENDENCY-ADVISORIES.md](docs/security/DEPENDENCY-ADVISORIES.md). |
| **Documentation coverage** | 🔲 In progress | Public API coverage is high; formal 90% audit pending tooling automation. |

### Post-2.0 Contract

- Breaking changes to Stable-tier crates require a 3.0.0 release
- Beta crates may have breaking changes in 2.x minor releases (with migration guides)
- Experimental crates may change without notice

## 3.0 Milestone

ADK-Rust 3.0.0 is the third major release. It keeps the Stable-tier commitment and spends
the major version on the breaking changes listed in the
[2.x → 3.0 migration guide](docs/official_docs/migration/2.x-to-3.0.md).

### What the major version bought

| Area | Change |
|---|---|
| Tool authorization | One governed path (`authorize_tool_call`): kill switch, `ToolPolicy`, tool guardrails, and confirmation, after plugin and callback rewrites; fingerprints name the acting agent and the tool's origin |
| Agent identity | Every run has an acting agent (`ActingContext`), and a run acts for its session's user unless a subject delegation policy allows another |
| Retries and side effects | `ToolEffect` governs retries; an `ActionLedger` keeps `NonIdempotent` calls at most once, answering unknown outcomes as `outcome_unknown` |
| Budgets and spend | `RunBudget`, `SpendLedger`, and `ModelCallScope` meter every model call of a run |
| Durable runtime | `LeaseStore`, `DurableRunStore`, `ScheduleStore`, `GovernanceStateStore`, approval and audit log stores, with backends for every `adk-session` database |
| Server exposure | `adk-server` binds `127.0.0.1` through the launchers, refuses unlisted hosts, and allows no cross-origin access by default |
| Errors | `ErrorCategory` gains `Conflict` and `ResourceExhausted`; guardrail blocks and tool argument errors carry stable codes |
| Dependencies | SQLx 0.9 for every SQL backend; see the [Public Dependency Policy](#public-dependency-policy) |

`cargo semver-checks` runs in the `semver` gate with `SEMVER_RELEASE_TYPE: major` while the
3.0.0 release candidate is open.

### Post-3.0 Contract

- Breaking changes to Stable-tier crates require a 4.0.0 release
- Beta crates may have breaking changes in 3.x minor releases (with migration guides)
- Experimental crates may change without notice
