# Google Cloud Rust SDK Migration Plan

## Motivation

The long-term goal is to align `adk-gemini` with the official Google Cloud Rust SDK so that
SDK-maintained authentication, endpoint discovery, and API evolution are handled upstream.
This improves operational reliability and reduces the maintenance burden for the HTTP client
surface that we currently own.

## Refactoring Plan (Phased)

### Phase 0: Alignment & Research
- Inventory the `google-cloud-rust` API surface for Generative AI, including streaming, embeddings,
  batch operations, file uploads, and caching.
- Confirm auth patterns (ADC, workload identity, service accounts) and how they map to the existing
  API key setup.

### Phase 1: Backend Abstraction
- Introduce an internal backend trait to allow multiple transport implementations without changing
  public APIs.
- Keep the HTTP backend as the default to preserve compatibility.

### Phase 2: SDK Wrapper Scaffold
- Add a `GoogleGeminiClient` wrapper that implements the backend trait once the SDK surface is verified.

### Phase 3: Request/Response Mapping
- Implement mapping helpers between ADK request/response structs and SDK request/response types.
- Validate conversions for `Content`, `Part`, function calls, and safety settings.

### Phase 4: End-to-End Parity
- Implement streaming generation, embeddings, batch jobs, file management, and caching over the SDK.
- Update examples to demonstrate ADC-based auth flows.

### Phase 5: Deprecation & Cleanup
- Mark the legacy HTTP implementation as deprecated.
- Document migration guidance and eventually remove the HTTP backend in a major version.

## Design Notes

- **Public API stability:** `Gemini`, `GeminiBuilder`, and all builder/handle APIs remain unchanged.
  The backend selection is internal, which keeps existing applications compatible.
- **Backend selection:** A feature flag will toggle the SDK backend once the official API surface
  is validated, so we can stabilize the integration without forcing downstream dependency changes.
- **Error model:** The existing `ClientError` is retained. SDK errors should be translated into
  this error type for consistent handling.

## Task Breakdown

1. **SDK research and mapping**
   - Identify SDK client initialization patterns and request/response types.
   - Map streaming behavior to `GenerationResponse` streaming semantics.
2. **SDK integration**
   - Wire up `GoogleGeminiClient` using verified SDK client types.
   - Implement auth and endpoint configuration.
3. **Type conversion layer**
   - Introduce adapters for request/response conversion.
   - Add unit tests to validate round-trip conversions.
4. **Feature parity tests**
   - Add integration tests for content generation, embeddings, batch jobs, file upload, and cache.
5. **Docs & examples**
   - Update README and examples to show SDK-based configuration and ADC usage.

## Current State

- The backend trait is in place and the SDK integration work is queued behind the
  research phase (no SDK types are assumed or wired yet).
- No SDK dependency is enabled until the official API surface is validated.

## SDK API Validation Log

The initial validation pass requires network access to the official crate metadata and
repository. The following attempts were made and blocked by the environment:

- `cargo search google-cloud-rust` → 403 (registry access blocked)
- `curl https://crates.io/api/v1/crates/google-cloud-rust` → 403
- `git ls-remote https://github.com/googleapis/google-cloud-rust` → 403
- `git ls-remote https://github.com/jkmaina/google-cloud-rust` → 403
- `git clone --depth 1 https://github.com/jkmaina/google-cloud-rust /tmp/google-cloud-rust` → 403
- Retry after internet enabled: `git ls-remote https://github.com/jkmaina/google-cloud-rust` → 403
- Retry after internet enabled: `git ls-remote https://github.com/googleapis/google-cloud-rust` → 403

Once network access is available, repeat the steps above, then capture:

- Crate version and feature flags
- Module paths for Generative AI / Gemini clients
- Request/response types for generation, streaming, embeddings, batch operations, files, and cache
