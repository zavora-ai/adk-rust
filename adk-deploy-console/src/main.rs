use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tower_http::trace::TraceLayer;
use tracing::info;

const INDEX_HTML: &str = include_str!("../public/index.html");
const APP_JS: &str = include_str!("../public/app.js");
const STYLES_CSS: &str = include_str!("../public/styles.css");

#[derive(Clone)]
struct AppState {
    api_base_url: Arc<str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleConfig {
    api_base_url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "adk_deploy_console=info,tower_http=info".to_string()),
        )
        .init();

    let port = std::env::var("ADK_DEPLOY_CONSOLE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8091);
    let api_base_url = std::env::var("ADK_DEPLOY_API_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:8090/api/v1".to_string());
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let app = create_app(AppState { api_base_url: Arc::from(api_base_url) });

    info!(address = %addr, "starting deploy console");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/styles.css", get(styles_css))
        .route("/config.json", get(config))
        .route("/favicon.ico", get(favicon))
        .route("/health", get(health))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn index(axum::extract::State(state): axum::extract::State<AppState>) -> impl IntoResponse {
    with_security_headers(Html(INDEX_HTML).into_response(), &state.api_base_url, "no-store")
}

async fn app_js(axum::extract::State(state): axum::extract::State<AppState>) -> impl IntoResponse {
    static_asset_response(
        APP_JS,
        "application/javascript; charset=utf-8",
        &state.api_base_url,
        "public, max-age=300",
    )
}

async fn styles_css(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    static_asset_response(
        STYLES_CSS,
        "text/css; charset=utf-8",
        &state.api_base_url,
        "public, max-age=300",
    )
}

async fn config(axum::extract::State(state): axum::extract::State<AppState>) -> impl IntoResponse {
    with_security_headers(
        Json(ConsoleConfig { api_base_url: state.api_base_url.to_string() }).into_response(),
        &state.api_base_url,
        "no-store",
    )
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" })))
}

async fn favicon() -> StatusCode {
    StatusCode::NO_CONTENT
}

fn static_asset_response(
    body: &'static str,
    content_type: &'static str,
    api_base_url: &str,
    cache_control: &'static str,
) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    with_security_headers(response, api_base_url, cache_control)
}

fn with_security_headers(
    mut response: Response,
    api_base_url: &str,
    cache_control: &'static str,
) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_control));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    if let Ok(policy) = HeaderValue::from_str(&content_security_policy(api_base_url)) {
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    response
}

fn content_security_policy(api_base_url: &str) -> String {
    let connect_src = api_origin(api_base_url)
        .map_or_else(|| "'self'".to_string(), |origin| format!("'self' {origin}"));
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src {connect_src}; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"
    )
}

fn api_origin(api_base_url: &str) -> Option<String> {
    let parsed = url::Url::parse(api_base_url).ok()?;
    let scheme = parsed.scheme();
    let host = parsed.host_str()?;
    let port = parsed.port().map(|port| format!(":{port}")).unwrap_or_default();
    Some(format!("{scheme}://{host}{port}"))
}

#[cfg(test)]
mod tests {
    use super::{api_origin, content_security_policy};

    #[test]
    fn api_origin_returns_scheme_host_and_port() {
        assert_eq!(
            api_origin("http://127.0.0.1:8090/api/v1"),
            Some("http://127.0.0.1:8090".to_string())
        );
    }

    #[test]
    fn content_security_policy_includes_api_origin_in_connect_src() {
        let policy = content_security_policy("http://127.0.0.1:8090/api/v1");
        assert!(policy.contains("connect-src 'self' http://127.0.0.1:8090"));
    }
}
