//! # Secret Provider Example
//!
//! Demonstrates ADK-Rust's secret management — retrieving secrets from a
//! provider at runtime instead of hardcoding API keys.
//!
//! ## What This Shows
//!
//! - Implementing a custom `SecretProvider` that reads secrets from environment
//!   variables (mock/local-dev provider)
//! - Wrapping a provider with `CachedSecretProvider` for TTL-based caching
//! - Using `SecretServiceAdapter` to bridge `SecretProvider` into the runner's
//!   `InvocationContext` so tools can call `ctx.get_secret("name")`
//! - Error handling: requesting a nonexistent secret and inspecting the error
//!   category (`NotFound`)
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the LLM provider)
//! - `SECRET_API_TOKEN` environment variable set (the secret to retrieve)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/secret_provider/Cargo.toml
//! ```

use std::time::Duration;

use adk_auth::secrets::{CachedSecretProvider, SecretProvider, SecretServiceAdapter};
use adk_core::{AdkError, ErrorComponent};
use async_trait::async_trait;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Helper: require an environment variable or exit with a descriptive message
// ---------------------------------------------------------------------------

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {name}\n\
             Set it in your .env file or export it in your shell.\n\
             See .env.example for all required variables."
        )
    })
}

// ---------------------------------------------------------------------------
// EnvSecretProvider — reads secrets from environment variables
// ---------------------------------------------------------------------------

/// A simple `SecretProvider` that reads secrets from environment variables.
///
/// This is a mock/local-dev provider. In production you would use one of the
/// cloud providers (`AwsSecretProvider`, `AzureSecretProvider`,
/// `GcpSecretProvider`) from `adk-auth`.
///
/// The mapping is straightforward: the secret name is uppercased and prefixed
/// with `SECRET_` to form the environment variable name. For example,
/// `get_secret("api_token")` reads `SECRET_API_TOKEN`.
struct EnvSecretProvider;

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        // Map secret name → environment variable name.
        // "api_token" → "SECRET_API_TOKEN"
        let env_var = format!("SECRET_{}", name.to_uppercase());

        tracing::debug!(secret_name = name, env_var = %env_var, "looking up secret");

        std::env::var(&env_var).map_err(|_| {
            AdkError::not_found(
                ErrorComponent::Auth,
                "auth.secret.not_found",
                format!("secret '{name}' not found (env var '{env_var}' is not set)"),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Secret Provider — ADK-Rust v0.7.0       ║");
    println!("╚══════════════════════════════════════════╝\n");

    // Verify the LLM key is available (not used for inference in this example,
    // but listed as a prerequisite for consistency with other examples).
    let _api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // Step 1: Create the base EnvSecretProvider
    // -----------------------------------------------------------------------
    //
    // In production you would swap this for a cloud provider:
    //   let provider = AwsSecretProvider::new().await?;
    //   let provider = GcpSecretProvider::new("my-project").await?;
    //   let provider = AzureSecretProvider::new("https://vault.azure.net").await?;

    println!("--- Step 1: EnvSecretProvider (reads secrets from env vars) ---\n");

    let base_provider = EnvSecretProvider;

    // Retrieve a secret directly from the base provider.
    match base_provider.get_secret("api_token").await {
        Ok(value) => {
            // Mask the value for security — only show the first 4 chars.
            let masked = if value.len() > 4 {
                format!("{}****", &value[..4])
            } else {
                "****".to_string()
            };
            println!("  ✓ Retrieved secret 'api_token': {masked}");
        }
        Err(e) => {
            println!("  ⚠ Could not retrieve 'api_token': {e}");
            println!("    (Set SECRET_API_TOKEN in your .env to see the value)\n");
        }
    }

    // -----------------------------------------------------------------------
    // Step 2: Wrap with CachedSecretProvider (60-second TTL)
    // -----------------------------------------------------------------------
    //
    // CachedSecretProvider caches secret values in memory. Within the TTL
    // window, repeated calls return the cached value without hitting the
    // inner provider. After the TTL expires, the next call refreshes the
    // cache from the inner provider.

    println!("\n--- Step 2: CachedSecretProvider (60s TTL) ---\n");

    let ttl = Duration::from_secs(60);
    let cached_provider = CachedSecretProvider::new(EnvSecretProvider, ttl);
    println!("  Created CachedSecretProvider with TTL = {ttl:?}");

    // First call — cache miss, hits the inner EnvSecretProvider.
    let result1 = cached_provider.get_secret("api_token").await;
    match &result1 {
        Ok(v) => {
            let masked = if v.len() > 4 { format!("{}****", &v[..4]) } else { "****".to_string() };
            println!("  Call 1 (cache miss): {masked}");
        }
        Err(e) => println!("  Call 1 (cache miss): error — {e}"),
    }

    // Second call — cache hit, returns immediately without calling inner.
    let result2 = cached_provider.get_secret("api_token").await;
    match &result2 {
        Ok(v) => {
            let masked = if v.len() > 4 { format!("{}****", &v[..4]) } else { "****".to_string() };
            println!("  Call 2 (cache hit):  {masked}  ← returned from cache, no inner call");
        }
        Err(e) => println!("  Call 2 (cache hit): error — {e}"),
    }

    // Third call — still cached.
    let result3 = cached_provider.get_secret("api_token").await;
    match &result3 {
        Ok(v) => {
            let masked = if v.len() > 4 { format!("{}****", &v[..4]) } else { "****".to_string() };
            println!("  Call 3 (cache hit):  {masked}  ← still cached");
        }
        Err(e) => println!("  Call 3 (cache hit): error — {e}"),
    }

    // -----------------------------------------------------------------------
    // Step 3: SecretServiceAdapter — bridging into InvocationContext
    // -----------------------------------------------------------------------
    //
    // The runner's InvocationContext accepts a `SecretService` (from adk-core).
    // SecretServiceAdapter bridges the adk-auth `SecretProvider` trait into
    // the adk-core `SecretService` trait, so tools can call
    // `ctx.get_secret("name")` during execution.
    //
    // Usage with the runner:
    //   let adapter = Arc::new(SecretServiceAdapter::new(Arc::new(cached_provider)));
    //   let ctx = InvocationContext::new(...)?.with_secret_service(adapter);
    //
    // Once wired, any tool can retrieve secrets:
    //   async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
    //       if let Some(token) = ctx.get_secret("api_token").await? {
    //           // use the token
    //       }
    //   }

    println!("\n--- Step 3: SecretServiceAdapter (bridges to InvocationContext) ---\n");

    let adapter_provider = std::sync::Arc::new(
        CachedSecretProvider::new(EnvSecretProvider, Duration::from_secs(60)),
    );
    let _service = std::sync::Arc::new(SecretServiceAdapter::new(adapter_provider));
    println!("  ✓ Created SecretServiceAdapter");
    println!("    → Wire into runner: ctx = InvocationContext::new(...)?.with_secret_service(service)");
    println!("    → Tools call: ctx.get_secret(\"api_token\").await");

    // -----------------------------------------------------------------------
    // Step 4: Error handling — requesting a nonexistent secret
    // -----------------------------------------------------------------------
    //
    // When a secret doesn't exist, the provider returns an AdkError with
    // category NotFound. The error includes the component (Auth), a machine-
    // readable code, and a human-readable message.

    println!("\n--- Step 4: Error handling (nonexistent secret) ---\n");

    let provider = EnvSecretProvider;
    match provider.get_secret("nonexistent_key").await {
        Ok(value) => {
            println!("  Unexpected success: {value}");
        }
        Err(err) => {
            println!("  ✓ Requesting nonexistent secret returned an error:");
            println!("    Component:  {}", err.component);
            println!("    Category:   {}", err.category);
            println!("    Code:       {}", err.code);
            println!("    Message:    {}", err.message);
            println!("    NotFound?   {}", err.is_not_found());
            println!("    Retryable?  {}", err.is_retryable());
        }
    }

    println!("\n✅ Secret Provider example completed successfully.");
    Ok(())
}
