pub mod controllers;
mod routes;

pub use controllers::{
    A2aController, AppsController, ArtifactsController, DebugController, FreezeDelegate,
    FreezeReport, GovernanceController, GovernanceStatus, RunningRuns, RuntimeController,
    SessionController, a2a::A2aTaskRetention,
};

use crate::{
    SecurityConfig, ServerConfig,
    auth_bridge::{RequestContext, RequestContextError, RequestContextExtractor},
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

/// Builds the CORS layer: only [`SecurityConfig::allowed_origins`] get an
/// `Access-Control-Allow-Origin`.
fn build_cors_layer(security: &Arc<SecurityConfig>) -> CorsLayer {
    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static(REQUEST_ID_HEADER),
            HeaderName::from_static("x-adk-ui-protocol"),
            HeaderName::from_static("x-adk-ui-transport"),
        ]);

    if security.allowed_origins.iter().any(|origin| origin.trim() == "*") {
        cors.allow_origin(AllowOrigin::any())
    } else {
        let security = Arc::clone(security);
        cors.allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin.to_str().is_ok_and(|origin| security.allows_origin(origin))
        }))
    }
}

/// The authority a request names: the URI authority (HTTP/2) or the `Host` header.
fn request_authority(request: &Request<Body>) -> Option<String> {
    request.uri().authority().map(|authority| authority.as_str().to_string()).or_else(|| {
        request
            .headers()
            .get(header::HOST)
            .map(|host| String::from_utf8_lossy(host.as_bytes()).into_owned())
    })
}

fn forbidden(error: &str, hint: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": error, "hint": hint })))
        .into_response()
}

/// Refuses a request for a host the server does not answer.
///
/// A page on a DNS-rebinding domain reaches a loopback server as a same-origin request,
/// so CORS never applies; the `Host` header still names the attacker's domain.
/// Whether `path` names a route that runs an agent, which takes
/// [`SecurityConfig::run_timeout`] instead of the request timeout.
fn runs_an_agent(path: &str) -> bool {
    matches!(path, "/api/run" | "/api/run_sse" | "/a2a" | "/a2a/stream")
        || path.starts_with("/api/run/")
}

/// Answers `408` when the response has not started within the route's timeout: the run
/// timeout for a route that runs an agent, the request timeout for any other.
async fn request_timeout(
    State(security): State<Arc<SecurityConfig>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let limit = if runs_an_agent(request.uri().path()) {
        security.run_timeout
    } else {
        security.request_timeout
    };
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => StatusCode::REQUEST_TIMEOUT.into_response(),
    }
}

async fn host_guard(
    State(security): State<Arc<SecurityConfig>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(authority) = request_authority(&request)
        && !security.allows_host(&authority)
    {
        tracing::warn!(http.host = %authority, "refused a request for a host not in allowed_hosts");
        return forbidden(
            "host not allowed",
            "add the host to SecurityConfig::allowed_hosts to serve it",
        );
    }
    next.run(request).await
}

/// Refuses a request whose `Origin` names another origin not in
/// [`SecurityConfig::allowed_origins`].
///
/// CORS stops a browser from reading the response, but a simple cross-origin request still
/// executes; this stops it before it reaches a handler.
async fn origin_guard(
    State(security): State<Arc<SecurityConfig>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let origin = String::from_utf8_lossy(origin.as_bytes());
        let same_origin = origin.split_once("://").is_some_and(|(_, authority)| {
            request_authority(&request)
                .is_some_and(|requested| requested.eq_ignore_ascii_case(authority))
        });
        if !same_origin && !security.allows_origin(&origin) {
            tracing::warn!(http.origin = %origin, "refused a cross-origin request");
            return forbidden(
                "cross-origin request refused",
                "add the origin to SecurityConfig::allowed_origins to allow it",
            );
        }
    }
    next.run(request).await
}

/// `security` with the hosts of the configured A2A base URL and backend URL added to
/// its allowed hosts: the server advertises them, so peers and the web UI address it there.
fn effective_security(security: &SecurityConfig, urls: [Option<&str>; 2]) -> Arc<SecurityConfig> {
    let mut security = security.clone();
    security.allowed_hosts.extend(
        urls.into_iter()
            .flatten()
            .filter_map(|url| adk_core::Url::parse(url).ok())
            .filter_map(|url| url.host_str().map(str::to_string)),
    );
    Arc::new(security)
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

/// Public A2A discovery routes.
///
/// The card is served at `/.well-known/agent-card.json`, the well-known path since
/// A2A 0.3.0 and the one the bundled v1 client fetches, and at the earlier
/// `/.well-known/agent.json` that existing peers use.
fn a2a_discovery_router(controller: A2aController) -> Router {
    Router::new()
        .route("/.well-known/agent-card.json", get(controllers::a2a::get_agent_card))
        .route("/.well-known/agent.json", get(controllers::a2a::get_agent_card))
        .with_state(controller)
}

/// Create the server application with A2A support at the specified base URL.
///
/// Builds the same router as [`ServerBuilder`] with only [`ServerBuilder::with_a2a`] set.
pub fn create_app_with_a2a(config: ServerConfig, a2a_base_url: Option<&str>) -> Router {
    let builder = ServerBuilder::new(config);
    match a2a_base_url {
        Some(base_url) => builder.with_a2a(base_url),
        None => builder,
    }
    .build()
}

// ---------------------------------------------------------------------------
// ServerBuilder — extensible server construction with custom routes
// ---------------------------------------------------------------------------

/// Builder for constructing an ADK server with custom routes.
///
/// `ServerBuilder` allows registering additional Axum routers alongside the
/// built-in REST and A2A routes. Custom routes benefit from the same
/// middleware stack (host and origin checks, auth, CORS, tracing, timeout,
/// security headers) as the built-in routes.
///
/// The host and origin checks follow [`SecurityConfig`]: by
/// default only loopback names and IP addresses are answered, and no other origin may
/// call the server from a browser.
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
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
/// axum::serve(listener, app).await?;
/// ```
pub struct ServerBuilder {
    config: ServerConfig,
    a2a_base_url: Option<String>,
    api_routes: Vec<Router>,
    root_routes: Vec<Router>,
    shutdown_endpoint: bool,
    governance_endpoints: bool,
    freeze_delegate: Option<Arc<dyn FreezeDelegate>>,
    skill_index: Option<Arc<adk_skill::SkillIndex>>,
    a2a_task_retention: Option<A2aTaskRetention>,
    #[cfg(feature = "agent-engine")]
    agent_engine: bool,
    #[cfg(feature = "background")]
    background_state: Option<crate::background::BackgroundState>,
    #[cfg(feature = "background")]
    cron_state: Option<crate::background::CronState>,
    #[cfg(feature = "webhook-ingress")]
    webhook_ingress: Option<crate::webhooks::WebhookIngress>,
    #[cfg(feature = "approvals")]
    approval_service: Option<Arc<dyn adk_core::ApprovalService>>,
    #[cfg(feature = "awp")]
    awp_edge: bool,
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
            governance_endpoints: false,
            freeze_delegate: None,
            skill_index: None,
            a2a_task_retention: None,
            #[cfg(feature = "agent-engine")]
            agent_engine: false,
            #[cfg(feature = "background")]
            background_state: None,
            #[cfg(feature = "background")]
            cron_state: None,
            #[cfg(feature = "webhook-ingress")]
            webhook_ingress: None,
            #[cfg(feature = "approvals")]
            approval_service: None,
            #[cfg(feature = "awp")]
            awp_edge: false,
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
    /// These routes, such as the web home of `adk-surface`'s `web` feature, are merged at
    /// the top level of the application, alongside the A2A routes. They receive the full
    /// middleware stack (CORS, tracing, timeout, security headers, host and origin checks)
    /// but NOT the auth middleware.
    ///
    /// Use this for routes that need their own auth handling or public endpoints.
    pub fn add_root_routes(mut self, routes: Router) -> Self {
        self.root_routes.push(routes);
        self
    }

    /// Mounts the AWP 2.0 agent edge (spec 3.5 §7.9) through
    /// [`add_root_routes`](Self::add_root_routes), so the host guard, CORS, timeouts and security
    /// headers apply, and attaches the server's kill switch to it unless it carries its own.
    ///
    /// The edge serves `/.well-known/agent-card.json`; with an edge mounted,
    /// [`with_a2a`](Self::with_a2a) keeps its JSON-RPC routes but not its card route.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let app = ServerBuilder::new(config).awp(edge).build();
    /// ```
    #[cfg(feature = "awp")]
    pub fn awp(mut self, edge: crate::awp::AwpEdge) -> Self {
        edge.attach_governance(self.config.governance.clone());
        self.awp_edge = true;
        self.add_root_routes(edge.router())
    }

    /// Enable A2A protocol support at the specified base URL.
    ///
    /// The base URL is used to construct the agent card's endpoint URL.
    pub fn with_a2a(mut self, base_url: impl Into<String>) -> Self {
        self.a2a_base_url = Some(base_url.into());
        self
    }

    /// Expose the skills in `skill_index` on the A2A agent card.
    ///
    /// When A2A is enabled via [`with_a2a`](Self::with_a2a), the served agent
    /// card appends one `skills[]` entry per indexed
    /// skill, mapped by [`agent_skills_from_index`](crate::a2a::agent_skills_from_index).
    /// Has no effect without `with_a2a`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::{ServerBuilder, ServerConfig};
    /// use std::sync::Arc;
    ///
    /// let index = adk_skill::load_skill_index(".")?;
    /// let app = ServerBuilder::new(config)
    ///     .with_a2a("http://localhost:8080")
    ///     .with_skill_index(Arc::new(index))
    ///     .build();
    /// ```
    pub fn with_skill_index(mut self, skill_index: Arc<adk_skill::SkillIndex>) -> Self {
        self.skill_index = Some(skill_index);
        self
    }

    /// Bound how many finished A2A task records the server keeps, and for how long.
    ///
    /// Defaults to [`A2aTaskRetention::default`]: the newest 1,000 records, each for
    /// at most one hour. Has no effect without [`with_a2a`](Self::with_a2a).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::{A2aTaskRetention, ServerBuilder};
    /// use std::time::Duration;
    ///
    /// let app = ServerBuilder::new(config)
    ///     .with_a2a("http://localhost:8080")
    ///     .with_a2a_task_retention(A2aTaskRetention::default().with_ttl(Duration::from_secs(300)))
    ///     .build();
    /// ```
    pub fn with_a2a_task_retention(mut self, retention: A2aTaskRetention) -> Self {
        self.a2a_task_retention = Some(retention);
        self
    }

    /// Mount the background run endpoints under `/api`, behind the auth middleware.
    ///
    /// Serves `POST /api/runs`, `GET /api/runs`, `GET /api/runs/{run_id}`, and
    /// `DELETE /api/runs/{run_id}` from `state`, and starts its embedded worker when the
    /// router is built inside a Tokio runtime. The state obeys
    /// [`ServerConfig::governance`] unless it carries its own control. When a
    /// `RequestContextExtractor` is configured, every request must authenticate, and an
    /// authenticated run belongs to the caller's user id: the caller lists, reads, cancels,
    /// and wakes only their own runs, another owner's run answers `404`, and the caller's
    /// idempotency and concurrency keys are scoped to the caller. Mounting
    /// [`background_runs_router_with_state`](crate::background::background_runs_router_with_state)
    /// yourself skips that layer.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::background::BackgroundState;
    ///
    /// let app = ServerBuilder::new(config)
    ///     .with_background_runs(BackgroundState::new().with_executor(executor))
    ///     .build();
    /// ```
    #[cfg(feature = "background")]
    pub fn with_background_runs(mut self, state: crate::background::BackgroundState) -> Self {
        self.background_state = Some(state);
        self
    }

    /// Mount the cron job endpoints under `/api`, behind the auth middleware.
    ///
    /// Serves `POST /api/cron`, `GET /api/cron`, and `GET`/`PATCH`/`DELETE
    /// /api/cron/{job_id}` from `state`. The scheduler is not started here — call
    /// [`start_cron_scheduler`](crate::background::start_cron_scheduler) with a
    /// clone of the same state on every replica; each fire time is enqueued once.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::background::{BackgroundState, CronState, start_cron_scheduler};
    ///
    /// let cron_state = CronState::new(BackgroundState::new());
    /// start_cron_scheduler(cron_state.clone());
    /// let app = ServerBuilder::new(config).with_cron_jobs(cron_state).build();
    /// ```
    #[cfg(feature = "background")]
    pub fn with_cron_jobs(mut self, state: crate::background::CronState) -> Self {
        self.cron_state = Some(state);
        self
    }

    /// Mount `POST /hooks/{name}`: each signed delivery enqueues a durable run.
    ///
    /// The route is served at the root, outside `/api` and without the auth middleware:
    /// the Standard Webhooks signature is the authentication. When `ingress` has no kill
    /// switch of its own, [`ServerConfig::governance`] is installed, so a freeze refuses
    /// deliveries with `503`. Runs execute when a worker claims them from the ingress's
    /// run store.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::webhooks::{WebhookHook, WebhookIngress};
    ///
    /// let ingress = WebhookIngress::new(run_store.clone())
    ///     .with_hook(WebhookHook::new("billing", verifier, "billing", "invoice_agent"));
    /// let app = ServerBuilder::new(config).with_webhook_ingress(ingress).build();
    /// ```
    #[cfg(feature = "webhook-ingress")]
    pub fn with_webhook_ingress(mut self, ingress: crate::webhooks::WebhookIngress) -> Self {
        self.webhook_ingress = Some(ingress);
        self
    }

    /// Mount the Agent Engine dispatch endpoints alongside the built-in routes.
    ///
    /// When enabled, `POST /api/reasoning_engine` and
    /// `POST /api/stream_reasoning_engine` serve the Gemini Enterprise Agent
    /// Platform runtime contract for the loader's root agent, using the
    /// configured session and artifact services. The routes carry the shared
    /// middleware stack but **not** the auth middleware or the host check — a
    /// deployed engine is fronted by the platform, which authenticates callers
    /// before they reach the container and chooses the `Host` it forwards. The memory class methods report `Unsupported`; use
    /// [`serve_agent_engine`](crate::agent_engine::serve_agent_engine) with
    /// [`AgentEngineOptions`](crate::agent_engine::AgentEngineOptions) to
    /// configure a memory service.
    ///
    /// # Panics
    ///
    /// [`build`](Self::build) panics when the root agent's name is not a
    /// valid app name, mirroring the builder's other misuse panics.
    #[cfg(feature = "agent-engine")]
    pub fn with_agent_engine(mut self, enabled: bool) -> Self {
        self.agent_engine = enabled;
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

    /// Mount the governance admin endpoints under `/api/admin`, behind the auth middleware.
    ///
    /// | Route | Effect |
    /// |-------|--------|
    /// | `POST /api/admin/freeze` | Freezes [`ServerConfig::governance`], failing new runs and stopping running ones before their next model or tool call; background workers stop claiming and schedules stop firing; running durable runs are asked to cancel, or parked by the [`FreezeDelegate`] registered with [`with_freeze_delegate`](Self::with_freeze_delegate). A mirrored control writes the freeze to the shared store, so it reaches every replica. Body: `{"reason": "..."}`; answers the status with `parkedRuns` and `cancelledRuns` |
    /// | `POST /api/admin/unfreeze` | Lifts the freeze; a delegate also resumes what the freeze parked |
    /// | `GET /api/admin/governance` | Reports `{"frozen", "reason", "mirrored", "stale", "runningRuns"}`, where `runningRuns` is `park` under a delegate and `cancel` otherwise |
    ///
    /// The endpoints carry the same authentication as every other mutation surface. Without
    /// a `RequestContextExtractor` they are open to anyone who reaches the server, so enable
    /// them only behind an authenticating layer.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let app = ServerBuilder::new(config).enable_governance_endpoints().build();
    /// ```
    pub fn enable_governance_endpoints(mut self) -> Self {
        self.governance_endpoints = true;
        self
    }

    /// Hands the governance endpoints' freezes and unfreezes to `delegate` (spec 3.2 §10
    /// S4), so `POST /api/admin/freeze` parks the running runs the delegate supervises
    /// instead of cancelling the background run stores' runs, and `POST /api/admin/unfreeze`
    /// resumes them. A kernel registers itself here. Implies
    /// [`enable_governance_endpoints`](Self::enable_governance_endpoints).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let app = ServerBuilder::new(config).with_freeze_delegate(kernel).build();
    /// ```
    pub fn with_freeze_delegate(mut self, delegate: Arc<dyn FreezeDelegate>) -> Self {
        self.freeze_delegate = Some(delegate);
        self.governance_endpoints = true;
        self
    }

    /// Mount the approval inbox endpoints for `service` under `/api/approvals`, behind
    /// the auth middleware. See [`crate::approvals`] for the routes.
    ///
    /// Protocol-native AG-UI runs route the calls they hold to `service` as well: each ends
    /// the run with an AG-UI interrupt naming its inbox entry, and a later AG-UI `resume`
    /// decides the entry (spec 3.3 §10 S2,
    /// [`RuntimeController::with_approval_service`]).
    ///
    /// The endpoints need an authenticated caller, so configure a
    /// `RequestContextExtractor`; without one every route answers `401`. Run
    /// [`spawn_approval_ticker`](crate::approvals::spawn_approval_ticker) as well so
    /// requests expire and escalate.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let app = ServerBuilder::new(config).with_approval_service(service).build();
    /// ```
    #[cfg(feature = "approvals")]
    pub fn with_approval_service(mut self, service: Arc<dyn adk_core::ApprovalService>) -> Self {
        self.approval_service = Some(service.clone());
        self.add_api_routes(crate::approvals::approvals_router(
            crate::approvals::ApprovalEndpoints::new(service),
        ))
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
    /// let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
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
        #[cfg(feature = "approvals")]
        let runtime_controller = match &self.approval_service {
            Some(service) => runtime_controller.with_approval_service(service.clone()),
            None => runtime_controller,
        };
        // An AG-UI resume decides inbox entries for an authenticated caller.
        let resume_wired =
            runtime_controller.answers_ag_ui_resume() && config.request_context_extractor.is_some();
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
            .route("/ui/agents/{name}", get(controllers::apps::get_agent_details))
            .with_state(apps_controller)
            .route(
                "/ui/capabilities",
                get(controllers::ui::served_ui_capabilities).with_state(resume_wired),
            )
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
            .route("/ui/resources/register", post(controllers::ui::register_ui_resource))
            // These routes mutate and read shared bridge and resource state, so they
            // carry the same authentication as the session, artifact, and debug routers.
            .layer(auth_layer.clone());

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
            .route("/run", post(controllers::runtime::run_collect_for_request))
            .route(
                "/run/{app_name}/{user_id}/{session_id}",
                post(controllers::runtime::run_sse_for_request),
            )
            .route("/run_sse", post(controllers::runtime::run_sse_compat_for_request))
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

        // Background runs and cron schedules obey the server's kill switch unless they
        // carry their own, and a freeze asks their running runs to cancel.
        #[cfg(feature = "background")]
        let mut background_run_stores: Vec<Arc<dyn adk_core::DurableRunStore>> = Vec::new();
        #[cfg(feature = "background")]
        for state in self
            .background_state
            .iter()
            .chain(self.cron_state.iter().map(|cron| &cron.background_state))
        {
            state.governance_or(&config.governance);
            background_run_stores.push(Arc::clone(state.run_store()));
        }

        if self.governance_endpoints {
            let controller = controllers::GovernanceController::new(config.governance.clone());
            #[cfg(feature = "background")]
            let controller = background_run_stores
                .iter()
                .fold(controller, |controller, store| controller.with_run_store(Arc::clone(store)));
            let controller = match self.freeze_delegate {
                Some(delegate) => controller.with_freeze_delegate(delegate),
                None => controller,
            };
            api_router = api_router.merge(
                Router::new()
                    .route("/admin/freeze", post(controllers::governance::freeze))
                    .route("/admin/unfreeze", post(controllers::governance::unfreeze))
                    .route("/admin/governance", get(controllers::governance::status))
                    .with_state(controller)
                    .layer(auth_layer.clone()),
            );
        }

        // Background runs and cron jobs submit and schedule work, so they carry the
        // same authentication as every other mutation surface.
        #[cfg(feature = "background")]
        if let Some(state) = self.background_state {
            api_router = api_router.merge(
                crate::background::background_runs_router_with_state(state)
                    .layer(auth_layer.clone()),
            );
        }
        #[cfg(feature = "background")]
        if let Some(state) = self.cron_state {
            api_router = api_router.merge(
                crate::background::cron_jobs_router_with_state(state).layer(auth_layer.clone()),
            );
        }

        // Add shutdown endpoint if enabled
        let shutdown_handle = if self.shutdown_endpoint {
            let handle = ShutdownHandle::new();
            let shutdown_router = Router::new()
                .route("/shutdown", post(handle_shutdown))
                .with_state(handle.token.clone())
                .layer(auth_layer.clone());
            api_router = api_router.merge(shutdown_router);
            Some(handle)
        } else {
            None
        };

        let mut app = Router::new().nest("/api", api_router);

        #[cfg(feature = "webhook-ingress")]
        if let Some(ingress) = self.webhook_ingress {
            let ingress = if ingress.has_governance() {
                ingress
            } else {
                ingress.with_governance(config.governance.clone())
            };
            app = app.merge(crate::webhooks::webhook_ingress_router(ingress));
        }

        // Merge custom root routes
        for custom_routes in self.root_routes {
            app = app.merge(custom_routes);
        }

        if let Some(base_url) = &self.a2a_base_url {
            let a2a_controller = match &self.skill_index {
                Some(skill_index) => {
                    A2aController::with_skill_index(config.clone(), base_url, skill_index.clone())
                }
                None => A2aController::new(config.clone(), base_url),
            };
            let a2a_controller = match self.a2a_task_retention {
                Some(retention) => a2a_controller.with_task_retention(retention),
                None => a2a_controller,
            };
            // Same split as `create_app_with_a2a`: discovery is public, RPC is authenticated.
            let a2a_discovery = a2a_discovery_router(a2a_controller.clone());
            // The AWP edge serves the site's signed card at the same path.
            #[cfg(feature = "awp")]
            let a2a_discovery = if self.awp_edge { Router::new() } else { a2a_discovery };
            let a2a_rpc = Router::new()
                .route("/a2a", post(controllers::a2a::handle_jsonrpc_for_caller))
                .route("/a2a/stream", post(controllers::a2a::handle_jsonrpc_stream_for_caller))
                .with_state(a2a_controller)
                .layer(auth_layer.clone());
            app = app.merge(a2a_discovery).merge(a2a_rpc);
        }

        let security = effective_security(
            &config.security,
            [self.a2a_base_url.as_deref(), config.backend_url.as_deref()],
        );
        app = app.layer(middleware::from_fn_with_state(Arc::clone(&security), host_guard));

        // Agent Engine dispatch surface: root-level merge (its routes carry
        // their own /api/... paths) so the platform host reaches it without
        // the local auth middleware or the host check — the platform fronts the
        // container, authenticates callers, and chooses the Host it forwards.
        #[cfg(feature = "agent-engine")]
        if self.agent_engine {
            let root_agent = config.agent_loader.root_agent();
            let mut runner_builder = adk_runner::Runner::builder()
                .app_name(root_agent.name())
                .agent(root_agent.clone())
                .session_service(config.session_service.clone())
                .governance(config.governance.clone());
            if let Some(artifact_service) = &config.artifact_service {
                runner_builder = runner_builder.artifact_service(artifact_service.clone());
            }
            let runner = runner_builder
                .build()
                .expect("with_agent_engine requires the root agent's name to be a valid app name");
            let mut state = crate::agent_engine::AgentEngineState::new(Arc::new(runner));
            if let Some(artifact_service) = &config.artifact_service {
                state = state.with_artifact_service(artifact_service.clone());
            }
            app = app.merge(crate::agent_engine::agent_engine_router(state));
        }

        let cors_layer = build_cors_layer(&security);
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
                    .layer(middleware::from_fn_with_state(Arc::clone(&security), request_timeout))
                    .layer(DefaultBodyLimit::max(config.security.max_body_size))
                    .layer(cors_layer)
                    .layer(middleware::from_fn_with_state(Arc::clone(&security), origin_guard))
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
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
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
