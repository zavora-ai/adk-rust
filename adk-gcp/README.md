# adk-gcp

Shared Google Cloud REST plumbing for [ADK-Rust](https://github.com/zavora-ai/adk-rust) Vertex AI backends: Application Default Credentials with cached auth headers, a bounded redirect-disabled HTTP transport, and long-running-operation polling.

[![Crates.io](https://img.shields.io/crates/v/adk-gcp.svg)](https://crates.io/crates/adk-gcp)
[![Documentation](https://docs.rs/adk-gcp/badge.svg)](https://docs.rs/adk-gcp)
[![License](https://img.shields.io/crates/l/adk-gcp.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

Every Vertex integration in the workspace — sessions, memory, artifact storage, RAG, evaluation, code sandboxes, the example store and registries, the Agent Engine server and deployment — needs the same three pieces. This crate holds the single copy.

## Installation

```toml
[dependencies]
adk-gcp = "3.0"
adk-core = "3.0"
reqwest = { version = "0.12", default-features = false, features = ["json"] }
serde_json = "1"
```

The crate has no feature flags. Applications usually reach it through the Vertex features of
other crates, such as `adk-memory`'s `vertex-memory` or `adk-code`'s `vertex-sandbox`.

## Components

| Type | Purpose |
|------|---------|
| `GcpHttpClient` | ADC (or explicit) credentials with `CacheableResource` header caching, HTTPS-or-loopback endpoint validation, connect/request timeouts, bounded JSON response reads |
| `LroPoller` | `google.longrunning.Operation` polling with capped exponential backoff, operation identity pinning, and project/location scope validation |
| `VertexResourceName` | Parse and format `projects/*/locations/*/reasoningEngines/*` resource names |
| `GcpErrorContext` | Consumer-branded error construction — component, code table, subject, provider — across the `adk_core::AdkError` boundary |

## Error identity

`AdkError` codes are `&'static str`, so each consuming crate declares its code table as a `const` and the shared plumbing stamps every failure with that identity. Errors produced through this crate carry the consuming backend's component, codes, and provider.

```rust
use adk_core::ErrorComponent;
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller};
use serde_json::json;

const CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "memory.vertex.invalid_input",
    unauthorized: "memory.vertex.unauthorized",
    forbidden: "memory.vertex.forbidden",
    not_found: "memory.vertex.not_found",
    rate_limited: "memory.vertex.rate_limited",
    timeout: "memory.vertex.timeout",
    unavailable: "memory.vertex.unavailable",
    credentials_unavailable: "memory.vertex.credentials_unavailable",
    invalid_response: "memory.vertex.invalid_response",
    invalid_request: "memory.vertex.invalid_request",
    upstream_error: "memory.vertex.upstream_error",
    operation_failed: "memory.vertex.operation_failed",
};

async fn generate() -> adk_core::Result<()> {
    let client = GcpHttpClient::builder(
        GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory"),
        "https://us-central1-aiplatform.googleapis.com",
    )
    .build()?;

    let parent = "projects/my-project/locations/us-central1/reasoningEngines/4242";
    let request = client
        .request(reqwest::Method::POST, &format!("{parent}/memories:generate"))
        .await?
        .json(&json!({ "directContentsSource": { "events": [] } }));
    let operation = client.send_value(request).await?;

    LroPoller::new()
        .wait_for_operation(
            &client,
            operation,
            "memories generate",
            false,
            "my-project",
            "us-central1",
        )
        .await?;
    Ok(())
}
```

## Configuration

Defaults mirror the workspace's Vertex backends; every knob is a builder method for deployments that need different bounds (e.g. engine provisioning widens the LRO deadline to 900 s).

| Knob | Default |
|------|---------|
| Connect timeout | 10 s |
| Request timeout | 120 s |
| Credential acquisition timeout | 30 s |
| Max response bytes | 64 MiB |
| API version | `v1beta1` |
| OAuth scope | `https://www.googleapis.com/auth/cloud-platform` |
| LRO deadline / initial delay / delay cap | 120 s / 100 ms / 2 s |

## Security posture

- Endpoints must be bare HTTPS origins (HTTP only to loopback hosts, for tests); userinfo, path, query, and fragment components are rejected.
- Redirects are never followed.
- Operation names are validated against the configured project and location, and polling refuses to follow a changed operation identity.
- Responses are size-bounded at the `Content-Length` declaration and again while streaming.
- Untrusted text is sanitized and truncated before entering error messages.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
