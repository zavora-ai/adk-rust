use axum::{
    Json,
    body::Body,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use serde::Serialize;

#[derive(RustEmbed)]
#[folder = "assets/webui"]
struct Assets;

fn embedded_asset_response(path: &str, content: rust_embed::EmbeddedFile) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = Body::from(content.data).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        if path == "index.html" {
            header::HeaderValue::from_static("no-cache")
        } else {
            header::HeaderValue::from_static("public, max-age=31536000, immutable")
        },
    );
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    if path == "index.html" {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(
                "default-src 'self'; connect-src 'self'; img-src 'self' data: blob:; style-src 'self'; script-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'",
            ),
        );
    }
    response
}

#[derive(Serialize)]
pub struct RuntimeConfig {
    #[serde(rename = "backendUrl")]
    pub backend_url: String,
}

pub async fn serve_runtime_config(State(config): State<crate::ServerConfig>) -> impl IntoResponse {
    // Use configured backend URL or default to relative "/api"
    // Relative URLs work better as they adapt to the actual host/port
    let backend_url = config.backend_url.unwrap_or_else(|| "/api".to_string());

    Json(RuntimeConfig { backend_url })
}

pub async fn serve_ui_assets(uri: Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches("/ui/").to_string();

    if path.is_empty() {
        path = "index.html".to_string();
    }

    match Assets::get(&path) {
        Some(content) => embedded_asset_response(&path, content),
        None => {
            // If file not found, serve index.html for SPA routing (if we were doing that),
            // but for static assets, 404 is correct.
            // However, Angular apps often use HTML5 pushState, so we might need to fallback to index.html
            // for non-asset paths.
            // Let's check if it looks like a file extension.
            if path.contains('.') {
                StatusCode::NOT_FOUND.into_response()
            } else {
                // Fallback to index.html
                match Assets::get("index.html") {
                    Some(content) => embedded_asset_response("index.html", content),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }
    }
}

pub async fn root_redirect() -> impl IntoResponse {
    axum::response::Redirect::to("/ui/")
}

pub async fn serve_ui_index() -> impl IntoResponse {
    match Assets::get("index.html") {
        Some(content) => embedded_asset_response("index.html", content),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
