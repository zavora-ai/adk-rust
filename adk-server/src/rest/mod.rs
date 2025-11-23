pub mod controllers;
mod routes;

pub use controllers::{RuntimeController, SessionController};

use crate::ServerConfig;
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;

pub fn create_app(config: ServerConfig) -> Router {
    let session_controller = SessionController::new(config.session_service.clone());
    let runtime_controller = RuntimeController::new(config.clone());

    Router::new()
        .route("/health", get(health_check))
        .route("/sessions", post(controllers::session::create_session))
        .route(
            "/sessions/:app_name/:user_id/:session_id",
            get(controllers::session::get_session)
                .delete(controllers::session::delete_session),
        )
        .with_state(session_controller)
        .route(
            "/run/:app_name/:user_id/:session_id",
            post(controllers::runtime::run_sse),
        )
        .with_state(runtime_controller)
        .layer(CorsLayer::permissive())
}

async fn health_check() -> &'static str {
    "OK"
}
