use axum::{
    body::Body,
    http::{header, Response, StatusCode},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "ui/dist/"]
pub struct Assets;

/// Serve embedded static files
pub fn serve_embedded(path: String) -> Response<Body> {
    let path = if path.is_empty() || path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };

    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => {
            // For SPA routing, serve index.html for non-file paths
            if !path.contains('.') {
                if let Some(content) = Assets::get("index.html") {
                    return Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/html")
                        .body(Body::from(content.data.into_owned()))
                        .unwrap();
                }
            }
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Not Found"))
                .unwrap()
        }
    }
}

/// Serve the index.html for root path
pub fn serve_index() -> Response<Body> {
    serve_embedded(String::new())
}
