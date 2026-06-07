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
| `adk-enterprise` | **Experimental** | Enterprise client SDK for ADK-Rust Managed Agent Service |
| `adk-managed` | **Experimental** | Managed agent runtime engine |

### Beta Crate Rationale

These crates are fully functional but remain Beta at 1.0 because their APIs depend on rapidly evolving external specifications or hardware capabilities:

| Crate | Reason for Beta | Path to Stable |
|-------|----------------|----------------|
| `adk-realtime` | WebRTC and Live API specs are evolving (OpenAI, Gemini, LiveKit) | Stabilize after upstream APIs settle |
| `adk-browser` | WebDriver protocol and browser automation patterns still changing | Stabilize when WebDriver BiDi adoption matures |
| `adk-eval` | Evaluation methodology is an active research area; API surface may expand | Promote after 1-2 release cycles without breaking changes |
| `adk-code` | Code execution security model under active development | Stabilize alongside `adk-sandbox` |
| `adk-sandbox` | OS-level sandboxing APIs differ across platforms; API may evolve | Stabilize after cross-platform testing at scale |
| `adk-audio` | ONNX model ecosystem and audio format support expanding rapidly | Promote when model selection stabilizes |
| `adk-mistralrs` | Upstream mistral.rs releases new model architectures frequently | Track upstream stability |

### Excluded from Workspace

The following crates are not part of the main workspace and are not covered by the tier system above:

| Crate | Reason |
|-------|--------|
| `adk-studio` | Extracted to [standalone repo](https://github.com/zavora-ai/adk-studio). |
| `adk-ui` | Extracted to [standalone repo](https://github.com/zavora-ai/adk-ui). |

## Deprecation Policy

ADK-Rust follows a predictable deprecation lifecycle so that consumers have time to migrate away from removed APIs.

1. **Announcement.** When a public item is deprecated, the crate annotates it with `#[deprecated(since = "X.Y.Z", note = "Use <replacement> instead. Will be removed in X.(Y+2).0.")]`. The `since` field records the version that introduced the deprecation. The `note` field provides migration instructions.

2. **Grace period.** Deprecated public items remain available and functional for at least **N+2 minor releases** after the release in which the deprecation is announced. For example, an item deprecated in `0.6.0` will not be removed before `0.8.0`.

3. **Removal.** After the grace period, the item may be removed in the next minor (pre-1.0) or major (post-1.0) release. Removal is documented in the `CHANGELOG.md` with a reference to the original deprecation notice.

### Example

```rust
#[deprecated(
    since = "0.6.0",
    note = "Use `SessionService::get_or_create` instead. Will be removed in 0.8.0."
)]
pub fn create_session(/* ... */) -> Result<Session> {
    // ...
}
```

## Non-Breaking Field Addition Policy

Public structs in Stable-tier crates that are constructed by downstream consumers (e.g., `RunnerConfig`, `RunConfig`, session service request structs) follow strict rules to prevent compilation failures when new fields are introduced.

1. **Optional fields only.** New fields added to public structs in Stable-tier crates MUST be `Option<T>` with a default value, or the struct MUST use a builder pattern that assigns defaults for all new fields. Existing call sites — whether struct literals using `..Default::default()` or builder chains — MUST continue to compile without modification.

2. **Required fields are breaking.** Adding a required field (one that has no default and must be explicitly provided) to a Stable-tier public struct is a **breaking change**. It follows the same deprecation lifecycle defined above: announce in version N, remove the old API no earlier than version N+2.

3. **Applies to.** This policy applies to all public structs in Stable-tier crates that downstream consumers construct directly, including but not limited to:
   - `RunnerConfig` and `RunConfig` in `adk-runner` / `adk-core`
   - Session service request structs in `adk-session`
   - Any future configuration or request structs added to Stable-tier crates

4. **Struct literal construction.** All public structs support direct struct literal construction with `..Default::default()` for forward compatibility. Builder patterns are provided as a convenience but are not required.

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
