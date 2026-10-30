//! Files API client: upload, list, retrieve, download, and delete files the
//! Chat and Responses endpoints can reference (`/v1/files`).
//!
//! Inline inputs are capped at 50MB; Files API uploads go to 1GiB. Supported
//! types include PNG/JPEG/GIF/WebP, MP4, MP3/WAV, PDF, TXT/JSON/JSONL.

use super::config::MuseConfig;
use adk_core::{AdkError, ErrorComponent};
use reqwest::Client;
use serde::Deserialize;

/// One stored file.
#[derive(Debug, Clone, Deserialize)]
pub struct FileObject {
    /// File id (`file-..`) usable in `input_file`/`input_image` parts.
    pub id: String,
    /// Stored file name.
    #[serde(default)]
    pub filename: Option<String>,
    /// Size in bytes.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// Upload purpose (for example `user_data`).
    #[serde(default)]
    pub purpose: Option<String>,
    /// Unix creation timestamp.
    #[serde(default)]
    pub created_at: Option<u64>,
}

/// File list envelope.
#[derive(Debug, Clone, Deserialize)]
struct FileList {
    #[serde(default)]
    data: Vec<FileObject>,
}

/// Files API client.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::muse::{MuseConfig, MuseFilesClient};
///
/// let files = MuseFilesClient::new(MuseConfig::new(api_key, "muse-spark-1.3"))?;
/// let stored = files.upload(pdf_bytes, "spec.pdf", "application/pdf", "user_data").await?;
/// ```
pub struct MuseFilesClient {
    client: Client,
    config: MuseConfig,
}

impl MuseFilesClient {
    /// Create a Files client.
    pub fn new(config: MuseConfig) -> Result<Self, AdkError> {
        let client = crate::HttpTimeouts::default()
            .apply(Client::builder())
            .build()
            .map_err(|e| AdkError::model(format!("Failed to create HTTP client: {e}")))?;
        Ok(Self { client, config })
    }

    /// Upload a file (multipart). `purpose` defaults server-side to `user_data`.
    ///
    /// # Errors
    ///
    /// Returns the API error (HTTP status, code `model.muse.api_error`) on rejection.
    pub async fn upload(
        &self,
        bytes: Vec<u8>,
        filename: impl Into<String>,
        content_type: &str,
        purpose: impl Into<String>,
    ) -> Result<FileObject, AdkError> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.into())
            .mime_str(content_type)
            .map_err(|e| AdkError::model(format!("invalid file content type: {e}")))?;
        let form =
            reqwest::multipart::Form::new().text("purpose", purpose.into()).part("file", part);
        let response = self
            .client
            .post(format!("{}/files", self.base()))
            .header("Authorization", self.auth())
            .multipart(form)
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        self.read_json(response).await
    }

    /// List stored files.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn list(&self) -> Result<Vec<FileObject>, AdkError> {
        let response = self
            .client
            .get(format!("{}/files", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        let list: FileList = self.read_json(response).await?;
        Ok(list.data)
    }

    /// Retrieve one file's metadata.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn get(&self, file_id: &str) -> Result<FileObject, AdkError> {
        let response = self
            .client
            .get(format!("{}/files/{file_id}", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        self.read_json(response).await
    }

    /// Download a file's raw bytes.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn content(&self, file_id: &str) -> Result<Vec<u8>, AdkError> {
        let response = self
            .client
            .get(format!("{}/files/{file_id}/content", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        if !response.status().is_success() {
            return Err(self.api_error(response).await);
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| AdkError::model(format!("Failed to read file content: {e}")))
    }

    /// Delete a stored file.
    ///
    /// # Errors
    ///
    /// Returns the API error on rejection.
    pub async fn delete(&self, file_id: &str) -> Result<(), AdkError> {
        let response = self
            .client
            .delete(format!("{}/files/{file_id}", self.base()))
            .header("Authorization", self.auth())
            .send()
            .await
            .map_err(|e| self.transport(e))?;
        if !response.status().is_success() {
            return Err(self.api_error(response).await);
        }
        Ok(())
    }

    fn base(&self) -> String {
        self.config.effective_base_url().trim_end_matches('/').to_string()
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.config.api_key)
    }

    fn transport(&self, error: reqwest::Error) -> AdkError {
        AdkError::new(
            ErrorComponent::Model,
            crate::http_timeouts::transport_category(&error),
            "model.muse.request",
            format!("Muse files request failed: {error}"),
        )
        .with_provider("muse")
    }

    async fn api_error(&self, response: reqwest::Response) -> AdkError {
        let status = response.status();
        let status_code = status.as_u16();
        let error_text = response.text().await.unwrap_or_default();
        AdkError::new(
            ErrorComponent::Model,
            crate::retry::category_for_status_code(status_code),
            "model.muse.api_error",
            format!(
                "Muse files API error (HTTP {status}): {}",
                adk_core::upstream_excerpt(&error_text)
            ),
        )
        .with_upstream_status(status_code)
        .with_provider("muse")
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, AdkError> {
        if !response.status().is_success() {
            return Err(self.api_error(response).await);
        }
        let text = response
            .text()
            .await
            .map_err(|e| AdkError::model(format!("Failed to read files response: {e}")))?;
        serde_json::from_str(&text).map_err(|e| {
            AdkError::model(format!(
                "Failed to parse files response: {}",
                adk_core::json_error_summary(&e)
            ))
        })
    }
}
