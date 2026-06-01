//! Background mode operations for long-running OpenAI Responses API requests.
//!
//! Provides polling and cancellation support for background responses submitted
//! with `background: true`. Deep research models (`o3-deep-research`,
//! `o4-mini-deep-research`) automatically enable background mode since they
//! require asynchronous execution.

use super::responses_client::{OpenAIResponsesClient, map_openai_error};
use super::responses_convert;
use adk_core::{AdkError, LlmResponse};
use async_openai::types::responses::Status;

impl OpenAIResponsesClient {
    /// Poll a background response by ID.
    ///
    /// Calls `GET /v1/responses/{response_id}` and returns the current status
    /// or completed result as an `LlmResponse`.
    ///
    /// # Status Handling
    ///
    /// - **Completed**: Returns a full `LlmResponse` with content, usage, and metadata.
    /// - **In Progress / Queued**: Returns an `LlmResponse` with `provider_metadata`
    ///   containing `response_id` and `status`, but no content.
    /// - **Failed**: Returns an `LlmResponse` with `error_code` and `error_message`
    ///   populated from the response error object.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` if the HTTP request fails (network error, auth error, etc.).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let response = client.poll_response("resp_abc123").await?;
    /// if let Some(metadata) = &response.provider_metadata {
    ///     let status = metadata["openai"]["status"].as_str();
    ///     println!("Response status: {:?}", status);
    /// }
    /// ```
    pub async fn poll_response(&self, response_id: &str) -> Result<LlmResponse, AdkError> {
        let response = self
            .openai_client()
            .responses()
            .retrieve(response_id)
            .await
            .map_err(map_openai_error)?;

        match &response.status {
            Status::Completed | Status::Incomplete => {
                // Use existing conversion logic for completed/incomplete responses
                let mut llm_response = responses_convert::from_response(&response);
                // Ensure status is in provider_metadata
                inject_status_metadata(&mut llm_response, response_id, &response.status);
                Ok(llm_response)
            }
            Status::InProgress | Status::Queued => {
                // Return a minimal response with status metadata
                let status_str = status_to_str(&response.status);
                let provider_metadata = serde_json::json!({
                    "openai": {
                        "response_id": response_id,
                        "status": status_str
                    }
                });
                Ok(LlmResponse {
                    provider_metadata: Some(provider_metadata),
                    partial: false,
                    turn_complete: false,
                    ..Default::default()
                })
            }
            Status::Failed => {
                // Extract error details from the response
                let (error_code, error_message) = if let Some(err) = &response.error {
                    (Some(err.code.clone()), Some(err.message.clone()))
                } else {
                    (Some("unknown".to_string()), Some("Background response failed".to_string()))
                };

                let provider_metadata = serde_json::json!({
                    "openai": {
                        "response_id": response_id,
                        "status": "failed"
                    }
                });

                Ok(LlmResponse {
                    error_code,
                    error_message,
                    provider_metadata: Some(provider_metadata),
                    partial: false,
                    turn_complete: true,
                    ..Default::default()
                })
            }
            Status::Cancelled => {
                let provider_metadata = serde_json::json!({
                    "openai": {
                        "response_id": response_id,
                        "status": "cancelled"
                    }
                });

                Ok(LlmResponse {
                    error_code: Some("cancelled".to_string()),
                    error_message: Some("Background response was cancelled".to_string()),
                    provider_metadata: Some(provider_metadata),
                    partial: false,
                    turn_complete: true,
                    ..Default::default()
                })
            }
        }
    }

    /// Cancel a background response by ID.
    ///
    /// Calls `POST /v1/responses/{response_id}/cancel` to cancel a response
    /// that was created with `background: true`. Only background responses
    /// can be cancelled.
    ///
    /// # Returns
    ///
    /// Returns an `LlmResponse` with `status: "cancelled"` in `provider_metadata`
    /// on success.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` if:
    /// - The HTTP request fails (network error, auth error)
    /// - The response is not a background response
    /// - The response has already completed
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let response = client.cancel_response("resp_abc123").await?;
    /// println!("Response cancelled: {:?}", response.provider_metadata);
    /// ```
    pub async fn cancel_response(&self, response_id: &str) -> Result<LlmResponse, AdkError> {
        let url = format!("{}/responses/{}/cancel", self.base_url(), response_id);

        let response = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key()))
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| AdkError::model(format!("cancel request failed: {e}")))?;

        let status_code = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AdkError::model(format!("failed to read cancel response body: {e}")))?;

        if !status_code.is_success() {
            return Err(AdkError::model(format!(
                "cancel response failed with status {status_code}: {body}"
            )));
        }

        // Parse the response to extract status confirmation
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| AdkError::model(format!("failed to parse cancel response: {e}")))?;

        let response_status = json.get("status").and_then(|v| v.as_str()).unwrap_or("cancelled");

        let provider_metadata = serde_json::json!({
            "openai": {
                "response_id": response_id,
                "status": response_status
            }
        });

        Ok(LlmResponse {
            error_code: Some("cancelled".to_string()),
            error_message: Some("Background response was cancelled".to_string()),
            provider_metadata: Some(provider_metadata),
            partial: false,
            turn_complete: true,
            ..Default::default()
        })
    }
}

/// Inject `response_id` and `status` into existing provider_metadata.
fn inject_status_metadata(response: &mut LlmResponse, response_id: &str, status: &Status) {
    let status_str = status_to_str(status);

    if let Some(ref mut metadata) = response.provider_metadata {
        if let Some(openai) = metadata.get_mut("openai") {
            if let Some(obj) = openai.as_object_mut() {
                obj.insert("status".to_string(), serde_json::Value::String(status_str.to_string()));
                // response_id should already be present from from_response, but ensure it
                obj.entry("response_id".to_string())
                    .or_insert_with(|| serde_json::Value::String(response_id.to_string()));
            }
        }
    } else {
        response.provider_metadata = Some(serde_json::json!({
            "openai": {
                "response_id": response_id,
                "status": status_str
            }
        }));
    }
}

/// Convert a `Status` enum to its string representation.
fn status_to_str(status: &Status) -> &'static str {
    match status {
        Status::Completed => "completed",
        Status::Failed => "failed",
        Status::InProgress => "in_progress",
        Status::Cancelled => "cancelled",
        Status::Queued => "queued",
        Status::Incomplete => "incomplete",
    }
}
