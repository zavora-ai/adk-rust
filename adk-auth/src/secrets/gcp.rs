//! GCP Secret Manager provider.
//!
//! Implements [`SecretProvider`] using the Google Cloud Secret Manager SDK.
//!
//! Enable with the `gcp-secrets` feature flag.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::secrets::GcpSecretProvider;
//!
//! // Create with a project ID (uses Application Default Credentials)
//! let provider = GcpSecretProvider::new("my-gcp-project").await?;
//! let secret = provider.get_secret("my-api-key").await?;
//!
//! // Or create from a pre-configured client
//! let client = google_cloud_secretmanager_v1::client::SecretManagerService::builder()
//!     .build()
//!     .await?;
//! let provider = GcpSecretProvider::from_client(client, "my-gcp-project");
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use async_trait::async_trait;
use google_cloud_secretmanager_v1::client::SecretManagerService;

use super::provider::SecretProvider;

/// GCP Secret Manager implementation of [`SecretProvider`].
///
/// Retrieves secrets from Google Cloud Secret Manager using the official
/// Google Cloud SDK. Supports both default Application Default Credentials
/// and pre-configured clients.
///
/// Secret names are resolved using the format:
/// `projects/{project_id}/secrets/{secret_name}/versions/latest`
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::secrets::GcpSecretProvider;
///
/// let provider = GcpSecretProvider::new("my-gcp-project").await?;
/// let api_key = provider.get_secret("my-api-key").await?;
/// ```
pub struct GcpSecretProvider {
    client: SecretManagerService,
    project_id: String,
}

impl GcpSecretProvider {
    /// Create a new provider with the given GCP project ID.
    ///
    /// Uses Application Default Credentials (ADC) for authentication.
    /// Credentials are loaded from the standard GCP configuration chain
    /// (environment variables, `gcloud auth`, service account keys, etc.).
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] if the client cannot be initialized
    /// (e.g., missing credentials or network issues).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_auth::secrets::GcpSecretProvider;
    ///
    /// let provider = GcpSecretProvider::new("my-gcp-project").await?;
    /// ```
    pub async fn new(project_id: impl Into<String>) -> Result<Self, AdkError> {
        let client = SecretManagerService::builder().build().await.map_err(|err| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.gcp_secrets.init_failed",
                format!("failed to initialize GCP Secret Manager client: {err}"),
            )
            .with_provider("gcp-secret-manager")
        })?;
        Ok(Self { client, project_id: project_id.into() })
    }

    /// Create a new provider from a pre-configured GCP Secret Manager client.
    ///
    /// Use this when you need custom client configuration (e.g., a specific
    /// endpoint, credentials, or client options).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use google_cloud_secretmanager_v1::client::SecretManagerService;
    ///
    /// let client = SecretManagerService::builder().build().await?;
    /// let provider = GcpSecretProvider::from_client(client, "my-gcp-project");
    /// ```
    pub fn from_client(client: SecretManagerService, project_id: impl Into<String>) -> Self {
        Self { client, project_id: project_id.into() }
    }

    /// Build the fully-qualified secret version resource name.
    fn secret_resource_name(&self, secret_name: &str) -> String {
        format!("projects/{}/secrets/{secret_name}/versions/latest", self.project_id)
    }
}

/// Map a GCP SDK error to an [`AdkError`] with the appropriate error category.
///
/// Error mapping:
/// - `Unauthenticated` / `PermissionDenied` → `Unauthorized`
/// - `NotFound` → `NotFound`
/// - `Unavailable` / timeout / exhausted → `Unavailable`
/// - Other → `Internal`
fn map_gcp_error(err: google_cloud_secretmanager_v1::Error) -> AdkError {
    // Check for gRPC status codes first
    if let Some(status) = err.status() {
        use google_cloud_gax::error::rpc::Code;

        return match status.code {
            Code::Unauthenticated | Code::PermissionDenied => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "auth.gcp_secrets.unauthorized",
                format!("GCP Secret Manager authentication failed: {err}"),
            )
            .with_provider("gcp-secret-manager"),

            Code::NotFound => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::NotFound,
                "auth.gcp_secrets.not_found",
                format!("GCP secret not found: {err}"),
            )
            .with_provider("gcp-secret-manager"),

            Code::Unavailable | Code::DeadlineExceeded | Code::ResourceExhausted => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unavailable,
                "auth.gcp_secrets.unavailable",
                format!("GCP Secret Manager unavailable: {err}"),
            )
            .with_provider("gcp-secret-manager"),

            _ => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.gcp_secrets.service_error",
                format!("GCP Secret Manager service error: {err}"),
            )
            .with_provider("gcp-secret-manager"),
        };
    }

    // Check for HTTP status codes (e.g., from proxies or load balancers)
    if let Some(http_status) = err.http_status_code() {
        return match http_status {
            401 | 403 => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "auth.gcp_secrets.unauthorized",
                format!("GCP Secret Manager authentication failed: {err}"),
            )
            .with_provider("gcp-secret-manager")
            .with_upstream_status(http_status),

            404 => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::NotFound,
                "auth.gcp_secrets.not_found",
                format!("GCP secret not found: {err}"),
            )
            .with_provider("gcp-secret-manager")
            .with_upstream_status(http_status),

            429 | 503 => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unavailable,
                "auth.gcp_secrets.unavailable",
                format!("GCP Secret Manager unavailable: {err}"),
            )
            .with_provider("gcp-secret-manager")
            .with_upstream_status(http_status),

            _ => AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.gcp_secrets.service_error",
                format!("GCP Secret Manager service error: {err}"),
            )
            .with_provider("gcp-secret-manager")
            .with_upstream_status(http_status),
        };
    }

    // Timeout and exhausted errors → Unavailable
    if err.is_timeout() || err.is_exhausted() {
        return AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "auth.gcp_secrets.timeout",
            format!("GCP Secret Manager request timed out: {err}"),
        )
        .with_provider("gcp-secret-manager");
    }

    // Authentication errors → Unauthorized
    if err.is_authentication() {
        return AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unauthorized,
            "auth.gcp_secrets.credential",
            format!("GCP Secret Manager credential error: {err}"),
        )
        .with_provider("gcp-secret-manager");
    }

    // IO / connect / transport errors → Unavailable
    if err.is_io() || err.is_connect() || err.is_transport() {
        return AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "auth.gcp_secrets.network",
            format!("GCP Secret Manager network error: {err}"),
        )
        .with_provider("gcp-secret-manager");
    }

    // Fallback for unknown errors
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Internal,
        "auth.gcp_secrets.unknown",
        format!("GCP Secret Manager error: {err}"),
    )
    .with_provider("gcp-secret-manager")
}

#[async_trait]
impl SecretProvider for GcpSecretProvider {
    async fn get_secret(&self, name: &str) -> Result<String, AdkError> {
        let resource_name = self.secret_resource_name(name);

        let response = self
            .client
            .access_secret_version()
            .set_name(&resource_name)
            .send()
            .await
            .map_err(map_gcp_error)?;

        let payload = response.payload.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.gcp_secrets.no_payload",
                format!("GCP secret '{name}' exists but has no payload"),
            )
            .with_provider("gcp-secret-manager")
        })?;

        String::from_utf8(payload.data.to_vec()).map_err(|err| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Internal,
                "auth.gcp_secrets.invalid_utf8",
                format!("GCP secret '{name}' contains invalid UTF-8 data: {err}"),
            )
            .with_provider("gcp-secret-manager")
        })
    }
}
