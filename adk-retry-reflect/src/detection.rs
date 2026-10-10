//! Error detection from tool result JSON values.

use serde_json::Value;

/// Determine whether a tool result represents an error.
///
/// A result is considered an error if:
/// 1. It is a JSON object with an `"error"` key at the top level, OR
/// 2. It is a JSON object with `"isError": true`, OR
/// 3. It is a JSON string starting with `"Error:"` or `"error:"`
///
/// # Example
///
/// ```rust
/// use adk_retry_reflect::detection::is_error_result;
/// use serde_json::json;
///
/// assert!(is_error_result(&json!({"error": "not found"})));
/// assert!(is_error_result(&json!({"isError": true})));
/// assert!(is_error_result(&json!("Error: connection refused")));
/// assert!(!is_error_result(&json!({"result": "ok"})));
/// assert!(!is_error_result(&json!(42)));
/// ```
pub fn is_error_result(result: &Value) -> bool {
    match result {
        Value::Object(map) => {
            map.contains_key("error")
                || map.get("isError").and_then(|v| v.as_bool()).unwrap_or(false)
        }
        Value::String(s) => s.starts_with("Error:") || s.starts_with("error:"),
        _ => false,
    }
}

/// Phrases that mark an error as an authorization or approval refusal.
const AUTHORIZATION_DENIAL_MARKERS: &[&str] = &[
    "access denied",
    "permission denied",
    "not authorized",
    "unauthorized",
    "forbidden",
    "missing required scope",
    "denied by",
    "execution denied",
    "requires confirmation",
    "requires approval",
];

/// Determine whether an error message reports an authorization or approval refusal.
///
/// A refusal does not change when the call is repeated, so the plugin never
/// suggests retrying one.
///
/// # Example
///
/// ```rust
/// use adk_retry_reflect::detection::is_authorization_denial;
///
/// assert!(is_authorization_denial("tool.internal: missing required scopes: payments:checkout:complete"));
/// assert!(is_authorization_denial("Tool 'pay' execution denied by confirmation policy"));
/// assert!(is_authorization_denial("auth.forbidden: role 'viewer' cannot call 'pay'"));
/// assert!(!is_authorization_denial("connection reset by peer"));
/// ```
pub fn is_authorization_denial(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    AUTHORIZATION_DENIAL_MARKERS.iter().any(|marker| message.contains(marker))
}

/// Determine whether an error message reports that a call timed out.
///
/// # Example
///
/// ```rust
/// use adk_retry_reflect::detection::is_timeout;
///
/// assert!(is_timeout("Tool 'pay' timed out after 300 seconds"));
/// assert!(is_timeout("tool.timeout: upstream timeout"));
/// assert!(!is_timeout("card declined"));
/// ```
pub fn is_timeout(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}
