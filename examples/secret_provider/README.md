# Secret Provider Example

Demonstrates ADK-Rust's secret management — retrieving secrets from a provider at runtime instead of hardcoding API keys.

## What This Shows

- Implementing a custom `SecretProvider` that reads secrets from environment variables (mock/local-dev provider)
- Wrapping a provider with `CachedSecretProvider` for TTL-based caching (60-second TTL)
- Using `SecretServiceAdapter` to bridge `SecretProvider` into the runner's `InvocationContext` so tools can call `ctx.get_secret("name")`
- Error handling: requesting a nonexistent secret and inspecting the error category (`NotFound`)

## Prerequisites

- Rust 1.85+
- `GOOGLE_API_KEY` environment variable set

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key |
| `SECRET_API_TOKEN` | No | Example secret value (demonstrates retrieval) |
| `RUST_LOG` | No | Log level filter (default: `info`) |

## Run

```bash
cargo run --manifest-path examples/secret_provider/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Secret Provider — ADK-Rust v0.7.0       ║
╚══════════════════════════════════════════╝

--- Step 1: EnvSecretProvider (reads secrets from env vars) ---

  ✓ Retrieved secret 'api_token': my-s****

--- Step 2: CachedSecretProvider (60s TTL) ---

  Created CachedSecretProvider with TTL = 60s
  Call 1 (cache miss): my-s****
  Call 2 (cache hit):  my-s****  ← returned from cache, no inner call
  Call 3 (cache hit):  my-s****  ← still cached

--- Step 3: SecretServiceAdapter (bridges to InvocationContext) ---

  ✓ Created SecretServiceAdapter
    → Wire into runner: ctx = InvocationContext::new(...)?.with_secret_service(service)
    → Tools call: ctx.get_secret("api_token").await

--- Step 4: Error handling (nonexistent secret) ---

  ✓ Requesting nonexistent secret returned an error:
    Component:  auth
    Category:   not_found
    Code:       auth.secret.not_found
    Message:    secret 'nonexistent_key' not found (env var 'SECRET_NONEXISTENT_KEY' is not set)
    NotFound?   true
    Retryable?  false

✅ Secret Provider example completed successfully.
```

## Secret Provider Architecture

```
┌─────────────────────────────────────────────────┐
│                  Tool Execution                  │
│  ctx.get_secret("api_token") → Option<String>   │
└──────────────────────┬──────────────────────────┘
                       │
              ┌────────▼────────┐
              │ SecretService   │  (adk-core trait)
              │ Adapter         │
              └────────┬────────┘
                       │
           ┌───────────▼───────────┐
           │ CachedSecretProvider  │  TTL-based cache
           └───────────┬───────────┘
                       │
           ┌───────────▼───────────┐
           │  EnvSecretProvider    │  Reads from env vars
           │  (or cloud provider)  │  (AWS / Azure / GCP)
           └───────────────────────┘
```

In production, replace `EnvSecretProvider` with a cloud provider:

```rust
// AWS Secrets Manager
let provider = AwsSecretProvider::new().await?;

// GCP Secret Manager
let provider = GcpSecretProvider::new("my-project").await?;

// Azure Key Vault
let provider = AzureSecretProvider::new("https://vault.azure.net").await?;
```
