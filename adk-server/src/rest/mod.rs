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
use tokio_util::sync::CancellationToken;
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

/// Start hot reload watchers for configured YAML agent directories.
///
/// For each directory in `config.yaml_agent_dirs`, creates an
/// [`AgentConfigLoader`](crate::yaml_agent::AgentConfigLoader) and
/// [`HotReloadWatcher`](crate::yaml_agent::HotReloadWatcher), performs
/// the initial load, and spawns a background watcher task.
///
/// Returns the list of active watchers so route handlers can look up
/// YAML-defined agents.
#[cfg(feature = "yaml-agent")]
async fn start_yaml_agent_watchers(
    dirs: &[std::path::PathBuf],
) -> Vec<Arc<crate::yaml_agent::HotReloadWatcher>> {
    use crate::yaml_agent::{AgentConfigLoader, HotReloadWatcher};

    let mut watchers = Vec::new();

    for dir in dirs {
        // Create a minimal tool registry (no pre-registered tools) and a
        // placeholder model factory. Real deployments should configure these
        // via ServerConfig extensions; for now we use empty defaults so the
        // watcher can start and load YAML definitions.
        let registry: Arc<dyn adk_core::ToolRegistry> = Arc::new(EmptyToolRegistry);
        let factory: Arc<dyn crate::yaml_agent::ModelFactory> = Arc::new(NoOpModelFactory);
        let loader = Arc::new(AgentConfigLoader::new(registry, factory));
        let watcher = Arc::new(HotReloadWatcher::new(loader));

        match watcher.watch(dir).await {
            Ok(handle) => {
                tracing::info!("started YAML agent hot reload watcher for {}", dir.display());
                // Detach the watcher task — it runs until the server shuts down.
                drop(handle);
                watchers.push(watcher);
            }
            Err(e) => {
                tracing::warn!("failed to start YAML agent watcher for {}: {e}", dir.display());
            }
        }
    }

    watchers
}

/// Empty tool registry used as default when no tools are pre-registered.
#[cfg(feature = "yaml-agent")]
struct EmptyToolRegistry;

#[cfg(feature = "yaml-agent")]
impl adk_core::ToolRegistry for EmptyToolRegistry {
    fn resolve(&self, _tool_name: &str) -> Option<Arc<dyn adk_core::Tool>> {
        None
    }

    fn available_tools(&self) -> Vec<String> {
        vec![]
    }
}

/// Placeholder model factory that returns an error for any provider.
///
/// Real deployments should provide a proper `ModelFactory` via
/// `ServerConfig` extensions. This exists so the watcher can start
/// even when no model factory is explicitly configured.
#[cfg(feature = "yaml-agent")]
struct NoOpModelFactory;

#[cfg(feature = "yaml-agent")]
#[async_trait::async_trait]
impl crate::yaml_agent::ModelFactory for NoOpModelFactory {
    async fn create_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> adk_core::Result<Arc<dyn adk_core::Llm>> {
        Err(adk_core::AdkError::config(format!(
            "no model factory configured for YAML agent loading \
             (requested provider='{provider}', model_id='{model_id}'). \
             Configure a ModelFactory on the server to enable YAML agent model creation."
        )))
    }
}

/// Create the server application with A2A support at the specified base URL
pub fn create_app_with_a2a(config: ServerConfig, a2a_base_url: Option<&str>) -> Router {
    let session_controller = SessionController::new(config.session_service.clone());
    let runtime_controller = RuntimeController::new(config.clone());
    let apps_controller = AppsController::new(config.clone());
    let artifacts_controller = ArtifactsController::new(config.clone());
    let debug_controller = DebugController::new(config.clone());
    let health_controller = HealthController::new(&config);

    // Start YAML agent hot reload watchers if configured.
    #[cfg(feature = "yaml-agent")]
    {
        let dirs = config.yaml_agent_dirs.clone();
        if !dirs.is_empty() {
            tokio::spawn(async move {
                let _watchers = start_yaml_agent_watchers(&dirs).await;
                // Watchers are kept alive for the lifetime of this task.
                // They run background filesystem watch loops internally.
                // We hold them here so they aren't dropped.
                std::future::pending::<()>().await;
            });
        }
    }

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

// ---------------------------------------------------------------------------
// ServerBuilder — extensible server construction with custom routes
// ---------------------------------------------------------------------------

/// Builder for constructing an ADK server with custom routes.
///
/// `ServerBuilder` allows registering additional Axum routers alongside the
/// built-in REST, A2A, and UI routes. Custom routes benefit from the same
/// middleware stack (auth, CORS, tracing, timeout, security headers) as the
/// built-in routes.
///
/// # Example
///
/// ```rust,ignore
/// use adk_server::{ServerBuilder, ServerConfig};
/// use axum::{Router, routing::get};
///
/// let config = ServerConfig::new(agent, session_service);
///
/// let app = ServerBuilder::new(config)
///     .add_api_routes(
///         Router::new()
///             .route("/projects", get(list_projects))
///             .route("/projects/{id}", get(get_project))
///     )
///     .add_api_routes(
///         Router::new()
///             .route("/automations", get(list_automations))
///     )
///     .with_a2a("http://localhost:8080")
///     .build();
///
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
/// axum::serve(listener, app).await?;
/// ```
pub struct ServerBuilder {
    config: ServerConfig,
    a2a_base_url: Option<String>,
    api_routes: Vec<Router>,
    root_routes: Vec<Router>,
    shutdown_endpoint: bool,
}

impl ServerBuilder {
    /// Create a new server builder with the given configuration.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            a2a_base_url: None,
            api_routes: Vec::new(),
            root_routes: Vec::new(),
            shutdown_endpoint: false,
        }
    }

    /// Add custom routes nested under `/api`.
    ///
    /// These routes are merged into the API router and benefit from the auth
    /// middleware layer. Multiple calls accumulate routes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// builder.add_api_routes(
    ///     Router::new()
    ///         .route("/projects", get(list_projects))
    ///         .route("/projects/{id}", get(get_project))
    /// )
    /// ```
    pub fn add_api_routes(mut self, routes: Router) -> Self {
        self.api_routes.push(routes);
        self
    }

    /// Add custom routes at the root level (not nested under `/api`).
    ///
    /// These routes are merged at the top level of the application, alongside
    /// the UI and A2A routes. They receive the full middleware stack (CORS,
    /// tracing, timeout, security headers) but NOT the auth middleware.
    ///
    /// Use this for routes that need their own auth handling or public endpoints.
    pub fn add_root_routes(mut self, routes: Router) -> Self {
        self.root_routes.push(routes);
        self
    }

    /// Enable A2A protocol support at the specified base URL.
    ///
    /// The base URL is used to construct the agent card's endpoint URL.
    pub fn with_a2a(mut self, base_url: impl Into<String>) -> Self {
        self.a2a_base_url = Some(base_url.into());
        self
    }

    /// Enable the `POST /api/shutdown` endpoint for graceful shutdown.
    ///
    /// When enabled, the server exposes a shutdown endpoint that triggers
    /// graceful shutdown: stops accepting new connections, completes in-flight
    /// requests, and then exits. Use [`build_with_shutdown`](Self::build_with_shutdown)
    /// to get the [`ShutdownHandle`] for wiring into `axum::serve().with_graceful_shutdown()`.
    ///
    /// The endpoint is protected by the auth middleware when a
    /// `RequestContextExtractor` is configured.
    pub fn enable_shutdown_endpoint(mut self) -> Self {
        self.shutdown_endpoint = true;
        self
    }

    /// Build the final Axum router with all routes and middleware applied.
    pub fn build(self) -> Router {
        self.build_inner().0
    }

    /// Build the final Axum router and return a [`ShutdownHandle`].
    ///
    /// Use this when [`enable_shutdown_endpoint()`](Self::enable_shutdown_endpoint) is set.
    /// Pass the handle's signal to `axum::serve().with_graceful_shutdown()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let (app, shutdown_handle) = ServerBuilder::new(config)
    ///     .enable_shutdown_endpoint()
    ///     .build_with_shutdown();
    ///
    /// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    /// axum::serve(listener, app)
    ///     .with_graceful_shutdown(shutdown_handle.signal())
    ///     .await?;
    /// ```
    pub fn build_with_shutdown(self) -> (Router, ShutdownHandle) {
        let (router, handle) = self.build_inner();
        (router, handle.expect("build_with_shutdown requires enable_shutdown_endpoint()"))
    }

    fn build_inner(self) -> (Router, Option<ShutdownHandle>) {
        let config = &self.config;
        let session_controller = SessionController::new(config.session_service.clone());
        let runtime_controller = RuntimeController::new(config.clone());
        let apps_controller = AppsController::new(config.clone());
        let artifacts_controller = ArtifactsController::new(config.clone());
        let debug_controller = DebugController::new(config.clone());
        let health_controller = HealthController::new(config);

        // Start YAML agent hot reload watchers if configured.
        #[cfg(feature = "yaml-agent")]
        {
            let dirs = config.yaml_agent_dirs.clone();
            if !dirs.is_empty() {
                tokio::spawn(async move {
                    let _watchers = start_yaml_agent_watchers(&dirs).await;
                    std::future::pending::<()>().await;
                });
            }
        }

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

        let debug_router =
            debug_router.with_state(debug_controller.clone()).layer(auth_layer.clone());

        // Assemble the API router with built-in + custom routes
        let mut api_router = Router::new()
            .merge(health_router)
            .merge(ui_api_router)
            .merge(session_router)
            .merge(runtime_router)
            .merge(artifacts_router)
            .merge(debug_router);

        // Merge custom API routes — these get the same /api prefix and auth middleware
        for custom_routes in self.api_routes {
            api_router = api_router.merge(custom_routes.layer(auth_layer.clone()));
        }

        // Add shutdown endpoint if enabled
        let shutdown_handle = if self.shutdown_endpoint {
            let handle = ShutdownHandle::new();
            let shutdown_router = Router::new()
                .route("/shutdown", post(handle_shutdown))
                .with_state(handle.token.clone())
                .layer(auth_layer);
            api_router = api_router.merge(shutdown_router);
            Some(handle)
        } else {
            None
        };

        let ui_router = Router::new()
            .route("/", get(web_ui::root_redirect))
            .route("/ui/", get(web_ui::serve_ui_index))
            .route("/ui/assets/config/runtime-config.json", get(web_ui::serve_runtime_config))
            .with_state(config.clone())
            .route("/ui/{*path}", get(web_ui::serve_ui_assets));

        let mut app = Router::new().nest("/api", api_router).merge(ui_router);

        // Merge custom root routes
        for custom_routes in self.root_routes {
            app = app.merge(custom_routes);
        }

        if let Some(base_url) = &self.a2a_base_url {
            let a2a_controller = A2aController::new(config.clone(), base_url);
            let a2a_router = Router::new()
                .route("/.well-known/agent.json", get(controllers::a2a::get_agent_card))
                .route("/a2a", post(controllers::a2a::handle_jsonrpc))
                .route("/a2a/stream", post(controllers::a2a::handle_jsonrpc_stream))
                .with_state(a2a_controller);
            app = app.merge(a2a_router);
        }

        let cors_layer = build_cors_layer(config);
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

        (
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
            ),
            shutdown_handle,
        )
    }
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

// ---------------------------------------------------------------------------
// ShutdownHandle — programmatic graceful shutdown trigger
// ---------------------------------------------------------------------------

/// Handle for triggering graceful server shutdown.
///
/// Returned by [`ServerBuilder::build_with_shutdown`]. Pass the future from
/// [`ShutdownHandle::signal()`] to `axum::serve(...).with_graceful_shutdown()`
/// to enable both OS signal-based and HTTP endpoint-based shutdown.
///
/// # Example
///
/// ```rust,ignore
/// use adk_server::{ServerBuilder, ServerConfig};
///
/// let (app, shutdown_handle) = ServerBuilder::new(config)
///     .enable_shutdown_endpoint()
///     .build_with_shutdown();
///
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
/// axum::serve(listener, app)
///     .with_graceful_shutdown(shutdown_handle.signal())
///     .await?;
/// ```
#[derive(Clone)]
pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    /// Create a new shutdown handle.
    fn new() -> Self {
        Self { token: CancellationToken::new() }
    }

    /// Trigger graceful shutdown programmatically.
    ///
    /// This has the same effect as calling `POST /api/shutdown` — the server
    /// stops accepting new connections and completes in-flight requests.
    pub fn shutdown(&self) {
        tracing::info!("graceful shutdown triggered programmatically");
        self.token.cancel();
    }

    /// Returns a future that resolves when shutdown is triggered.
    ///
    /// Combines OS signals (Ctrl+C, SIGTERM) with the programmatic/HTTP trigger.
    /// Pass this to `axum::serve(...).with_graceful_shutdown()`.
    pub async fn signal(self) {
        let token = self.token.clone();

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
            _ = ctrl_c => {
                tracing::info!("received Ctrl+C, initiating graceful shutdown");
            }
            _ = terminate => {
                tracing::info!("received SIGTERM, initiating graceful shutdown");
            }
            _ = token.cancelled() => {
                // Shutdown triggered via POST /api/shutdown or programmatic call
            }
        }
    }

    /// Returns whether shutdown has been triggered.
    pub fn is_shutdown(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Handler for `POST /api/shutdown`.
///
/// Triggers graceful shutdown: the server stops accepting new connections,
/// completes in-flight requests, and then exits.
async fn handle_shutdown(State(token): State<CancellationToken>) -> impl IntoResponse {
    tracing::info!("POST /api/shutdown received, initiating graceful shutdown");
    token.cancel();
    (StatusCode::OK, Json(serde_json::json!({ "status": "shutting_down" })))
}
