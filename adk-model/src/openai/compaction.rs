//! Compaction service for the OpenAI Responses API.
//!
//! Provides explicit conversation compaction via `POST /v1/responses/{id}/compact`.
//! The compacted response ID is preserved in `provider_metadata["openai"]["response_id"]`
//! for subsequent conversation continuity via `previous_response_id`.

use super::responses_client::OpenAIResponsesClient;
use super::responses_convert;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, LlmResponse};
use async_openai::types::responses::Response;

impl OpenAIResponsesClient {
    /// Explicitly compact a conversation by response ID.
    ///
    /// Calls `POST /v1/responses/{id}/compact` and returns the compacted response.
    /// The compacted response ID is preserved in `provider_metadata["openai"]["response_id"]`
    /// for subsequent `previous_response_id` chaining.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with category `NotFound` and code
    /// `model.openai_responses.compaction_not_found` if the response ID does not exist (404).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let compacted = client.compact_response("resp_abc123").await?;
    /// let new_id = compacted.provider_metadata
    ///     .as_ref()
    ///     .and_then(|m| m.get("openai"))
    ///     .and_then(|o| o.get("response_id"))
    ///     .and_then(|v| v.as_str());
    /// ```
    pub async fn compact_response(&self, response_id: &str) -> Result<LlmResponse, AdkError> {
        let url = format!("{}/responses/{}/compact", self.base_url(), response_id);

        let response = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key()))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openai_responses.request",
                    format!("OpenAI Responses API network error during compaction: {e}"),
                )
                .with_provider("openai-responses")
            })?;

        let status = response.status();

        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::NotFound,
                "model.openai_responses.compaction_not_found",
                format!("Response '{response_id}' not found for compaction"),
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
                "model.openai_responses.compaction_failed",
                format!(
                    "OpenAI Responses API compaction failed with status {status}: {error_body}"
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
                format!("Failed to read compaction response body: {e}"),
            )
            .with_provider("openai-responses")
        })?;

        let api_response: Response = serde_json::from_str(&body).map_err(|e| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.openai_responses.parse",
                format!(
                    "Failed to parse compaction response JSON: {}",
                    adk_core::json_error_summary(&e)
                ),
            )
            .with_provider("openai-responses")
        })?;

        let mut llm_response = responses_convert::from_response(&api_response);
        llm_response.turn_complete = true;
        llm_response.partial = false;

        Ok(llm_response)
    }
}
