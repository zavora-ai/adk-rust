use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 Request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    pub id: Option<Value>,
}

/// JSON-RPC 2.0 Response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), result: Some(result), error: None, id }
    }

    pub fn error(id: Option<Value>, error: JsonRpcError) -> Self {
        Self { jsonrpc: "2.0".to_string(), result: None, error: Some(error), id }
    }
}

/// JSON-RPC 2.0 Error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self { code: -32700, message: message.into(), data: None }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self { code: -32600, message: message.into(), data: None }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self { code: -32601, message: format!("Method not found: {}", method), data: None }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self { code: -32602, message: message.into(), data: None }
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self { code: -32603, message: message.into(), data: None }
    }

    /// Create an internal error with sanitized message for production.
    /// Logs the detailed error but returns a generic message to the client.
    pub fn internal_error_sanitized(error: &dyn std::fmt::Display, expose_details: bool) -> Self {
        if expose_details {
            Self::internal_error(error.to_string())
        } else {
            // Log the actual error for debugging
            tracing::error!(error = %error, "Internal server error");
            Self::internal_error("Internal server error")
        }
    }
}

/// A2A Protocol Methods
pub mod methods {
    pub const MESSAGE_SEND: &str = "message/send";
    pub const MESSAGE_SEND_STREAM: &str = "message/stream";
    pub const TASKS_GET: &str = "tasks/get";
    pub const TASKS_CANCEL: &str = "tasks/cancel";
}

/// Parameters for message/send method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSendParams {
    pub message: super::Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<MessageSendConfig>,
}

/// Configuration for message send
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSendConfig {
    #[serde(skip_serializing_if = "Option::is_none", rename = "acceptedOutputModes")]
    pub accepted_output_modes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "historyLength")]
    pub history_length: Option<u32>,
}

/// Parameters for tasks/get method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksGetParams {
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "historyLength")]
    pub history_length: Option<u32>,
}

/// Parameters for tasks/cancel method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksCancelParams {
    #[serde(rename = "taskId")]
    pub task_id: String,
}

/// Task representation returned by A2A
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "contextId")]
    pub context_id: Option<String>,
    pub status: super::TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<super::Artifact>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<super::Message>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jsonrpc_request_parse() {
        let json = r#"{"jsonrpc":"2.0","method":"message/send","params":{},"id":1}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "message/send");
        assert_eq!(req.id, Some(Value::Number(1.into())));
    }

    #[test]
    fn test_jsonrpc_response_success() {
        let resp =
            JsonRpcResponse::success(Some(Value::Number(1.into())), Value::String("ok".into()));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_jsonrpc_response_error() {
        let resp = JsonRpcResponse::error(
            Some(Value::Number(1.into())),
            JsonRpcError::method_not_found("unknown"),
        );
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
