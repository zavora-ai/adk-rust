//! Conversations API client for server-managed conversation state.
//!
//! Provides methods to create, retrieve, and delete conversations via the
//! OpenAI `/v1/conversations` endpoint. Conversations enable server-side
//! history management, allowing items to be automatically prepended to
//! requests without re-sending full history.
//!
//! This module is gated behind the `openai-conversations` feature flag.

use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// Conversations API client for server-managed conversation state.
///
/// Makes direct HTTP calls to the OpenAI Conversations API using `reqwest`
/// with Bearer token authentication.
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::openai::ConversationsClient;
///
/// let client = ConversationsClient::new("sk-...", None);
/// let conversation_id = client.create().await?;
/// let metadata = client.get(&conversation_id).await?;
/// client.delete(&conversation_id).await?;
/// ```
pub struct ConversationsClient {
    /// HTTP client for API calls.
    http: reqwest::Client,
    /// API key for authentication.
    api_key: String,
    /// Base URL for the API.
    base_url: String,
}

impl ConversationsClient {
    /// Create a new `ConversationsClient`.
    ///
    /// # Arguments
    ///
    /// * `api_key` - OpenAI API key for Bearer token authentication.
    /// * `base_url` - Optional custom base URL. Defaults to `https://api.openai.com/v1`.
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
        }
    }

    /// Create a new conversation.
    ///
    /// Calls `POST /v1/conversations` and returns the conversation ID.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` if the API request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let conversation_id = client.create().await?;
    /// println!("Created conversation: {conversation_id}");
    /// ```
    pub async fn create(&self) -> Result<String, AdkError> {
        let url = format!("{}/conversations", self.base_url);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openai_responses.request",
                    format!("OpenAI Conversations API network error during create: {e}"),
                )
                .with_provider("openai-responses")
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let error_body = adk_core::upstream_excerpt(&error_body);
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.conversation_create_failed",
                format!(
                    "OpenAI Conversations API create failed with status {status}: {error_body}"
                ),
            )
            .with_provider("openai-responses")
            .with_upstream_status(status.as_u16()));
        }

        let body = response.text().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.parse",
                format!("Failed to read conversation create response body: {e}"),
            )
            .with_provider("openai-responses")
        })?;

        let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.parse",
                format!("Failed to parse conversation create response JSON: {e}"),
            )
            .with_provider("openai-responses")
        })?;

        let id = json
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Internal,
                    "model.openai_responses.parse",
                    "Conversation create response missing 'id' field",
                )
                .with_provider("openai-responses")
            })?
            .to_string();

        Ok(id)
    }

    /// Retrieve conversation metadata.
    ///
    /// Calls `GET /v1/conversations/{id}` and returns the conversation metadata
    /// as a JSON value.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with category `NotFound` and code
    /// `model.openai_responses.conversation_not_found` if the conversation does not exist (404).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let metadata = client.get("conv_abc123").await?;
    /// println!("Conversation: {metadata}");
    /// ```
    pub async fn get(&self, conversation_id: &str) -> Result<serde_json::Value, AdkError> {
        let url = format!("{}/conversations/{conversation_id}", self.base_url);

        let response = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openai_responses.request",
                    format!("OpenAI Conversations API network error during get: {e}"),
                )
                .with_provider("openai-responses")
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::NotFound,
                "model.openai_responses.conversation_not_found",
                format!("Conversation '{conversation_id}' not found"),
            )
            .with_provider("openai-responses")
            .with_upstream_status(404));
        }

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let error_body = adk_core::upstream_excerpt(&error_body);
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.conversation_get_failed",
                format!("OpenAI Conversations API get failed with status {status}: {error_body}"),
            )
            .with_provider("openai-responses")
            .with_upstream_status(status.as_u16()));
        }

        let body = response.text().await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.parse",
                format!("Failed to read conversation get response body: {e}"),
            )
            .with_provider("openai-responses")
        })?;

        let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.parse",
                format!("Failed to parse conversation get response JSON: {e}"),
            )
            .with_provider("openai-responses")
        })?;

        Ok(json)
    }

    /// Delete a conversation.
    ///
    /// Calls `DELETE /v1/conversations/{id}`.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with category `NotFound` and code
    /// `model.openai_responses.conversation_not_found` if the conversation does not exist (404).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// client.delete("conv_abc123").await?;
    /// ```
    pub async fn delete(&self, conversation_id: &str) -> Result<(), AdkError> {
        let url = format!("{}/conversations/{conversation_id}", self.base_url);

        let response = self
            .http
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openai_responses.request",
                    format!("OpenAI Conversations API network error during delete: {e}"),
                )
                .with_provider("openai-responses")
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::NotFound,
                "model.openai_responses.conversation_not_found",
                format!("Conversation '{conversation_id}' not found"),
            )
            .with_provider("openai-responses")
            .with_upstream_status(404));
        }

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let error_body = adk_core::upstream_excerpt(&error_body);
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.conversation_delete_failed",
                format!(
                    "OpenAI Conversations API delete failed with status {status}: {error_body}"
                ),
            )
            .with_provider("openai-responses")
            .with_upstream_status(status.as_u16()));
        }

        Ok(())
    }
}
