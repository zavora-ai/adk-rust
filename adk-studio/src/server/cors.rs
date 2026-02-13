use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// Build the standard CORS layer for ADK Studio.
///
/// Restricts origins to localhost (127.0.0.1, [::1], and localhost) to prevent
/// drive-by attacks while allowing local development and usage.
pub fn build_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let origin_str = origin.to_str().unwrap_or("");

            if let Some(rest) =
                origin_str.strip_prefix("http://").or_else(|| origin_str.strip_prefix("https://"))
            {
                const ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];
                ALLOWED_HOSTS
                    .iter()
                    .any(|host| rest == *host || rest.starts_with(&format!("{}:", host)))
            } else {
                false
            }
        }))
        .allow_methods(Any)
        .allow_headers(Any)
}
