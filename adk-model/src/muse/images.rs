//! Image generation and editing via the Meta Model API (`muse-image-1.0`).
//!
//! OpenAI Images-compatible endpoints:
//! - `POST /v1/images/generations` (text-to-image)
//! - `POST /v1/images/edits` (image edit/compose, multipart)
//!
//! Every call is metered against the current run ([`adk_core::meter_usage`]): the spend
//! hold covers the requested image count at the catalog's per-image price, and the images
//! returned are committed. A call refused by the run's budget or spend ledger is not sent.

use super::config::MuseConfig;
use crate::pricing::PricingCatalog;
use crate::retry::{RetryConfig, execute_with_retry, is_retryable_model_error};
use adk_core::{AdkError, ErrorComponent, MeteredUsage, UsageCall, UsageUnit, UsageVector};
use reqwest::Client;
use serde::Deserialize;

/// Model served by the Images endpoints.
pub const MUSE_IMAGE_MODEL: &str = "muse-image-1.0";

/// One generated or edited image.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageData {
    /// Base64 PNG/JPEG bytes, when `response_format` is `b64_json` (the default).
    #[serde(default)]
    pub b64_json: Option<String>,
    /// Hosted URL, when `response_format` is `url`.
    #[serde(default)]
    pub url: Option<String>,
    /// The prompt the model used after any rewrite.
    #[serde(default)]
    pub revised_prompt: Option<String>,
}

impl ImageData {
    /// Decoded image bytes, for `b64_json` results.
    ///
    /// # Errors
    ///
    /// Returns `model.muse.image_decode` when the result carries no base64 body or
    /// the body is not valid base64.
    pub fn bytes(&self) -> Result<Vec<u8>, AdkError> {
        use base64::Engine as _;
        let b64 = self.b64_json.as_deref().ok_or_else(|| {
            AdkError::model(
                "muse image result has no b64_json body; request b64_json or use the url",
            )
        })?;
        base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                adk_core::ErrorCategory::InvalidInput,
                "model.muse.image_decode",
                format!("muse image result is not valid base64: {e}"),
            )
            .with_provider("muse")
        })
    }
}

/// Images response envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ImagesResponse {
    /// Unix creation timestamp.
    pub created: u64,
    /// Generated images.
    #[serde(default)]
    pub data: Vec<ImageData>,
}

/// Options for one image call.
#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    /// Number of images (billed per image; failures are free).
    pub n: Option<u32>,
    /// `WxH` size (aspect over 16:1 is rejected).
    pub size: Option<String>,
    /// `b64_json` (default) or `url`.
    pub response_format: Option<String>,
}

/// Client for `muse-image-1.0` generation and edits.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseConfig, MuseImagesClient};
///
/// let images = MuseImagesClient::new(MuseConfig::new(api_key, "muse-image-1.0"))?;
/// let response = images.generate("a red barn at dawn", Default::default()).await?;
/// ```
pub struct MuseImagesClient {
    client: Client,
    config: MuseConfig,
    retry_config: RetryConfig,
}

impl MuseImagesClient {
    /// Create an Images client. The config model is forced to `muse-image-1.0`
    /// unless a custom base URL already implies another deployment.
    pub fn new(mut config: MuseConfig) -> Result<Self, AdkError> {
        if config.model.is_empty() || config.model.starts_with("muse-spark") {
            config.model = MUSE_IMAGE_MODEL.to_string();
        }
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        Ok(Self { client, config, retry_config: RetryConfig::default() })
    }

    /// Generate images from a text prompt.
    ///
    /// # Errors
    ///
    /// Returns the API error (HTTP status, code `model.muse.api_error`) on rejection, or
    /// the current run's refusal (spend limit, budget) before anything is sent.
    pub async fn generate(
        &self,
        prompt: impl Into<String>,
        options: ImageOptions,
    ) -> Result<ImagesResponse, AdkError> {
        let url = format!("{}/images/generations", self.base());
        let call = self.usage_call(options.n);
        let body = serde_json::json!({
            "model": self.config.model,
            "prompt": prompt.into(),
            "n": options.n,
            "size": options.size,
            "response_format": options.response_format,
        });
        adk_core::meter_usage(call, async {
            let response = self.post_json(&url, body).await?;
            let usage = self.usage(&response);
            Ok((response, usage))
        })
        .await
    }

    /// Edit or compose from an existing image (multipart upload).
    ///
    /// # Errors
    ///
    /// Returns the API error (HTTP status, code `model.muse.api_error`) on rejection, or
    /// the current run's refusal (spend limit, budget) before anything is sent.
    pub async fn edit(
        &self,
        image: Vec<u8>,
        filename: impl Into<String>,
        content_type: &str,
        prompt: impl Into<String>,
        options: ImageOptions,
    ) -> Result<ImagesResponse, AdkError> {
        let url = format!("{}/images/edits", self.base());
        let call = self.usage_call(options.n);
        let filename = filename.into();
        let prompt = prompt.into();
        let (opt_n, opt_size, opt_format) =
            (options.n, options.size.clone(), options.response_format.clone());
        let client = self.client.clone();
        let api_key = self.config.api_key.clone();
        let model = self.config.model.clone();
        let retry_config = self.retry_config.clone();
        let edited = execute_with_retry(&retry_config, is_retryable_model_error, || {
            let client = client.clone();
            let url = url.clone();
            let api_key = api_key.clone();
            let image = image.clone();
            let filename = filename.clone();
            let prompt = prompt.clone();
            let model = model.clone();
            let (opt_n, opt_size, opt_format) = (opt_n, opt_size.clone(), opt_format.clone());
            async move {
                let part = reqwest::multipart::Part::bytes(image)
                    .file_name(filename)
                    .mime_str(content_type)
                    .map_err(|e| AdkError::model(format!("invalid image content type: {e}")))?;
                let mut form = reqwest::multipart::Form::new()
                    .text("model", model)
                    .text("prompt", prompt)
                    .part("image", part);
                if let Some(n) = opt_n {
                    form = form.text("n", n.to_string());
                }
                if let Some(size) = &opt_size {
                    form = form.text("size", size.clone());
                }
                if let Some(format) = &opt_format {
                    form = form.text("response_format", format.clone());
                }
                let response = client
                    .post(&url)
                    .header("Authorization", format!("Bearer {api_key}"))
                    .multipart(form)
                    .send()
                    .await
                    .map_err(|e| {
                        AdkError::new(
                            ErrorComponent::Model,
                            crate::http_timeouts::transport_category(&e),
                            "model.muse.request",
                            format!("Muse images request failed: {e}"),
                        )
                        .with_provider("muse")
                    })?;
                Self::read_response(response).await
            }
        });
        adk_core::meter_usage(call, async {
            let response = edited.await?;
            let usage = self.usage(&response);
            Ok((response, usage))
        })
        .await
    }

    /// The metered call for `n` images (one when unset), bounded at the catalog price.
    fn usage_call(&self, n: Option<u32>) -> UsageCall {
        let requested = UsageVector::new().with(UsageUnit::Images, u64::from(n.unwrap_or(1)));
        let call = UsageCall::new("muse", self.config.model.clone());
        match PricingCatalog::standard().unit_cost_micro_usd(
            Some("muse"),
            &self.config.model,
            &requested,
        ) {
            Some(bound) => call.with_max_cost_micro_usd(bound),
            None => call,
        }
    }

    /// The images a response returned, which are the ones billed.
    fn usage(&self, response: &ImagesResponse) -> MeteredUsage {
        let returned = UsageVector::new().with(UsageUnit::Images, response.data.len() as u64);
        let cost = PricingCatalog::standard().unit_cost_micro_usd(
            Some("muse"),
            &self.config.model,
            &returned,
        );
        let usage = MeteredUsage::new(returned);
        match cost {
            Some(cost) => usage.with_cost_micro_usd(cost),
            None => usage,
        }
    }

    fn base(&self) -> String {
        self.config.effective_base_url().trim_end_matches('/').to_string()
    }

    async fn post_json(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<ImagesResponse, AdkError> {
        let client = self.client.clone();
        let api_key = self.config.api_key.clone();
        let retry_config = self.retry_config.clone();
        let url = url.to_string();
        execute_with_retry(&retry_config, is_retryable_model_error, || {
            let client = client.clone();
            let url = url.clone();
            let api_key = api_key.clone();
            let body = body.clone();
            async move {
                let response = client
                    .post(&url)
                    .header("Authorization", format!("Bearer {api_key}"))
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| {
                        AdkError::new(
                            ErrorComponent::Model,
                            crate::http_timeouts::transport_category(&e),
                            "model.muse.request",
                            format!("Muse images request failed: {e}"),
                        )
                        .with_provider("muse")
                    })?;
                Self::read_response(response).await
            }
        })
        .await
    }

    async fn read_response(response: reqwest::Response) -> Result<ImagesResponse, AdkError> {
        if !response.status().is_success() {
            let status = response.status();
            let status_code = status.as_u16();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(crate::retry::parse_retry_after);
            let error_text = response.text().await.unwrap_or_default();
            let error = AdkError::new(
                ErrorComponent::Model,
                crate::retry::category_for_status_code(status_code),
                "model.muse.api_error",
                format!(
                    "Muse images API error (HTTP {status}): {}",
                    adk_core::upstream_excerpt(&error_text)
                ),
            )
            .with_upstream_status(status_code)
            .with_provider("muse");
            return Err(crate::retry::with_retry_after(error, retry_after));
        }
        let text = response
            .text()
            .await
            .map_err(|e| AdkError::model(format!("Failed to read images response: {e}")))?;
        serde_json::from_str(&text).map_err(|e| {
            AdkError::model(format!(
                "Failed to parse images response: {}",
                adk_core::json_error_summary(&e)
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_bytes_decode_base64() {
        let data =
            ImageData { b64_json: Some("aGVsbG8=".to_string()), url: None, revised_prompt: None };
        assert_eq!(data.bytes().unwrap(), b"hello");
    }

    #[test]
    fn image_bytes_require_a_body() {
        let data = ImageData {
            b64_json: None,
            url: Some("https://example.com/x.png".to_string()),
            revised_prompt: None,
        };
        assert!(data.bytes().is_err());
    }
}
