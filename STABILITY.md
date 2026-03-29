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
| `adk-server` | **Beta** | HTTP server (Axum) and A2A protocol |
| `adk-graph` | **Beta** | Graph-based workflow orchestration with checkpoints |
| `adk-memory` | **Beta** | Semantic memory and RAG search |
| `adk-artifact` | **Beta** | Binary artifact storage for agents |
| `adk-auth` | **Beta** | Authentication: API keys, JWT, OAuth2, OIDC, SSO |
| `adk-telemetry` | **Beta** | OpenTelemetry integration for agent observability |
| `adk-guardrail` | **Beta** | Input/output guardrails: validation, content filtering, PII redaction |
| `adk-plugin` | **Beta** | Plugin system for agent lifecycle hooks |
| `adk-skill` | **Beta** | Skill discovery, parsing, and convention-based agent capabilities |
| `adk-anthropic` | **Beta** | Dedicated Anthropic client and tool search |
| `adk-cli` | **Beta** | Command-line launcher for agents |
| `adk-rag` | **Beta** | Retrieval-augmented generation pipelines |
| `adk-action` | **Beta** | Action node execution for deterministic workflow operations |
| `adk-deploy` | **Beta** | Deployment utilities |
| `adk-payments` | **Beta** | Payment integration for agent services |
| `adk-rust-macros` | **Beta** | Procedural macros for ADK-Rust |
| `cargo-adk` | **Beta** | Cargo subcommand for ADK project management |
| `adk-realtime` | **Experimental** | Real-time bidirectional audio/video streaming |
| `adk-browser` | **Experimental** | Browser automation tools via WebDriver |
| `adk-eval` | **Experimental** | Evaluation framework: trajectory, semantic, rubric, LLM-judge |
| `adk-code` | **Experimental** | Code generation and execution |
| `adk-sandbox` | **Experimental** | Sandboxed execution environments |
| `adk-audio` | **Experimental** | Audio processing and STT/TTS providers |

### Excluded from Workspace

The following crates are not part of the main workspace and are not covered by the tier system above:

| Crate | Reason |
|-------|--------|
| `adk-mistralrs` | GPU dependencies — build explicitly. Has its own CI workflow. |
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

## 1.0 Milestone

The ADK-Rust 1.0 release represents a commitment to long-term API stability for all Stable-tier crates. Progress is tracked in the [GitHub 1.0 Milestone](https://github.com/zavora-ai/adk-rust/milestone/1).

### Criteria

The following criteria must be met before the 1.0 release:

- **Semver compliance.** All Stable-tier crates pass `cargo semver-checks` with no breaking changes relative to the last published version.
- **Documentation coverage.** All Stable-tier crates achieve 90%+ rustdoc coverage for public items, verified by `adk-doc-audit`.
- **Test coverage.** All Stable-tier crates have unit tests, integration tests, and property-based tests for core functionality.
- **Deprecation cleanup.** All items deprecated before the 1.0 grace-period cutoff have been removed.
- **Beta promotion.** Crates intended for Stable at 1.0 have been promoted from Beta and have passed at least one release cycle without breaking changes.
- **CI enforcement.** `cargo semver-checks` runs on every pull request, failing for Stable-tier crates and warning for Beta/Experimental crates.
