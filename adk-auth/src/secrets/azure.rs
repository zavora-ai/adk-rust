//! Azure Key Vault provider.
//!
//! Implements [`SecretProvider`] using the Azure SDK for Key Vault Secrets.
//!
//! Enable with the `azure-keyvault` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::secrets::AzureSecretProvider;
//!
//! // Create from a pre-configured SecretClient
//! let credential = azure_identity::DeveloperToolsCredential::new(None).unwrap();
//! let client = azure_security_keyvault_secrets::SecretClient::new(
//!     "https://my-vault.vault.azure.net/",
//!     credential.clone(),
//!     None,
//! ).unwrap();
//! let provider = AzureSecretProvider::from_client(client);
//! let secret = provider.get_secret("my-api-key").await?;
//!
//! // Or create with a vault URL and credential
//! let provider = AzureSecretProvider::new(
//!     "https://my-vault.vault.azure.net/",
//!     credential.clone(),
//! ).unwrap();
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_trait::async_trait;
use azure_security_keyvault_secrets::SecretClient;
use std::sync::Arc;

use super::provider::SecretProvider;

/// Azure Key Vault implementation of [`SecretProvider`].
///
/// Retrieves secrets from Azure Key Vault using the Azure SDK.
/// Supports both pre-configured clients and construction from a vault URL
/// with an Azure credential.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::AzureSecretProvider;
///
/// let provider = AzureSecretProvider::from_client(client);
/// let api_key = provider.get_secret("prod/my-service/api-key").await?;
/// ```
pub struct AzureSecretProvider {
    client: SecretClient,
}

impl AzureSecretProvider {
    /// Create a new provider with a vault URL and Azure credential.
    ///
    /// The credential must implement `azure_core::credentials::TokenCredential`
    /// (from the `azure_core` version used by `azure_security_keyvault_secrets`).
    ///
    /// # Arguments
    ///
    /// * `vault_url` — The Key Vault URL (e.g., `https://my-vault.vault.azure.net/`).
    /// * `credential` — An Azure token credential for authentication.
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] if the vault URL is invalid.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use azure_identity::DeveloperToolsCredential;
    /// use adk_auth::secrets::AzureSecretProvider;
    ///
    /// let credential = DeveloperToolsCredential::new(None)?;
    /// let provider = AzureSecretProvider::new(
    ///     "https://my-vault.vault.azure.net/",
    ///     credential.clone(),
    /// )?;
    /// ```
    pub fn new(
        vault_url: &str,
        credential: Arc<dyn azure_core::credentials::TokenCredential>,
    ) -> Result<Self, AdkError> {
        let client = SecretClient::new(vault_url, credential, None).map_err(|err| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.azure_keyvault.invalid_config",
                format!("failed to create Azure Key Vault client: {err}"),
            )
            .with_provider("azure-keyvault")
        })?;
        Ok(Self { client })
    }

    /// Create a new provider from a pre-configured Azure Key Vault `SecretClient`.
    ///
    /// Use this when you need custom client configuration (e.g., a specific
    /// API version, client options, or credential provider).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use azure_security_keyvault_secrets::SecretClient;
    /// use azure_identity::DeveloperToolsCredential;
    ///
    /// let credential = DeveloperToolsCredential::new(None)?;
    /// let client = SecretClient::new(
    ///     "https://my-vault.vault.azure.net/",
    ///     credential.clone(),
    ///     None,
    /// )?;
    /// let provider = AzureSecretProvider::from_client(client);
    /// ```
    pub fn from_client(client: SecretClient) -> Self {
        Self { client }
    }
}

/// Map an Azure SDK error to an [`AdkError`] with the appropriate error category.
///
/// Error mapping:
/// - HTTP 401/403 → `Unauthorized`
/// - HTTP 404 → `NotFound`
/// - `Credential` errors → `Unauthorized`
/// - `Io` errors → `Unavailable`
/// - Other HTTP errors → `Internal`
fn map_azure_error(err: azure_core::Error) -> AdkError {
    use azure_core::error::ErrorKind;

    match err.kind() {
        ErrorKind::HttpResponse { status, .. } => {
            let status_code = u16::from(*status);
            match status_code {
                401 | 403 => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::Unauthorized,
                    "auth.azure_keyvault.unauthorized",
                    format!("Azure Key Vault authentication failed: {err}"),
                )
                .with_provider("azure-keyvault")
                .with_upstream_status(status_code),

                404 => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::NotFound,
                    "auth.azure_keyvault.not_found",
                    format!("Azure Key Vault secret not found: {err}"),
                )
                .with_provider("azure-keyvault")
                .with_upstream_status(status_code),

                _ => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::Internal,
                    "auth.azure_keyvault.service_error",
                    format!("Azure Key Vault service error: {err}"),
                )
                .with_provider("azure-keyvault")
                .with_upstream_status(status_code),
            }
        }

        ErrorKind::Credential => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unauthorized,
            "auth.azure_keyvault.credential",
            format!("Azure Key Vault credential error: {err}"),
        )
        .with_provider("azure-keyvault"),

        ErrorKind::Io => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "auth.azure_keyvault.network",
            format!("Azure Key Vault network error: {err}"),
        )
        .with_provider("azure-keyvault"),

        _ => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Internal,
            "auth.azure_keyvault.unknown",
            format!("Azure Key Vault error: {err}"),
        )
        .with_provider("azure-keyvault"),
    }
}

#[async_trait]
impl SecretProvider for AzureSecretProvider {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        let response = self.client.get_secret(name, "", None).await.map_err(map_azure_error)?;

        let secret_bundle = response.into_body().await.map_err(|err| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.azure_keyvault.response_parse",
                format!("failed to parse Azure Key Vault response: {err}"),
            )
            .with_provider("azure-keyvault")
        })?;

        secret_bundle.value.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.azure_keyvault.no_value",
                format!("Azure Key Vault secret '{name}' exists but has no value"),
            )
            .with_provider("azure-keyvault")
        })
    }
}
