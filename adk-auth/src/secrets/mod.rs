//! Cloud secret manager integration for ADK agents.
//!
//! Provides the [`SecretProvider`] trait and cloud-specific implementations
//! for retrieving secrets from external secret management services.
//!
//! ## Feature Flags
//!
//! - `aws-secrets` — AWS Secrets Manager provider
//! - `azure-keyvault` — Azure Key Vault provider
//! - `gcp-secrets` — GCP Secret Manager provider
//!
//! ## Example
//!
//! ```rust,ignore
//! use adk_auth::secrets::SecretProvider;
//!
//! let secret = provider.get_secret("my-api-key").await?;
//! ```

pub mod cached;
pub mod provider;

#[cfg(feature = "aws-secrets")]
pub mod aws;

#[cfg(feature = "azure-keyvault")]
pub mod azure;

#[cfg(feature = "gcp-secrets")]
pub mod gcp;

pub use cached::CachedSecretProvider;
pub use provider::{SecretProvider, SecretServiceAdapter};

#[cfg(feature = "aws-secrets")]
pub use aws::AwsSecretProvider;

#[cfg(feature = "azure-keyvault")]
pub use azure::AzureSecretProvider;

#[cfg(feature = "gcp-secrets")]
pub use gcp::GcpSecretProvider;
