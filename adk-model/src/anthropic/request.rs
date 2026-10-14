//! Request customization at the Anthropic SDK dispatch boundary.

use adk_anthropic::{Anthropic, HeaderMap, MessageCreateParams};
use adk_core::{AdkError, ErrorCategory, ErrorComponent};

/// Adapts a serialized Anthropic Messages request and its outgoing HTTP headers.
///
/// The closure runs once for each HTTP attempt, retries included, after the model
/// builds the request and the SDK's default and feature-derived beta headers are
/// prepared. It may replace or remove any header, including authentication headers.
///
/// - **Prompt caching**: the body arrives with `cache_control` breakpoints already
///   placed. Edits to `system`, `tools`, or a cached message block change the cached
///   prefix and can miss the prompt cache.
/// - **Body shape**: the body must stay valid [`MessageCreateParams`] with the same
///   `stream` value. A top-level field `MessageCreateParams` does not model fails the
///   request rather than being dropped.
/// - **No I/O**: the closure is synchronous. Refresh tokens out of band and read the
///   current value here, for example from an `Arc<std::sync::RwLock<String>>`.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::HeaderValue;
/// use adk_model::anthropic::{AnthropicClient, AnthropicConfig, RequestAdapter};
/// use std::sync::{Arc, RwLock};
///
/// // A background task refreshes the token; the adapter only reads it.
/// let token = Arc::new(RwLock::new(String::from("initial-token")));
/// let current = token.clone();
/// let adapter: RequestAdapter = Arc::new(move |body, headers| {
///     body["metadata"] = serde_json::json!({"user_id": "session-1"});
///     let bearer = format!("Bearer {}", current.read().expect("token lock").as_str());
///     headers.remove("x-api-key");
///     headers.insert(
///         "authorization",
///         HeaderValue::from_str(&bearer).expect("token is a valid header value"),
///     );
///     Ok(())
/// });
/// let client = AnthropicClient::new(AnthropicConfig::new("api-key", "claude-sonnet-4-6"))?
///     .with_request_adapter(adapter);
/// # let _ = client;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub type RequestAdapter = std::sync::Arc<
    dyn Fn(&mut serde_json::Value, &mut HeaderMap) -> Result<(), AdkError> + Send + Sync,
>;

pub(super) fn prepare(
    client: &Anthropic,
    params: MessageCreateParams,
    adapter: &RequestAdapter,
) -> Result<(MessageCreateParams, HeaderMap), AdkError> {
    let streaming = params.stream;
    let mut headers = client.message_headers_for_request(&params, streaming).map_err(|error| {
        invalid_request(&format!("Could not build Anthropic request headers: {error}"))
    })?;
    let mut body = serde_json::to_value(params).map_err(|error| {
        invalid_request(&format!("Could not serialize Anthropic message parameters: {error}"))
    })?;
    adapter(&mut body, &mut headers)?;
    let params: MessageCreateParams = serde_json::from_value(body.clone()).map_err(|error| {
        invalid_request(&format!(
            "Request adapter produced invalid Anthropic message parameters: {error}"
        ))
    })?;
    let kept = serde_json::to_value(&params).map_err(|error| {
        invalid_request(&format!("Could not serialize Anthropic message parameters: {error}"))
    })?;
    // Every optional field is skipped when `None`, so a key set to `null` is absent by
    // design; any other missing key is a field the typed parameters cannot carry.
    let dropped: Vec<&str> = match (body.as_object(), kept.as_object()) {
        (Some(body), Some(kept)) => body
            .iter()
            .filter(|(key, value)| !value.is_null() && !kept.contains_key(key.as_str()))
            .map(|(key, _)| key.as_str())
            .collect(),
        _ => Vec::new(),
    };
    if !dropped.is_empty() {
        return Err(invalid_request(&format!(
            "Request adapter added fields that MessageCreateParams does not model and \
             would be dropped: {}. Remove them or set a field MessageCreateParams supports",
            dropped.join(", ")
        )));
    }
    if params.stream != streaming {
        return Err(invalid_request("Request adapter changed Anthropic response mode"));
    }
    params.validate().map_err(|error| {
        invalid_request(&format!(
            "Request adapter produced invalid Anthropic message parameters: {error}"
        ))
    })?;
    Ok((params, headers))
}

fn invalid_request(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.anthropic.request_adapter",
        message,
    )
    .with_provider("anthropic")
}

#[cfg(test)]
mod tests;
