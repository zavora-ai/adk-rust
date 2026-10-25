//! The MCP server's `axum::Router`: Streamable HTTP, the Server Card and an AI Catalog
//! (spec 3.5 §7.9).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Url};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use rmcp::model::{ClientJsonRpcMessage, ClientRequest};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::card::{
    AI_CATALOG_MEDIA_TYPE, AI_CATALOG_WELL_KNOWN_PATH, AiCatalog, AiCatalogHost,
    SERVER_CARD_ENDPOINT_SUFFIX, SERVER_CARD_MEDIA_TYPE, SERVER_CARD_WELL_KNOWN_PATH,
    ServerCardHeader,
};
use super::{AnonymousCallers, McpCaller, McpCallerResolver, McpHttpRequest, McpServer, refusal};

/// The header carrying the MCP session id.
const SESSION_HEADER: &str = "mcp-session-id";

/// How the Streamable HTTP endpoint keeps sessions.
///
/// | Mode | `Mcp-Session-Id` | `GET` stream, `DELETE` | Responses |
/// |------|------------------|------------------------|-----------|
/// | `Stateful` (default) | Issued on `initialize`, bound to the caller | Yes | SSE |
/// | `Stateless` | Never issued | No (`405`) | JSON |
///
/// Clients that negotiate MCP `2026-07-28` are served statelessly in either mode.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// use adk_tool::mcp::server::McpSessions;
///
/// let sessions = McpSessions::stateful(Duration::from_secs(120), 64);
/// assert_ne!(sessions, McpSessions::Stateless);
/// assert_eq!(McpSessions::default(), McpSessions::stateful(
///     McpSessions::DEFAULT_IDLE_TIMEOUT,
///     McpSessions::DEFAULT_MAX_SESSIONS,
/// ));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpSessions {
    /// Sessions kept in memory, each closed after `idle_timeout` without a request; at most
    /// `max_sessions` live at once, after which `initialize` answers `503`.
    Stateful {
        /// How long an idle session lives.
        idle_timeout: Duration,
        /// The most live sessions.
        max_sessions: usize,
    },
    /// No sessions: each request stands alone.
    Stateless,
}

impl McpSessions {
    /// The default idle lifetime of a session.
    pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
    /// The default limit on live sessions.
    pub const DEFAULT_MAX_SESSIONS: usize = 1024;

    /// Stateful sessions with the given idle lifetime and limit.
    pub fn stateful(idle_timeout: Duration, max_sessions: usize) -> Self {
        Self::Stateful { idle_timeout, max_sessions }
    }
}

impl Default for McpSessions {
    fn default() -> Self {
        Self::stateful(Self::DEFAULT_IDLE_TIMEOUT, Self::DEFAULT_MAX_SESSIONS)
    }
}

/// Builds the `axum::Router` serving an [`McpServer`] (spec 3.5 §7.9).
///
/// | Route | Serves |
/// |-------|--------|
/// | `POST`, `GET`, `DELETE` at the [path](Self::path) (`/mcp`) | MCP Streamable HTTP |
/// | `GET {path}/server-card` | The Server Card at the location SEP-2127 reserves, with a [public URL](Self::public_url) |
/// | `GET /.well-known/ai-catalog.json` | An AI Catalog whose entry points at that card, with a public URL, unless [`without_ai_catalog`](Self::without_ai_catalog) |
/// | `GET /.well-known/mcp/server-card.json` | The same card at the path spec 3.5 §7.9 names, with a public URL |
///
/// Every request to the MCP endpoint first goes through the [`McpCallerResolver`]; a refusal
/// answers with the error's HTTP status. A `POST` body over [`max_body_bytes`](Self::max_body_bytes)
/// answers `413`, a body that is not JSON `400` with JSON-RPC error `-32700`, and JSON that is
/// not a JSON-RPC message `400` with `-32600` (or `-32602` for a well-formed request whose
/// parameters do not fit its method). A server error from the transport answers a JSON-RPC
/// `-32603` with no detail. Stateful sessions belong to the caller that opened them: another
/// caller naming the session gets `404`.
///
/// Card responses carry `Content-Type: application/mcp-server-card+json`, open CORS,
/// `Cache-Control: public, max-age=3600` and an `ETag`, and answer `If-None-Match` with `304`.
///
/// The transport accepts loopback `Host` headers only until [`allowed_hosts`](Self::allowed_hosts)
/// names the public ones.
///
/// # Example
///
/// ```rust
/// use adk_core::Url;
/// use adk_tool::mcp::server::{AiCatalogHost, McpRouterBuilder, McpServer, McpSessions};
///
/// let server = McpServer::builder("com.example/solar", "1.0.0")
///     .description("Quotes for Example Solar installations")
///     .build()?;
/// let builder = McpRouterBuilder::new(server)
///     .public_url(Url::parse("https://agent.example.com/mcp").unwrap())
///     .allowed_hosts(["agent.example.com"])
///     .sessions(McpSessions::Stateless)
///     .ai_catalog(AiCatalogHost::new("Example Solar"));
/// assert_eq!(
///     builder.server_card_url().unwrap().as_str(),
///     "https://agent.example.com/mcp/server-card"
/// );
/// assert_eq!(
///     builder.well_known_card_url().unwrap().as_str(),
///     "https://agent.example.com/.well-known/mcp/server-card.json"
/// );
/// let router: axum::Router = builder.build()?;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct McpRouterBuilder {
    server: McpServer,
    path: String,
    public_url: Option<Url>,
    resolver: Arc<dyn McpCallerResolver>,
    sessions: McpSessions,
    max_body_bytes: usize,
    allowed_hosts: Option<Vec<String>>,
    allowed_origins: Vec<String>,
    card_headers: Vec<ServerCardHeader>,
    catalog_host: Option<AiCatalogHost>,
    serve_catalog: bool,
    cancellation: CancellationToken,
}

impl McpRouterBuilder {
    /// The default limit on a `POST` body: 1 MiB.
    pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

    /// Starts a router for `server` at `/mcp`, resolving every caller as anonymous.
    pub fn new(server: McpServer) -> Self {
        Self {
            server,
            path: "/mcp".to_string(),
            public_url: None,
            resolver: Arc::new(AnonymousCallers),
            sessions: McpSessions::default(),
            max_body_bytes: Self::DEFAULT_MAX_BODY_BYTES,
            allowed_hosts: None,
            allowed_origins: Vec::new(),
            card_headers: Vec::new(),
            catalog_host: None,
            serve_catalog: true,
            cancellation: CancellationToken::new(),
        }
    }

    /// The path of the Streamable HTTP endpoint within the router; `/mcp` by default.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// The endpoint's public URL, which the Server Card names; the card and catalog routes are
    /// served only when it is set.
    pub fn public_url(mut self, url: Url) -> Self {
        self.public_url = Some(url);
        self
    }

    /// Resolves each request's caller with `resolver`.
    pub fn caller_resolver(mut self, resolver: Arc<dyn McpCallerResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Sets how sessions are kept.
    pub fn sessions(mut self, sessions: McpSessions) -> Self {
        self.sessions = sessions;
        self
    }

    /// Limits a `POST` body to `bytes`.
    pub fn max_body_bytes(mut self, bytes: usize) -> Self {
        self.max_body_bytes = bytes;
        self
    }

    /// Accepts requests whose `Host` is one of `hosts` (`example.com` or `example.com:8443`).
    ///
    /// The default accepts loopback hosts only, against DNS rebinding. An empty list accepts any
    /// `Host`, for a router mounted behind a server that checks hosts itself.
    pub fn allowed_hosts(mut self, hosts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_hosts = Some(hosts.into_iter().map(Into::into).collect());
        self
    }

    /// Refuses browser requests whose `Origin` is not one of `origins`, such as
    /// `https://app.example.com:443`; no check by default.
    pub fn allowed_origins(mut self, origins: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Describes a header clients send, such as `Authorization`, in the Server Card.
    pub fn card_header(mut self, header: ServerCardHeader) -> Self {
        self.card_headers.push(header);
        self
    }

    /// Names `host` as the operator of the AI Catalog at `/.well-known/ai-catalog.json`. Needs a
    /// [public URL](Self::public_url).
    pub fn ai_catalog(mut self, host: AiCatalogHost) -> Self {
        self.catalog_host = Some(host);
        self.serve_catalog = true;
        self
    }

    /// Leaves `/.well-known/ai-catalog.json` to the host application, which lists this server
    /// with [`McpServer::ai_catalog_entry`] beside its other artifacts, such as an A2A card.
    pub fn without_ai_catalog(mut self) -> Self {
        self.catalog_host = None;
        self.serve_catalog = false;
        self
    }

    /// Ends every session and stream when `token` is cancelled.
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation = token;
        self
    }

    /// The card's URL at the location SEP-2127 reserves and the AI Catalog entry names:
    /// `{public URL}/server-card`.
    pub fn server_card_url(&self) -> Option<Url> {
        let public = self.public_url.as_ref()?;
        let base = public.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}{SERVER_CARD_ENDPOINT_SUFFIX}")).ok()
    }

    /// The card's alias at the path spec 3.5 §7.9 names: `/.well-known/mcp/server-card.json` on
    /// the public URL's origin.
    pub fn well_known_card_url(&self) -> Option<Url> {
        self.public_url.as_ref()?.join(SERVER_CARD_WELL_KNOWN_PATH).ok()
    }

    /// Builds the router.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidInput` error when the path does not start with `/`, the public URL is
    /// not `http` or `https` or carries a query or fragment, or an AI Catalog is requested
    /// without a public URL.
    pub fn build(self) -> Result<Router> {
        if !self.path.starts_with('/') || self.path.len() < 2 {
            return Err(invalid(
                "mcp.server.path",
                "the MCP path must start with '/', such as '/mcp'",
            ));
        }
        if let Some(url) = &self.public_url
            && (!matches!(url.scheme(), "http" | "https")
                || url.query().is_some()
                || url.fragment().is_some())
        {
            return Err(invalid(
                "mcp.server.public_url",
                "the public URL must be an http or https URL without a query or fragment",
            ));
        }
        if self.catalog_host.is_some() && self.public_url.is_none() {
            return Err(invalid(
                "mcp.server.ai_catalog",
                "an AI Catalog lists the card's URL; set the public URL with `public_url(..)`",
            ));
        }

        let mut config = StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(self.max_body_bytes)
            .with_allowed_origins(self.allowed_origins.clone())
            .with_cancellation_token(self.cancellation.clone());
        config = match &self.allowed_hosts {
            None => config,
            Some(hosts) if hosts.is_empty() => config.disable_allowed_hosts(),
            Some(hosts) => config.with_allowed_hosts(hosts.clone()),
        };
        let server = self.server.clone();
        let factory = move || Ok(server.clone());
        let (transport, sessions) = match self.sessions {
            McpSessions::Stateful { idle_timeout, max_sessions } => {
                let mut manager = LocalSessionManager::default();
                manager.session_config.keep_alive = Some(idle_timeout);
                let manager = Arc::new(manager);
                let service = StreamableHttpService::new(
                    factory,
                    Arc::clone(&manager),
                    config.with_legacy_session_mode(true),
                );
                let sessions = SessionGuard {
                    manager,
                    max_sessions,
                    owners: Mutex::new(HashMap::new()),
                    admission: Mutex::new(()),
                };
                (Router::new().route_service(&self.path, service), Some(sessions))
            }
            McpSessions::Stateless => {
                let service = StreamableHttpService::new(
                    factory,
                    Arc::new(NeverSessionManager::default()),
                    config.with_legacy_session_mode(false).with_json_response(true),
                );
                (Router::new().route_service(&self.path, service), None)
            }
        };
        let guard = Arc::new(Guard {
            resolver: Arc::clone(&self.resolver),
            max_body_bytes: self.max_body_bytes,
            sessions,
        });
        let mut router = transport.layer(middleware::from_fn_with_state(guard, guard_request));

        if let Some(public_url) = &self.public_url {
            let card = self.server.server_card(public_url, &self.card_headers);
            let card = Arc::new(Document::new(SERVER_CARD_MEDIA_TYPE, &card)?);
            let card_path =
                format!("{}{SERVER_CARD_ENDPOINT_SUFFIX}", self.path.trim_end_matches('/'));
            for path in [card_path.as_str(), SERVER_CARD_WELL_KNOWN_PATH] {
                let document = Arc::clone(&card);
                router =
                    router.route(path, get(serve_document).options(preflight).with_state(document));
            }
            if self.serve_catalog {
                let card_url = self.server_card_url().ok_or_else(|| {
                    invalid("mcp.server.public_url", "the card URL could not be formed")
                })?;
                let mut catalog = AiCatalog::new([self.server.ai_catalog_entry(&card_url)]);
                if let Some(host) = self.catalog_host.clone() {
                    catalog = catalog.with_host(host);
                }
                let catalog = Arc::new(Document::new(AI_CATALOG_MEDIA_TYPE, &catalog)?);
                router = router.route(
                    AI_CATALOG_WELL_KNOWN_PATH,
                    get(serve_document).options(preflight).with_state(catalog),
                );
            }
        }
        Ok(router)
    }
}

/// What the middleware in front of the transport needs.
struct Guard {
    resolver: Arc<dyn McpCallerResolver>,
    max_body_bytes: usize,
    sessions: Option<SessionGuard>,
}

/// The live stateful sessions: who owns each, and how many may exist.
struct SessionGuard {
    manager: Arc<LocalSessionManager>,
    max_sessions: usize,
    /// Session id to the id of the caller that opened it.
    owners: Mutex<HashMap<String, String>>,
    /// Held while an `initialize` opens a session, so the limit is exact.
    admission: Mutex<()>,
}

impl SessionGuard {
    /// Whether `caller` may use session `id`: it opened it, or the session is unknown here and
    /// the transport answers for it.
    async fn admits(&self, id: &str, caller: &str) -> bool {
        self.owners.lock().await.get(id).is_none_or(|owner| owner == caller)
    }
}

/// Resolves the caller, bounds and checks the body, keeps sessions to their owners and the
/// limit, then hands the request to the transport.
async fn guard_request(State(guard): State<Arc<Guard>>, request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    let caller = match guard.resolver.resolve(&McpHttpRequest::new(&parts)).await {
        Ok(caller) => caller,
        Err(error) => return refused(&error),
    };
    let session =
        parts.headers.get(SESSION_HEADER).and_then(|value| value.to_str().ok()).map(str::to_string);
    if let (Some(sessions), Some(id)) = (&guard.sessions, &session)
        && !sessions.admits(id, caller.id()).await
    {
        tracing::info!(
            caller.id = caller.id(),
            "mcp session used by a caller that did not open it"
        );
        return (StatusCode::NOT_FOUND, "Not Found: Session not found").into_response();
    }

    let mut initialize = false;
    let body = if parts.method == Method::POST {
        let bytes = match read_bounded(body, &parts.headers, guard.max_body_bytes).await {
            Ok(bytes) => bytes,
            Err(response) => return *response,
        };
        let is_json = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/json"));
        if is_json {
            match parse_message(&bytes) {
                Ok(message) => {
                    initialize = matches!(
                        &message,
                        ClientJsonRpcMessage::Request(request)
                            if matches!(request.request, ClientRequest::InitializeRequest(_))
                    );
                }
                Err(response) => return *response,
            }
        }
        Body::from(bytes)
    } else {
        body
    };
    let method = parts.method.clone();
    let caller_id = caller.id().to_string();
    parts.extensions.insert::<McpCaller>(caller);
    let request = Request::from_parts(parts, body);

    let response = match (&guard.sessions, &session) {
        (Some(sessions), None) if initialize => {
            let _admission = sessions.admission.lock().await;
            let live = sessions.manager.sessions.read().await;
            if live.len() >= sessions.max_sessions {
                drop(live);
                tracing::warn!(sessions.max = sessions.max_sessions, "mcp session limit reached");
                return jsonrpc_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    Value::Null,
                    -32603,
                    "the server has reached its session limit; retry later",
                );
            }
            sessions.owners.lock().await.retain(|id, _| live.contains_key(id.as_str()));
            drop(live);
            let response = next.run(request).await;
            if let Some(id) = response.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
                sessions.owners.lock().await.insert(id.to_string(), caller_id);
            }
            response
        }
        (Some(sessions), Some(id)) if method == Method::DELETE => {
            let response = next.run(request).await;
            if response.status().is_success() {
                sessions.owners.lock().await.remove(id);
            }
            response
        }
        _ => next.run(request).await,
    };
    if response.status().is_server_error() {
        tracing::warn!(http.status = response.status().as_u16(), "mcp transport failed a request");
        return jsonrpc_error(response.status(), Value::Null, -32603, "internal error");
    }
    response
}

/// Reads `body` up to `limit` bytes; a longer body answers `413`.
async fn read_bounded(
    body: Body,
    headers: &HeaderMap,
    limit: usize,
) -> std::result::Result<Bytes, Box<Response>> {
    let too_large = || {
        Box::new(jsonrpc_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            Value::Null,
            -32600,
            &format!("the request body exceeds {limit} bytes"),
        ))
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > limit as u64) {
        return Err(too_large());
    }
    let mut stream = body.into_data_stream();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return Err(Box::new(
                (StatusCode::BAD_REQUEST, "Bad Request: the request body could not be read")
                    .into_response(),
            ));
        };
        if collected.len() + chunk.len() > limit {
            return Err(too_large());
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(collected))
}

/// The methods this server answers: a request for one of them that rmcp could only read as a
/// custom request had parameters that do not fit the method.
const SERVED_METHODS: [&str; 5] =
    ["initialize", "ping", "server/discover", "tools/list", "tools/call"];

/// Parses `bytes` as one client JSON-RPC message, or answers with the JSON-RPC error.
fn parse_message(bytes: &[u8]) -> std::result::Result<ClientJsonRpcMessage, Box<Response>> {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Err(Box::new(jsonrpc_error(
            StatusCode::BAD_REQUEST,
            Value::Null,
            -32700,
            "parse error: the body is not JSON",
        )));
    };
    let id = match value.get("id") {
        Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
        _ => Value::Null,
    };
    let invalid_params = |method: &str| {
        let method: String = method.chars().take(64).collect();
        Box::new(jsonrpc_error(
            StatusCode::BAD_REQUEST,
            id.clone(),
            -32602,
            &format!("invalid params for '{method}'"),
        ))
    };
    match serde_json::from_value::<ClientJsonRpcMessage>(value.clone()) {
        Ok(ClientJsonRpcMessage::Request(request))
            if matches!(
                &request.request,
                ClientRequest::CustomRequest(custom) if SERVED_METHODS.contains(&custom.method.as_str())
            ) =>
        {
            let method = value["method"].as_str().unwrap_or_default();
            Err(invalid_params(method))
        }
        Ok(message) => Ok(message),
        Err(_) => match (value.get("jsonrpc"), value.get("method"), value.get("id")) {
            (Some(Value::String(version)), Some(Value::String(method)), Some(_))
                if version == "2.0" =>
            {
                Err(invalid_params(method))
            }
            _ => Err(Box::new(jsonrpc_error(
                StatusCode::BAD_REQUEST,
                id,
                -32600,
                "invalid request: the body is not one JSON-RPC 2.0 message",
            ))),
        },
    }
}

/// A request the resolver refused, answered with the error's status.
fn refused(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    tracing::debug!(http.status = status.as_u16(), error = %error.diagnostics(), "mcp request refused");
    let data = refusal(error);
    let body = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": data.code.0, "message": data.message, "data": data.data },
    });
    let mut response =
        (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response();
    if error.category == ErrorCategory::Unauthorized {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

/// A JSON-RPC error response with `status`.
fn jsonrpc_error(status: StatusCode, id: Value, code: i32, message: &str) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_string()).into_response()
}

/// A discovery document served as is: its bytes, media type and entity tag.
struct Document {
    body: Bytes,
    media_type: &'static str,
    etag: HeaderValue,
}

impl Document {
    fn new(media_type: &'static str, value: &impl serde::Serialize) -> Result<Self> {
        let body = serde_json::to_vec(value).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                "mcp.server.card",
                format!("the discovery document did not serialise: {error}"),
            )
        })?;
        let digest = Sha256::digest(&body);
        let etag = HeaderValue::from_str(&format!("\"{}\"", URL_SAFE_NO_PAD.encode(&digest[..16])))
            .map_err(|_| invalid("mcp.server.card", "the entity tag is not a header value"))?;
        Ok(Self { body: Bytes::from(body), media_type, etag })
    }
}

/// The headers every discovery response carries: open CORS and an hour of caching.
fn discovery_headers(document: &Document) -> [(HeaderName, HeaderValue); 6] {
    [
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*")),
        (header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET")),
        (
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, If-None-Match"),
        ),
        (header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static("ETag")),
        (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600")),
        (header::ETAG, document.etag.clone()),
    ]
}

/// Serves a discovery document, or `304` when the client holds the current one.
async fn serve_document(State(document): State<Arc<Document>>, headers: HeaderMap) -> Response {
    let current = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| {
            tag == "*" || tag.trim_start_matches("W/").as_bytes() == document.etag.as_bytes()
        });
    if current {
        return (StatusCode::NOT_MODIFIED, discovery_headers(&document)).into_response();
    }
    (
        StatusCode::OK,
        discovery_headers(&document),
        [(header::CONTENT_TYPE, HeaderValue::from_static(document.media_type))],
        document.body.clone(),
    )
        .into_response()
}

/// Answers a CORS preflight for a discovery document.
async fn preflight(State(document): State<Arc<Document>>) -> Response {
    (StatusCode::NO_CONTENT, discovery_headers(&document)).into_response()
}

fn invalid(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::InvalidInput, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_and_body(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let body =
            futures::executor::block_on(axum::body::to_bytes(response.into_body(), 4096)).unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[test]
    fn a_body_that_is_not_json_is_a_parse_error() {
        let (status, body) = status_and_body(*parse_message(b"{not json").unwrap_err());
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], -32700);
        assert_eq!(body["id"], Value::Null);
    }

    #[test]
    fn a_batch_is_an_invalid_request() {
        let batch = br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#;
        let (_, body) = status_and_body(*parse_message(batch).unwrap_err());
        assert_eq!(body["error"]["code"], -32600);
    }

    #[test]
    fn a_known_method_with_bad_params_is_invalid_params() {
        let call = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"arguments":3}}"#;
        let (_, body) = status_and_body(*parse_message(call).unwrap_err());
        assert_eq!(body["error"]["code"], -32602);
        assert_eq!(body["id"], 7);
    }

    #[test]
    fn an_unknown_method_is_left_to_the_transport() {
        let custom = br#"{"jsonrpc":"2.0","id":1,"method":"vendor/extra","params":{}}"#;
        assert!(parse_message(custom).is_ok());
    }

    #[test]
    fn a_valid_request_parses() {
        let ping = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        assert!(parse_message(ping).is_ok());
    }
}
