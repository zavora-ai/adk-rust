use serde::{Deserialize, Serialize};

use crate::types::Message;

/// A message batch object returned by the Batches API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageBatch {
    /// Unique batch identifier.
    pub id: String,
    /// Object type (always "message_batch").
    #[serde(rename = "type")]
    pub batch_type: String,
    /// Processing status (e.g. "in_progress", "ended", "canceling").
    pub processing_status: String,
    /// Counts of requests in each state.
    pub request_counts: BatchRequestCounts,
}

/// Counts of batch requests in each processing state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRequestCounts {
    /// Number of requests still processing.
    pub processing: u32,
    /// Number of requests that succeeded.
    pub succeeded: u32,
    /// Number of requests that errored.
    pub errored: u32,
    /// Number of requests that were canceled.
    pub canceled: u32,
    /// Number of requests that expired.
    pub expired: u32,
}

/// A single result item from a completed batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchResultItem {
    /// Custom ID provided in the original request.
    pub custom_id: String,
    /// The result of this batch request.
    pub result: BatchResult,
}

/// An error object returned in batch results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchError {
    /// Error type string.
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error message.
    pub message: String,
}

/// The result of a single batch request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BatchResult {
    /// The request succeeded.
    Succeeded {
        /// The completed message.
        message: Message,
    },
    /// The request errored.
    Errored {
        /// The error details.
        error: BatchError,
    },
    /// The request was canceled.
    Canceled,
    /// The request expired.
    Expired,
}

/// A batch request to be submitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRequest {
    /// Custom ID for correlating results.
    pub custom_id: String,
    /// The message creation parameters.
    pub params: crate::types::MessageCreateParams,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn batch_request_counts_roundtrip() {
        let counts = BatchRequestCounts {
            processing: 5,
            succeeded: 10,
            errored: 2,
            canceled: 1,
            expired: 0,
        };
        let json = serde_json::to_value(&counts).unwrap();
        let deserialized: BatchRequestCounts = serde_json::from_value(json).unwrap();
        assert_eq!(counts, deserialized);
    }

    #[test]
    fn batch_result_succeeded_serialization() {
        // Just test the tag serialization
        let json = json!({"type": "canceled"});
        let result: BatchResult = serde_json::from_value(json).unwrap();
        assert!(matches!(result, BatchResult::Canceled));
    }

    #[test]
    fn batch_result_errored_serialization() {
        let json = json!({
            "type": "errored",
            "error": {"type": "invalid_request", "message": "bad input"}
        });
        let result: BatchResult = serde_json::from_value(json).unwrap();
        match result {
            BatchResult::Errored { error } => {
                assert_eq!(error.error_type, "invalid_request");
                assert_eq!(error.message, "bad input");
            }
            _ => panic!("Expected Errored variant"),
        }
    }
}
