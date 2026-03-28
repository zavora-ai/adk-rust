//! Configuration types for the OpenRouter provider.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// Default OpenRouter API base URL.
pub const OPENROUTER_API_BASE: &str = "https://openrouter.ai/api/v1";

const HTTP_REFERER_HEADER: &str = "http-referer";
const X_OPENROUTER_TITLE_HEADER: &str = "x-openrouter-title";
const X_TITLE_HEADER: &str = "x-title";

/// Default API surface used by the `Llm` adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenRouterApiMode {
    #[default]
    ChatCompletions,
    Responses,
}

/// OpenRouter configuration shared by native APIs and the `Llm` adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterConfig {
    /// OpenRouter API key.
    pub api_key: String,
    /// Default model name.
    pub model: String,
    /// Base URL for the OpenRouter API.
    pub base_url: String,
    /// Optional site URL sent as `HTTP-Referer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_referer: Option<String>,
    /// Optional app title sent as `X-OpenRouter-Title` and `X-Title`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Default API mode used when the generic `Llm` adapter is invoked.
    #[serde(default)]
    pub default_api_mode: OpenRouterApiMode,
}

impl OpenRouterConfig {
    /// Create a new OpenRouter config using the default API base URL.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: OPENROUTER_API_BASE.to_string(),
            http_referer: None,
            title: None,
            default_api_mode: OpenRouterApiMode::default(),
        }
    }

    /// Override the API base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Set the optional `HTTP-Referer` attribution header.
    pub fn with_http_referer(mut self, http_referer: impl Into<String>) -> Self {
        self.http_referer = Some(http_referer.into());
        self
    }

    /// Set the optional OpenRouter app title header.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set the default API mode used by the generic `Llm` adapter.
    pub fn with_default_api_mode(mut self, default_api_mode: OpenRouterApiMode) -> Self {
        self.default_api_mode = default_api_mode;
        self
    }

    /// Return the normalized API base URL without a trailing slash.
    pub fn effective_base_url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// Build an absolute endpoint URL from a relative OpenRouter API path.
    pub fn endpoint_url(&self, path: &str) -> String {
        format!("{}/{}", self.effective_base_url(), path.trim_start_matches('/'))
    }

    /// Build the shared default headers used for all OpenRouter requests.
    pub fn default_headers(&self) -> Result<HeaderMap, AdkError> {
        let mut headers = HeaderMap::new();

        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|err| {
                invalid_header_error(
                    "authorization",
                    "OpenRouter API key produced an invalid Authorization header",
                )
                .with_source(err)
            })?;
        authorization.set_sensitive(true);

        headers.insert(AUTHORIZATION, authorization);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(http_referer) = &self.http_referer {
            headers.insert(
                HeaderName::from_static(HTTP_REFERER_HEADER),
                HeaderValue::from_str(http_referer).map_err(|err| {
                    invalid_header_error(
                        "http_referer",
                        "OpenRouter HTTP-Referer header contains invalid characters",
                    )
                    .with_source(err)
                })?,
            );
        }

        if let Some(title) = &self.title {
            let title_value = HeaderValue::from_str(title).map_err(|err| {
                invalid_header_error("title", "OpenRouter title header contains invalid characters")
                    .with_source(err)
            })?;

            headers.insert(HeaderName::from_static(X_OPENROUTER_TITLE_HEADER), title_value.clone());
            headers.insert(HeaderName::from_static(X_TITLE_HEADER), title_value);
        }

        Ok(headers)
    }
}

fn invalid_header_error(field: &'static str, message: &'static str) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.openrouter.invalid_header",
        format!("{message}: {field}"),
    )
    .with_provider("openrouter")
}

#[cfg(test)]
mod tests {
    use super::{
        HTTP_REFERER_HEADER, OpenRouterApiMode, OpenRouterConfig, X_OPENROUTER_TITLE_HEADER,
        X_TITLE_HEADER,
    };
    use reqwest::header::{AUTHORIZATION, HeaderName};

    #[test]
    fn default_headers_include_authorization_and_attribution_headers() {
        let config = OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2")
            .with_http_referer("https://example.com")
            .with_title("Example App");

        let headers = config.default_headers().expect("headers should build");

        assert_eq!(
            headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok()),
            Some("Bearer sk-or-test")
        );
        assert_eq!(
            headers
                .get(HeaderName::from_static(HTTP_REFERER_HEADER))
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com")
        );
        assert_eq!(
            headers
                .get(HeaderName::from_static(X_OPENROUTER_TITLE_HEADER))
                .and_then(|value| value.to_str().ok()),
            Some("Example App")
        );
        assert_eq!(
            headers
                .get(HeaderName::from_static(X_TITLE_HEADER))
                .and_then(|value| value.to_str().ok()),
            Some("Example App")
        );
    }

    #[test]
    fn endpoint_url_normalizes_trailing_slashes() {
        let config = OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2")
            .with_base_url("https://openrouter.ai/api/v1/");

        assert_eq!(
            config.endpoint_url("/chat/completions"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn config_builder_sets_default_api_mode() {
        let config = OpenRouterConfig::new("sk-or-test", "openai/gpt-5.2")
            .with_default_api_mode(OpenRouterApiMode::Responses);

        assert_eq!(config.default_api_mode, OpenRouterApiMode::Responses);
    }
}
