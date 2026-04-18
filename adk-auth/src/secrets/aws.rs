//! AWS Secrets Manager provider.
//!
//! Implements [`SecretProvider`] using the AWS SDK for Secrets Manager.
//!
//! Enable with the `aws-secrets` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::secrets::AwsSecretProvider;
//!
//! // Create with default AWS config (reads from environment / IAM role)
//! let provider = AwsSecretProvider::new().await;
//! let secret = provider.get_secret("my-api-key").await?;
//!
//! // Or create from a pre-configured client
//! let client = aws_sdk_secretsmanager::Client::new(&aws_config);
//! let provider = AwsSecretProvider::from_client(client);
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_trait::async_trait;
use aws_sdk_secretsmanager::Client;

use super::provider::SecretProvider;

/// AWS Secrets Manager implementation of [`SecretProvider`].
///
/// Retrieves secrets from AWS Secrets Manager using the AWS SDK.
/// Supports both default AWS configuration (environment variables, IAM roles,
/// instance profiles) and pre-configured clients.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::AwsSecretProvider;
///
/// let provider = AwsSecretProvider::new().await;
/// let api_key = provider.get_secret("prod/my-service/api-key").await?;
/// ```
pub struct AwsSecretProvider {
    client: Client,
}

impl AwsSecretProvider {
    /// Create a new provider using the default AWS configuration.
    ///
    /// Loads credentials and region from the standard AWS configuration
    /// chain (environment variables, `~/.aws/config`, IAM roles, etc.).
    pub async fn new() -> Self {
        let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        Self { client: Client::new(&config) }
    }

    /// Create a new provider from a pre-configured AWS Secrets Manager client.
    ///
    /// Use this when you need custom client configuration (e.g., a specific
    /// region, endpoint, or credentials provider).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use aws_sdk_secretsmanager::Client;
    ///
    /// let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    /// let client = Client::new(&aws_config);
    /// let provider = AwsSecretProvider::from_client(client);
    /// ```
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }
}

/// Map an AWS Secrets Manager SDK error to an [`AdkError`] with the
/// appropriate error category.
fn map_aws_error(err: aws_sdk_secretsmanager::error::SdkError<impl std::fmt::Debug>) -> AdkError {
    use aws_sdk_secretsmanager::error::SdkError;

    match &err {
        SdkError::ServiceError(service_err) => {
            let raw = service_err.raw();
            let status = raw.status().as_u16();

            match status {
                401 | 403 => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::Unauthorized,
                    "auth.aws_secrets.unauthorized",
                    format!("AWS Secrets Manager authentication failed: {err}"),
                )
                .with_provider("aws-secrets-manager")
                .with_upstream_status(status),

                404 => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::NotFound,
                    "auth.aws_secrets.not_found",
                    format!("AWS secret not found: {err}"),
                )
                .with_provider("aws-secrets-manager")
                .with_upstream_status(status),

                _ => AdkError::new(
                    ErrorComponent::Auth,
                    ErrorCategory::Internal,
                    "auth.aws_secrets.service_error",
                    format!("AWS Secrets Manager service error: {err}"),
                )
                .with_provider("aws-secrets-manager")
                .with_upstream_status(status),
            }
        }

        SdkError::TimeoutError(_) => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "auth.aws_secrets.timeout",
            format!("AWS Secrets Manager request timed out: {err}"),
        )
        .with_provider("aws-secrets-manager"),

        SdkError::DispatchFailure(_) => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "auth.aws_secrets.network",
            format!("AWS Secrets Manager network error: {err}"),
        )
        .with_provider("aws-secrets-manager"),

        _ => AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Internal,
            "auth.aws_secrets.unknown",
            format!("AWS Secrets Manager error: {err}"),
        )
        .with_provider("aws-secrets-manager"),
    }
}

#[async_trait]
impl SecretProvider for AwsSecretProvider {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        let response =
            self.client.get_secret_value().secret_id(name).send().await.map_err(map_aws_error)?;

        response.secret_string().map(|s| s.to_string()).ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.aws_secrets.no_string_value",
                format!("AWS secret '{name}' exists but has no string value (it may be binary)"),
            )
            .with_provider("aws-secrets-manager")
        })
    }
}
