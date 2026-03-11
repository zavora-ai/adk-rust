//! Vertex AI backend for the Gemini API.
//!
//! This backend communicates with `{region}-aiplatform.googleapis.com` for
//! regional endpoints, or `aiplatform.googleapis.com` when the location is
//! `global`. It uses Google Cloud credentials (ADC, service account, WIF, or
//! API key), the gRPC SDK for non-streaming requests (with REST fallback on
//! transport errors), and REST SSE for streaming.
//!
//! Streaming support inspired by [PR #74](https://github.com/zavora-ai/adk-rust/pull/74)
//! by @mikefaille.

use super::{BackendStream, GeminiBackend};
use crate::{
    client::{
        BadResponseSnafu, DecodeResponseSnafu, DeserializeSnafu, Error,
        GoogleCloudCredentialHeadersSnafu, GoogleCloudCredentialHeadersUnavailableSnafu,
        GoogleCloudRequestDeserializeSnafu, GoogleCloudRequestNotObjectSnafu,
        GoogleCloudRequestSerializeSnafu, GoogleCloudResponseDeserializeSnafu,
        GoogleCloudResponseSerializeSnafu, Model, UrlParseSnafu,
    },
    embedding::{ContentEmbeddingResponse, EmbedContentRequest},
    generation::{GenerateContentRequest, GenerationResponse},
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::TryStreamExt;
use google_cloud_aiplatform_v1::client::PredictionService;
use google_cloud_auth::credentials::Credentials;
use reqwest::Client;
use snafu::{OptionExt, ResultExt};
use url::Url;

/// Vertex AI backend.
#[derive(Debug)]
pub struct VertexBackend {
    pub(crate) prediction: PredictionService,
    pub(crate) credentials: Credentials,
    pub(crate) endpoint: String,
    pub(crate) model: Model,
}

impl VertexBackend {
    /// Create a new Vertex backend.
    pub fn new(
        model: Model,
        prediction: PredictionService,
        credentials: Credentials,
        endpoint: String,
    ) -> Self {
        Self { prediction, credentials, endpoint, model }
    }

    /// Get auth headers from credentials.
    async fn auth_headers(&self) -> Result<reqwest::header::HeaderMap, Error> {
        match self
            .credentials
            .headers(Default::default())
            .await
            .context(GoogleCloudCredentialHeadersSnafu)?
        {
            google_cloud_auth::credentials::CacheableResource::New { data, .. } => Ok(data),
            google_cloud_auth::credentials::CacheableResource::NotModified => {
                GoogleCloudCredentialHeadersUnavailableSnafu.fail()
            }
        }
    }

    /// Check HTTP response status.
    async fn check_response(response: reqwest::Response) -> Result<reqwest::Response, Error> {
        let status = response.status();
        if !status.is_success() {
            let description = response.text().await.ok();
            BadResponseSnafu { code: status.as_u16(), description }.fail()
        } else {
            Ok(response)
        }
    }

    pub fn is_transport_error(message: &str) -> bool {
        let normalized = message.to_ascii_lowercase();
        normalized.contains("transport reports an error")
            || normalized.contains("http2 error")
            || normalized.contains("client error (sendrequest)")
            || normalized.contains("stream error")
    }

    /// Non-streaming generate via REST (fallback when gRPC has transport issues).
    async fn generate_content_rest(
        &self,
        request: &GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = Url::parse(&format!(
            "{}/v1/{}:generateContent",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .query(&[("$alt", "json;enum-encoding=int")])
            .json(request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;

        let vertex_resp: google_cloud_aiplatform_v1::model::GenerateContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;
        let value =
            serde_json::to_value(&vertex_resp).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
    }
}

#[async_trait]
impl GeminiBackend for VertexBackend {
    async fn generate_content(
        &self,
        request: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        // Try gRPC first, fall back to REST on transport errors.
        let rest_request = request.clone();
        let mut request_value =
            serde_json::to_value(&request).context(GoogleCloudRequestSerializeSnafu)?;
        let model = self.model.to_string();
        let request_object =
            request_value.as_object_mut().context(GoogleCloudRequestNotObjectSnafu)?;
        request_object.insert("model".to_string(), serde_json::Value::String(model));

        let grpc_request: google_cloud_aiplatform_v1::model::GenerateContentRequest =
            serde_json::from_value(request_value).context(GoogleCloudRequestDeserializeSnafu)?;

        match self.prediction.generate_content().with_request(grpc_request).send().await {
            Ok(response) => {
                let value =
                    serde_json::to_value(&response).context(GoogleCloudResponseSerializeSnafu)?;
                serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
            }
            Err(source) => {
                if Self::is_transport_error(&source.to_string()) {
                    tracing::warn!(
                        error = %source,
                        "Vertex SDK transport error on generateContent; falling back to REST"
                    );
                    self.generate_content_rest(&rest_request).await
                } else {
                    Err(Error::GoogleCloudRequest { source })
                }
            }
        }
    }

    async fn generate_content_stream(
        &self,
        request: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        // Vertex AI REST supports streamGenerateContent with SSE, same as AI Studio.
        let url = Url::parse(&format!(
            "{}/v1/{}:streamGenerateContent?alt=sse",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(&request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;

        let stream = response
            .bytes_stream()
            .eventsource()
            .map_err(|e| Error::BadPart { source: e })
            .and_then(|event| async move {
                serde_json::from_str::<GenerationResponse>(&event.data).context(DeserializeSnafu)
            });

        Ok(Box::pin(stream))
    }

    async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        // Use REST for embeddings (same pattern as existing code).
        let content_value =
            serde_json::to_value(&request.content).context(GoogleCloudRequestSerializeSnafu)?;
        let content: google_cloud_aiplatform_v1::model::Content =
            serde_json::from_value(content_value).context(GoogleCloudRequestDeserializeSnafu)?;

        let mut vertex_request =
            google_cloud_aiplatform_v1::model::EmbedContentRequest::new().set_content(content);

        if let Some(title) = request.title {
            vertex_request = vertex_request.set_title(title);
        }
        if let Some(task_type) = request.task_type {
            let task_type =
                google_cloud_aiplatform_v1::model::embed_content_request::EmbeddingTaskType::from(
                    task_type.as_ref(),
                );
            vertex_request = vertex_request.set_task_type(task_type);
        }
        if let Some(output_dimensionality) = request.output_dimensionality {
            vertex_request = vertex_request.set_output_dimensionality(output_dimensionality);
        }

        let url = Url::parse(&format!(
            "{}/v1/{}:embedContent",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .query(&[("$alt", "json;enum-encoding=int")])
            .json(&vertex_request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;

        let vertex_resp: google_cloud_aiplatform_v1::model::EmbedContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;
        let value =
            serde_json::to_value(&vertex_resp).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
    }
}
