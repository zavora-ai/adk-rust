use super::{BackendStream, GeminiBackend};
use crate::{
    batch::model::{BatchGenerateContentRequest, BatchOperation, ListBatchesResponse},
    cache::model::{
        CacheExpirationRequest, CachedContent, CreateCachedContentRequest,
        ListCachedContentsResponse,
    },
    common::model::Model,
    embedding::model::{
        BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
        EmbedContentRequest,
    },
    error::{
        BadResponseSnafu, DecodeResponseSnafu, Error, GoogleCloudAuthSnafu,
        GoogleCloudClientBuildSnafu, GoogleCloudCredentialHeadersSnafu,
        GoogleCloudCredentialHeadersUnavailableSnafu, GoogleCloudCredentialParseSnafu,
        GoogleCloudRequestDeserializeSnafu, GoogleCloudRequestSerializeSnafu,
        GoogleCloudResponseDeserializeSnafu, GoogleCloudResponseSerializeSnafu,
        GoogleCloudUnsupportedSnafu, PerformRequestSnafu, TokioRuntimeSnafu, UrlParseSnafu,
    },
    files::model::{File, ListFilesResponse},
    generation::model::{GenerateContentRequest, GenerationResponse},
};
use async_trait::async_trait;
use futures::TryStreamExt;
use google_cloud_aiplatform_v1::client::{GenAiCacheService, JobService, PredictionService};
use google_cloud_auth::credentials::{self, Credentials};
use reqwest::{Client, Response};

use snafu::ResultExt;

use eventsource_stream::Eventsource;
use url::Url;

/// Authentication configuration for Vertex AI
#[derive(Debug, Clone)]
pub enum GoogleCloudAuth {
    ApiKey(String),
    Adc,
    ServiceAccountJson(String),
    WifJson(String),
    Credentials(Credentials),
}

async fn check_response(response: Response) -> Result<Response, Error> {
    let status = response.status();
    if !status.is_success() {
        let description = response.text().await.ok();
        BadResponseSnafu { code: status.as_u16(), description }.fail()
    } else {
        Ok(response)
    }
}

#[derive(Debug, Clone)]
pub struct VertexBackend {
    prediction: PredictionService,
    job: JobService,
    cache: GenAiCacheService,
    credentials: Credentials,
    endpoint: String,
    project: String,
    location: String,
    model: Model,
    full_model_name: String,
}

impl VertexBackend {
    pub fn new(
        endpoint: String,
        project: String,
        location: String,
        auth: GoogleCloudAuth,
        model: Model,
    ) -> Result<Self, Error> {
        let (prediction, job, cache, credentials) =
            build_vertex_prediction_service(endpoint.clone(), auth)?;

        let s = model.as_str();
        let full_model_name = if s.starts_with("projects/") || s.starts_with("publishers/") {
            s.to_string()
        } else {
            let name = s.trim_start_matches("models/");
            format!("projects/{}/locations/{}/publishers/google/models/{}", project, location, name)
        };

        Ok(Self {
            full_model_name,
            prediction,
            job,
            cache,
            credentials,
            endpoint,
            project,
            location,
            model,
        })
    }

    fn is_vertex_transport_error_message(msg: &str) -> bool {
        msg.contains("hyper::Error(IncompleteMessage)")
            || msg.contains("h2::Error(StreamClosed)")
            || msg.contains("broken pipe")
            || msg.contains("connection reset")
    }

    async fn generate_content_vertex_rest(
        &self,
        vertex_req: google_cloud_aiplatform_v1::model::GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = Url::parse(&format!(
            "{}/v1/{}:generateContent",
            self.endpoint.trim_end_matches('/'),
            self.full_model_name
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = match self
            .credentials
            .headers(Default::default())
            .await
            .context(GoogleCloudCredentialHeadersSnafu)?
        {
            google_cloud_auth::credentials::CacheableResource::New { data, .. } => data,
            google_cloud_auth::credentials::CacheableResource::NotModified => {
                return GoogleCloudCredentialHeadersUnavailableSnafu.fail();
            }
        };

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(&vertex_req)
            .send()
            .await
            .context(PerformRequestSnafu { url: url.clone() })?;

        let response = check_response(response).await?;
        let response: google_cloud_aiplatform_v1::model::GenerateContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;

        let response_value =
            serde_json::to_value(&response).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(response_value).context(GoogleCloudResponseDeserializeSnafu)
    }

    fn convert_request<T, U>(&self, req: T) -> Result<U, Error>
    where
        T: serde::Serialize,
        U: serde::de::DeserializeOwned,
    {
        let request_value = serde_json::to_value(&req).context(GoogleCloudRequestSerializeSnafu)?;
        serde_json::from_value(request_value).context(GoogleCloudRequestDeserializeSnafu)
    }

    fn convert_response<T, U>(&self, res: T) -> Result<U, Error>
    where
        T: serde::Serialize,
        U: serde::de::DeserializeOwned,
    {
        let response_value =
            serde_json::to_value(&res).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(response_value).context(GoogleCloudResponseDeserializeSnafu)
    }
}

#[async_trait]
impl GeminiBackend for VertexBackend {
    fn model(&self) -> &str {
        self.model.as_str()
    }

    async fn generate_content(
        &self,
        req: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let mut vertex_req: google_cloud_aiplatform_v1::model::GenerateContentRequest =
            self.convert_request(&req)?;
        vertex_req.model = self.full_model_name.clone();

        let mut vertex_req_clone: google_cloud_aiplatform_v1::model::GenerateContentRequest =
            self.convert_request(&req)?;
        vertex_req_clone.model = self.full_model_name.clone();

        match self.prediction.generate_content().with_request(vertex_req).send().await {
            Ok(response) => self.convert_response(response),
            Err(source) => {
                if VertexBackend::is_vertex_transport_error_message(&source.to_string()) {
                    tracing::warn!(
                        error = %source,
                        "Vertex SDK transport error on generateContent; falling back to REST"
                    );
                    return self.generate_content_vertex_rest(vertex_req_clone).await;
                }
                return Err(Error::GoogleCloudRequest { source });
            }
        }
    }

    async fn generate_content_stream(
        &self,
        req: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        // Use REST SSE implementation since gRPC streaming is problematic/missing in current SDK binding
        let mut url = Url::parse(&format!(
            "{}/v1/{}:streamGenerateContent",
            self.endpoint.trim_end_matches('/'),
            self.full_model_name
        ))
        .context(UrlParseSnafu)?;

        url.query_pairs_mut().append_pair("alt", "sse");

        let auth_headers = match self
            .credentials
            .headers(Default::default())
            .await
            .context(GoogleCloudCredentialHeadersSnafu)?
        {
            google_cloud_auth::credentials::CacheableResource::New { data, .. } => data,
            google_cloud_auth::credentials::CacheableResource::NotModified => {
                return GoogleCloudCredentialHeadersUnavailableSnafu.fail();
            }
        };

        // We use convert_request to get a JSON value we can send, or pass req directly if reqwest handles serialization
        let mut vertex_req: google_cloud_aiplatform_v1::model::GenerateContentRequest =
            self.convert_request(&req)?;

        vertex_req.model = self.full_model_name.clone();
        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(&vertex_req)
            .send()
            .await
            .context(PerformRequestSnafu { url: url.clone() })?;

        let response = check_response(response).await?;
        let stream = response.bytes_stream();

        let stream = stream.eventsource().map_err(|e| Error::BadPart { source: e }).and_then(
            |event| async move {
                // Parse Vertex SSE event format which might differ slightly or be standard GenerationResponse
                // Usually it's same structure.
                serde_json::from_str::<GenerationResponse>(&event.data)
                    .map_err(|e| Error::Deserialize { source: e })
            },
        );

        Ok(Box::pin(stream))
    }

    async fn count_tokens(&self, _req: GenerateContentRequest) -> Result<u32, Error> {
        GoogleCloudUnsupportedSnafu { operation: "countTokens" }.fail()
    }

    async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        let content_value =
            serde_json::to_value(&request.content).context(GoogleCloudRequestSerializeSnafu)?;
        let content: google_cloud_aiplatform_v1::model::Content =
            serde_json::from_value(content_value).context(GoogleCloudRequestDeserializeSnafu)?;

        let mut vertex_request = google_cloud_aiplatform_v1::model::EmbedContentRequest::new()
            .set_content(content)
            .set_model(self.full_model_name.clone());

        if let Some(title) = request.title.clone() {
            vertex_request = vertex_request.set_title(title);
        }
        if let Some(task_type) = request.task_type.clone() {
            let task_type =
                google_cloud_aiplatform_v1::model::embed_content_request::EmbeddingTaskType::from(
                    task_type.as_ref(),
                );
            vertex_request = vertex_request.set_task_type(task_type);
        }
        if let Some(output_dimensionality) = request.output_dimensionality {
            vertex_request = vertex_request.set_output_dimensionality(output_dimensionality);        }

        let url = Url::parse(&format!(
            "{}/v1/{}:embedContent",
            self.endpoint.trim_end_matches('/'),
            self.full_model_name
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = match self
            .credentials
            .headers(Default::default())
            .await
            .context(GoogleCloudCredentialHeadersSnafu)?
        {
            google_cloud_auth::credentials::CacheableResource::New { data, .. } => data,
            google_cloud_auth::credentials::CacheableResource::NotModified => {
                return GoogleCloudCredentialHeadersUnavailableSnafu.fail();
            }
        };

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(&vertex_request)
            .send()
            .await
            .context(PerformRequestSnafu { url: url.clone() })?;

        let response = check_response(response).await?;
        let response: google_cloud_aiplatform_v1::model::EmbedContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;

        self.convert_response(response)
    }

    async fn batch_embed_contents(
        &self,
        _req: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        GoogleCloudUnsupportedSnafu { operation: "batchEmbedContents" }.fail()
    }

    async fn create_batch(
        &self,
        req: BatchGenerateContentRequest,
    ) -> Result<BatchOperation, Error> {
        let request: google_cloud_aiplatform_v1::model::CreateBatchPredictionJobRequest =
            self.convert_request(req)?;

        let response = self
            .job
            .create_batch_prediction_job()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn get_batch(&self, name: &str) -> Result<BatchOperation, Error> {
        let request = google_cloud_aiplatform_v1::model::GetBatchPredictionJobRequest::new()
            .set_name(name.to_string());
        let response = self
            .job
            .get_batch_prediction_job()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn list_batches(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        let parent = format!("projects/{}/locations/{}", self.project, self.location);
        let request = google_cloud_aiplatform_v1::model::ListBatchPredictionJobsRequest::new()
            .set_parent(parent)
            .set_page_size(page_size.unwrap_or(10) as i32)
            .set_page_token(page_token.unwrap_or_default());

        let response = self
            .job
            .list_batch_prediction_jobs()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn cancel_batch(&self, _name: &str) -> Result<(), Error> {
        GoogleCloudUnsupportedSnafu { operation: "cancelBatch" }.fail()
    }

    async fn delete_batch(&self, _name: &str) -> Result<(), Error> {
        GoogleCloudUnsupportedSnafu { operation: "deleteBatch" }.fail()
    }

    async fn upload_file(
        &self,
        _display_name: Option<String>,
        _file_bytes: Vec<u8>,
        _mime_type: mime::Mime,
    ) -> Result<File, Error> {
        GoogleCloudUnsupportedSnafu { operation: "uploadFile" }.fail()
    }

    async fn get_file(&self, _name: &str) -> Result<File, Error> {
        GoogleCloudUnsupportedSnafu { operation: "getFile" }.fail()
    }

    async fn list_files(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        GoogleCloudUnsupportedSnafu { operation: "listFiles" }.fail()
    }

    async fn delete_file(&self, _name: &str) -> Result<(), Error> {
        GoogleCloudUnsupportedSnafu { operation: "deleteFile" }.fail()
    }

    async fn download_file(&self, _name: &str) -> Result<Vec<u8>, Error> {
        GoogleCloudUnsupportedSnafu { operation: "downloadFile" }.fail()
    }

    async fn create_cached_content(
        &self,
        req: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        let request: google_cloud_aiplatform_v1::model::CreateCachedContentRequest =
            self.convert_request(req)?;

        let response = self
            .cache
            .create_cached_content()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn get_cached_content(&self, name: &str) -> Result<CachedContent, Error> {
        let request = google_cloud_aiplatform_v1::model::GetCachedContentRequest::new()
            .set_name(name.to_string());
        let response = self
            .cache
            .get_cached_content()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn list_cached_contents(
        &self,
        page_size: Option<i32>,
        page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        let parent = format!("projects/{}/locations/{}", self.project, self.location);
        let request = google_cloud_aiplatform_v1::model::ListCachedContentsRequest::new()
            .set_parent(parent)
            .set_page_size(page_size.unwrap_or(10))
            .set_page_token(page_token.unwrap_or_default());
        let response = self
            .cache
            .list_cached_contents()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;

        self.convert_response(response)
    }

    async fn update_cached_content(
        &self,
        _name: &str,
        _req: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "updateCachedContent" })
    }

    async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        let request = google_cloud_aiplatform_v1::model::DeleteCachedContentRequest::new()
            .set_name(name.to_string());
        self.cache
            .delete_cached_content()
            .with_request(request)
            .send()
            .await
            .map_err(|source| Error::GoogleCloudRequest { source })?;
        Ok(())
    }
}

fn build_vertex_prediction_service(
    endpoint: String,
    auth: GoogleCloudAuth,
) -> Result<(PredictionService, JobService, GenAiCacheService, Credentials), Error> {
    let build_in_runtime =
        |endpoint: String,
         auth: GoogleCloudAuth|
         -> Result<(PredictionService, JobService, GenAiCacheService, Credentials), Error> {
            let runtime = tokio::runtime::Runtime::new().context(TokioRuntimeSnafu)?;
            runtime.block_on(async {
                let credentials = match auth {
                    GoogleCloudAuth::ApiKey(api_key) => {
                        credentials::api_key_credentials::Builder::new(api_key).build()
                    }
                    GoogleCloudAuth::Adc => {
                        let scopes = ["https://www.googleapis.com/auth/cloud-platform"];
                        credentials::Builder::default()
                            .with_scopes(scopes)
                            .build()
                            .context(GoogleCloudAuthSnafu)?
                    }
                    GoogleCloudAuth::ServiceAccountJson(json) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&json).context(GoogleCloudCredentialParseSnafu)?;
                        credentials::service_account::Builder::new(value)
                            .build()
                            .context(GoogleCloudAuthSnafu)?
                    }
                    GoogleCloudAuth::WifJson(json) => {
                        let value: serde_json::Value =
                            serde_json::from_str(&json).context(GoogleCloudCredentialParseSnafu)?;
                        credentials::external_account::Builder::new(value)
                            .build()
                            .context(GoogleCloudAuthSnafu)?
                    }
                    GoogleCloudAuth::Credentials(c) => c,
                };

                let prediction = PredictionService::builder()
                    .with_endpoint(endpoint.clone())
                    .with_credentials(credentials.clone())
                    .build()
                    .await
                    .context(GoogleCloudClientBuildSnafu)?;

                let job = JobService::builder()
                    .with_endpoint(endpoint.clone())
                    .with_credentials(credentials.clone())
                    .build()
                    .await
                    .context(GoogleCloudClientBuildSnafu)?;

                let cache = GenAiCacheService::builder()
                    .with_endpoint(endpoint)
                    .with_credentials(credentials.clone())
                    .build()
                    .await
                    .context(GoogleCloudClientBuildSnafu)?;

                Ok((prediction, job, cache, credentials))
            })
        };

    if tokio::runtime::Handle::try_current().is_ok() {
        let worker = std::thread::Builder::new()
            .name("adk-gemini-vertex-init".to_string())
            .spawn(move || build_in_runtime(endpoint, auth))
            .map_err(|source| Error::TokioRuntime { source })?;

        return worker.join().map_err(|_| Error::GoogleCloudInitThreadPanicked)?;
    }

    build_in_runtime(endpoint, auth)
}

/// Extracts the project_id from a service account JSON string.
pub fn extract_service_account_project_id(json: &str) -> Result<String, Error> {
    let v: serde_json::Value =
        serde_json::from_str(json).context(GoogleCloudCredentialParseSnafu)?;
    v.get("project_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or(Error::MissingGoogleCloudProjectId)
}
