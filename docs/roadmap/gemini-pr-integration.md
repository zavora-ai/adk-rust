# Gemini PR Integration Roadmap

*Priority: P0 | Target: Immediate stabilization before merge*

## Objective

Integrate Gemini-related pull requests without breaking existing ADK-Rust behavior for API key users, while enabling a safe Vertex AI path grounded in actual SDK support.

## Scope

- PR #26: `refactor(gemini): introduce GeminiBackend trait...`
- PR #27: `Gemini: add Google Cloud service account auth, Vertex AI support, and v1 API helpers`
- PR #28: `chore(adk-realtime): upgrade tokio-tungstenite to 0.26 and fix compilation`
- PR #30: `feat(gemini): add service account auth and configurable retry logic`

## Current Assessment

| PR | Status | Summary | Decision |
| --- | --- | --- | --- |
| #26 | Superseded | Architecture-only refactor; no user-facing capability | Close |
| #27 | High value, blocked | Adds Vertex + v1 path but has runtime and constructor issues | Fix then merge |
| #28 | Independent | Realtime dependency upgrade | Merge independently after CI |
| #30 | Unsafe as-is | Introduces breaking API changes and token lifecycle issues | Split and redesign |

## Merge Strategy

1. Stabilize and merge PR #27.
2. Merge PR #28 independently.
3. Replace PR #30 with a narrow follow-up that preserves API compatibility.
4. Close PR #26 as superseded.

## Phase 1: Stabilize PR #27

### Required fixes

- Remove runtime-in-runtime panic risk in Vertex client construction.
- Fix service account convenience constructors that currently fail due to missing Google Cloud config.
- Ensure unsupported Vertex operations return operation-specific, actionable errors.
- Keep AI Studio API key behavior unchanged.

### Verification

- `cargo test -p adk-gemini`
- `cargo check -p adk-examples --example quickstart --features gemini`
- One Vertex smoke test (ADC or service account) for non-streaming `generateContent` and `embedContent`

### Exit criteria

- No panic in async contexts when constructing Vertex clients.
- Service account constructors work or are removed from public API.
- Existing API key examples compile and run unchanged.

## Phase 2: Merge PR #28

### Required checks

- Realtime compile and tests green for affected crates.
- No regressions in websocket-related examples.

### Exit criteria

- PR #28 merged without introducing failures in non-realtime crates.

## Phase 3: Replace PR #30 with a Compatible Follow-up

### Design constraints

- Do not change `GeminiModel::new` from sync to async in a patch/minor release.
- Do not duplicate auth/token lifecycle logic in `adk-model` when `adk-gemini` already owns provider behavior.
- Retry behavior must not claim coverage for failures it cannot catch (especially streaming mid-flight errors).

### Required changes

- Keep `GeminiModel::new` backward compatible.
- Introduce retry configuration through additive API only.
- Use SDK-backed auth path via `adk-gemini` constructors.
- Add tests for:
  - backward compatibility of `GeminiModel::new`
  - retry behavior on retryable error responses
  - service account flow with token refresh behavior

### Exit criteria

- In-tree examples and tests compile without call-site migrations.
- Service account behavior is accurate and documented.
- Retry semantics match implementation and docs.

## Risk Register

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Runtime initialization inside async context | Panic at runtime | Move Vertex client init to async-safe path |
| Public constructor behavior drift | Broad compile breakage | Preserve `GeminiModel::new` signature |
| Token refresh gaps | Production auth failures | Rely on SDK-managed credentials path |
| Streaming retry mismatch | False reliability expectations | Document limits; retry only pre-stream setup |

## Work Breakdown

1. Patch PR #27 blockers and run targeted tests.
2. Merge PR #27.
3. Validate and merge PR #28.
4. Close PR #26 as superseded.
5. Open new PR to replace #30 with compatibility-safe implementation.

## Done Definition

- API key users on AI Studio remain unaffected.
- Vertex path works for supported SDK operations.
- No hidden runtime panics introduced by constructor paths.
- Documentation matches implementation behavior exactly.

## Implementation Status (2026-02-05)

### Completed now (Phase 1 blockers)

- `upload_file` now uses authenticated `perform_request` for upload finalize.
- Vertex initialization no longer uses runtime-in-runtime construction; it uses an async-safe builder path.
- Service account convenience constructors now infer `project_id` from JSON and set Vertex config (`us-central1` default).
- Vertex unsupported operations now return operation-specific `GoogleCloudUnsupported` errors for file/cache APIs.
- Added unit tests for service-account `project_id` extraction logic.
- Added Vertex enum interop handling for SDK integer-encoded enums (`finishReason`, safety enums, modality).
- Fixed Vertex `embedContent` request construction to avoid SDK transport oneof conflict by binding model in URL and sending model-less body payload.

### Completed now (Phase 2)

- PR #28 websocket upgrade (`tokio-tungstenite 0.26`, rustls migration) cherry-picked and validated.

### Completed now (Phase 3)

- Added `RetryConfig` in `adk-model` with additive configuration API:
  - `GeminiModel::with_retry_config(...)`
  - `GeminiModel::set_retry_config(...)`
  - `GeminiModel::retry_config(...)`
- Standardized retry logic across remote providers via shared `adk-model` retry module and provider-level additive APIs:
  - Gemini
  - OpenAI / Azure OpenAI
  - Anthropic
  - DeepSeek
  - Groq
- Preserved existing synchronous constructors (`GeminiModel::new`, `new_google_cloud*`) with no call-site breakage.
- Implemented retry handling for retryable startup/request errors (`429`, `RESOURCE_EXHAUSTED`, `UNAVAILABLE`, `DEADLINE_EXCEEDED`, `502/503/504`).
- Documented retry limits in code: streaming errors after stream start are surfaced to caller and are not auto-replayed.
- Kept service-account/token lifecycle ownership in `adk-gemini` (SDK-backed credentials flow), with no duplicated token cache logic in `adk-model`.
- Added tests for constructor compatibility and retry behavior semantics.

### Verification run

- `cargo test -p adk-gemini` (pass)
- `cargo check --manifest-path examples/Cargo.toml --example quickstart` (pass)
- `cargo check --manifest-path examples/Cargo.toml --example graph_gemini` (pass)
- `cargo test -p adk-model --tests --features gemini` (pass)
- `cargo test -p adk-model --tests --all-features` (pass)
- `cargo check -p adk-model --all-features` (pass)
- `cargo test -p adk-realtime --all-features` (pass)
- `cargo check --manifest-path examples/Cargo.toml --features realtime-openai --example realtime_basic --example realtime_vad --example realtime_tools --example realtime_handoff --example eval_realtime` (pass)
- Live Vertex smoke with ADC:
  - `generateContent` (pass)
  - `embedContent` reaches Vertex and authenticates; current tested publisher embedding models are rejected in `us-central1` for this project (`INVALID_ARGUMENT`, model not supported).

### Still pending from roadmap

- None for Phases 1-3 implementation scope.
- Optional follow-up: run `embedContent` smoke with a model that is explicitly enabled/supported in target project+region.
