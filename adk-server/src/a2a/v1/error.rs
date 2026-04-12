//! A2A v1.0.0 error types per spec §5.4.
//!
//! Defines [`A2aError`] with JSON-RPC error codes, HTTP status codes,
//! `google.rpc.ErrorInfo` production, and AIP-193 HTTP error responses.

use serde_json::{Map, Value, json};

/// A2A v1.0.0 protocol errors.
///
/// Each variant maps to a specific JSON-RPC error code and HTTP status code
/// as defined in the A2A Protocol v1.0.0 specification §5.4.
#[derive(Debug, thiserror::Error)]
pub enum A2aError {
    /// Task not found in the task store.
    /// JSON-RPC: -32001, HTTP: 404
    #[error("Task not found: {task_id}")]
    TaskNotFound { task_id: String },

    /// Task cannot be canceled because it is in a terminal state.
    /// JSON-RPC: -32002, HTTP: 409
    #[error("Task not cancelable: {task_id} is in state {current_state}")]
    TaskNotCancelable { task_id: String, current_state: String },

    /// Agent does not support push notifications.
    /// JSON-RPC: -32003, HTTP: 400
    #[error("Push notifications not supported")]
    PushNotificationNotSupported,

    /// A valid method was called but the agent does not support it.
    /// JSON-RPC: -32004, HTTP: 400
    #[error("Unsupported operation: {method}")]
    UnsupportedOperation { method: String },

    /// A media type in the request is not supported.
    /// JSON-RPC: -32005, HTTP: 415
    #[error("Content type not supported: {media_type}")]
    ContentTypeNotSupported { media_type: String },

    /// The agent returned a non-conformant response.
    /// JSON-RPC: -32006, HTTP: 502
    #[error("Invalid agent response: {message}")]
    InvalidAgentResponse { message: String },

    /// Extended agent card is declared but not configured.
    /// JSON-RPC: -32007, HTTP: 400
    #[error("Extended agent card not configured")]
    ExtendedAgentCardNotConfigured,

    /// A required extension is not declared by the client.
    /// JSON-RPC: -32008, HTTP: 400
    #[error("Extension support required: {uri}")]
    ExtensionSupportRequired { uri: String },

    /// Requested protocol version is not supported.
    /// JSON-RPC: -32009, HTTP: 400
    #[error("Version not supported: {requested}")]
    VersionNotSupported { requested: String, supported: Vec<String> },

    /// Request parameters failed validation.
    /// JSON-RPC: -32602, HTTP: 400
    #[error("Invalid parameters: {message}")]
    InvalidParams { message: String },

    /// Unexpected server-side failure.
    /// JSON-RPC: -32603, HTTP: 500
    #[error("Internal error: {message}")]
    Internal { message: String },

    /// Requested JSON-RPC method is not recognized.
    /// JSON-RPC: -32601, HTTP: 404
    #[error("Method not found: {method}")]
    MethodNotFound { method: String },

    /// Attempted an invalid task state transition.
    /// JSON-RPC: -32603, HTTP: 409
    #[error("Invalid state transition from {from} to {to}")]
    InvalidStateTransition { from: String, to: String },

    /// Push notification webhook delivery failed after retries.
    /// JSON-RPC: -32603, HTTP: 500
    #[error("Push notification delivery failed: {message}")]
    PushDeliveryFailed { message: String },
}

impl A2aError {
    /// Map to JSON-RPC error code per A2A v1.0.0 spec §5.4.
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::TaskNotFound { .. } => -32001,
            Self::TaskNotCancelable { .. } => -32002,
            Self::PushNotificationNotSupported => -32003,
            Self::UnsupportedOperation { .. } => -32004,
            Self::ContentTypeNotSupported { .. } => -32005,
            Self::InvalidAgentResponse { .. } => -32006,
            Self::ExtendedAgentCardNotConfigured => -32007,
            Self::ExtensionSupportRequired { .. } => -32008,
            Self::VersionNotSupported { .. } => -32009,
            Self::InvalidParams { .. } => -32602,
            Self::Internal { .. }
            | Self::InvalidStateTransition { .. }
            | Self::PushDeliveryFailed { .. } => -32603,
            Self::MethodNotFound { .. } => -32601,
        }
    }

    /// Map to HTTP status code per A2A v1.0.0 spec §5.4.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::TaskNotFound { .. } | Self::MethodNotFound { .. } => 404,
            Self::TaskNotCancelable { .. } | Self::InvalidStateTransition { .. } => 409,
            Self::ContentTypeNotSupported { .. } => 415,
            Self::InvalidAgentResponse { .. } => 502,
            Self::Internal { .. } | Self::PushDeliveryFailed { .. } => 500,
            Self::PushNotificationNotSupported
            | Self::UnsupportedOperation { .. }
            | Self::ExtendedAgentCardNotConfigured
            | Self::ExtensionSupportRequired { .. }
            | Self::VersionNotSupported { .. }
            | Self::InvalidParams { .. } => 400,
        }
    }

    /// Produce `google.rpc.ErrorInfo` data per spec §9.5.
    ///
    /// Returns a JSON array containing a single `google.rpc.ErrorInfo` object
    /// with `@type`, `reason` (UPPER_SNAKE_CASE, no "Error" suffix), `domain`,
    /// and variant-specific `metadata`.
    pub fn to_error_info(&self) -> Value {
        json!([{
            "@type": "type.googleapis.com/google.rpc.ErrorInfo",
            "reason": self.reason_code(),
            "domain": "a2a-protocol.org",
            "metadata": self.error_metadata()
        }])
    }

    /// Produce a JSON-RPC error object: `{ "code": ..., "message": ..., "data": <error_info> }`.
    pub fn to_jsonrpc_error(&self) -> Value {
        json!({
            "code": self.json_rpc_code(),
            "message": self.to_string(),
            "data": self.to_error_info()
        })
    }

    /// Produce an AIP-193 HTTP error response.
    ///
    /// ```json
    /// {
    ///   "error": {
    ///     "code": 404,
    ///     "status": "NOT_FOUND",
    ///     "message": "Task not found: task_abc123",
    ///     "details": [<error_info>]
    ///   }
    /// }
    /// ```
    pub fn to_http_error_response(&self) -> Value {
        json!({
            "error": {
                "code": self.http_status(),
                "status": self.http_status_string(),
                "message": self.to_string(),
                "details": self.to_error_info()
            }
        })
    }

    /// UPPER_SNAKE_CASE reason code without "Error" suffix.
    fn reason_code(&self) -> &'static str {
        match self {
            Self::TaskNotFound { .. } => "TASK_NOT_FOUND",
            Self::TaskNotCancelable { .. } => "TASK_NOT_CANCELABLE",
            Self::PushNotificationNotSupported => "PUSH_NOTIFICATION_NOT_SUPPORTED",
            Self::UnsupportedOperation { .. } => "UNSUPPORTED_OPERATION",
            Self::ContentTypeNotSupported { .. } => "CONTENT_TYPE_NOT_SUPPORTED",
            Self::InvalidAgentResponse { .. } => "INVALID_AGENT_RESPONSE",
            Self::ExtendedAgentCardNotConfigured => "EXTENDED_AGENT_CARD_NOT_CONFIGURED",
            Self::ExtensionSupportRequired { .. } => "EXTENSION_SUPPORT_REQUIRED",
            Self::VersionNotSupported { .. } => "VERSION_NOT_SUPPORTED",
            Self::InvalidParams { .. } => "INVALID_PARAMS",
            Self::Internal { .. } => "INTERNAL",
            Self::MethodNotFound { .. } => "METHOD_NOT_FOUND",
            Self::InvalidStateTransition { .. } => "INVALID_STATE_TRANSITION",
            Self::PushDeliveryFailed { .. } => "PUSH_DELIVERY_FAILED",
        }
    }

    /// Variant-specific metadata for the `google.rpc.ErrorInfo` object.
    fn error_metadata(&self) -> Map<String, Value> {
        let mut map = Map::new();
        match self {
            Self::TaskNotFound { task_id } => {
                map.insert("task_id".to_string(), Value::String(task_id.clone()));
            }
            Self::TaskNotCancelable { task_id, current_state } => {
                map.insert("task_id".to_string(), Value::String(task_id.clone()));
                map.insert("current_state".to_string(), Value::String(current_state.clone()));
            }
            Self::UnsupportedOperation { method } => {
                map.insert("method".to_string(), Value::String(method.clone()));
            }
            Self::ContentTypeNotSupported { media_type } => {
                map.insert("media_type".to_string(), Value::String(media_type.clone()));
            }
            Self::InvalidAgentResponse { message } => {
                map.insert("message".to_string(), Value::String(message.clone()));
            }
            Self::ExtensionSupportRequired { uri } => {
                map.insert("uri".to_string(), Value::String(uri.clone()));
            }
            Self::VersionNotSupported { requested, supported } => {
                map.insert("requested".to_string(), Value::String(requested.clone()));
                map.insert("supported".to_string(), Value::String(supported.join(", ")));
            }
            Self::InvalidParams { message } => {
                map.insert("message".to_string(), Value::String(message.clone()));
            }
            Self::Internal { message } => {
                map.insert("message".to_string(), Value::String(message.clone()));
            }
            Self::MethodNotFound { method } => {
                map.insert("method".to_string(), Value::String(method.clone()));
            }
            Self::InvalidStateTransition { from, to } => {
                map.insert("from".to_string(), Value::String(from.clone()));
                map.insert("to".to_string(), Value::String(to.clone()));
            }
            Self::PushDeliveryFailed { message } => {
                map.insert("message".to_string(), Value::String(message.clone()));
            }
            Self::PushNotificationNotSupported | Self::ExtendedAgentCardNotConfigured => {
                // No variant-specific metadata
            }
        }
        map
    }

    /// Map HTTP status code to a canonical status string for AIP-193 responses.
    fn http_status_string(&self) -> &'static str {
        match self.http_status() {
            400 => "BAD_REQUEST",
            404 => "NOT_FOUND",
            409 => "CONFLICT",
            415 => "UNSUPPORTED_MEDIA_TYPE",
            500 => "INTERNAL_SERVER_ERROR",
            502 => "BAD_GATEWAY",
            _ => "UNKNOWN",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_not_found_json_rpc_code() {
        let err = A2aError::TaskNotFound { task_id: "task_123".to_string() };
        assert_eq!(err.json_rpc_code(), -32001);
        assert_eq!(err.http_status(), 404);
    }

    #[test]
    fn test_task_not_found_error_info() {
        let err = A2aError::TaskNotFound { task_id: "task_abc".to_string() };
        let info = err.to_error_info();
        let arr = info.as_array().expect("should be array");
        assert_eq!(arr.len(), 1);
        let obj = &arr[0];
        assert_eq!(obj["@type"], "type.googleapis.com/google.rpc.ErrorInfo");
        assert_eq!(obj["reason"], "TASK_NOT_FOUND");
        assert_eq!(obj["domain"], "a2a-protocol.org");
        assert_eq!(obj["metadata"]["task_id"], "task_abc");
    }

    #[test]
    fn test_version_not_supported_metadata() {
        let err = A2aError::VersionNotSupported {
            requested: "2.0".to_string(),
            supported: vec!["0.3".to_string(), "1.0".to_string()],
        };
        assert_eq!(err.json_rpc_code(), -32009);
        assert_eq!(err.http_status(), 400);
        let info = err.to_error_info();
        let meta = &info[0]["metadata"];
        assert_eq!(meta["requested"], "2.0");
        assert_eq!(meta["supported"], "0.3, 1.0");
    }

    #[test]
    fn test_jsonrpc_error_structure() {
        let err = A2aError::MethodNotFound { method: "UnknownMethod".to_string() };
        let rpc_err = err.to_jsonrpc_error();
        assert_eq!(rpc_err["code"], -32601);
        assert!(rpc_err["message"].as_str().unwrap().contains("UnknownMethod"));
        assert!(rpc_err["data"].is_array());
    }

    #[test]
    fn test_http_error_response_structure() {
        let err = A2aError::TaskNotFound { task_id: "task_abc123".to_string() };
        let resp = err.to_http_error_response();
        let error_obj = &resp["error"];
        assert_eq!(error_obj["code"], 404);
        assert_eq!(error_obj["status"], "NOT_FOUND");
        assert!(error_obj["message"].as_str().unwrap().contains("task_abc123"));
        assert!(error_obj["details"].is_array());
        assert_eq!(error_obj["details"][0]["reason"], "TASK_NOT_FOUND");
    }

    #[test]
    fn test_invalid_state_transition() {
        let err = A2aError::InvalidStateTransition {
            from: "COMPLETED".to_string(),
            to: "WORKING".to_string(),
        };
        assert_eq!(err.json_rpc_code(), -32603);
        assert_eq!(err.http_status(), 409);
        let info = err.to_error_info();
        assert_eq!(info[0]["metadata"]["from"], "COMPLETED");
        assert_eq!(info[0]["metadata"]["to"], "WORKING");
    }

    #[test]
    fn test_push_notification_not_supported() {
        let err = A2aError::PushNotificationNotSupported;
        assert_eq!(err.json_rpc_code(), -32003);
        assert_eq!(err.http_status(), 400);
        let info = err.to_error_info();
        assert_eq!(info[0]["reason"], "PUSH_NOTIFICATION_NOT_SUPPORTED");
        assert!(info[0]["metadata"].as_object().unwrap().is_empty());
    }

    #[test]
    fn test_content_type_not_supported() {
        let err = A2aError::ContentTypeNotSupported { media_type: "text/xml".to_string() };
        assert_eq!(err.json_rpc_code(), -32005);
        assert_eq!(err.http_status(), 415);
        assert_eq!(err.to_error_info()[0]["metadata"]["media_type"], "text/xml");
    }

    #[test]
    fn test_invalid_agent_response() {
        let err = A2aError::InvalidAgentResponse { message: "missing required field".to_string() };
        assert_eq!(err.json_rpc_code(), -32006);
        assert_eq!(err.http_status(), 502);
    }

    #[test]
    fn test_extended_agent_card_not_configured() {
        let err = A2aError::ExtendedAgentCardNotConfigured;
        assert_eq!(err.json_rpc_code(), -32007);
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.to_error_info()[0]["reason"], "EXTENDED_AGENT_CARD_NOT_CONFIGURED");
    }

    #[test]
    fn test_all_variants_have_valid_codes() {
        let variants: Vec<A2aError> = vec![
            A2aError::TaskNotFound { task_id: "t".into() },
            A2aError::TaskNotCancelable { task_id: "t".into(), current_state: "s".into() },
            A2aError::PushNotificationNotSupported,
            A2aError::UnsupportedOperation { method: "m".into() },
            A2aError::ContentTypeNotSupported { media_type: "x".into() },
            A2aError::InvalidAgentResponse { message: "m".into() },
            A2aError::ExtendedAgentCardNotConfigured,
            A2aError::ExtensionSupportRequired { uri: "u".into() },
            A2aError::VersionNotSupported { requested: "v".into(), supported: vec![] },
            A2aError::InvalidParams { message: "m".into() },
            A2aError::Internal { message: "m".into() },
            A2aError::MethodNotFound { method: "m".into() },
            A2aError::InvalidStateTransition { from: "a".into(), to: "b".into() },
            A2aError::PushDeliveryFailed { message: "m".into() },
        ];

        for err in &variants {
            // JSON-RPC codes are negative
            assert!(err.json_rpc_code() < 0, "code should be negative for {err}");
            // HTTP status codes are in valid range
            let status = err.http_status();
            assert!((400..=599).contains(&status), "status {status} out of range for {err}");
            // error_info is a non-empty array
            let info = err.to_error_info();
            assert!(info.is_array());
            assert_eq!(info.as_array().unwrap().len(), 1);
            // reason is non-empty UPPER_SNAKE_CASE
            let reason = info[0]["reason"].as_str().unwrap();
            assert!(!reason.is_empty());
            assert!(
                reason.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "reason {reason} is not UPPER_SNAKE_CASE"
            );
            // domain is correct
            assert_eq!(info[0]["domain"], "a2a-protocol.org");
            // jsonrpc error has required fields
            let rpc = err.to_jsonrpc_error();
            assert!(rpc["code"].is_i64());
            assert!(rpc["message"].is_string());
            assert!(rpc["data"].is_array());
            // http error response has required fields
            let http = err.to_http_error_response();
            assert!(http["error"]["code"].is_u64());
            assert!(http["error"]["status"].is_string());
            assert!(http["error"]["message"].is_string());
            assert!(http["error"]["details"].is_array());
        }
    }
}
