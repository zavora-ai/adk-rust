pub mod controllers;
mod routes;

pub use controllers::{
    A2aController, AppsController, ArtifactsController, DebugController, RuntimeController,
    SessionController,
};

use crate::{web_ui, ServerConfig};
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

/// Create the server application with optional A2A support
pub fn create_app(config: ServerConfig) -> Router {
    create_app_with_a2a(config, None)
}

/// Create the server application with A2A support at the specified base URL
pub fn create_app_with_a2a(config: ServerConfig, a2a_base_url: Option<&str>) -> Router {
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
            get(controllers::session::list_sessions)
                .post(controllers::session::create_session_from_path),
        )
        .route(
            "/apps/:app_name/users/:user_id/sessions/:session_id",
            get(controllers::session::get_session_from_path)
                .post(controllers::session::create_session_from_path)
                .delete(controllers::session::delete_session_from_path),
        )
        .with_state(session_controller)
        .route("/run/:app_name/:user_id/:session_id", post(controllers::runtime::run_sse))
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
        .route("/debug/trace/:event_id", get(controllers::debug::get_trace))
        .route(
            "/debug/graph/:app_name/:user_id/:session_id/:event_id",
            get(controllers::debug::get_graph),
        )
        .with_state(debug_controller);

    let ui_router = Router::new()
        .route("/", get(web_ui::root_redirect))
        .route("/ui/", get(web_ui::serve_ui_index))
        .route("/ui/assets/config/runtime-config.json", get(web_ui::serve_runtime_config))
        .with_state(config.clone())
        .route("/ui/*path", get(web_ui::serve_ui_assets));

    let mut app = Router::new().nest("/api", api_router).merge(ui_router);

    // Add A2A routes if base URL is provided
    if let Some(base_url) = a2a_base_url {
        let a2a_controller = A2aController::new(config, base_url);
        let a2a_router = Router::new()
            .route("/.well-known/agent.json", get(controllers::a2a::get_agent_card))
            .route("/a2a", post(controllers::a2a::handle_jsonrpc))
            .route("/a2a/stream", post(controllers::a2a::handle_jsonrpc_stream))
            .with_state(a2a_controller);
        app = app.merge(a2a_router);
    }

    app.layer(TraceLayer::new_for_http()).layer(CorsLayer::permissive())
}

async fn health_check() -> &'static str {
    "OK"
}
