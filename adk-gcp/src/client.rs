//! Authenticated, bounded HTTP transport for Google Cloud REST APIs.

use crate::error::{GcpErrorContext, truncate_for_error};
use adk_core::Result;
use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_API_VERSION: &str = "v1beta1";

/// Builder for [`GcpHttpClient`].
///
/// Defaults mirror the workspace's Vertex backends: 10 s connect timeout,
/// 120 s request timeout, 30 s credential-acquisition timeout, 64 MiB
/// response cap, API version `v1beta1`, and the `cloud-platform` OAuth scope.
#[derive(Debug)]
pub struct GcpHttpClientBuilder {
    errors: GcpErrorContext,
    endpoint: String,
    api_version: String,
    credentials: Option<Credentials>,
    scopes: Vec<String>,
    connect_timeout: Duration,
    request_timeout: Duration,
    auth_timeout: Duration,
    max_response_bytes: usize,
}

impl GcpHttpClientBuilder {
    /// Sets the API version segment prefixed to every request path.
    #[must_use]
    pub fn api_version(mut self, api_version: impl Into<String>) -> Self {
        self.api_version = api_version.into();
        self
    }

    /// Uses explicit credentials instead of Application Default Credentials.
    #[must_use]
    pub fn credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Overrides the OAuth scopes requested when building ADC.
    ///
    /// Ignored when explicit [`credentials`](Self::credentials) are set.
    #[must_use]
    pub fn scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the TCP connect timeout.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the whole-request timeout, including body streaming.
    #[must_use]
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Sets the credential-header acquisition timeout.
    #[must_use]
    pub fn auth_timeout(mut self, timeout: Duration) -> Self {
        self.auth_timeout = timeout;
        self
    }

    /// Sets the maximum accepted response size in bytes.
    ///
    /// Responses declaring or streaming more than this are rejected. The
    /// caller is responsible for selecting a bound appropriate for the
    /// deployment.
    #[must_use]
    pub fn max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Validates the endpoint, resolves credentials, and builds the client.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin
    /// (HTTPS, or HTTP to a loopback host, with no userinfo, path, query,
    /// or fragment), ADC cannot be constructed, or the redirect-disabled
    /// HTTP client cannot be built.
    pub fn build(self) -> Result<GcpHttpClient> {
        let errors = self.errors;
        let base_url = validate_endpoint(&self.endpoint, &errors)?;

        let credentials = match self.credentials {
            Some(credentials) => credentials,
            None => credentials::Builder::default().with_scopes(&self.scopes).build().map_err(
                |error| {
                    let error = truncate_for_error(&error.to_string());
                    errors.unauthorized(format!(
                        "failed to build {} ADC credentials: {error}",
                        errors.subject(),
                    ))
                },
            )?,
        };

        let http_client = Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                let error = truncate_for_error(&error.without_url().to_string());
                errors.invalid_response(format!(
                    "failed to build bounded {} HTTP client: {error}",
                    errors.subject(),
                ))
            })?;

        Ok(GcpHttpClient {
            http_client,
            base_url,
            api_version: self.api_version,
            credentials,
            auth_headers: Arc::new(RwLock::new(None)),
            request_timeout: self.request_timeout,
            auth_timeout: self.auth_timeout,
            max_response_bytes: self.max_response_bytes,
            errors,
        })
    }
}

/// Authenticated, bounded HTTP client for Google Cloud REST APIs.
///
/// Consolidates the pattern shared by every Vertex backend in the
/// workspace: cached ADC auth headers (honoring `CacheableResource`
/// semantics), a redirect-disabled `reqwest` client with connect and
/// request timeouts, HTTPS-or-loopback endpoint validation, and bounded
/// JSON response reads with typed, consumer-branded errors.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::ErrorComponent;
/// use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient};
/// use serde_json::json;
///
/// const CODES: GcpErrorCodes = GcpErrorCodes {
///     invalid_input: "memory.vertex.invalid_input",
///     unauthorized: "memory.vertex.unauthorized",
///     forbidden: "memory.vertex.forbidden",
///     not_found: "memory.vertex.not_found",
///     rate_limited: "memory.vertex.rate_limited",
///     timeout: "memory.vertex.timeout",
///     unavailable: "memory.vertex.unavailable",
///     credentials_unavailable: "memory.vertex.credentials_unavailable",
///     invalid_response: "memory.vertex.invalid_response",
///     invalid_request: "memory.vertex.invalid_request",
///     upstream_error: "memory.vertex.upstream_error",
///     operation_failed: "memory.vertex.operation_failed",
/// };
///
/// # async fn call() -> adk_core::Result<()> {
/// let client = GcpHttpClient::builder(
///     GcpErrorContext::new(ErrorComponent::Memory, CODES, "vertex memory"),
///     "https://us-central1-aiplatform.googleapis.com",
/// )
/// .build()?;
///
/// let request = client
///     .request(reqwest::Method::POST, "projects/p/locations/l/reasoningEngines/1/memories:generate")
///     .await?
///     .json(&json!({ "scope": { "app_name": "app" } }));
/// let operation = client.send_value(request).await?;
/// # let _ = operation;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct GcpHttpClient {
    http_client: Client,
    base_url: Url,
    api_version: String,
    credentials: Credentials,
    auth_headers: Arc<RwLock<Option<HeaderMap>>>,
    request_timeout: Duration,
    auth_timeout: Duration,
    max_response_bytes: usize,
    errors: GcpErrorContext,
}

impl GcpHttpClient {
    /// Starts building a client for the given error identity and endpoint.
    pub fn builder(errors: GcpErrorContext, endpoint: impl Into<String>) -> GcpHttpClientBuilder {
        GcpHttpClientBuilder {
            errors,
            endpoint: endpoint.into(),
            api_version: DEFAULT_API_VERSION.to_string(),
            credentials: None,
            scopes: vec![CLOUD_PLATFORM_SCOPE.to_string()],
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            auth_timeout: DEFAULT_AUTH_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    /// The error identity this client stamps on failures.
    pub fn errors(&self) -> &GcpErrorContext {
        &self.errors
    }

    /// The configured maximum response size in bytes.
    pub fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// Overrides the maximum accepted response size after construction.
    ///
    /// Lets consumers expose their own post-construction bound override
    /// without rebuilding credentials. The caller is responsible for
    /// selecting a bound appropriate for the deployment.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    /// Builds the absolute URL for an API path (version prefix applied).
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting URL cannot be constructed.
    pub fn url(&self, path: &str) -> Result<Url> {
        let mut url = self.base_url.clone();
        url.set_path(&format!("/{}/{}", self.api_version, path.trim_start_matches('/')));
        Ok(url)
    }

    /// Builds an authorized request for an API path.
    ///
    /// # Errors
    ///
    /// Returns an error when credential headers cannot be acquired within
    /// the configured auth timeout.
    pub async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = self.url(path)?;
        let request = self.http_client.request(method, url);
        self.apply_auth(request).await
    }

    /// Applies cached credential headers to a request.
    ///
    /// # Errors
    ///
    /// Returns an error when credential headers cannot be acquired within
    /// the configured auth timeout.
    pub async fn apply_auth(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let headers = self.auth_headers().await?;
        Ok(request.headers(headers))
    }

    /// Acquires credential headers, serving cached headers on `NotModified`.
    ///
    /// # Errors
    ///
    /// Returns an error when acquisition times out, the credential source
    /// fails, or the source reports `NotModified` before any headers were
    /// cached.
    pub async fn auth_headers(&self) -> Result<HeaderMap> {
        let cacheable_headers =
            tokio::time::timeout(self.auth_timeout, self.credentials.headers(Default::default()))
                .await
                .map_err(|_| {
                    self.errors.timeout(format!(
                        "{} credential header acquisition timed out after {} seconds",
                        self.errors.subject(),
                        self.auth_timeout.as_secs_f64(),
                    ))
                })?
                .map_err(|error| self.errors.credentials_error(&error))?;

        match cacheable_headers {
            CacheableResource::New { data, .. } => {
                *self.auth_headers.write().await = Some(data.clone());
                Ok(data)
            }
            CacheableResource::NotModified => {
                self.auth_headers.read().await.clone().ok_or_else(|| {
                    self.errors.unauthorized(
                        "google cloud credentials returned NotModified before any cached auth headers were available",
                    )
                })
            }
        }
    }

    /// Sends a request and parses the response as JSON.
    ///
    /// Empty or whitespace-only success bodies parse as an empty object.
    ///
    /// # Errors
    ///
    /// Returns an error when the request times out, transport fails, the
    /// response exceeds the configured size bound, the status is not a
    /// success, or the body is not valid JSON.
    pub async fn send_value(&self, request: RequestBuilder) -> Result<Value> {
        match self.send_value_internal(request, false).await? {
            Some((value, _)) => Ok(value),
            None => Ok(Value::Object(Map::new())),
        }
    }

    /// Sends a request, returning the parsed JSON and the decoded body size.
    ///
    /// The byte count lets paginated callers enforce an aggregate response
    /// bound across pages.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`send_value`](Self::send_value).
    pub async fn send_value_counted(&self, request: RequestBuilder) -> Result<(Value, usize)> {
        match self.send_value_internal(request, false).await? {
            Some(value) => Ok(value),
            None => Ok((Value::Object(Map::new()), 0)),
        }
    }

    /// Sends a request, mapping `404 Not Found` to `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`send_value`](Self::send_value), except
    /// a 404 status which becomes `Ok(None)`.
    pub async fn send_value_allow_not_found(
        &self,
        request: RequestBuilder,
    ) -> Result<Option<Value>> {
        self.send_value_internal(request, true).await.map(|option| option.map(|(value, _)| value))
    }

    async fn send_value_internal(
        &self,
        request: RequestBuilder,
        allow_not_found: bool,
    ) -> Result<Option<(Value, usize)>> {
        let (status, body) = tokio::time::timeout(self.request_timeout, async {
            let mut response =
                request.send().await.map_err(|error| self.errors.transport_error(error))?;
            let status = response.status();
            if let Some(declared) = response.content_length()
                && declared > self.max_response_bytes as u64
            {
                return Err(self
                    .errors
                    .response_too_large(
                        "response Content-Length",
                        self.max_response_bytes,
                        declared,
                    )
                    .with_upstream_status(status.as_u16()));
            }
            let capacity = response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or_default();
            let mut body = Vec::with_capacity(capacity);
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                let timeout = error.is_timeout();
                let detail = truncate_for_error(&error.without_url().to_string());
                let error = if timeout {
                    self.errors.timeout(format!(
                        "{} response body timed out: {detail}",
                        self.errors.subject(),
                    ))
                } else {
                    self.errors.unavailable(format!(
                        "failed to read {} response body: {detail}",
                        self.errors.subject(),
                    ))
                };
                error.with_upstream_status(status.as_u16())
            })? {
                let observed = body.len().checked_add(chunk.len()).ok_or_else(|| {
                    self.errors
                        .response_too_large("response body", self.max_response_bytes, u64::MAX)
                        .with_upstream_status(status.as_u16())
                })?;
                if observed > self.max_response_bytes {
                    return Err(self
                        .errors
                        .response_too_large(
                            "response body",
                            self.max_response_bytes,
                            observed as u64,
                        )
                        .with_upstream_status(status.as_u16()));
                }
                body.extend_from_slice(&chunk);
            }
            Ok::<_, adk_core::AdkError>((status, body))
        })
        .await
        .map_err(|_| {
            self.errors.timeout(format!(
                "{} request timed out after {} seconds",
                self.errors.subject(),
                self.request_timeout.as_secs(),
            ))
        })??;

        if allow_not_found && status == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !status.is_success() {
            let body = String::from_utf8_lossy(&body);
            let body = if body.trim().is_empty() { "<empty body>" } else { body.as_ref() };
            return Err(self.errors.status_error(status, body));
        }

        let body_len = body.len();
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Some((Value::Object(Map::new()), body_len)));
        }

        let value = serde_json::from_slice(&body).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            self.errors
                .invalid_response(format!(
                    "failed to parse {} response JSON: {error}",
                    self.errors.subject(),
                ))
                .with_upstream_status(status.as_u16())
        })?;
        Ok(Some((value, body_len)))
    }
}

/// Validates an endpoint as a bare secure origin.
///
/// Requires HTTPS (or HTTP to a loopback host, for tests) and rejects
/// userinfo, path, query, and fragment components, so credentialed traffic
/// can never be redirected or downgraded by configuration.
fn validate_endpoint(endpoint: &str, errors: &GcpErrorContext) -> Result<Url> {
    let url = Url::parse(endpoint).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        errors.invalid_input(format!("invalid {} endpoint URL: {error}", errors.subject()))
    })?;
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
        return Err(errors.invalid_input(format!(
            "{} endpoint must use HTTPS for secure transmission",
            errors.subject(),
        )));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(errors.invalid_input(format!(
            "{} endpoint must be an origin without userinfo, path, query, or fragment",
            errors.subject(),
        )));
    }
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GcpErrorCodes;
    use adk_core::{ErrorCategory, ErrorComponent};

    const CODES: GcpErrorCodes = GcpErrorCodes {
        invalid_input: "session.vertex.invalid_input",
        unauthorized: "session.vertex.unauthorized",
        forbidden: "session.vertex.forbidden",
        not_found: "session.vertex.not_found",
        rate_limited: "session.vertex.rate_limited",
        timeout: "session.vertex.timeout",
        unavailable: "session.vertex.unavailable",
        credentials_unavailable: "session.vertex.credentials_unavailable",
        invalid_response: "session.vertex.invalid_response",
        invalid_request: "session.vertex.invalid_request",
        upstream_error: "session.vertex.upstream_error",
        operation_failed: "session.vertex.operation_failed",
    };

    fn errors() -> GcpErrorContext {
        GcpErrorContext::new(ErrorComponent::Session, CODES, "vertex session")
    }

    #[test]
    fn endpoint_validation_requires_secure_origins() {
        assert!(
            validate_endpoint("https://us-central1-aiplatform.googleapis.com", &errors()).is_ok()
        );
        assert!(validate_endpoint("http://127.0.0.1:8080", &errors()).is_ok());
        assert!(validate_endpoint("http://localhost:8080", &errors()).is_ok());

        let rejected = [
            "http://example.com",
            "ftp://example.com",
            "https://user:pass@example.com",
            "https://example.com/path",
            "https://example.com?query=1",
            "https://example.com#fragment",
            "not a url",
        ];
        for endpoint in rejected {
            let error = validate_endpoint(endpoint, &errors()).unwrap_err();
            assert_eq!(error.category, ErrorCategory::InvalidInput, "accepted {endpoint:?}");
        }
    }

    #[tokio::test]
    async fn url_prefixes_the_api_version() {
        let client = GcpHttpClient::builder(errors(), "https://example.googleapis.com")
            .api_version("v1")
            .credentials(
                google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build(),
            )
            .build()
            .unwrap();
        let url = client.url("projects/p/locations/l/reasoningEngines/1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.googleapis.com/v1/projects/p/locations/l/reasoningEngines/1",
        );
    }
}
