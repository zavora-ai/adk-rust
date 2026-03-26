//! HTTP action node executor (requires `action-http` feature).
//!
//! Implements HTTP requests with method, URL interpolation, headers,
//! authentication (bearer/basic/api_key), body (json/form/raw),
//! response parsing (json/text), and status code validation.

use std::collections::HashMap;

use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, interpolate_variables};
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute an HTTP action node.
pub async fn execute_http(config: &HttpNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;
    let state = &ctx.state;

    // Interpolate URL
    let url = interpolate_variables(&config.url, state);
    tracing::debug!(node = %node_id, url = %url, method = ?config.method, "executing HTTP node");

    // Build request
    let client = reqwest::Client::new();
    let mut request = match config.method {
        HttpMethod::Get => client.get(&url),
        HttpMethod::Post => client.post(&url),
        HttpMethod::Put => client.put(&url),
        HttpMethod::Patch => client.patch(&url),
        HttpMethod::Delete => client.delete(&url),
        HttpMethod::Head => client.head(&url),
        HttpMethod::Options => client.request(reqwest::Method::OPTIONS, &url),
    };

    // Apply headers with interpolation
    for (key, value) in &config.headers {
        let interpolated_value = interpolate_variables(value, state);
        request = request.header(key.as_str(), interpolated_value);
    }

    // Apply authentication
    request = apply_auth(request, &config.auth, state);

    // Apply body
    request = apply_body(request, &config.body, state)?;

    // Send request
    let response = request.send().await.map_err(|e| GraphError::NodeExecutionFailed {
        node: node_id.clone(),
        message: format!("HTTP request failed: {e}"),
    })?;

    let status = response.status().as_u16();

    // Validate status code
    if let Some(pattern) = &config.response.status_validation {
        if !validate_status(status, pattern) {
            return Err(GraphError::NodeExecutionFailed {
                node: node_id.clone(),
                message: format!(
                    "HTTP status {status} does not match validation pattern '{pattern}'"
                ),
            });
        }
    }

    // Parse response
    let result = parse_response(response, &config.response.response_type, node_id).await?;

    let output_value = json!({
        "status": status,
        "data": result,
    });

    Ok(NodeOutput::new().with_update(output_key, output_value))
}

/// Apply authentication to the request builder.
fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &HttpAuth,
    state: &HashMap<String, Value>,
) -> reqwest::RequestBuilder {
    match auth {
        HttpAuth::None => request,
        HttpAuth::Bearer(bearer) => {
            let token = interpolate_variables(&bearer.token, state);
            request.bearer_auth(token)
        }
        HttpAuth::Basic(basic) => {
            let username = interpolate_variables(&basic.username, state);
            let password = interpolate_variables(&basic.password, state);
            request.basic_auth(username, Some(password))
        }
        HttpAuth::ApiKey(api_key) => {
            let header = interpolate_variables(&api_key.header, state);
            let value = interpolate_variables(&api_key.value, state);
            request.header(header, value)
        }
    }
}

/// Apply body to the request builder.
fn apply_body(
    request: reqwest::RequestBuilder,
    body: &HttpBody,
    state: &HashMap<String, Value>,
) -> Result<reqwest::RequestBuilder> {
    match body {
        HttpBody::None => Ok(request),
        HttpBody::Json { data } => {
            // Interpolate string values within the JSON data
            let interpolated = interpolate_json_values(data, state);
            Ok(request.json(&interpolated))
        }
        HttpBody::Form { fields } => {
            let interpolated: HashMap<String, String> =
                fields.iter().map(|(k, v)| (k.clone(), interpolate_variables(v, state))).collect();
            Ok(request.form(&interpolated))
        }
        HttpBody::Raw { content, content_type } => {
            let interpolated_content = interpolate_variables(content, state);
            let interpolated_ct = interpolate_variables(content_type, state);
            Ok(request.header("Content-Type", interpolated_ct).body(interpolated_content))
        }
    }
}

/// Recursively interpolate string values within a JSON value.
fn interpolate_json_values(value: &Value, state: &HashMap<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            let interpolated = interpolate_variables(s, state);
            Value::String(interpolated)
        }
        Value::Object(map) => {
            let new_map: serde_json::Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), interpolate_json_values(v, state))).collect();
            Value::Object(new_map)
        }
        Value::Array(arr) => {
            let new_arr: Vec<Value> =
                arr.iter().map(|v| interpolate_json_values(v, state)).collect();
            Value::Array(new_arr)
        }
        other => other.clone(),
    }
}

/// Parse the HTTP response based on the configured response type.
async fn parse_response(
    response: reqwest::Response,
    response_type: &str,
    node_id: &str,
) -> Result<Value> {
    match response_type {
        "json" => {
            let json_value: Value =
                response.json().await.map_err(|e| GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: format!("failed to parse JSON response: {e}"),
                })?;
            Ok(json_value)
        }
        _ => {
            // Default to text
            let text = response.text().await.map_err(|e| GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: format!("failed to read response text: {e}"),
            })?;
            Ok(Value::String(text))
        }
    }
}

/// Validate an HTTP status code against a pattern string.
///
/// Supported patterns:
/// - Single code: `"200"`
/// - Comma-separated: `"200,201,204"`
/// - Range: `"200-299"`
/// - Mixed: `"200-299,404"`
fn validate_status(status: u16, pattern: &str) -> bool {
    for part in pattern.split(',') {
        let part = part.trim();
        if let Some((start_str, end_str)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) =
                (start_str.trim().parse::<u16>(), end_str.trim().parse::<u16>())
            {
                if status >= start && status <= end {
                    return true;
                }
            }
        } else if let Ok(code) = part.parse::<u16>() {
            if status == code {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_status_single() {
        assert!(validate_status(200, "200"));
        assert!(!validate_status(201, "200"));
    }

    #[test]
    fn test_validate_status_range() {
        assert!(validate_status(200, "200-299"));
        assert!(validate_status(250, "200-299"));
        assert!(validate_status(299, "200-299"));
        assert!(!validate_status(300, "200-299"));
        assert!(!validate_status(199, "200-299"));
    }

    #[test]
    fn test_validate_status_comma_separated() {
        assert!(validate_status(200, "200,201,204"));
        assert!(validate_status(201, "200,201,204"));
        assert!(validate_status(204, "200,201,204"));
        assert!(!validate_status(202, "200,201,204"));
    }

    #[test]
    fn test_validate_status_mixed() {
        assert!(validate_status(200, "200-299,404"));
        assert!(validate_status(404, "200-299,404"));
        assert!(!validate_status(500, "200-299,404"));
    }
}
