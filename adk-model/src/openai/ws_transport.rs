//! WebSocket transport for the OpenAI Responses API.
//!
//! Provides persistent WebSocket connections to `/v1/responses` (WebSocket mode) for
//! lower-latency agentic workflows. Each request is sent as a `response.create` event and
//! its server events are read with the same projection and stream recovery as the HTTP
//! SSE path, so a WebSocket stream ends with the same outcomes as an HTTP stream.
//!
//! Gated behind the `openai-ws` feature flag.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, LlmResponseStream, RetryHint};
use async_openai::types::responses::{CreateResponse, Tool};
use async_stream::try_stream;
use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        self, Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue, header},
    },
};

use super::config::OpenAIResponsesConfig;
use super::responses_client::{parse, wire};
use super::responses_stream::StreamState;
use crate::retry::{
    RetryConfig, category_for_status_code, execute_with_retry, is_retryable_model_error,
    parse_retry_after, with_retry_after,
};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Error codes WebSocket mode documents in addition to the Responses error codes.
const WEBSOCKET_ERROR_CODES: &[&str] = &[
    "previous_response_not_found",
    "invalid_stream_id",
    "websocket_stream_limit_reached",
    "websocket_connection_limit_reached",
];

/// Code of the error for a request whose frame may have reached the server: its outcome is
/// unknown, so it is neither resent nor retried.
pub(super) const SEND_OUTCOME_UNKNOWN: &str = "model.openai_responses.ws_send";

const DEFAULT_MAX_CONNECTIONS: usize = 4;
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Persistent WebSocket connections to the OpenAI Responses API.
///
/// Keeps a pool of up to [`with_max_connections`](Self::with_max_connections) connections
/// (default 4), reused across [`send_request`](Self::send_request) calls to cut
/// per-request connection overhead for latency-sensitive agentic workflows. Each
/// connection carries one response at a time, so concurrent requests — from different
/// sessions sharing one client — each take their own connection; a request beyond the
/// limit waits up to [`with_acquire_timeout`](Self::with_acquire_timeout) for one.
///
/// A request that continues with `previous_response_id` takes the connection that produced
/// that response when it is idle, because an unstored (`store = false`) response is cached
/// only on its connection; on another connection it needs `store = true`. A connection
/// returns to the pool when its response's terminal event arrives, before the final chunk
/// is yielded. A stream dropped or failed before its terminal event discards the
/// connection, so the rest of that response never reaches another request. A connection
/// the server closed while idle is replaced before anything is sent on it.
///
/// A request is sent at most once. Once its frame has been written, a failure means the
/// server may be generating it: the stream fails without resending, and the error is not
/// retryable.
///
/// Handshakes are retried under the [`RetryConfig`] for transient failures (network
/// errors, HTTP 408, 429, and 5xx); an invalid URL or an HTTP 4xx rejection such as 401
/// fails at once. Each attempt is bounded by
/// [`with_connect_timeout`](Self::with_connect_timeout) (default 30 s), and the wait for
/// each server event by [`with_read_timeout`](Self::with_read_timeout) (default 600 s, as
/// the HTTP idle timeout).
///
/// # Example
///
/// ```rust,ignore
/// use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
/// use async_openai::types::responses::{CreateResponseArgs, InputParam};
/// use futures::StreamExt;
///
/// let config = OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra");
/// let transport = WsTransport::connect(&config).await?;
/// let request = CreateResponseArgs::default()
///     .model("gpt-5.6-terra")
///     .input(InputParam::Text("Hello".into()))
///     .build()?;
/// let mut stream = transport.send_request(request).await?;
/// while let Some(chunk) = stream.next().await {
///     let chunk = chunk?;
///     // `chunk.partial` deltas, then one final chunk with the rest of the response.
/// }
/// ```
pub struct WsTransport {
    pool: Arc<Pool>,
    /// Connection URL (derived from base_url).
    url: String,
    /// Handshake headers: the bearer token and the organization and project, when set.
    headers: Vec<(HeaderName, HeaderValue)>,
    /// Retry configuration for handshakes.
    retry_config: RetryConfig,
    connect_timeout: Duration,
    read_timeout: Duration,
    acquire_timeout: Duration,
}

/// The connections of one transport.
struct Pool {
    /// One permit per connection a response may hold.
    permits: Arc<Semaphore>,
    /// Open connections no response holds.
    idle: std::sync::Mutex<Vec<Idle>>,
    /// Open connections, idle or held.
    open: AtomicUsize,
}

struct Idle {
    socket: WsStream,
    /// The id of the last response this connection carried.
    last_response: Option<String>,
}

impl Pool {
    fn new(max_connections: usize, idle: Vec<Idle>) -> Arc<Self> {
        let open = AtomicUsize::new(idle.len());
        Arc::new(Self {
            permits: Arc::new(Semaphore::new(max_connections.max(1))),
            idle: std::sync::Mutex::new(idle),
            open,
        })
    }

    /// An idle connection, preferring the one that carried `previous_response`.
    fn take(&self, previous_response: Option<&str>) -> Option<WsStream> {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        let index = idle
            .iter()
            .position(|entry| {
                previous_response.is_some() && entry.last_response.as_deref() == previous_response
            })
            .or_else(|| idle.len().checked_sub(1))?;
        Some(idle.swap_remove(index).socket)
    }

    fn put(&self, socket: WsStream, last_response: Option<String>) {
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Idle { socket, last_response });
    }

    fn discard(&self) {
        self.open.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One response's use of one connection.
///
/// Dropped before [`release`](Self::release) — the stream was dropped or failed before the
/// response's terminal event — it discards the connection: frames of that response may
/// still be in flight.
struct Lease {
    socket: Option<WsStream>,
    pool: Arc<Pool>,
    _permit: OwnedSemaphorePermit,
}

impl Lease {
    /// Returns the connection to the pool, clean after the terminal event of `response`.
    fn release(mut self, response: Option<String>) {
        if let Some(socket) = self.socket.take() {
            self.pool.put(socket, response);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.socket.take().is_some() {
            self.pool.discard();
        }
    }
}

impl WsTransport {
    /// Creates a transport that opens its first connection with its first request.
    ///
    /// The WebSocket URL is derived from the configured base URL by replacing `https://`
    /// with `wss://` and appending `/v1/responses`. The handshake sends the API key as a
    /// bearer token, and the organization and project when the config sets them.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
    /// use std::time::Duration;
    ///
    /// let config = OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra");
    /// let transport = WsTransport::new(&config)?
    ///     .with_max_connections(8)
    ///     .with_read_timeout(Duration::from_secs(300));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the API key, organization or project is not a valid
    /// HTTP header value.
    pub fn new(config: &OpenAIResponsesConfig) -> Result<Self, AdkError> {
        let base_url = config.base_url.as_deref().unwrap_or("https://api.openai.com/v1");
        let header_value = |value: String| {
            HeaderValue::from_str(&value).map_err(|_| {
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::InvalidInput,
                    "model.openai_responses.ws_invalid_api_key",
                    "OpenAI API key, organization or project is not a valid HTTP header value",
                )
                .with_provider("openai-responses")
            })
        };
        let mut headers =
            vec![(header::AUTHORIZATION, header_value(format!("Bearer {}", config.api_key))?)];
        if let Some(organization) = &config.organization_id {
            headers.push((
                HeaderName::from_static("openai-organization"),
                header_value(organization.clone())?,
            ));
        }
        if let Some(project) = &config.project_id {
            headers
                .push((HeaderName::from_static("openai-project"), header_value(project.clone())?));
        }
        Ok(Self {
            pool: Pool::new(DEFAULT_MAX_CONNECTIONS, Vec::new()),
            url: derive_ws_url(base_url),
            headers,
            retry_config: RetryConfig::default(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        })
    }

    /// Establish a new WebSocket connection to the OpenAI Responses API.
    ///
    /// As [`new`](Self::new), with the first connection opened now. Transient handshake
    /// failures are retried under [`RetryConfig::default`]; use
    /// [`connect_with_retry_config`](Self::connect_with_retry_config) to choose the policy.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with category `Unavailable` when the server cannot be reached,
    /// `InvalidInput` for an invalid URL or API key, and the category of the HTTP status
    /// (for example `Unauthorized` for 401) when the handshake is rejected.
    pub async fn connect(config: &OpenAIResponsesConfig) -> Result<Self, AdkError> {
        Self::connect_with_retry_config(config, RetryConfig::default()).await
    }

    /// Establish a new WebSocket connection, retrying transient handshake failures under
    /// `retry_config`, which also governs later connections.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
    /// use adk_model::retry::RetryConfig;
    ///
    /// let config = OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra");
    /// let retry = RetryConfig::default().with_max_retries(5);
    /// let transport = WsTransport::connect_with_retry_config(&config, retry).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`connect`](Self::connect).
    pub async fn connect_with_retry_config(
        config: &OpenAIResponsesConfig,
        retry_config: RetryConfig,
    ) -> Result<Self, AdkError> {
        let transport = Self::new(config)?.with_retry_config(retry_config);
        let socket = transport.handshake().await?;
        transport.pool.put(socket, None);
        transport.pool.open.fetch_add(1, Ordering::Relaxed);
        Ok(transport)
    }

    /// Set the retry configuration for handshakes.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Set how many connections the transport keeps, which bounds how many responses it
    /// carries at once (default 4; at least 1).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let transport = WsTransport::new(&config)?.with_max_connections(16);
    /// ```
    #[must_use]
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        let idle =
            std::mem::take(&mut *self.pool.idle.lock().unwrap_or_else(PoisonError::into_inner));
        self.pool = Pool::new(max_connections, idle);
        self
    }

    /// Set how long a request waits for a connection while all are carrying responses
    /// (default 30 s). It then fails with `model.openai_responses.ws_pool_timeout`
    /// (`Unavailable`), having sent nothing.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let transport = WsTransport::new(&config)?.with_acquire_timeout(Duration::from_secs(5));
    /// ```
    #[must_use]
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Set the bound on each handshake attempt (default 30 s).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let transport = WsTransport::new(&config)?.with_connect_timeout(Duration::from_secs(10));
    /// ```
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set how long a response may go without a server event (default 600 s). The stream
    /// then fails with `model.openai_responses.ws_timeout` (`Timeout`, not retryable: the
    /// server may still be generating) and the connection is discarded.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let transport = WsTransport::new(&config)?.with_read_timeout(Duration::from_secs(120));
    /// ```
    #[must_use]
    pub fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Returns whether the transport holds an open WebSocket connection.
    ///
    /// `false` before the first connection, and after the server closed every connection or
    /// streams were dropped before their terminal events; the next
    /// [`send_request`](Self::send_request) connects.
    pub fn is_connected(&self) -> bool {
        self.pool.open.load(Ordering::Relaxed) > 0
    }

    /// Send a request over the WebSocket and return a response stream.
    ///
    /// The request is sent as a `response.create` event; `stream` and `background` are
    /// HTTP transport fields and are not sent. The stream yields partial deltas and then
    /// one final chunk carrying the rest of the response, exactly as the HTTP SSE path
    /// does, and ends after the response's terminal event.
    ///
    /// The call takes an idle connection — the one that carried the request's
    /// `previous_response_id` when there is one — or opens a new one, and waits while every
    /// connection carries a response. An idle connection the server closed is replaced
    /// before anything is written.
    ///
    /// The stream fails with:
    /// - `model.openai_responses.unfinished_stream` (`Unavailable`) when the connection
    ///   closes before a terminal event;
    /// - `model.openai_responses.ws_timeout` (`Timeout`, not retryable) when no event
    ///   arrives within the read timeout;
    /// - `model.openai_responses.response_failed` for `response.failed`;
    /// - `model.openai_responses.ws_error` for an `error` event, categorized by its status;
    /// - `model.openai_responses.incomplete` or `model.openai_responses.cancelled` for a
    ///   response that ended incomplete (other than at `max_output_tokens` or by the
    ///   content filter, which end with a finish reason) or cancelled;
    /// - `model.openai_responses.parse` when an event does not match the Responses schema.
    ///
    /// # Errors
    ///
    /// Before anything is sent: `model.openai_responses.ws_pool_timeout` (`Unavailable`)
    /// when no connection frees up within the acquire timeout, the handshake error when
    /// connecting fails, or `model.openai_responses.ws_serialize` (`Internal`). After the
    /// frame was written: `model.openai_responses.ws_send` (`Unavailable`, not retryable),
    /// because the server may have received the request.
    pub async fn send_request(
        &self,
        request: CreateResponse,
    ) -> Result<LlmResponseStream, AdkError> {
        let native_tools = request
            .tools
            .iter()
            .flatten()
            .any(|tool| !matches!(tool, Tool::Function(_) | Tool::Custom(_)));
        let frame = create_event(&request)?;

        let permit = tokio::time::timeout(
            self.acquire_timeout,
            Arc::clone(&self.pool.permits).acquire_owned(),
        )
        .await
        .map_err(|_| pool_timeout())?
        .map_err(|_| pool_timeout())?;
        let mut lease = Lease { socket: None, pool: Arc::clone(&self.pool), _permit: permit };
        while let Some(mut socket) = self.pool.take(request.previous_response_id.as_deref()) {
            if idle_and_open(&mut socket) {
                lease.socket = Some(socket);
                break;
            }
            self.pool.discard();
        }
        let socket = match lease.socket.take() {
            Some(socket) => socket,
            None => {
                let socket = self.handshake().await?;
                self.pool.open.fetch_add(1, Ordering::Relaxed);
                socket
            }
        };
        let socket = lease.socket.insert(socket);
        // From here the server may have the request: it is never sent again.
        if let Err(error) = socket.send(Message::Text(frame.into())).await {
            return Err(send_failed(&error));
        }

        let read_timeout = self.read_timeout;
        let stream = try_stream! {
            let mut lease = Some(lease);
            let mut state = StreamState::new(native_tools);
            while let Some(socket) = lease.as_mut().and_then(|lease| lease.socket.as_mut()) {
                let frame = tokio::time::timeout(read_timeout, socket.next())
                    .await
                    .map_err(|_| read_timed_out())?;
                let text = match frame {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(_) | Message::Frame(_))) => continue,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) | None => Err(closed_before_terminal())?,
                    Some(Err(error)) => Err(read_error(&error))?,
                };
                let value: Value = serde_json::from_str(text.as_str()).map_err(|error| {
                    parse::error(&error, text.as_str(), parse::Layer::StreamEvent)
                })?;
                if value.get("type").and_then(Value::as_str) == Some("error") {
                    // The connection is discarded: an error event does not say whether
                    // frames of this response are still in flight.
                    Err(error_event(&value))?;
                }
                // After a terminal event the connection is clean, whatever its outcome: it
                // goes back to the pool before the final chunk is yielded, so a caller that
                // sends another request while holding that chunk never waits on itself.
                if is_terminal(&value)
                    && let Some(lease) = lease.take()
                {
                    let id = value.pointer("/response/id").and_then(Value::as_str);
                    lease.release(id.map(str::to_string));
                }
                match wire::event(value.clone())? {
                    wire::Event::Terminal(response) => {
                        yield state.finish(&response)?;
                        break;
                    }
                    wire::Event::Content(event) => {
                        if let Some(response) = state.partial(*event) {
                            yield response;
                        }
                    }
                    // An event type this version does not know that carries a terminal
                    // response still ends the response.
                    wire::Event::Activity if lease.is_none() => {
                        let response = wire::response(
                            value.get("response").cloned().unwrap_or(Value::Null),
                            &value,
                            parse::Layer::StreamEvent,
                        )?;
                        yield state.finish(&response)?;
                        break;
                    }
                    wire::Event::Activity => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// Opens a connection, retrying transient failures and bounding each attempt.
    async fn handshake(&self) -> Result<WsStream, AdkError> {
        execute_with_retry(&self.retry_config, is_retryable_model_error, || async move {
            let mut request =
                self.url.as_str().into_client_request().map_err(|e| handshake_error(&e))?;
            request.headers_mut().extend(self.headers.iter().cloned());
            let (socket, _response) =
                tokio::time::timeout(self.connect_timeout, connect_async(request))
                    .await
                    .map_err(|_| connect_timed_out())?
                    .map_err(|e| handshake_error(&e))?;
            Ok(socket)
        })
        .await
    }
}

/// Serializes a request as a WebSocket-mode `response.create` event.
fn create_event(request: &CreateResponse) -> Result<String, AdkError> {
    let serialize_error = |message: String| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Internal,
            "model.openai_responses.ws_serialize",
            message,
        )
        .with_provider("openai-responses")
    };
    let mut event = serde_json::to_value(request)
        .map_err(|e| serialize_error(format!("Failed to serialize WebSocket request: {e}")))?;
    let Some(fields) = event.as_object_mut() else {
        return Err(serialize_error("WebSocket request is not a JSON object".into()));
    };
    fields.remove("stream");
    fields.remove("background");
    fields.insert("type".into(), Value::from("response.create"));
    Ok(event.to_string())
}

/// Whether an idle socket is still open with nothing left to read.
///
/// A Close frame, a read error, or a leftover data frame makes the socket unusable for
/// the next request.
fn idle_and_open(socket: &mut WsStream) -> bool {
    loop {
        match socket.next().now_or_never() {
            None => return true,
            Some(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Some(Some(Ok(
                Message::Text(_) | Message::Binary(_) | Message::Close(_) | Message::Frame(_),
            )))
            | Some(Some(Err(_)))
            | Some(None) => return false,
        }
    }
}

/// Whether a server event ends its response on the wire.
fn is_terminal(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.failed" | "response.cancelled" | "response.canceled")
    ) || matches!(
        value.pointer("/response/status").and_then(Value::as_str),
        Some("completed" | "incomplete" | "failed" | "cancelled")
    )
}

fn send_failed(error: &tungstenite::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        SEND_OUTCOME_UNKNOWN,
        format!("Failed to send WebSocket message; the server may have received it: {error}"),
    )
    .with_provider("openai-responses")
    .with_retry(RetryHint { should_retry: false, ..Default::default() })
}

fn pool_timeout() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.ws_pool_timeout",
        "every OpenAI Responses WebSocket connection stayed busy for the acquire timeout",
    )
    .with_provider("openai-responses")
}

fn connect_timed_out() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Timeout,
        "model.openai_responses.ws_connect",
        "OpenAI Responses WebSocket handshake did not finish within the connect timeout",
    )
    .with_provider("openai-responses")
}

fn read_timed_out() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Timeout,
        "model.openai_responses.ws_timeout",
        "OpenAI Responses WebSocket sent no event within the read timeout",
    )
    .with_provider("openai-responses")
    .with_retry(RetryHint { should_retry: false, ..Default::default() })
}

fn closed_before_terminal() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.unfinished_stream",
        "OpenAI Responses WebSocket closed before the response completed",
    )
    .with_provider("openai-responses")
}

fn read_error(error: &tungstenite::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.ws_read",
        format!("WebSocket read error: {error}"),
    )
    .with_provider("openai-responses")
}

/// Maps a WebSocket-mode `error` event.
///
/// Its `status` carries the HTTP status the request would have had; only documented codes
/// leave this boundary, because the message is provider text that can echo the request.
fn error_event(value: &Value) -> AdkError {
    let status = value.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok());
    let code =
        value.pointer("/error/code").or_else(|| value.get("code")).and_then(Value::as_str).filter(
            |code| {
                WEBSOCKET_ERROR_CODES.contains(code) || wire::DOCUMENTED_ERROR_CODES.contains(code)
            },
        );
    let category = match code {
        // A new connection is the documented remedy.
        Some("websocket_connection_limit_reached" | "server_error") => ErrorCategory::Unavailable,
        Some("rate_limit_exceeded") => ErrorCategory::RateLimited,
        Some("previous_response_not_found") => ErrorCategory::NotFound,
        _ => status.map_or(ErrorCategory::Internal, category_for_status_code),
    };
    let message = match code {
        Some(code) => format!("OpenAI Responses WebSocket error: {code}"),
        None => "OpenAI Responses WebSocket error".to_string(),
    };
    let error =
        AdkError::new(ErrorComponent::Model, category, "model.openai_responses.ws_error", message)
            .with_provider("openai-responses");
    crate::error_metadata::with_code(error, code)
}

/// Derive the WebSocket URL from the base HTTP URL.
///
/// Replaces `https://` with `wss://` (or `http://` with `ws://`) and
/// ensures the path ends with `/responses`.
fn derive_ws_url(base_url: &str) -> String {
    let ws_base = if base_url.starts_with("https://") {
        base_url.replacen("https://", "wss://", 1)
    } else if base_url.starts_with("http://") {
        base_url.replacen("http://", "ws://", 1)
    } else {
        format!("wss://{base_url}")
    };

    // Strip trailing slash and ensure /responses path
    let trimmed = ws_base.trim_end_matches('/');
    if trimmed.ends_with("/responses") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/responses")
    } else {
        format!("{trimmed}/v1/responses")
    }
}

/// Classifies a handshake failure so only transient failures are retried.
fn handshake_error(error: &tungstenite::Error) -> AdkError {
    let (category, code) = match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after);
            let error = AdkError::new(
                ErrorComponent::Model,
                category_for_status_code(status),
                "model.openai_responses.ws_handshake",
                format!("WebSocket handshake rejected with HTTP {status}"),
            )
            .with_provider("openai-responses")
            .with_upstream_status(status);
            return with_retry_after(error, retry_after);
        }
        tungstenite::Error::Url(_) | tungstenite::Error::HttpFormat(_) => {
            (ErrorCategory::InvalidInput, "model.openai_responses.ws_invalid_url")
        }
        tungstenite::Error::Io(_)
        | tungstenite::Error::Tls(_)
        | tungstenite::Error::Protocol(_)
        | tungstenite::Error::ConnectionClosed
        | tungstenite::Error::AlreadyClosed => {
            (ErrorCategory::Unavailable, "model.openai_responses.ws_connect")
        }
        tungstenite::Error::Capacity(_)
        | tungstenite::Error::WriteBufferFull(_)
        | tungstenite::Error::Utf8(_)
        | tungstenite::Error::AttackAttempt => {
            (ErrorCategory::Internal, "model.openai_responses.ws_connect")
        }
    };
    AdkError::new(
        ErrorComponent::Model,
        category,
        code,
        format!("WebSocket connection failed: {error}"),
    )
    .with_provider("openai-responses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::{CreateResponseArgs, InputParam};

    #[test]
    fn requests_are_response_create_events_without_http_fields() {
        let mut request = CreateResponseArgs::default()
            .model("gpt-5.6-terra")
            .input(InputParam::Text("hi".into()))
            .build()
            .unwrap();
        request.stream = Some(true);
        request.background = Some(false);
        let event: Value = serde_json::from_str(&create_event(&request).unwrap()).unwrap();
        assert_eq!(
            (
                &event["type"],
                &event["model"],
                &event["input"],
                event.get("stream"),
                event.get("background")
            ),
            (
                &Value::from("response.create"),
                &Value::from("gpt-5.6-terra"),
                &Value::from("hi"),
                None,
                None
            )
        );
    }

    #[test]
    fn terminal_events_are_recognized_by_type_or_response_status() {
        for (kind, status, terminal) in [
            ("response.completed", Some("completed"), true),
            ("response.incomplete", Some("incomplete"), true),
            ("response.failed", None, true),
            ("response.cancelled", None, true),
            ("response.expired", Some("cancelled"), true),
            ("response.in_progress", Some("in_progress"), false),
            ("response.output_text.delta", None, false),
        ] {
            let mut event = serde_json::json!({"type": kind});
            if let Some(status) = status {
                event["response"] = serde_json::json!({"status": status});
            }
            assert_eq!(is_terminal(&event), terminal, "{event}");
        }
    }

    #[test]
    fn error_events_are_categorized_without_provider_text() {
        let cases = [
            (
                serde_json::json!({"type": "error", "status": 429,
                "error": {"code": "rate_limit_exceeded", "message": "slow down: secret"}}),
                ErrorCategory::RateLimited,
                Some("rate_limit_exceeded"),
            ),
            (
                serde_json::json!({"type": "error", "status": 400,
                "error": {"code": "previous_response_not_found", "message": "resp_secret"}}),
                ErrorCategory::NotFound,
                Some("previous_response_not_found"),
            ),
            (
                serde_json::json!({"type": "error",
                "error": {"code": "websocket_connection_limit_reached"}}),
                ErrorCategory::Unavailable,
                Some("websocket_connection_limit_reached"),
            ),
            (
                serde_json::json!({"type": "error", "status": 401,
                "error": {"code": "user content echoed", "message": "secret"}}),
                ErrorCategory::Unauthorized,
                None,
            ),
            (
                serde_json::json!({"type": "error", "code": "server_error", "message": "secret"}),
                ErrorCategory::Unavailable,
                Some("server_error"),
            ),
        ];
        for (event, category, code) in cases {
            let error = error_event(&event);
            assert_eq!(
                (
                    error.category,
                    error.code,
                    error.details.metadata.get("provider_error_code").and_then(Value::as_str)
                ),
                (category, "model.openai_responses.ws_error", code),
                "{event}"
            );
            assert!(!error.message.contains("secret"), "{}", error.message);
        }
    }

    #[test]
    fn a_failed_send_is_never_retried() {
        let error = send_failed(&tungstenite::Error::ConnectionClosed);
        assert_eq!(
            (error.category, error.code, error.is_retryable()),
            (ErrorCategory::Unavailable, SEND_OUTCOME_UNKNOWN, false)
        );
        assert!(!read_timed_out().is_retryable());
        assert!(pool_timeout().is_retryable());
    }

    #[test]
    fn handshake_rejections_are_categorized_by_status() {
        let rejection = |status: u16| {
            let response =
                tungstenite::http::Response::builder().status(status).body(None).unwrap();
            handshake_error(&tungstenite::Error::Http(Box::new(response)))
        };
        assert_eq!(rejection(401).category, ErrorCategory::Unauthorized);
        assert!(!rejection(401).is_retryable());
        assert!(!rejection(400).is_retryable());
        assert!(rejection(429).is_retryable());
        assert!(rejection(503).is_retryable());
        assert_eq!(rejection(503).details.upstream_status_code, Some(503));
        let invalid = "not a url".into_client_request().expect_err("invalid URL");
        assert_eq!(handshake_error(&invalid).category, ErrorCategory::InvalidInput);
    }

    #[test]
    fn derive_ws_url_maps_schemes_and_paths() {
        for (base, expected) in [
            ("https://api.openai.com/v1", "wss://api.openai.com/v1/responses"),
            ("http://127.0.0.1:8080/v1/", "ws://127.0.0.1:8080/v1/responses"),
            ("https://proxy.example.com", "wss://proxy.example.com/v1/responses"),
        ] {
            assert_eq!(derive_ws_url(base), expected);
        }
    }
}
