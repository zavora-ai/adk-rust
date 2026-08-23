//! Google Cloud Storage (GCS) artifact backend.
//!
//! Blob-name layout, version numbering, and object metadata keys are
//! transcribed from adk-python's `GcsArtifactService`
//! (google/adk-python, `src/google/adk/artifacts/gcs_artifact_service.py`,
//! transcribed from google-adk 2.6.3) so that a Rust agent, a Python agent,
//! and the Gemini Enterprise console all read the same objects.
//!
//! # Dependency choice
//!
//! This module talks to the GCS JSON API directly with `reqwest`; credential
//! construction and auth-header caching come from `adk-gcp`'s
//! [`GcpHttpClient`], the shared plumbing used by the workspace's Vertex
//! backends. GCS is not the aiplatform API — binary up/downloads
//! (`alt=media`), `multipart/related` uploads, and the `storage/v1` /
//! `upload/storage/v1` URL scheme don't fit `adk-gcp`'s JSON-only helpers —
//! so request building and response handling stay local and the
//! `GcpHttpClient` serves purely as the credential/auth-header provider.
//! The official `google-cloud-storage` crate would add a substantially
//! heavier dependency tree (gRPC, prost, tower) for the five plain-HTTP
//! calls this service needs.

use crate::service::{
    ArtifactService, DeleteRequest, ListRequest, ListResponse, LoadRequest, LoadResponse,
    SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
use adk_core::{AdkError, ErrorComponent, Part, Result};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient};
use async_trait::async_trait;
use google_cloud_auth::credentials::Credentials;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "https://storage.googleapis.com";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

// Error identity stamped by the shared adk-gcp plumbing. Only the
// credential-path slots (unauthorized, credentials_unavailable, timeout) and
// the builder's invalid_response slot are ever produced — GCS request and
// response handling stays local — so the remaining slots carry the closest
// existing literal and introduce no new codes.
const GCS_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "artifact.gcs.invalid_input",
    unauthorized: "artifact.gcs.unauthorized",
    forbidden: "artifact.gcs.unauthorized",
    not_found: "artifact.gcs.not_found",
    rate_limited: "artifact.gcs.rate_limited",
    timeout: "artifact.gcs.unavailable",
    unavailable: "artifact.gcs.unavailable",
    credentials_unavailable: "artifact.gcs.unavailable",
    invalid_response: "artifact.gcs.internal",
    invalid_request: "artifact.gcs.invalid_input",
    upstream_error: "artifact.gcs.internal",
    operation_failed: "artifact.gcs.internal",
};

fn gcp_error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Artifact, GCS_ERROR_CODES, "gcs").with_provider("gcs")
}

// GCS object metadata keys shared with adk-python's GcsArtifactService.
const GCS_DISPLAY_NAME_METADATA_KEY: &str = "adkDisplayName";
const GCS_IS_TEXT_METADATA_KEY: &str = "adkIsText";
const GCS_FILE_URI_METADATA_KEY: &str = "adkFileUri";
const GCS_FILE_MIME_TYPE_METADATA_KEY: &str = "adkFileMimeType";

/// Artifact storage backed by a Google Cloud Storage bucket.
///
/// Blob names match adk-python's `GcsArtifactService` byte for byte
/// (transcribed from google-adk 2.6.3):
///
/// - Session-scoped: `{app_name}/{user_id}/{session_id}/{filename}/{version}`
/// - User-namespaced (filename starts with `user:`):
///   `{app_name}/{user_id}/user/{filename}/{version}`
///
/// Auto-assigned versions start at `0` and increment by one, matching
/// adk-python. This differs from [`InMemoryArtifactService`](crate::InMemoryArtifactService),
/// which starts at `1`.
///
/// # `app_name` consistency
///
/// The `app_name` is the leading blob-name segment, so **save and load must
/// resolve the same `app_name`** — when deployed to Agent Engine that is the
/// engine ID. adk-python has a live save/load path-mismatch bug where the two
/// operations resolved different app names and artifacts silently vanished
/// (googleapis/python-aiplatform#6521). This implementation takes `app_name`
/// verbatim from each request and never rewrites it; callers own keeping it
/// stable across operations.
///
/// # Example
///
/// ```rust,no_run
/// use adk_artifact::{ArtifactService, GcsArtifactService, SaveRequest};
/// use adk_core::Part;
///
/// #[tokio::main]
/// async fn main() -> adk_core::Result<()> {
///     // Uses Application Default Credentials.
///     let service = GcsArtifactService::new_with_adc("my-artifact-bucket")?;
///
///     let saved = service
///         .save(SaveRequest {
///             app_name: "my_app".to_string(),
///             user_id: "user_123".to_string(),
///             session_id: "session_456".to_string(),
///             file_name: "report.pdf".to_string(),
///             part: Part::InlineData {
///                 mime_type: "application/pdf".to_string(),
///                 data: vec![0x25, 0x50, 0x44, 0x46],
///                 uri: None,
///                 annotations: None,
///             },
///             version: None,
///         })
///         .await?;
///     assert_eq!(saved.version, 0); // versions start at 0, like adk-python
///     Ok(())
/// }
/// ```
pub struct GcsArtifactService {
    http_client: Client,
    bucket: String,
    endpoint: String,
    // Credential/auth-header provider only — its transport and base URL are
    // never used, so `with_endpoint` overrides don't need to rebuild it.
    gcp: GcpHttpClient,
}

/// One object returned by the GCS JSON API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectResource {
    name: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListObjectsResponse {
    #[serde(default)]
    items: Option<Vec<ObjectResource>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

impl GcsArtifactService {
    /// Creates a service using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed or the HTTP client
    /// cannot be built.
    pub fn new_with_adc(bucket: impl Into<String>) -> Result<Self> {
        // The builder's default scopes are already `cloud-platform`.
        let gcp = GcpHttpClient::builder(gcp_error_context(), DEFAULT_ENDPOINT).build()?;
        Self::with_auth_provider(bucket, gcp)
    }

    /// Creates a service with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be built.
    pub fn with_credentials(bucket: impl Into<String>, credentials: Credentials) -> Result<Self> {
        let gcp = GcpHttpClient::builder(gcp_error_context(), DEFAULT_ENDPOINT)
            .credentials(credentials)
            .build()?;
        Self::with_auth_provider(bucket, gcp)
    }

    fn with_auth_provider(bucket: impl Into<String>, gcp: GcpHttpClient) -> Result<Self> {
        let http_client = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                Self::internal_error(format!(
                    "failed to build gcs artifact HTTP client: {}",
                    error.without_url()
                ))
            })?;
        Ok(Self { http_client, bucket: bucket.into(), endpoint: DEFAULT_ENDPOINT.to_string(), gcp })
    }

    /// Overrides the GCS API origin, e.g. for a local emulator or test server.
    ///
    /// Both the JSON API path (`/storage/v1/...`) and the upload path
    /// (`/upload/storage/v1/...`) are resolved against this origin.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into().trim_end_matches('/').to_string();
        self
    }

    fn internal_error(message: impl Into<String>) -> AdkError {
        AdkError::internal(ErrorComponent::Artifact, "artifact.gcs.internal", message)
            .with_provider("gcs")
    }

    fn invalid_input(message: impl Into<String>) -> AdkError {
        AdkError::new(
            ErrorComponent::Artifact,
            adk_core::ErrorCategory::InvalidInput,
            "artifact.gcs.invalid_input",
            message,
        )
        .with_provider("gcs")
    }

    fn not_found_error(message: impl Into<String>) -> AdkError {
        AdkError::not_found(ErrorComponent::Artifact, "artifact.gcs.not_found", message)
            .with_provider("gcs")
    }

    fn auth_error(message: impl Into<String>) -> AdkError {
        AdkError::unauthorized(ErrorComponent::Artifact, "artifact.gcs.unauthorized", message)
            .with_provider("gcs")
    }

    fn http_error(status: StatusCode, context: &str, body: &str) -> AdkError {
        let body: String = body.chars().take(512).collect();
        let message = format!("gcs {context} failed with status {status}: {body}");
        let error = match status {
            StatusCode::NOT_FOUND => Self::not_found_error(message),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Self::auth_error(message),
            StatusCode::TOO_MANY_REQUESTS => AdkError::rate_limited(
                ErrorComponent::Artifact,
                "artifact.gcs.rate_limited",
                message,
            )
            .with_provider("gcs"),
            status if status.is_server_error() => {
                AdkError::unavailable(ErrorComponent::Artifact, "artifact.gcs.unavailable", message)
                    .with_provider("gcs")
            }
            _ => Self::internal_error(message),
        };
        error.with_upstream_status(status.as_u16())
    }

    fn objects_url(&self) -> String {
        format!("{}/storage/v1/b/{}/o", self.endpoint, percent_encode(&self.bucket))
    }

    fn object_url(&self, blob_name: &str) -> String {
        format!("{}/{}", self.objects_url(), percent_encode(blob_name))
    }

    fn upload_url(&self) -> String {
        format!("{}/upload/storage/v1/b/{}/o", self.endpoint, percent_encode(&self.bucket))
    }

    /// Returns the blob-name prefix (without the trailing `/{version}`).
    ///
    /// Matches `GcsArtifactService._get_blob_prefix` in adk-python 2.6.3.
    fn blob_prefix(
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<String> {
        validate_path_segment(app_name, "app_name")?;
        validate_path_segment(user_id, "user_id")?;
        validate_file_name(file_name)?;
        if file_has_user_namespace(file_name) {
            return Ok(format!("{app_name}/{user_id}/user/{file_name}"));
        }
        validate_path_segment(session_id, "session_id")?;
        Ok(format!("{app_name}/{user_id}/{session_id}/{file_name}"))
    }

    /// Returns the full blob name including the version segment.
    ///
    /// Matches `GcsArtifactService._get_blob_name` in adk-python 2.6.3.
    fn blob_name(
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<String> {
        Ok(format!("{}/{version}", Self::blob_prefix(app_name, user_id, session_id, file_name)?))
    }

    async fn list_objects(&self, prefix: &str) -> Result<Vec<ObjectResource>> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, &str)> = vec![("prefix", prefix)];
            if let Some(token) = page_token.as_deref() {
                query.push(("pageToken", token));
            }
            let request =
                self.gcp.apply_auth(self.http_client.get(self.objects_url()).query(&query)).await?;
            let response = request.send().await.map_err(|error| {
                Self::internal_error(format!(
                    "gcs list objects request failed: {}",
                    error.without_url()
                ))
            })?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(Self::http_error(status, "list objects", &body));
            }
            let page: ListObjectsResponse = response.json().await.map_err(|error| {
                Self::internal_error(format!(
                    "gcs list objects response was not valid JSON: {}",
                    error.without_url()
                ))
            })?;
            items.extend(page.items.unwrap_or_default());
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => break,
            }
        }
        Ok(items)
    }

    /// Lists available versions for an artifact, in ascending order.
    async fn list_versions(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<Vec<i64>> {
        let prefix = format!("{}/", Self::blob_prefix(app_name, user_id, session_id, file_name)?);
        let mut versions: Vec<i64> = self
            .list_objects(&prefix)
            .await?
            .iter()
            .filter_map(|object| parse_version(&object.name, &prefix))
            .collect();
        versions.sort_unstable();
        Ok(versions)
    }

    async fn get_object(&self, blob_name: &str) -> Result<Option<ObjectResource>> {
        let request = self.gcp.apply_auth(self.http_client.get(self.object_url(blob_name))).await?;
        let response = request.send().await.map_err(|error| {
            Self::internal_error(format!("gcs get object request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Self::http_error(status, "get object", &body));
        }
        let object: ObjectResource = response.json().await.map_err(|error| {
            Self::internal_error(format!(
                "gcs object resource was not valid JSON: {}",
                error.without_url()
            ))
        })?;
        Ok(Some(object))
    }

    async fn download_object(&self, blob_name: &str) -> Result<Vec<u8>> {
        let request = self
            .gcp
            .apply_auth(self.http_client.get(self.object_url(blob_name)).query(&[("alt", "media")]))
            .await?;
        let response = request.send().await.map_err(|error| {
            Self::internal_error(format!("gcs download request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Self::http_error(status, "download object", &body));
        }
        let bytes = response.bytes().await.map_err(|error| {
            Self::internal_error(format!("gcs download body read failed: {}", error.without_url()))
        })?;
        Ok(bytes.to_vec())
    }

    async fn upload_object(
        &self,
        blob_name: &str,
        content_type: Option<&str>,
        metadata: &HashMap<String, String>,
        data: &[u8],
    ) -> Result<()> {
        let mut resource = json!({ "name": blob_name });
        if let Some(content_type) = content_type {
            resource["contentType"] = Value::String(content_type.to_string());
        }
        if !metadata.is_empty() {
            resource["metadata"] = json!(metadata);
        }
        let (boundary, body) = multipart_related_body(&resource, content_type, data)?;
        let request = self
            .gcp
            .apply_auth(
                self.http_client
                    .post(self.upload_url())
                    .query(&[("uploadType", "multipart")])
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        format!("multipart/related; boundary={boundary}"),
                    )
                    .body(body),
            )
            .await?;
        let response = request.send().await.map_err(|error| {
            Self::internal_error(format!("gcs upload request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Self::http_error(status, "upload object", &body));
        }
        Ok(())
    }

    async fn delete_object(&self, blob_name: &str) -> Result<()> {
        let request =
            self.gcp.apply_auth(self.http_client.delete(self.object_url(blob_name))).await?;
        let response = request.send().await.map_err(|error| {
            Self::internal_error(format!("gcs delete request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        // Deleting an already-absent version is not an error, matching the
        // idempotent delete semantics of the other backends in this crate.
        if status == StatusCode::NOT_FOUND || status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        Err(Self::http_error(status, "delete object", &body))
    }
}

fn file_has_user_namespace(file_name: &str) -> bool {
    file_name.starts_with("user:")
}

fn validate_path_segment(segment: &str, field: &str) -> Result<()> {
    if segment.is_empty() || segment.contains('/') || segment == "." || segment == ".." {
        return Err(GcsArtifactService::invalid_input(format!(
            "invalid {field} '{segment}': must be a non-empty path segment without '/'"
        )));
    }
    Ok(())
}

// Filenames may contain '/' (adk-python allows nested artifact names), but no
// component may be a traversal pattern and the name may not start or end with '/'.
fn validate_file_name(file_name: &str) -> Result<()> {
    let valid = !file_name.is_empty()
        && !file_name.starts_with('/')
        && !file_name.ends_with('/')
        && !file_name.contains('\\')
        && file_name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..");
    if !valid {
        return Err(GcsArtifactService::invalid_input(format!(
            "invalid artifact file name '{file_name}': empty components, traversal patterns, and backslashes are not allowed"
        )));
    }
    Ok(())
}

/// Extracts the version number from a blob name under `prefix`.
///
/// GCS has a flat namespace, so listing by `prefix` also returns artifacts
/// nested under this one (filenames may contain `/`). A blob holds a version
/// of this artifact only when its name is exactly `{prefix}{version}`.
/// Matches `_parse_version` in adk-python 2.6.3.
fn parse_version(blob_name: &str, prefix: &str) -> Option<i64> {
    let suffix = blob_name.strip_prefix(prefix)?;
    if suffix.contains('/') {
        // Belongs to a distinct artifact nested under this one.
        return None;
    }
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        tracing::warn!(
            gcs.blob_name = blob_name,
            "skipping blob because it does not end with a version number"
        );
        return None;
    }
    suffix.parse().ok()
}

/// Percent-encodes every byte outside the RFC 3986 unreserved set, so blob
/// names containing `/` become a single URL path segment.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Builds a `multipart/related` body for a GCS multipart upload: a JSON
/// object-resource part followed by the media part.
fn multipart_related_body(
    resource: &Value,
    content_type: Option<&str>,
    data: &[u8],
) -> Result<(String, Vec<u8>)> {
    let resource_json = serde_json::to_vec(resource).map_err(|error| {
        GcsArtifactService::internal_error(format!(
            "failed to encode gcs object resource JSON: {error}"
        ))
    })?;
    // The boundary must not occur in either part's payload.
    let mut boundary = String::from("adk_gcs_artifact_boundary");
    while contains_subslice(data, boundary.as_bytes())
        || contains_subslice(&resource_json, boundary.as_bytes())
    {
        boundary.push('x');
    }
    let media_content_type = content_type.unwrap_or("application/octet-stream");
    let mut body = Vec::with_capacity(data.len() + resource_json.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(&resource_json);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Type: {media_content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((boundary, body))
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The upload payload derived from a [`Part`].
struct UploadPlan {
    content_type: Option<String>,
    metadata: HashMap<String, String>,
    data: Vec<u8>,
}

fn plan_upload(part: &Part) -> Result<UploadPlan> {
    match part {
        Part::InlineData { mime_type, data, annotations, .. } => {
            let mut metadata = HashMap::new();
            // adk-core's Part has no display-name field; a "displayName"
            // annotation round-trips through the adkDisplayName metadata key.
            if let Some(Value::Object(map)) = annotations
                && let Some(Value::String(display_name)) = map.get("displayName")
            {
                metadata.insert(GCS_DISPLAY_NAME_METADATA_KEY.to_string(), display_name.clone());
            }
            Ok(UploadPlan { content_type: Some(mime_type.clone()), metadata, data: data.clone() })
        }
        Part::Text { text } => {
            // Flagged so load reconstructs Part::Text instead of inline data,
            // matching adk-python.
            let metadata =
                HashMap::from([(GCS_IS_TEXT_METADATA_KEY.to_string(), "true".to_string())]);
            Ok(UploadPlan {
                content_type: Some("text/plain".to_string()),
                metadata,
                data: text.clone().into_bytes(),
            })
        }
        Part::FileData { mime_type, file_uri, .. } => {
            if file_uri.is_empty() {
                return Err(GcsArtifactService::invalid_input(
                    "artifact file_data must have a file_uri",
                ));
            }
            // URI reference only: metadata carries the URI, the object body is empty.
            let mut metadata =
                HashMap::from([(GCS_FILE_URI_METADATA_KEY.to_string(), file_uri.clone())]);
            let content_type = if mime_type.is_empty() {
                None
            } else {
                metadata.insert(GCS_FILE_MIME_TYPE_METADATA_KEY.to_string(), mime_type.clone());
                Some(mime_type.clone())
            };
            Ok(UploadPlan { content_type, metadata, data: Vec::new() })
        }
        Part::Thinking { .. }
        | Part::FunctionCall { .. }
        | Part::FunctionResponse { .. }
        | Part::ServerToolCall { .. }
        | Part::ServerToolResponse { .. }
        | Part::EmbeddedResource { .. } => Err(GcsArtifactService::invalid_input(
            "artifact must be text, inline data, or file data",
        )),
    }
}

fn reconstruct_part(object: &ObjectResource, data: Option<Vec<u8>>) -> Result<Part> {
    let metadata = object.metadata.clone().unwrap_or_default();
    // "file_uri" is the legacy key some older adk-python versions wrote.
    let file_uri = metadata
        .get(GCS_FILE_URI_METADATA_KEY)
        .or_else(|| metadata.get("file_uri"))
        .filter(|uri| !uri.is_empty());
    if let Some(file_uri) = file_uri {
        let mime_type = metadata
            .get(GCS_FILE_MIME_TYPE_METADATA_KEY)
            .cloned()
            .or_else(|| object.content_type.clone())
            .unwrap_or_default();
        return Ok(Part::FileData { mime_type, file_uri: file_uri.clone(), annotations: None });
    }
    let data = data.unwrap_or_default();
    if metadata.get(GCS_IS_TEXT_METADATA_KEY).is_some_and(|value| value == "true") {
        let text = String::from_utf8(data).map_err(|error| {
            GcsArtifactService::internal_error(format!(
                "gcs text artifact is not valid UTF-8: {error}"
            ))
        })?;
        return Ok(Part::Text { text });
    }
    let annotations = metadata
        .get(GCS_DISPLAY_NAME_METADATA_KEY)
        .map(|display_name| json!({ "displayName": display_name }));
    Ok(Part::InlineData {
        mime_type: object
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string()),
        data,
        uri: None,
        annotations,
    })
}

#[async_trait]
impl ArtifactService for GcsArtifactService {
    #[tracing::instrument(skip_all, fields(artifact.file_name = %req.file_name))]
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse> {
        let version = match req.version {
            Some(version) => version,
            // First auto-assigned version is 0, matching adk-python.
            None => self
                .list_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
                .await?
                .last()
                .map_or(0, |latest| latest + 1),
        };
        let blob_name =
            Self::blob_name(&req.app_name, &req.user_id, &req.session_id, &req.file_name, version)?;
        let plan = plan_upload(&req.part)?;
        self.upload_object(&blob_name, plan.content_type.as_deref(), &plan.metadata, &plan.data)
            .await?;
        tracing::debug!(gcs.blob_name = blob_name, artifact.version = version, "artifact saved");
        Ok(SaveResponse { version })
    }

    #[tracing::instrument(skip_all, fields(artifact.file_name = %req.file_name))]
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        let version = match req.version {
            Some(version) => version,
            None => *self
                .list_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
                .await?
                .last()
                .ok_or_else(|| Self::not_found_error("artifact not found"))?,
        };
        let blob_name =
            Self::blob_name(&req.app_name, &req.user_id, &req.session_id, &req.file_name, version)?;
        let object = self
            .get_object(&blob_name)
            .await?
            .ok_or_else(|| Self::not_found_error("artifact not found"))?;
        let is_uri_reference = object.metadata.as_ref().is_some_and(|metadata| {
            metadata.contains_key(GCS_FILE_URI_METADATA_KEY) || metadata.contains_key("file_uri")
        });
        let data =
            if is_uri_reference { None } else { Some(self.download_object(&blob_name).await?) };
        Ok(LoadResponse { part: reconstruct_part(&object, data)? })
    }

    #[tracing::instrument(skip_all, fields(artifact.file_name = %req.file_name))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let versions = match req.version {
            Some(version) => vec![version],
            None => {
                self.list_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
                    .await?
            }
        };
        for version in versions {
            let blob_name = Self::blob_name(
                &req.app_name,
                &req.user_id,
                &req.session_id,
                &req.file_name,
                version,
            )?;
            self.delete_object(&blob_name).await?;
        }
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn list(&self, req: ListRequest) -> Result<ListResponse> {
        validate_path_segment(&req.app_name, "app_name")?;
        validate_path_segment(&req.user_id, "user_id")?;
        validate_path_segment(&req.session_id, "session_id")?;
        let mut file_names = BTreeSet::new();
        let session_prefix = format!("{}/{}/{}/", req.app_name, req.user_id, req.session_id);
        let user_prefix = format!("{}/{}/user/", req.app_name, req.user_id);
        for prefix in [&session_prefix, &user_prefix] {
            for object in self.list_objects(prefix).await? {
                // Blob names are `{prefix}{filename}/{version}`; the filename
                // itself may contain '/', so only the final segment is dropped.
                let Some(suffix) = object.name.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                if let Some((file_name, _version)) = suffix.rsplit_once('/') {
                    file_names.insert(file_name.to_string());
                }
            }
        }
        Ok(ListResponse { file_names: file_names.into_iter().collect() })
    }

    #[tracing::instrument(skip_all, fields(artifact.file_name = %req.file_name))]
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse> {
        let versions = self
            .list_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
            .await?;
        if versions.is_empty() {
            return Err(Self::not_found_error("artifact not found"));
        }
        Ok(VersionsResponse { versions })
    }

    async fn health_check(&self) -> Result<()> {
        let url = format!("{}/storage/v1/b/{}", self.endpoint, percent_encode(&self.bucket));
        let request = self.gcp.apply_auth(self.http_client.get(url)).await?;
        let response = request.send().await.map_err(|error| {
            Self::internal_error(format!(
                "gcs health check request failed: {}",
                error.without_url()
            ))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Self::http_error(status, "health check", &body));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Golden blob names transcribed from adk-python's GcsArtifactService
    // (google-adk 2.6.3): {app_name}/{user_id}/{session_id}/{filename}/{version}
    #[test]
    fn test_blob_name_session_scoped_matches_adk_python() {
        let name =
            GcsArtifactService::blob_name("my-app", "user123", "session456", "report.pdf", 0)
                .unwrap();
        assert_eq!(name, "my-app/user123/session456/report.pdf/0");
    }

    // User-namespaced filenames keep the `user:` prefix in the blob name and
    // replace the session segment with the literal string "user".
    #[test]
    fn test_blob_name_user_namespace_matches_adk_python() {
        let name =
            GcsArtifactService::blob_name("my-app", "user123", "session456", "user:profile.png", 3)
                .unwrap();
        assert_eq!(name, "my-app/user123/user/user:profile.png/3");
    }

    // The engine ID is the app_name when deployed; save and load must build
    // identical names from it (googleapis/python-aiplatform#6521).
    #[test]
    fn test_blob_name_engine_id_app_name() {
        let name = GcsArtifactService::blob_name(
            "1234567890123456789",
            "user123",
            "session456",
            "chart.png",
            7,
        )
        .unwrap();
        assert_eq!(name, "1234567890123456789/user123/session456/chart.png/7");
    }

    #[test]
    fn test_blob_name_allows_nested_filenames() {
        let name =
            GcsArtifactService::blob_name("app", "user", "session", "reports/q1.pdf", 2).unwrap();
        assert_eq!(name, "app/user/session/reports/q1.pdf/2");
    }

    #[test]
    fn test_blob_name_rejects_invalid_segments() {
        for (app, user, session, file) in [
            ("", "user", "session", "file"),
            ("app/x", "user", "session", "file"),
            ("app", "..", "session", "file"),
            ("app", "user", "a/b", "file"),
            ("app", "user", "session", ""),
            ("app", "user", "session", "../etc/passwd"),
            ("app", "user", "session", "a//b"),
            ("app", "user", "session", "/leading"),
            ("app", "user", "session", "trailing/"),
            ("app", "user", "session", "back\\slash"),
        ] {
            assert!(
                GcsArtifactService::blob_name(app, user, session, file, 0).is_err(),
                "expected rejection for {app}/{user}/{session}/{file}"
            );
        }
    }

    #[test]
    fn test_parse_version_matches_adk_python() {
        let prefix = "app/user/session/a/";
        assert_eq!(parse_version("app/user/session/a/3", prefix), Some(3));
        assert_eq!(parse_version("app/user/session/a/0", prefix), Some(0));
        // Version 3 of the distinct nested artifact "a/b", not of "a".
        assert_eq!(parse_version("app/user/session/a/b/3", prefix), None);
        // Non-numeric suffixes are skipped.
        assert_eq!(parse_version("app/user/session/a/latest", prefix), None);
        assert_eq!(parse_version("app/user/session/a/ 3", prefix), None);
        assert_eq!(parse_version("app/user/session/a/1_0", prefix), None);
        // Different prefix entirely.
        assert_eq!(parse_version("other/user/session/a/3", prefix), None);
    }

    #[test]
    fn test_percent_encode_escapes_slashes_and_colons() {
        assert_eq!(
            percent_encode("app/user/user/user:profile.png/0"),
            "app%2Fuser%2Fuser%2Fuser%3Aprofile.png%2F0"
        );
        assert_eq!(percent_encode("simple-name_1.bin~"), "simple-name_1.bin~");
    }

    #[test]
    fn test_multipart_body_boundary_never_collides() {
        let data = b"payload with adk_gcs_artifact_boundary embedded".to_vec();
        let (boundary, body) =
            multipart_related_body(&json!({"name": "n"}), Some("text/plain"), &data).unwrap();
        assert!(boundary.starts_with("adk_gcs_artifact_boundary"));
        assert_ne!(boundary, "adk_gcs_artifact_boundary");
        assert!(contains_subslice(&body, format!("--{boundary}--\r\n").as_bytes()));
    }

    #[test]
    fn test_plan_upload_rejects_non_data_parts() {
        let part = Part::FunctionCall {
            name: "f".to_string(),
            args: json!({}),
            id: None,
            thought_signature: None,
        };
        assert!(plan_upload(&part).is_err());
    }
}
