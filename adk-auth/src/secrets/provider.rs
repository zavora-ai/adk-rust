//! Secret provider trait definition.
//!
//! The [`SecretProvider`] trait defines the interface for retrieving secrets
//! from external secret management services.

use adk_core::AdkError;
use async_trait::async_trait;

/// Trait for retrieving secrets from an external secret management service.
///
/// Implementations are provided for AWS Secrets Manager (`aws-secrets` feature),
/// Azure Key Vault (`azure-keyvault` feature), and GCP Secret Manager
/// (`gcp-secrets` feature).
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::SecretProvider;
///
/// async fn use_secret(provider: &dyn SecretProvider) -> Result<(), AdkError> {
///     let api_key = provider.get_secret("my-api-key").await?;
///     println!("retrieved secret of length {}", api_key.len());
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Retrieve a secret value by name.
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] with the appropriate error category:
    /// - `Unauthorized` for authentication failures
    /// - `Unavailable` for network errors
    /// - `NotFound` when the secret does not exist
    async fn get_secret(&self, name: &str) -> Result<String, AdkError>;
}

/// Adapter that wraps a [`SecretProvider`] as a
/// [`SecretService`](adk_core::SecretService) for use with the runner's
/// [`InvocationContext`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::{SecretProvider, SecretServiceAdapter};
/// use adk_runner::InvocationContext;
/// use std::sync::Arc;
///
/// let provider: Arc<dyn SecretProvider> = /* ... */;
/// let service = Arc::new(SecretServiceAdapter::new(provider));
/// let ctx = InvocationContext::new(/* ... */)?.with_secret_service(service);
/// ```
pub struct SecretServiceAdapter {
    inner: std::sync::Arc<dyn SecretProvider>,
}

impl SecretServiceAdapter {
    /// Create a new adapter wrapping the given secret provider.
    pub fn new(provider: std::sync::Arc<dyn SecretProvider>) -> Self {
        Self { inner: provider }
    }
}

#[async_trait]
impl adk_core::SecretService for SecretServiceAdapter {
    async fn get_secret(&self, name: &str) -> adk_core::Result<String> {
        self.inner.get_secret(name).await
    }
}
