pub mod controllers;
mod routes;

pub use controllers::{
    AppsController, ArtifactsController, DebugController, RuntimeController, SessionController,
};

use crate::{web_ui, ServerConfig};
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

pub fn create_app(config: ServerConfig) -> Router {
    let session_controller = SessionController::new(config.session_service.clone());
    let runtime_controller = RuntimeController::new(config.clone());
    let apps_controller = AppsController::new(config.clone());
    let artifacts_controller = ArtifactsController::new(config.clone());
    let debug_controller = DebugController::new(config.clone());

    let api_router = Router::new()
        .route("/health", get(health_check))
        .route("/apps", get(controllers::apps::list_apps))
        .route("/list-apps", get(controllers::apps::list_apps_compat))
        .with_state(apps_controller)
        .route("/sessions", post(controllers::session::create_session))
        .route(
            "/sessions/:app_name/:user_id/:session_id",
            get(controllers::session::get_session).delete(controllers::session::delete_session),
        )
        // adk-go compatible routes
        .route(
            "/apps/:app_name/users/:user_id/sessions",
            get(controllers::session::list_sessions).post(controllers::session::create_session_from_path),
        )
        .route(
            "/apps/:app_name/users/:user_id/sessions/:session_id",
            get(controllers::session::get_session_from_path)
                .post(controllers::session::create_session_from_path)
                .delete(controllers::session::delete_session_from_path),
        )
        .with_state(session_controller)
        .route(
            "/run/:app_name/:user_id/:session_id",
            post(controllers::runtime::run_sse),
        )
        .route("/run_sse", post(controllers::runtime::run_sse_compat))
        .with_state(runtime_controller)
        .route(
            "/sessions/:app_name/:user_id/:session_id/artifacts",
            get(controllers::artifacts::list_artifacts),
        )
        .route(
            "/sessions/:app_name/:user_id/:session_id/artifacts/:artifact_name",
            get(controllers::artifacts::get_artifact),
        )
        .with_state(artifacts_controller)
        .route(
            "/debug/trace/:event_id",
            get(controllers::debug::get_trace),
        )
        .route(
            "/debug/graph/:app_name/:user_id/:session_id/:event_id",
            get(controllers::debug::get_graph),
        )
        .with_state(debug_controller);

    let ui_router = Router::new()
        .route("/", get(web_ui::root_redirect))
        .route("/ui/", get(web_ui::serve_ui_index))
        .route("/ui/assets/config/runtime-config.json", get(web_ui::serve_runtime_config))
        .with_state(config)
        .route("/ui/*path", get(web_ui::serve_ui_assets));

    Router::new()
        .nest("/api", api_router)
        .merge(ui_router)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
}

async fn health_check() -> &'static str {
    "OK"
}
