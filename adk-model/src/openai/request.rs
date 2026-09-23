//! Request customization at the HTTP boundary.

/// Applies provider-specific JSON fields and headers before a generation request.
///
/// The adapter must preserve the requested response mode and must not perform I/O.
/// It runs for each HTTP attempt under the model's configured retry policy.
///
/// # Example
///
/// ```ignore
/// let adapter: adk_model::openai::RequestAdapter = std::sync::Arc::new(|body, headers| {
///     body["metadata"] = serde_json::json!({"application": "example"});
///     headers.insert("x-request-source", "example".parse().unwrap());
///     Ok(())
/// });
/// let client = client.with_request_adapter(adapter);
/// ```
pub type RequestAdapter = std::sync::Arc<
    dyn Fn(
            &mut serde_json::Value,
            &mut reqwest::header::HeaderMap,
        ) -> Result<(), adk_core::AdkError>
        + Send
        + Sync,
>;
