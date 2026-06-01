//! FilesClient implementation.

use std::sync::Arc;

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::multipart;

use super::types::{FileDeleteResponse, FileListResponse, FileObject};
use crate::{Error, Result};

/// Default base URL for the Anthropic API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Client for the Anthropic Files API.
///
/// Provides methods to upload, download, list, get, and delete files.
/// All requests include the `files-api-2025-04-14` beta header.
///
/// # Example
///
/// ```rust,ignore
/// use adk_anthropic::files::FilesClient;
///
/// let client = FilesClient::new("sk-ant-api03-...")?;
/// let file = client.upload_file("data.csv", csv_bytes).await?;
/// ```
#[derive(Debug, Clone)]
pub struct FilesClient {
    client: reqwest::Client,
    base_url: String,
    cached_headers: Arc<HeaderMap>,
    /// Headers for multipart uploads (no content-type, let reqwest set it).
    upload_headers: Arc<HeaderMap>,
}

impl FilesClient {
    /// Create a new Files client from an API key.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let cached_headers = Arc::new(build_headers(&api_key)?);
        let upload_headers = Arc::new(build_upload_headers(&api_key)?);

        Ok(Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            cached_headers,
            upload_headers,
        })
    }

    /// Create from the `ANTHROPIC_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| Error::Authentication {
            message: "ANTHROPIC_API_KEY environment variable is not set".to_string(),
        })?;
        Self::new(api_key)
    }

    /// Override the base URL.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn build_url(&self, endpoint: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1/{endpoint}")
    }

    // ─── File Operations ─────────────────────────────────────────────────

    /// Upload a file.
    ///
    /// The file is stored and a `FileObject` with the assigned `id` is returned.
    /// Use the `id` in Messages API requests to reference the file.
    ///
    /// # Arguments
    ///
    /// * `filename` - The filename (1-255 chars, no forbidden characters)
    /// * `data` - The file content as bytes
    pub async fn upload_file(
        &self,
        filename: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<FileObject> {
        let url = self.build_url("files");
        let filename = filename.into();

        // Infer MIME type from filename extension
        let mime = infer_mime_type(&filename);

        let part =
            multipart::Part::bytes(data).file_name(filename).mime_str(mime).map_err(|e| {
                Error::BadRequest { message: format!("invalid mime type: {e}"), param: None }
            })?;
        let form = multipart::Form::new().part("file", part);

        let response = self
            .client
            .post(&url)
            .headers((*self.upload_headers).clone())
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to upload file: {e}"), None))?;

        handle_response(response).await
    }

    /// Upload a file from a path (reads the file and uploads it).
    ///
    /// The filename is derived from the path.
    pub async fn upload_file_from_path(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<FileObject> {
        let path = path.as_ref();
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
        let data = tokio::fs::read(path).await.map_err(|e| Error::Connection {
            message: format!("failed to read file {}: {e}", path.display()),
            source: None,
        })?;
        self.upload_file(filename, data).await
    }

    /// List uploaded files.
    ///
    /// Returns a paginated list of files, newest first.
    pub async fn list_files(&self) -> Result<Vec<FileObject>> {
        let url = self.build_url("files");
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to list files: {e}"), None))?;

        let list: FileListResponse = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get file metadata by ID.
    pub async fn get_file(&self, file_id: &str) -> Result<FileObject> {
        let url = self.build_url(&format!("files/{file_id}"));
        let response = self
            .client
            .get(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to get file: {e}"), None))?;

        handle_response(response).await
    }

    /// Download file content.
    ///
    /// Only works for files created by skills or the code execution tool.
    /// Files you uploaded cannot be downloaded.
    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>> {
        let url = self.build_url(&format!("files/{file_id}/content"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to download file: {e}"), None))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(map_api_error(status, &body));
        }

        response.bytes().await.map(|b| b.to_vec()).map_err(|e| Error::Connection {
            message: format!("failed to read file content: {e}"),
            source: None,
        })
    }

    /// Delete a file permanently.
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let url = self.build_url(&format!("files/{file_id}"));
        let response = self
            .client
            .delete(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to delete file: {e}"), None))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(map_api_error(status, &body));
        }

        // Parse the delete response to confirm
        let _delete_resp: FileDeleteResponse =
            response.json().await.map_err(|e| Error::Serialization {
                message: format!("failed to parse delete response: {e}"),
                source: None,
            })?;

        Ok(())
    }
}

// ─── Header Construction ─────────────────────────────────────────────────────

/// Infer MIME type from filename extension.
fn infer_mime_type(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "text" | "md" | "markdown" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

fn build_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-api-key",
        HeaderValue::from_str(api_key).map_err(|e| Error::Authentication {
            message: format!("invalid API key header value: {e}"),
        })?,
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    headers.insert("anthropic-beta", HeaderValue::from_static("files-api-2025-04-14"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// Headers for multipart uploads — no content-type (reqwest sets it with boundary).
fn build_upload_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-api-key",
        HeaderValue::from_str(api_key).map_err(|e| Error::Authentication {
            message: format!("invalid API key header value: {e}"),
        })?,
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    headers.insert("anthropic-beta", HeaderValue::from_static("files-api-2025-04-14"));
    Ok(headers)
}

// ─── Response Handling ───────────────────────────────────────────────────────

async fn handle_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(map_api_error(status, &body));
    }
    let body = response.text().await.map_err(|e| Error::Serialization {
        message: format!("failed to read response body: {e}"),
        source: None,
    })?;
    serde_json::from_str::<T>(&body).map_err(|e| Error::Serialization {
        message: format!("failed to deserialize response: {e}\nBody: {body}"),
        source: None,
    })
}

fn map_api_error(status: reqwest::StatusCode, body: &str) -> Error {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| body.to_string());
    let error_type = parsed.as_ref().and_then(|v| {
        v.get("error").and_then(|e| e.get("type")).and_then(|t| t.as_str()).map(|s| s.to_string())
    });

    match status.as_u16() {
        400 => Error::BadRequest { message, param: None },
        401 => Error::Authentication { message },
        403 => Error::Permission { message },
        404 => Error::NotFound { message, resource_type: None, resource_id: None },
        413 => Error::BadRequest { message: format!("file too large: {message}"), param: None },
        429 => Error::RateLimit { message, retry_after: None },
        code => Error::Api { status_code: code, error_type, message, request_id: None },
    }
}
