use reqwest::{ClientBuilder, Url};
use snafu::ResultExt;
use std::sync::{Arc, LazyLock};

use crate::backend::studio::{
    AuthConfig, ServiceAccountKey, ServiceAccountTokenSource, StudioBackend,
};
#[cfg(feature = "vertex")]
use crate::backend::vertex::{GoogleCloudAuth, VertexBackend};
use crate::client::GeminiClient;
use crate::common::Model;
use crate::error::*;

static DEFAULT_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1beta/")
        .expect("unreachable error: failed to parse default base URL")
});

enum AuthMode {
    ApiKey(String),
    ServiceAccount(String),
    #[cfg(feature = "vertex")]
    Adc,
    #[cfg(feature = "vertex")]
    Wif(String),
    #[cfg(feature = "vertex")]
    Credentials(google_cloud_auth::credentials::Credentials),
    None,
}

#[cfg(feature = "vertex")]
struct VertexConfig {
    project_id: String,
    location: String,
}

/// A builder for the `Gemini` client.
pub struct GeminiBuilder {
    model: Model,
    client_builder: ClientBuilder,
    base_url: Url,
    auth: AuthMode,
    #[cfg(feature = "vertex")]
    vertex_config: Option<VertexConfig>,
}

impl GeminiBuilder {
    /// Creates a new `GeminiBuilder` with the given API key.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            model: Model::default(),
            client_builder: ClientBuilder::default(),
            base_url: DEFAULT_BASE_URL.clone(),
            auth: AuthMode::ApiKey(key.into()),
            #[cfg(feature = "vertex")]
            vertex_config: None,
        }
    }

    /// Creates a new `GeminiBuilder` without an API key.
    pub fn new_without_api_key() -> Self {
        Self {
            model: Model::default(),
            client_builder: ClientBuilder::default(),
            base_url: DEFAULT_BASE_URL.clone(),
            auth: AuthMode::None,
            #[cfg(feature = "vertex")]
            vertex_config: None,
        }
    }

    /// Sets the model for the client.
    pub fn with_model(mut self, model: impl Into<Model>) -> Self {
        self.model = model.into();
        self
    }

    /// Sets a custom `reqwest::ClientBuilder`.
    pub fn with_http_client(mut self, client_builder: ClientBuilder) -> Self {
        self.client_builder = client_builder;
        self
    }

    /// Sets a custom base URL for the API.
    pub fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = base_url;
        self
    }

    /// Configures the client to use a service account JSON key for authentication.
    pub fn with_service_account_json(mut self, service_account_json: &str) -> Result<Self, Error> {
        // Validate JSON
        let _ = serde_json::from_str::<serde_json::Value>(service_account_json)
            .context(ServiceAccountKeyParseSnafu)?;
        self.auth = AuthMode::ServiceAccount(service_account_json.to_string());
        Ok(self)
    }

    /// Configures the client to use Vertex AI (Google Cloud) endpoints.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud(
        mut self,
        project_id: impl Into<String>,
        location: impl Into<String>,
    ) -> Self {
        self.vertex_config =
            Some(VertexConfig { project_id: project_id.into(), location: location.into() });
        self
    }

    /// Configures the client to use Vertex AI with ADC.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_adc(mut self) -> Result<Self, Error> {
        self.auth = AuthMode::Adc;
        Ok(self)
    }

    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_wif_json(mut self, wif_json: &str) -> Result<Self, Error> {
        let _ = serde_json::from_str::<serde_json::Value>(wif_json)
            .context(GoogleCloudCredentialParseSnafu)?;
        self.auth = AuthMode::Wif(wif_json.to_string());
        Ok(self)
    }

    /// Configures the client with pre-built Google Cloud credentials.
    #[cfg(feature = "vertex")]
    pub fn with_credentials(
        mut self,
        credentials: google_cloud_auth::credentials::Credentials,
    ) -> Self {
        self.auth = AuthMode::Credentials(credentials);
        self
    }

    pub fn build(self) -> Result<GeminiClient, Error> {
        // DECISION LOGIC:

        #[cfg(feature = "vertex")]
        if let Some(config) = self.vertex_config {
            let auth = match self.auth {
                AuthMode::ApiKey(key) => GoogleCloudAuth::ApiKey(key),
                AuthMode::ServiceAccount(json) => GoogleCloudAuth::ServiceAccountJson(json),
                AuthMode::Adc => GoogleCloudAuth::Adc,
                AuthMode::Wif(json) => GoogleCloudAuth::WifJson(json),
                AuthMode::Credentials(c) => GoogleCloudAuth::Credentials(c),
                AuthMode::None => {
                    return Err(Error::Configuration {
                        message: "Vertex AI requires authentication".to_string(),
                    });
                }
            };

            let endpoint = format!("https://{}-aiplatform.googleapis.com", config.location);
            let backend = VertexBackend::new(
                endpoint,
                config.project_id,
                config.location,
                auth,
                self.model.clone(),
            )?;

            return Ok(GeminiClient::with_backend(Arc::new(backend)));
        }

        // 2. Otherwise, use StudioBackend
        let auth_config = match self.auth {
            AuthMode::ApiKey(key) => AuthConfig::ApiKey(key),
            AuthMode::ServiceAccount(json) => {
                let key: ServiceAccountKey =
                    serde_json::from_str(&json).context(ServiceAccountKeyParseSnafu)?;
                let source = ServiceAccountTokenSource::new(key);
                AuthConfig::ServiceAccount(source)
            }
            #[cfg(feature = "vertex")]
            AuthMode::Adc | AuthMode::Wif(_) | AuthMode::Credentials(_) => {
                return Err(Error::Configuration { message: "Selected auth mode requires Vertex AI configuration (call .with_google_cloud())".to_string() });
            }
            AuthMode::None => return Err(Error::MissingApiKey),
        };

        let mut headers = reqwest::header::HeaderMap::new();
        if let AuthConfig::ApiKey(ref key) = auth_config {
            headers.insert(
                "x-goog-api-key",
                reqwest::header::HeaderValue::from_str(key).context(InvalidApiKeySnafu)?,
            );
        }

        let http_client =
            self.client_builder.default_headers(headers).build().context(PerformRequestNewSnafu)?;

        let backend =
            StudioBackend::new_with_client(http_client, self.base_url, self.model, auth_config);

        Ok(GeminiClient::with_backend(Arc::new(backend)))
    }
}
