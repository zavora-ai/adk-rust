//! REST client for the Vertex AI Example Store v1beta1 data plane.

use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Result};
use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

const EXAMPLE_STORE_API_VERSION: &str = "v1beta1";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AUTH_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
const ENV_EXAMPLE_STORE_ID: &str = "EXAMPLE_STORE_ID";

/// Configuration for the Vertex AI Example Store client.
///
/// The `exampleStores` resource itself is assumed pre-provisioned; this client
/// only performs data-plane operations against it.
///
/// > **Note:** the Example Store API is **v1beta1 (Preview)** and is currently
/// > served from the `us-central1` region only.
#[derive(Debug, Clone)]
pub struct ExampleStoreConfig {
    /// Google Cloud project ID.
    pub project_id: String,
    /// GCP region. Example Store is currently `us-central1` only.
    pub location: String,
    /// Example Store ID (the last resource-name segment) or a full
    /// `projects/*/locations/*/exampleStores/*` resource name.
    pub example_store: String,
    /// Optional custom API origin.
    ///
    /// The origin receives Google authorization headers plus example data. It
    /// must not contain userinfo, a path, a query, or a fragment.
    pub endpoint: Option<String>,
}

impl ExampleStoreConfig {
    /// Creates a new config with the given project ID, location, and store.
    pub fn new(
        project_id: impl Into<String>,
        location: impl Into<String>,
        example_store: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            example_store: example_store.into(),
            endpoint: None,
        }
    }

    /// Builds a config from environment variables.
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
    /// `EXAMPLE_STORE_ID`. Values are trimmed; blank values count as missing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::example_store::{ExampleStoreClient, ExampleStoreConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = ExampleStoreConfig::from_env()?;
    /// let client = ExampleStoreClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error naming every missing or blank variable.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let project_id = read(ENV_GOOGLE_CLOUD_PROJECT);
        let location = read(ENV_GOOGLE_CLOUD_LOCATION);
        let example_store = read(ENV_EXAMPLE_STORE_ID);

        match (project_id, location, example_store) {
            (Some(project_id), Some(location), Some(example_store)) => {
                Ok(Self::new(project_id, location, example_store))
            }
            (project_id, location, example_store) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                    (ENV_EXAMPLE_STORE_ID, example_store.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "tool.example_store.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. Set them, or construct the config with ExampleStoreConfig::new",
                    ),
                )
                .with_provider("vertex_ai"))
            }
        }
    }

    /// Sets a custom API origin.
    ///
    /// Use only a trusted HTTPS origin, or loopback HTTP for local tests.
    /// Userinfo, paths, queries, and fragments are rejected before transport.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        let endpoint = self
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location));
        if endpoint.contains("://") { endpoint } else { format!("https://{endpoint}") }
    }

    fn store_path(&self) -> String {
        if self.example_store.contains('/') {
            self.example_store.clone()
        } else {
            format!(
                "projects/{}/locations/{}/exampleStores/{}",
                self.project_id, self.location, self.example_store,
            )
        }
    }
}

// ===== Wire types (v1beta1, camelCase JSON) =====

/// A single stored example (`google.cloud.aiplatform.v1beta1.Example`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    /// Optional human-readable display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Immutable unique example ID. Generated by the service when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_id: Option<String>,
    /// Output only. RFC 3339 creation timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_time: Option<String>,
    /// The example payload: contents plus expected model responses.
    pub stored_contents_example: StoredContentsExample,
}

impl Example {
    /// Creates a new example from a stored-contents payload.
    pub fn new(stored_contents_example: StoredContentsExample) -> Self {
        Self { display_name: None, example_id: None, create_time: None, stored_contents_example }
    }

    /// Sets the human-readable display name.
    #[must_use]
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    /// Sets the immutable example ID (used for overwrite-by-ID upserts).
    #[must_use]
    pub fn with_example_id(mut self, example_id: impl Into<String>) -> Self {
        self.example_id = Some(example_id.into());
        self
    }
}

/// Contents-based example payload
/// (`google.cloud.aiplatform.v1beta1.StoredContentsExample`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredContentsExample {
    /// Explicit search key. When absent, the service derives one via
    /// `search_key_generation_method`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_key: Option<String>,
    /// The conversation contents and expected model responses.
    pub contents_example: ContentsExample,
    /// How the service derives a search key when `search_key` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_key_generation_method: Option<SearchKeyGenerationMethod>,
}

impl StoredContentsExample {
    /// Creates a new stored-contents example.
    pub fn new(contents_example: ContentsExample) -> Self {
        Self { search_key: None, contents_example, search_key_generation_method: None }
    }

    /// Sets an explicit search key.
    #[must_use]
    pub fn with_search_key(mut self, search_key: impl Into<String>) -> Self {
        self.search_key = Some(search_key.into());
        self
    }

    /// Derives the search key from the last conversation entry (`lastEntry`).
    #[must_use]
    pub fn with_last_entry_search_key(mut self) -> Self {
        self.search_key_generation_method = Some(SearchKeyGenerationMethod::last_entry());
        self
    }
}

/// A conversation plus the expected model responses
/// (`google.cloud.aiplatform.v1beta1.StoredContentsExample.ContentsExample`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentsExample {
    /// The conversation contents leading up to the expected response.
    pub contents: Vec<Content>,
    /// The expected model responses for `contents`.
    pub expected_contents: Vec<ExpectedContent>,
}

impl ContentsExample {
    /// Creates a contents example, wrapping each expected response content.
    pub fn new(contents: Vec<Content>, expected: Vec<Content>) -> Self {
        Self {
            contents,
            expected_contents: expected
                .into_iter()
                .map(|content| ExpectedContent { content })
                .collect(),
        }
    }
}

/// One expected model response wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedContent {
    /// The expected response content.
    pub content: Content,
}

/// How the service derives a search key from example contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchKeyGenerationMethod {
    /// Derive the search key from the last conversation entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_entry: Option<LastEntry>,
}

impl SearchKeyGenerationMethod {
    /// The `{"lastEntry": {}}` generation method.
    pub fn last_entry() -> Self {
        Self { last_entry: Some(LastEntry {}) }
    }
}

/// Marker for last-entry search-key generation. Serializes as `{}`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LastEntry {}

/// Request body for `exampleStores.upsertExamples`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertExamplesRequest {
    /// The examples to insert or update.
    pub examples: Vec<Example>,
    /// When `true`, examples with matching IDs are overwritten.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<bool>,
}

impl UpsertExamplesRequest {
    /// Creates an upsert request for the given examples.
    pub fn new(examples: Vec<Example>) -> Self {
        Self { examples, overwrite: None }
    }

    /// Sets the overwrite flag.
    #[must_use]
    pub fn with_overwrite(mut self, overwrite: bool) -> Self {
        self.overwrite = Some(overwrite);
        self
    }
}

/// Response body for `exampleStores.upsertExamples`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpsertExamplesResponse {
    /// Per-example outcomes, in request order.
    #[serde(default)]
    pub results: Vec<UpsertResult>,
}

/// Per-example upsert outcome: the stored example on success, a
/// `google.rpc.Status` on failure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpsertResult {
    /// The stored example, present on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Example>,
    /// The failure status, present on error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<RpcStatus>,
}

/// A `google.rpc.Status` error payload.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RpcStatus {
    /// Canonical `google.rpc.Code` value.
    #[serde(default)]
    pub code: i32,
    /// Developer-facing error message.
    #[serde(default)]
    pub message: String,
    /// Detail messages, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<serde_json::Value>,
}

/// Request body for `exampleStores.searchExamples`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchExamplesRequest {
    /// Maximum number of results to return.
    ///
    /// This is a proto JSON `int64`, which serializes as a **string** on the
    /// wire (`"topK": "5"`).
    #[serde(with = "int64_string")]
    pub top_k: i64,
    /// The search parameters.
    pub parameters: SearchExamplesParameters,
}

impl SearchExamplesRequest {
    /// Searches by an explicit search-key string.
    pub fn by_search_key(search_key: impl Into<String>, top_k: i64) -> Self {
        Self {
            top_k,
            parameters: SearchExamplesParameters {
                stored_contents_example_parameters: StoredContentsExampleParameters {
                    function_names: None,
                    search_key: Some(search_key.into()),
                    content_search_key: None,
                },
            },
        }
    }

    /// Searches by conversation contents, deriving the key from the last entry.
    pub fn by_contents(contents: Vec<Content>, top_k: i64) -> Self {
        Self {
            top_k,
            parameters: SearchExamplesParameters {
                stored_contents_example_parameters: StoredContentsExampleParameters {
                    function_names: None,
                    search_key: None,
                    content_search_key: Some(ContentSearchKey {
                        contents,
                        search_key_generation_method: SearchKeyGenerationMethod::last_entry(),
                    }),
                },
            },
        }
    }

    /// Restricts results to examples matching the function-name filter.
    #[must_use]
    pub fn with_function_names(mut self, function_names: ExamplesArrayFilter) -> Self {
        self.parameters.stored_contents_example_parameters.function_names = Some(function_names);
        self
    }
}

/// Search parameters wrapper. Currently only stored-contents search exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchExamplesParameters {
    /// Parameters for searching `StoredContentsExample` data.
    pub stored_contents_example_parameters: StoredContentsExampleParameters,
}

/// Search parameters for stored-contents examples.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredContentsExampleParameters {
    /// Restricts results by the function names present in an example.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_names: Option<ExamplesArrayFilter>,
    /// Explicit search-key query. Mutually exclusive with `content_search_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_key: Option<String>,
    /// Contents-derived query. Mutually exclusive with `search_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_search_key: Option<ContentSearchKey>,
}

/// A search query expressed as conversation contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchKey {
    /// The conversation contents to derive the search key from.
    pub contents: Vec<Content>,
    /// How the search key is derived from `contents`.
    pub search_key_generation_method: SearchKeyGenerationMethod,
}

/// Array filter over string values
/// (`google.cloud.aiplatform.v1beta1.ExamplesArrayFilter`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExamplesArrayFilter {
    /// The values to match.
    pub values: Vec<String>,
    /// How `values` are combined.
    pub array_operator: ArrayOperator,
}

impl ExamplesArrayFilter {
    /// Matches examples containing any of the given values.
    pub fn contains_any(values: Vec<String>) -> Self {
        Self { values, array_operator: ArrayOperator::ContainsAny }
    }

    /// Matches examples containing all of the given values.
    pub fn contains_all(values: Vec<String>) -> Self {
        Self { values, array_operator: ArrayOperator::ContainsAll }
    }
}

/// Logical operator for [`ExamplesArrayFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ArrayOperator {
    /// Unspecified operator.
    ArrayOperatorUnspecified,
    /// Matches when any value is present.
    ContainsAny,
    /// Matches when all values are present.
    ContainsAll,
}

/// Response body for `exampleStores.searchExamples`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchExamplesResponse {
    /// Matching examples ordered by decreasing similarity.
    #[serde(default)]
    pub results: Vec<SearchExampleResult>,
}

/// One search hit: an example plus its similarity score.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchExampleResult {
    /// The matching example.
    pub example: Example,
    /// Similarity between the query and the example's search key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_score: Option<f64>,
}

/// Request body for `exampleStores.fetchExamples`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FetchExamplesRequest {
    /// Maximum number of examples per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    /// Continuation token from a previous response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// Restricts the fetch to these example IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub example_ids: Vec<String>,
    /// Restricts the fetch by stored-contents attributes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stored_contents_example_filter: Option<StoredContentsExampleFilter>,
}

impl FetchExamplesRequest {
    /// Creates an empty fetch request (first page, no filters).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the page size.
    #[must_use]
    pub fn with_page_size(mut self, page_size: i32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// Sets the continuation token.
    #[must_use]
    pub fn with_page_token(mut self, page_token: impl Into<String>) -> Self {
        self.page_token = Some(page_token.into());
        self
    }

    /// Restricts the fetch to the given example IDs.
    #[must_use]
    pub fn with_example_ids(mut self, example_ids: Vec<String>) -> Self {
        self.example_ids = example_ids;
        self
    }

    /// Restricts the fetch by stored-contents attributes.
    #[must_use]
    pub fn with_filter(mut self, filter: StoredContentsExampleFilter) -> Self {
        self.stored_contents_example_filter = Some(filter);
        self
    }
}

/// Fetch filter over stored-contents attributes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredContentsExampleFilter {
    /// Restricts results to examples with these search keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_keys: Vec<String>,
    /// Restricts results by the function names present in an example.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_names: Option<ExamplesArrayFilter>,
}

/// Response body for `exampleStores.fetchExamples`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FetchExamplesResponse {
    /// The fetched examples.
    #[serde(default)]
    pub examples: Vec<Example>,
    /// Continuation token; absent or empty on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Proto JSON `int64` codec: serializes as a decimal string, accepts either a
/// string or a bare number on input (both are valid per the proto3 JSON spec).
mod int64_string {
    use serde::de::{Deserializer, Error, Unexpected, Visitor};
    use serde::ser::Serializer;

    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        struct Int64Visitor;

        impl Visitor<'_> for Int64Visitor {
            type Value = i64;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an int64 as a decimal string or number")
            }

            fn visit_str<E: Error>(self, value: &str) -> Result<i64, E> {
                value.parse().map_err(|_| Error::invalid_value(Unexpected::Str(value), &self))
            }

            fn visit_i64<E: Error>(self, value: i64) -> Result<i64, E> {
                Ok(value)
            }

            fn visit_u64<E: Error>(self, value: u64) -> Result<i64, E> {
                i64::try_from(value)
                    .map_err(|_| Error::invalid_value(Unexpected::Unsigned(value), &self))
            }
        }

        deserializer.deserialize_any(Int64Visitor)
    }
}

// ===== Client =====

/// ADC-authenticated REST client for the Example Store v1beta1 data plane.
///
/// Performs [`upsert_examples`](Self::upsert_examples),
/// [`search_examples`](Self::search_examples), and
/// [`fetch_examples`](Self::fetch_examples) against a pre-provisioned
/// `projects/*/locations/*/exampleStores/*` resource. Store creation and
/// deletion are control-plane provisioning concerns and are out of scope.
///
/// > **Note:** the Example Store API is **v1beta1 (Preview)** and is currently
/// > served from the `us-central1` region only.
pub struct ExampleStoreClient {
    http_client: Client,
    endpoint: String,
    store_path: String,
    credentials: Credentials,
    auth_headers: Arc<RwLock<Option<reqwest::header::HeaderMap>>>,
}

impl std::fmt::Debug for ExampleStoreClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Credentials carry secret material and do not implement Debug.
        f.debug_struct("ExampleStoreClient")
            .field("endpoint", &self.endpoint)
            .field("store_path", &self.store_path)
            .finish_non_exhaustive()
    }
}

impl ExampleStoreClient {
    /// Creates a new client using Application Default Credentials (ADC).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_tool::example_store::{ExampleStoreClient, ExampleStoreConfig};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = ExampleStoreConfig::new("my-project", "us-central1", "my-store");
    /// let client = ExampleStoreClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not a
    /// valid secure origin, or the redirect-disabled HTTP client cannot be
    /// constructed.
    pub fn new_with_adc(config: ExampleStoreConfig) -> Result<Self> {
        let credentials = credentials::Builder::default()
            .with_scopes([CLOUD_PLATFORM_SCOPE])
            .build()
            .map_err(|error| {
                let error = truncate_for_error(&error.to_string());
                Self::auth_error(format!("failed to build example store ADC credentials: {error}"))
            })?;
        Self::with_credentials(config, credentials)
    }

    /// Creates a new client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or the
    /// redirect-disabled HTTP client cannot be constructed.
    pub fn with_credentials(config: ExampleStoreConfig, credentials: Credentials) -> Result<Self> {
        let http_client = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                let error = truncate_for_error(&error.without_url().to_string());
                Self::internal_error(format!("failed to build example store HTTP client: {error}"))
            })?;
        let client = Self {
            http_client,
            endpoint: config.endpoint(),
            store_path: config.store_path(),
            credentials,
            auth_headers: Arc::new(RwLock::new(None)),
        };
        client.build_url("")?;
        Ok(client)
    }

    /// The full `projects/*/locations/*/exampleStores/*` resource name this
    /// client operates on.
    pub fn store_resource_name(&self) -> &str {
        &self.store_path
    }

    /// Inserts or updates examples in the store.
    ///
    /// `POST {store}:upsertExamples` with `{"examples": [...], "overwrite": ...}`.
    /// The response carries one result per input example: the stored example on
    /// success, or a `google.rpc.Status` on per-example failure. Per-example
    /// failures do **not** fail the request; inspect
    /// [`UpsertResult::status`] to detect them.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn upsert_examples(
        &self,
        request: UpsertExamplesRequest,
    ) -> Result<UpsertExamplesResponse> {
        self.post_verb("upsertExamples", &request).await
    }

    /// Retrieves the top-k examples most similar to a query.
    ///
    /// `POST {store}:searchExamples`. The `topK` field is a proto JSON `int64`
    /// and serializes as a string. Results are ordered by decreasing
    /// [`SearchExampleResult::similarity_score`].
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn search_examples(
        &self,
        request: SearchExamplesRequest,
    ) -> Result<SearchExamplesResponse> {
        self.post_verb("searchExamples", &request).await
    }

    /// Fetches examples by ID or filter, with pagination.
    ///
    /// `POST {store}:fetchExamples`. Pass
    /// [`FetchExamplesResponse::next_page_token`] back via
    /// [`FetchExamplesRequest::with_page_token`] to continue.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, or an unparseable response body.
    pub async fn fetch_examples(
        &self,
        request: FetchExamplesRequest,
    ) -> Result<FetchExamplesResponse> {
        self.post_verb("fetchExamples", &request).await
    }

    async fn post_verb<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        verb: &str,
        body: &T,
    ) -> Result<R> {
        let url =
            self.build_url(&format!("{EXAMPLE_STORE_API_VERSION}/{}:{verb}", self.store_path))?;
        tracing::debug!(example_store.verb = verb, "sending example store request");
        let request = self.apply_auth(self.http_client.post(url).json(body)).await?;
        self.send_json(request).await
    }

    fn auth_error(message: impl Into<String>) -> AdkError {
        AdkError::unauthorized(ErrorComponent::Tool, "tool.example_store.unauthorized", message)
            .with_provider("vertex_ai")
    }

    fn invalid_input(message: impl Into<String>) -> AdkError {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "tool.example_store.invalid_input",
            message,
        )
        .with_provider("vertex_ai")
    }

    fn internal_error(message: impl Into<String>) -> AdkError {
        AdkError::internal(ErrorComponent::Tool, "tool.example_store.internal", message)
            .with_provider("vertex_ai")
    }

    fn timeout_error(message: impl Into<String>) -> AdkError {
        AdkError::timeout(ErrorComponent::Tool, "tool.example_store.timeout", message)
            .with_provider("vertex_ai")
    }

    fn credentials_error(error: google_cloud_auth::errors::CredentialsError) -> AdkError {
        let message = format!(
            "failed to obtain google cloud auth headers: {}",
            truncate_for_error(&error.to_string()),
        );
        if error.is_transient() {
            AdkError::unavailable(
                ErrorComponent::Tool,
                "tool.example_store.credentials_unavailable",
                message,
            )
            .with_provider("vertex_ai")
        } else {
            Self::auth_error(message)
        }
    }

    fn transport_error(error: reqwest::Error) -> AdkError {
        let timeout = error.is_timeout();
        let error = truncate_for_error(&error.without_url().to_string());
        if timeout {
            return Self::timeout_error(format!("example store HTTP request timed out: {error}"));
        }
        AdkError::unavailable(
            ErrorComponent::Tool,
            "tool.example_store.unavailable",
            format!("failed to send example store request: {error}"),
        )
        .with_provider("vertex_ai")
    }

    fn status_error(status: StatusCode, body: &str) -> AdkError {
        let message = format!(
            "example store request failed with status {}: {}",
            status.as_u16(),
            truncate_for_error(body),
        );
        let (category, code) = match status.as_u16() {
            400 | 409 | 422 => (ErrorCategory::InvalidInput, "tool.example_store.invalid_request"),
            401 => (ErrorCategory::Unauthorized, "tool.example_store.unauthorized"),
            403 => (ErrorCategory::Forbidden, "tool.example_store.forbidden"),
            404 => (ErrorCategory::NotFound, "tool.example_store.not_found"),
            408 | 504 => (ErrorCategory::Timeout, "tool.example_store.timeout"),
            429 => (ErrorCategory::RateLimited, "tool.example_store.rate_limited"),
            500 | 502 | 503 => (ErrorCategory::Unavailable, "tool.example_store.unavailable"),
            _ => (ErrorCategory::Internal, "tool.example_store.upstream_error"),
        };
        AdkError::new(ErrorComponent::Tool, category, code, message)
            .with_provider("vertex_ai")
            .with_upstream_status(status.as_u16())
    }

    /// Build a URL from the endpoint base, requiring HTTPS for non-loopback
    /// endpoints so example data is never sent in cleartext.
    fn build_url(&self, path: &str) -> Result<String> {
        let mut url = reqwest::Url::parse(&self.endpoint).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::invalid_input(format!("invalid example store endpoint URL: {error}"))
        })?;
        if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
            return Err(Self::invalid_input(
                "example store endpoint must use HTTPS for secure transmission of example data",
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Self::invalid_input(
                "example store endpoint must be an origin without userinfo, path, query, or fragment",
            ));
        }
        url.set_path(&format!("/{}", path.trim_start_matches('/')));
        Ok(url.to_string())
    }

    // ADC auth-header caching copied from adk-session's Vertex backend.
    // Deliberate debt: Wave 3 (PR 3.4) migrates every copy of this pattern to
    // the shared adk-gcp crate; do not extract a helper here.
    async fn auth_headers(&self) -> Result<reqwest::header::HeaderMap> {
        let cacheable_headers = tokio::time::timeout(
            AUTH_HEADERS_TIMEOUT,
            self.credentials.headers(Default::default()),
        )
        .await
        .map_err(|_| {
            Self::timeout_error(format!(
                "example store credential header acquisition timed out after {} seconds",
                AUTH_HEADERS_TIMEOUT.as_secs_f64(),
            ))
        })?
        .map_err(Self::credentials_error)?;

        match cacheable_headers {
            CacheableResource::New { data, .. } => {
                *self.auth_headers.write().await = Some(data.clone());
                Ok(data)
            }
            CacheableResource::NotModified => self
                .auth_headers
                .read()
                .await
                .clone()
                .ok_or_else(|| {
                    Self::auth_error(
                        "google cloud credentials returned NotModified before any cached auth headers were available",
                    )
                }),
        }
    }

    async fn apply_auth(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let headers = self.auth_headers().await?;
        Ok(request.headers(headers))
    }

    async fn send_json<R: for<'de> Deserialize<'de>>(&self, request: RequestBuilder) -> Result<R> {
        let (status, body) = tokio::time::timeout(HTTP_REQUEST_TIMEOUT, async {
            let response = request.send().await.map_err(Self::transport_error)?;
            let status = response.status();
            let body = response.bytes().await.map_err(Self::transport_error)?;
            Ok::<_, AdkError>((status, body))
        })
        .await
        .map_err(|_| {
            Self::timeout_error(format!(
                "example store request timed out after {} seconds",
                HTTP_REQUEST_TIMEOUT.as_secs(),
            ))
        })??;

        if !status.is_success() {
            let body = String::from_utf8_lossy(&body);
            let body = if body.trim().is_empty() { "<empty body>" } else { body.as_ref() };
            return Err(Self::status_error(status, body));
        }

        serde_json::from_slice(&body).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::internal_error(format!("failed to parse example store response JSON: {error}"))
                .with_upstream_status(status.as_u16())
        })
    }
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

fn truncate_for_error(value: &str) -> String {
    const MAX_LEN: usize = 512;
    let mut sanitized = String::with_capacity(value.len().min(MAX_LEN));
    let mut truncated = false;
    for character in value.chars() {
        let character =
            if character.is_control() { char::REPLACEMENT_CHARACTER } else { character };
        if sanitized.len() + character.len_utf8() > MAX_LEN {
            truncated = true;
            break;
        }
        sanitized.push(character);
    }
    if truncated {
        sanitized.push_str("...");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_top_k_serializes_as_a_string_and_accepts_both_forms() {
        let request = SearchExamplesRequest::by_search_key("query", 5);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["topK"], json!("5"));

        let from_string: SearchExamplesRequest = serde_json::from_value(json!({
            "topK": "7",
            "parameters": { "storedContentsExampleParameters": { "searchKey": "q" } },
        }))
        .unwrap();
        assert_eq!(from_string.top_k, 7);

        let from_number: SearchExamplesRequest = serde_json::from_value(json!({
            "topK": 7,
            "parameters": { "storedContentsExampleParameters": { "searchKey": "q" } },
        }))
        .unwrap();
        assert_eq!(from_number.top_k, 7);
    }

    #[test]
    fn test_search_key_generation_method_serializes_last_entry_as_empty_object() {
        let value = serde_json::to_value(SearchKeyGenerationMethod::last_entry()).unwrap();
        assert_eq!(value, json!({ "lastEntry": {} }));
    }

    #[test]
    fn test_config_resolves_bare_ids_and_full_resource_names() {
        let bare = ExampleStoreConfig::new("p", "us-central1", "store-1");
        assert_eq!(bare.store_path(), "projects/p/locations/us-central1/exampleStores/store-1");

        let full = ExampleStoreConfig::new(
            "p",
            "us-central1",
            "projects/other/locations/us-central1/exampleStores/store-2",
        );
        assert_eq!(full.store_path(), "projects/other/locations/us-central1/exampleStores/store-2");
    }

    // async: the credentials builder requires an ambient tokio runtime.
    #[tokio::test]
    async fn test_endpoint_rejects_cleartext_and_decorated_origins() {
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build();
        let config =
            ExampleStoreConfig::new("p", "us-central1", "s").with_endpoint("http://example.com");
        let error = ExampleStoreClient::with_credentials(config, credentials.clone()).unwrap_err();
        assert!(error.message.contains("HTTPS"), "unexpected error: {}", error.message);

        let config = ExampleStoreConfig::new("p", "us-central1", "s")
            .with_endpoint("https://example.com/path");
        let error = ExampleStoreClient::with_credentials(config, credentials).unwrap_err();
        assert!(error.message.contains("origin"), "unexpected error: {}", error.message);
    }
}
