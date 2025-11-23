mod routes;

use axum::{
    routing::get,
    Router,
};
use tower_http::cors::CorsLayer;

pub fn create_app() -> Router {
    Router::new()
        .route("/health", get(health_check))
        .layer(CorsLayer::permissive())
}

async fn health_check() -> &'static str {
    "OK"
}
