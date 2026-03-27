pub mod controllers;
mod routes;

pub use controllers::{
    A2aController, AppsController, ArtifactsController, DebugController, RuntimeController,
    SessionController,
};

use crate::{
    ServerConfig,
    auth_bridge::{RequestContext, RequestContextError, RequestContextExtractor},
    web_ui,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use std::sync::Arc;
use tower::ServiceBuilder;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    set_header::SetResponseHeaderLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Clone)]
struct HealthController {
    session_service: Arc<dyn adk_session::SessionService>,
    artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    memory_service: Option<Arc<dyn adk_core::Memory>>,
}

impl HealthController {
    fn new(config: &ServerConfig) -> Self {
        Self {
            session_service: config.session_service.clone(),
            artifact_service: config.artifact_service.clone(),
            memory_service: config.memory_service.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct RequestId(String);

impl RequestId {
    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    components: HealthComponents,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthComponents {
    session: ComponentHealth,
    memory: ComponentHealth,
    artifact: ComponentHealth,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComponentHealth {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ComponentHealth {
    fn healthy() -> Self {
        Self { status: "healthy", error: None }
    }

    fn unhealthy(error: impl Into<String>) -> Self {
        Self { status: "unhealthy", error: Some(error.into()) }
    }

    fn not_configured() -> Self {
        Self { status: "not_configured", error: None }
    }
}

/// Build CORS layer based on security configuration
fn build_cors_layer(config: &ServerConfig) -> CorsLayer {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static(REQUEST_ID_HEADER),
            HeaderName::from_static("x-adk-ui-protocol"),
            HeaderName::from_static("x-adk-ui-transport"),
        ]);

    if config.security.allowed_origins.is_empty() {
        cors.allow_origin(AllowOrigin::any())
    } else {
        let origins: Vec<HeaderValue> = config
            .security
            .allowed_origins
            .iter()
            .filter_map(|origin| origin.parse().ok())
            .collect();
        cors.allow_origin(origins)
    }
}

fn validate_request_id(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(REQUEST_ID_HEADER)?;
    let raw = value.to_str().ok()?;
    if raw.len() > 128 {
        return None;
    }
    uuid::Uuid::parse_str(raw).ok()?;
    Some(raw.to_string())
}

async fn request_id_middleware(mut request: Request<Body>, next: Next) -> Response {
    let request_id =
        validate_request_id(request.headers()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    request.extensions_mut().insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
    response
}

async fn auth_middleware(
    request: Request<Body>,
    next: Next,
    extractor: Option<Arc<dyn RequestContextExtractor>>,
) -> Response {
    let (mut parts, body) = request.into_parts();

    let request_context = match extractor {
        Some(extractor) => match extractor.extract(&parts).await {
            Ok(context) => Some(context),
            Err(RequestContextError::MissingAuth) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": "missing authorization" })),
                )
                    .into_response();
            }
            Err(RequestContextError::InvalidToken(message)) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({ "error": format!("invalid token: {message}") })),
                )
                    .into_response();
            }
            Err(RequestContextError::ExtractionFailed(message)) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("auth extraction failed: {message}")
                    })),
                )
                    .into_response();
            }
        },
        None => None,
    };

    parts.extensions.insert::<Option<RequestContext>>(request_context);
    next.run(Request::from_parts(parts, body)).await
}

async fn health_check(State(controller): State<HealthController>) -> impl IntoResponse {
    let session = match controller.session_service.health_check().await {
        Ok(()) => ComponentHealth::healthy(),
        Err(error) => ComponentHealth::unhealthy(error.to_string()),
    };

    let memory = match controller.memory_service.as_ref() {
        Some(service) => match service.health_check().await {
            Ok(()) => ComponentHealth::healthy(),
            Err(error) => ComponentHealth::unhealthy(error.to_string()),
        },
        None => ComponentHealth::not_configured(),
    };

    let artifact = match controller.artifact_service.as_ref() {
        Some(service) => match service.health_check().await {
            Ok(()) => ComponentHealth::healthy(),
            Err(error) => ComponentHealth::unhealthy(error.to_string()),
        },
        None => ComponentHealth::not_configured(),
    };

    let healthy = session.status == "healthy"
        && memory.status != "unhealthy"
        && artifact.status != "unhealthy";

    (
        if healthy { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE },
        Json(HealthResponse {
            status: if healthy { "healthy" } else { "unhealthy" },
            components: HealthComponents { session, memory, artifact },
        }),
    )
}

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
    let health_controller = HealthController::new(&config);

    let auth_layer = middleware::from_fn({
        let extractor = config.request_context_extractor.clone();
        move |request: Request<Body>, next: Next| {
            let extractor = extractor.clone();
            async move { auth_middleware(request, next, extractor).await }
        }
    });

    let health_router =
        Router::new().route("/health", get(health_check)).with_state(health_controller);

    let ui_api_router = Router::new()
        .route("/apps", get(controllers::apps::list_apps))
        .route("/list-apps", get(controllers::apps::list_apps_compat))
        .with_state(apps_controller)
        .route("/ui/capabilities", get(controllers::ui::ui_capabilities))
        .route("/ui/initialize", post(controllers::ui::ui_initialize))
        .route("/ui/message", post(controllers::ui::ui_message))
        .route("/ui/update-model-context", post(controllers::ui::ui_update_model_context))
        .route("/ui/notifications/poll", post(controllers::ui::ui_poll_notifications))
        .route(
            "/ui/notifications/resources-list-changed",
            post(controllers::ui::ui_notify_resources_list_changed),
        )
        .route(
            "/ui/notifications/tools-list-changed",
            post(controllers::ui::ui_notify_tools_list_changed),
        )
        .route("/ui/resources", get(controllers::ui::list_ui_resources))
        .route("/ui/resources/read", get(controllers::ui::read_ui_resource))
        .route("/ui/resources/register", post(controllers::ui::register_ui_resource));

    let session_router = Router::new()
        .route("/sessions", post(controllers::session::create_session))
        .route(
            "/sessions/{app_name}/{user_id}/{session_id}",
            get(controllers::session::get_session).delete(controllers::session::delete_session),
        )
        .route(
            "/apps/{app_name}/users/{user_id}/sessions",
            get(controllers::session::list_sessions)
                .post(controllers::session::create_session_from_path),
        )
        .route(
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
            get(controllers::session::get_session_from_path)
                .post(controllers::session::create_session_from_path)
                .delete(controllers::session::delete_session_from_path),
        )
        .with_state(session_controller)
        .layer(auth_layer.clone());

    let runtime_router = Router::new()
        .route("/run/{app_name}/{user_id}/{session_id}", post(controllers::runtime::run_sse))
        .route("/run_sse", post(controllers::runtime::run_sse_compat))
        .with_state(runtime_controller);

    let artifacts_router = Router::new()
        .route(
            "/sessions/{app_name}/{user_id}/{session_id}/artifacts",
            get(controllers::artifacts::list_artifacts),
        )
        .route(
            "/sessions/{app_name}/{user_id}/{session_id}/artifacts/{artifact_name}",
            get(controllers::artifacts::get_artifact),
        )
        .with_state(artifacts_controller)
        .layer(auth_layer.clone());

    let mut debug_router = Router::new()
        .route("/debug/trace/session/{session_id}", get(controllers::debug::get_session_traces))
        .route(
            "/debug/graph/{app_name}/{user_id}/{session_id}/{event_id}",
            get(controllers::debug::get_graph),
        )
        .route(
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph",
            get(controllers::debug::get_graph),
        )
        .route("/apps/{app_name}/eval_sets", get(controllers::debug::get_eval_sets))
        .route(
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}",
            get(controllers::debug::get_event),
        );

    if config.request_context_extractor.is_none() || config.security.expose_admin_debug {
        debug_router = debug_router
            .route("/debug/trace/{event_id}", get(controllers::debug::get_trace_by_event_id));
    }

    let debug_router = debug_router.with_state(debug_controller.clone()).layer(auth_layer.clone());

    let api_router = Router::new()
        .merge(health_router)
        .merge(ui_api_router)
        .merge(session_router)
        .merge(runtime_router)
        .merge(artifacts_router)
        .merge(debug_router);

    let ui_router = Router::new()
        .route("/", get(web_ui::root_redirect))
        .route("/ui/", get(web_ui::serve_ui_index))
        .route("/ui/assets/config/runtime-config.json", get(web_ui::serve_runtime_config))
        .with_state(config.clone())
        .route("/ui/{*path}", get(web_ui::serve_ui_assets));

    let mut app = Router::new().nest("/api", api_router).merge(ui_router);

    if let Some(base_url) = a2a_base_url {
        let a2a_controller = A2aController::new(config.clone(), base_url);
        let a2a_router = Router::new()
            .route("/.well-known/agent.json", get(controllers::a2a::get_agent_card))
            .route("/a2a", post(controllers::a2a::handle_jsonrpc))
            .route("/a2a/stream", post(controllers::a2a::handle_jsonrpc_stream))
            .with_state(a2a_controller);
        app = app.merge(a2a_router);
    }

    let cors_layer = build_cors_layer(&config);
    let trace_layer = TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
        let request_id =
            request.extensions().get::<RequestId>().map(RequestId::as_str).unwrap_or("");
        tracing::info_span!(
            "http.request",
            request.id = %request_id,
            http.method = %request.method(),
            http.path = %request.uri().path()
        )
    });

    app.layer(
        ServiceBuilder::new()
            .layer(middleware::from_fn(request_id_middleware))
            .layer(trace_layer)
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                config.security.request_timeout,
            ))
            .layer(DefaultBodyLimit::max(config.security.max_body_size))
            .layer(cors_layer)
            .layer(SetResponseHeaderLayer::if_not_present(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::X_FRAME_OPTIONS,
                HeaderValue::from_static("DENY"),
            ))
            .layer(SetResponseHeaderLayer::if_not_present(
                header::X_XSS_PROTECTION,
                HeaderValue::from_static("1; mode=block"),
            )),
    )
}

/// Wait for a process shutdown signal.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
