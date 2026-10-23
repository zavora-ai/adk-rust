//! Responses WebSocket mechanics.
//! Request policy, authorization and retries belong to the caller.
use super::{
    config::OpenAIResponsesConfig,
    responses_client::{parse, wire},
    responses_stream::StreamState,
};
use crate::retry::{
    RetryConfig, category_for_status_code, execute_with_retry, is_retryable_model_error,
    parse_retry_after, with_retry_after,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, LlmResponseStream};
use async_openai::types::responses::CreateResponse;
use async_stream::try_stream;
use futures::{FutureExt, SinkExt, StreamExt};
use http::{HeaderMap, HeaderValue};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OnceCell, OwnedMutexGuard};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{self, Message, client::IntoClientRequest},
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
const WS_BETA: &str = "responses_websockets=2026-02-06";

/// Whether a pre-event failure establishes that this endpoint refuses WebSocket ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsRefusal {
    /// The endpoint rejected WebSocket ingress.
    Definitive,
    /// A temporary failure does not establish protocol support.
    Transient,
}

/// Transport observations contain no request or authorization material.
#[derive(Clone, Copy, Debug)]
pub enum TransportEvent {
    /// HTTP was selected without a WebSocket attempt.
    Http,
    /// A WebSocket request received its first server event.
    WebSocket {
        /// The handshake was reused from an earlier request or preconnection.
        reused: bool,
        /// The request used a verified connection-local continuation.
        incremental: bool,
    },
    /// A pre-event failure selected HTTP for this generation.
    Fallback {
        /// Whether this failure establishes a protocol refusal.
        refusal: WsRefusal,
    },
    /// The HTTP request selected after a pre-event failure completed.
    /// This is a completion observation, not another transport selection.
    HttpCompleted {
        /// The original pre-event failure classification.
        refusal: WsRefusal,
    },
}

/// Caller policy callbacks and bounded socket lifetime. No internal retry layer is added.
///
/// # Example
///
/// ```rust
/// use adk_model::openai::{WsOptions, WsTransport};
/// use std::time::Duration;
/// let options = WsOptions { idle_timeout: Duration::from_secs(60), ..Default::default() };
/// let transport = WsTransport::lazy("https://api.example.com/v1", "key", options);
/// assert!(!transport.is_connected());
/// ```
#[derive(Clone)]
pub struct WsOptions {
    /// TCP, TLS and upgrade deadline.
    pub connect_timeout: Duration,
    /// Maximum wait for the first provider event; control frames do not restart it.
    /// Defaults to 15 seconds. HTTP fallback can generate and bill twice if the
    /// server accepted the WebSocket request without delivering its first event.
    pub first_event_timeout: Duration,
    /// Allow `OpenAIResponsesClient` to fall back to HTTP before its first provider event.
    /// Defaults to false. Enabling this can generate and bill twice when the server
    /// accepted the WebSocket request but its first event did not reach the client.
    pub fallback_to_http: bool,
    /// Maximum interval without any WebSocket frame after the provider acknowledges the request.
    pub read_timeout: Duration,
    /// Idle lifetime checked at a request boundary.
    pub idle_timeout: Duration,
    /// Maximum connection lifetime checked at a request boundary.
    pub max_age: Duration,
    /// Lifetime of a preconnected socket that has not sent a request. The
    /// transport closes it after this interval without another call.
    pub preconnect_timeout: Duration,
    /// Caller-owned capability policy, evaluated before a request.
    pub enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Receives transport observations without request data.
    pub observe: Arc<dyn Fn(TransportEvent) + Send + Sync>,
}
impl Default for WsOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            first_event_timeout: Duration::from_secs(15),
            fallback_to_http: false,
            read_timeout: Duration::from_secs(300),
            idle_timeout: Duration::from_secs(240),
            max_age: Duration::from_secs(55 * 60),
            preconnect_timeout: Duration::from_secs(25),
            enabled: Arc::new(|| true),
            observe: Arc::new(|_| {}),
        }
    }
}

/// A failure before any server event permits explicitly opted-in HTTP fallback. The server may still
/// have accepted the original request; absence of an event is not proof of non-delivery.
pub(super) struct BeforeEvent {
    pub error: AdkError,
    pub refusal: WsRefusal,
}

#[derive(Clone)]
struct Continuation {
    request: Value,
    response: String,
    output: Vec<Value>,
}
struct Connection {
    socket: Option<Socket>,
    headers: HeaderMap,
    opened: Instant,
    used: Instant,
    first_sent: bool,
    continuation: Option<Continuation>,
    response_headers: Option<HeaderMap>,
    preconnect_failure: Option<(HeaderMap, WsRefusal)>,
    unused: Option<tokio::task::JoinHandle<()>>,
}
impl Connection {
    fn expired(&self, options: &WsOptions, now: Instant) -> bool {
        now.duration_since(self.used) >= options.idle_timeout
            || now.duration_since(self.opened) >= options.max_age
            || (!self.first_sent && now.duration_since(self.opened) >= options.preconnect_timeout)
    }
    fn close(&mut self) {
        self.socket = None;
        self.continuation = None;
        if let Some(unused) = self.unused.take() {
            unused.abort();
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
struct Lease {
    guard: OwnedMutexGuard<Connection>,
    connected: Arc<AtomicBool>,
    finished: bool,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished {
            self.guard.close();
        }
        self.guard.used = Instant::now();
        self.connected.store(self.guard.socket.is_some(), Ordering::Relaxed);
    }
}

/// One session-owned socket, leased until completion or cancellation.
pub struct WsTransport {
    connection: Arc<Mutex<Connection>>,
    url: String,
    key: String,
    pub(super) options: WsOptions,
    retry_config: RetryConfig,
    connected: Arc<AtomicBool>,
    tls: OnceCell<Arc<rustls::ClientConfig>>,
}
impl WsTransport {
    /// Build a transport without opening a connection or enabling internal retries.
    pub fn lazy(base: &str, key: &str, options: WsOptions) -> Self {
        let now = Instant::now();
        Self {
            connection: Arc::new(Mutex::new(Connection {
                socket: None,
                headers: HeaderMap::new(),
                opened: now,
                used: now,
                first_sent: false,
                continuation: None,
                response_headers: None,
                preconnect_failure: None,
                unused: None,
            })),
            url: derive_ws_url(base),
            key: key.into(),
            options,
            retry_config: RetryConfig::disabled(),
            connected: Arc::new(AtomicBool::new(false)),
            tls: OnceCell::new(),
        }
    }
    /// Open a connection using the standalone transport's default retry policy.
    pub async fn connect(config: &OpenAIResponsesConfig) -> Result<Self, AdkError> {
        Self::connect_with_retry_config(config, RetryConfig::default()).await
    }
    /// Open a connection with an explicit standalone handshake retry policy.
    pub async fn connect_with_retry_config(
        config: &OpenAIResponsesConfig,
        retry_config: RetryConfig,
    ) -> Result<Self, AdkError> {
        let mut transport = Self::lazy(
            config.base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
            &config.api_key,
            WsOptions::default(),
        );
        transport.retry_config = retry_config;
        transport.preconnect(HeaderMap::new()).await?;
        Ok(transport)
    }
    #[must_use]
    /// Replace the standalone handshake retry policy.
    pub fn with_retry_config(mut self, retry: RetryConfig) -> Self {
        self.retry_config = retry;
        self
    }
    /// Whether a socket is currently retained, including an active lease.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    pub(super) fn enabled(&self) -> bool {
        (self.options.enabled)() && !proxy_in_effect(&self.url)
    }
    pub(super) fn observe(&self, event: TransportEvent) {
        (self.options.observe)(event);
    }
    /// Close an unused socket at a request boundary; an active lease is never interrupted.
    pub fn expire(&self) {
        self.close_if(|connection| connection.expired(&self.options, Instant::now()));
    }
    /// Registry eviction closes an idle socket without interrupting an active lease.
    pub fn close_idle(&self) {
        self.close_if(|_| true);
    }
    fn close_if(&self, predicate: impl FnOnce(&Connection) -> bool) {
        if let Ok(mut guard) = self.connection.try_lock()
            && predicate(&guard)
        {
            guard.close();
            self.connected.store(false, Ordering::Relaxed);
        }
    }
    fn lease(&self, guard: OwnedMutexGuard<Connection>) -> Lease {
        Lease { guard, connected: self.connected.clone(), finished: false }
    }

    /// Handshake only. It never sends a model generation or a generate=false warmup.
    /// The transport closes the socket if no request uses it within
    /// [`WsOptions::preconnect_timeout`]; callers need no separate cleanup.
    pub async fn preconnect(&self, headers: HeaderMap) -> Result<(), AdkError> {
        self.preconnect_observed(self.authorization(headers)?, None).await
    }
    pub(super) async fn preconnect_observed(
        &self,
        headers: HeaderMap,
        observer: Option<super::ResponseObserver>,
    ) -> Result<(), AdkError> {
        if !self.enabled() {
            return Ok(());
        }
        let mut lease = self.lease(self.connection.clone().lock_owned().await);
        if let Err(failure) = self.ensure(&mut lease, headers.clone(), observer.as_ref()).await {
            lease.guard.preconnect_failure = Some((headers, failure.refusal));
            return Err(failure.error);
        }
        observe_headers(&mut lease, observer.as_ref());
        if !lease.guard.first_sent && lease.guard.unused.is_none() {
            let connection = Arc::downgrade(&self.connection);
            let connected = self.connected.clone();
            let opened = lease.guard.opened;
            let deadline = tokio::time::Instant::now() + self.options.preconnect_timeout;
            lease.guard.unused = Some(tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                if let Some(connection) = connection.upgrade()
                    && let Ok(mut guard) = connection.try_lock()
                    && guard.opened == opened
                    && !guard.first_sent
                {
                    guard.close();
                    connected.store(false, Ordering::Relaxed);
                }
            }));
        }
        lease.finished = true;
        Ok(())
    }
    async fn ensure(
        &self,
        lease: &mut Lease,
        headers: HeaderMap,
        observer: Option<&super::ResponseObserver>,
    ) -> Result<bool, BeforeEvent> {
        let headers =
            upgrade_headers(headers).map_err(|error| before(error, WsRefusal::Definitive))?;
        let now = Instant::now();
        let valid = lease.guard.headers == headers
            && !lease.guard.expired(&self.options, now)
            && lease.guard.socket.as_mut().is_some_and(idle_and_open);
        if valid {
            return Ok(true);
        }
        lease.guard.close();
        let mut request = self.url.as_str().into_client_request().map_err(classify)?;
        request.headers_mut().extend(headers.clone());
        let connector = if self.url.starts_with("wss:") {
            let config =
                self.tls.get_or_try_init(|| async { tls_config() }).await.map_err(|_| {
                    before(
                        AdkError::new(
                            ErrorComponent::Model,
                            ErrorCategory::Unavailable,
                            "model.openai_responses.ws_tls_config",
                            "Responses WebSocket TLS configuration failed",
                        )
                        .with_provider("openai-responses"),
                        WsRefusal::Transient,
                    )
                })?;
            Some(Connector::Rustls(config.clone()))
        } else {
            None
        };
        let (socket, response) =
            execute_with_retry(&self.retry_config, is_retryable_model_error, || {
                let request = request.clone();
                let connector = connector.clone();
                async move {
                    tokio::time::timeout(
                        self.options.connect_timeout,
                        connect_async_tls_with_config(request, None, false, connector),
                    )
                    .await
                    .map_err(|_| timeout())?
                    .map_err(|error| {
                        if let Some(observer) = observer
                            && let tungstenite::Error::Http(response) = &error
                        {
                            observer(response.headers());
                        }
                        classify(error).error
                    })
                }
            })
            .await
            .map_err(|error| {
                let refusal = refusal(&error);
                before(error, refusal)
            })?;
        lease.guard.socket = Some(socket);
        lease.guard.headers = headers;
        lease.guard.opened = Instant::now();
        lease.guard.used = lease.guard.opened;
        lease.guard.first_sent = false;
        self.connected.store(true, Ordering::Relaxed);
        // Handshake headers are surfaced through the client's observer at request setup.
        lease.guard.response_headers = Some(response.headers().clone());
        lease.guard.preconnect_failure = None;
        Ok(false)
    }
    async fn initial(&self, lease: &mut Lease) -> Result<Value, BeforeEvent> {
        tokio::time::timeout(
            self.options.first_event_timeout,
            read(lease, self.options.read_timeout),
        )
        .await
        .map_err(|_| before(delivery(timeout()), WsRefusal::Transient))?
        .map_err(|failure| before(delivery(failure.error), failure.refusal))
    }
    /// Send a typed request using the standalone transport API.
    pub async fn send_request(
        &self,
        request: CreateResponse,
    ) -> Result<LlmResponseStream, AdkError> {
        let body = serde_json::to_value(request).map_err(serialize)?;
        let native_tools = super::responses_convert::uses_native_tools(&body);
        self.send_adapted(
            body,
            self.authorization(HeaderMap::new())?,
            None,
            native_tools,
            (false, false),
        )
        .await
        .map_err(|failure| failure.error)
    }
    fn authorization(&self, mut headers: HeaderMap) -> Result<HeaderMap, AdkError> {
        if !headers.contains_key(http::header::AUTHORIZATION) && !self.key.is_empty() {
            headers.insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", self.key)).map_err(|_| {
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::InvalidInput,
                        "model.openai_responses.ws_invalid_api_key",
                        "Responses WebSocket authorization header is invalid",
                    )
                    .with_provider("openai-responses")
                })?,
            );
        }
        Ok(headers)
    }

    // Request-adapted headers are final; an adapter may intentionally remove authorization.
    pub(super) async fn send_adapted(
        &self,
        body: Value,
        headers: HeaderMap,
        observer: Option<super::ResponseObserver>,
        native_tools: bool,
        normalization: (bool, bool),
    ) -> Result<LlmResponseStream, BeforeEvent> {
        let mut lease = self.lease(self.connection.clone().lock_owned().await);
        if let Some((attempted, refusal)) = lease.guard.preconnect_failure.take()
            && attempted == headers
        {
            return Err(before(
                AdkError::new(
                    ErrorComponent::Model,
                    ErrorCategory::Unavailable,
                    "model.openai_responses.ws_preconnect",
                    "Responses WebSocket preconnection failed",
                )
                .with_provider("openai-responses"),
                refusal,
            ));
        }
        let handshake_headers = headers.clone();
        let mut reused = self.ensure(&mut lease, headers, observer.as_ref()).await?;
        observe_headers(&mut lease, observer.as_ref());
        let mut incremental = false;
        let frame = match lease
            .guard
            .continuation
            .as_ref()
            .and_then(|previous| incremental_event(&body, previous))
        {
            Some(frame) => {
                incremental = true;
                frame
            }
            None => create_event(&body).map_err(|error| before(error, WsRefusal::Definitive))?,
        };
        // A failed write can have partially delivered the frame. Do not reconnect
        // and replay it automatically, even when reusing an existing socket.
        send(&mut lease, &frame)
            .await
            .map_err(|error| before(delivery(error), WsRefusal::Transient))?;
        let mut first = self.initial(&mut lease).await?;
        if incremental && missing(&first) {
            lease.guard.continuation = None;
            incremental = false;
            if !lease.guard.socket.as_mut().is_some_and(idle_and_open) {
                lease.guard.socket = None;
                self.ensure(&mut lease, handshake_headers.clone(), observer.as_ref()).await?;
                observe_headers(&mut lease, observer.as_ref());
                reused = false;
            }
            let full = create_event(&body).map_err(|error| before(error, WsRefusal::Definitive))?;
            send(&mut lease, &full)
                .await
                .map_err(|error| before(delivery(error), WsRefusal::Transient))?;
            first = self.initial(&mut lease).await?;
        }
        if first.get("type").and_then(Value::as_str) == Some("error")
            && first.pointer("/error/code").or_else(|| first.get("code")).and_then(Value::as_str)
                == Some("websocket_connection_limit_reached")
        {
            return Err(before(error_event(&first), WsRefusal::Transient));
        }
        self.observe(TransportEvent::WebSocket { reused, incremental });
        let timeout = self.options.read_timeout;
        Ok(Box::pin(try_stream! {
            let mut lease = lease;
            let mut next = Some(first);
            let mut state = StreamState::new(native_tools);
            let mut normalized = super::responses_client::OpenResponsesStreamState::default();
            // Whether this request's response may still deliver events on the socket.
            let mut in_flight = false;
            loop {
                let value = match next.take() { Some(value) => value, None => read(&mut lease, timeout).await.map_err(|failure| delivery(failure.error))? };
                if value.get("type").and_then(Value::as_str) == Some("error") {
                    // A request-level error rejects the frame before any response
                    // starts, so the socket stays reusable. After a response has
                    // started, a later terminal event could reach the next request;
                    // the unfinished lease closes the socket instead.
                    lease.finished = !in_flight;
                    lease.guard.continuation = None;
                    Err(error_event(&value))?;
                }
                in_flight = true;
                let bytes = normalized.normalize_line(value.to_string().as_bytes(), normalization.0, normalization.1).map_err(|_| invalid())?;
                let value:Value = serde_json::from_slice(&bytes).map_err(|error| parse::error(&error, &value.to_string(), parse::Layer::StreamEvent))?;
                // A terminal provider outcome finishes the lease even when its
                // projection is an error. No frames from this response remain pending.
                lease.finished = is_terminal(&value);
                if lease.finished { lease.guard.continuation = None; }
                match wire::event(value.clone())? {
                    wire::Event::Terminal(response) => {
                        let result = state.finish(&response)?;
                        if let (Some(id), Some(output)) = (value.pointer("/response/id").and_then(Value::as_str), value.pointer("/response/output").and_then(Value::as_array)) {
                            lease.guard.continuation = Some(Continuation { request: body.clone(), response: id.into(), output: output.clone() });
                        }
                        lease.finished = true;
                        yield result;
                        break;
                    },
                    wire::Event::Content(event) => if let Some(response) = state.partial(*event) { yield response; },
                    wire::Event::Activity if is_terminal(&value) => {
                        let response = wire::response(value.get("response").cloned().unwrap_or(Value::Null), &value, parse::Layer::StreamEvent)?;
                        let result = state.finish(&response)?;
                        lease.finished = true;
                        yield result;
                        break;
                    },
                    wire::Event::Activity => {},
                }
            }
        }))
    }
}
fn observe_headers(lease: &mut Lease, observer: Option<&super::ResponseObserver>) {
    if let Some(observer) = observer
        && let Some(headers) = lease.guard.response_headers.take()
    {
        observer(&headers);
    }
}
// A provider event confirms that this generation may have started. Losing its
// socket is an uncertain delivery outcome, even before any text is emitted.
fn delivery(mut error: AdkError) -> AdkError {
    error.retry.should_retry = false;
    error
}
fn before(error: AdkError, refusal: WsRefusal) -> BeforeEvent {
    BeforeEvent { error, refusal }
}
fn create_event(body: &Value) -> Result<Value, AdkError> {
    let mut event = body.clone();
    let fields = event.as_object_mut().ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.openai_responses.ws_serialize",
            "Responses WebSocket request is not a JSON object",
        )
        .with_provider("openai-responses")
    })?;
    if fields.get("background").and_then(Value::as_bool) == Some(true) {
        return Err(AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "model.openai_responses.ws_serialize",
            "Responses WebSocket does not support background requests",
        )
        .with_provider("openai-responses"));
    }
    fields.remove("stream");
    // False is foreground; WebSocket has no background field.
    fields.remove("background");
    fields.insert("type".into(), Value::from("response.create"));
    Ok(event)
}
fn comparable(mut value: Value) -> Value {
    if let Some(fields) = value.as_object_mut() {
        for key in ["id", "status"] {
            fields.remove(key);
        }
        fields.retain(|_, value| !value.is_null());
        if fields.contains_key("role")
            && matches!(fields.get("type").and_then(Value::as_str), None | Some("message"))
        {
            fields.remove("type");
            if let Some(content) = fields.get_mut("content") {
                match content {
                    Value::String(text) => {
                        *content = serde_json::json!([{"type":"text","text":text}])
                    }
                    Value::Array(parts) => {
                        for part in parts {
                            if matches!(
                                part.get("type").and_then(Value::as_str),
                                Some("input_text" | "output_text")
                            ) {
                                *part = serde_json::json!({"type":"text","text":part.get("text").cloned().unwrap_or(Value::Null)});
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if fields.get("type").and_then(Value::as_str) == Some("function_call")
            && let Some(arguments) = fields.get_mut("arguments")
            && let Some(text) = arguments.as_str()
            && let Ok(parsed) = serde_json::from_str(text)
        {
            *arguments = parsed;
        }
    }
    value
}
fn incremental_event(body: &Value, previous: &Continuation) -> Option<Value> {
    if body.get("store") != Some(&Value::Bool(false)) || body.get("previous_response_id").is_some()
    {
        return None;
    }
    let input = body.get("input")?.as_array()?;
    let mut baseline = previous.request.get("input")?.as_array()?.clone();
    baseline.extend(previous.output.clone());
    if input.len() < baseline.len()
        || !input
            .iter()
            .zip(&baseline)
            .all(|(left, right)| comparable(left.clone()) == comparable(right.clone()))
    {
        return None;
    }
    let mut current = create_event(body).ok()?;
    let mut old = create_event(&previous.request).ok()?;
    current.as_object_mut()?.remove("input");
    old.as_object_mut()?.remove("input");
    if current != old {
        return None;
    }
    current["input"] = Value::Array(input[baseline.len()..].to_vec());
    current["previous_response_id"] = Value::String(previous.response.clone());
    Some(current)
}
async fn send(lease: &mut Lease, frame: &Value) -> Result<(), AdkError> {
    lease
        .guard
        .socket
        .as_mut()
        .ok_or_else(closed)?
        .send(Message::Text(frame.to_string().into()))
        .await
        .map_err(|error| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Unavailable,
                "model.openai_responses.ws_send",
                "Responses WebSocket send failed",
            )
            .with_provider("openai-responses")
            .with_source(error)
        })?;
    lease.guard.first_sent = true;
    if let Some(unused) = lease.guard.unused.take() {
        unused.abort();
    }
    Ok(())
}
async fn read(lease: &mut Lease, timeout: Duration) -> Result<Value, BeforeEvent> {
    loop {
        let socket =
            lease.guard.socket.as_mut().ok_or_else(|| before(closed(), WsRefusal::Transient))?;
        match tokio::time::timeout(timeout, socket.next())
            .await
            .map_err(|_| before(self::timeout(), WsRefusal::Transient))?
        {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|error| {
                    before(
                        parse::error(&error, &text, parse::Layer::StreamEvent),
                        WsRefusal::Transient,
                    )
                });
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(frame))) => {
                let code = frame.map(|frame| u16::from(frame.code));
                return Err(before(
                    closed(),
                    if matches!(code, Some(1003 | 1008)) {
                        WsRefusal::Definitive
                    } else {
                        WsRefusal::Transient
                    },
                ));
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                return Err(before(
                    AdkError::new(
                        ErrorComponent::Model,
                        ErrorCategory::Unavailable,
                        "model.openai_responses.ws_read",
                        "Responses WebSocket read failed",
                    )
                    .with_provider("openai-responses")
                    .with_source(error),
                    WsRefusal::Transient,
                ));
            }
            None => return Err(before(closed(), WsRefusal::Transient)),
        }
    }
}
fn idle_and_open(socket: &mut Socket) -> bool {
    loop {
        match socket.next().now_or_never() {
            None => return true,
            Some(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            _ => return false,
        }
    }
}
fn is_terminal(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.failed" | "response.cancelled" | "response.canceled")
    ) || matches!(
        value.pointer("/response/status").and_then(Value::as_str),
        Some("completed" | "incomplete" | "failed" | "cancelled")
    )
}
fn missing(value: &Value) -> bool {
    matches!(
        value.pointer("/error/code").or_else(|| value.get("code")).and_then(Value::as_str),
        Some("previous_response_not_found" | "invalid_stream_id")
    )
}
fn invalid() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.openai_responses.ws_request",
        "Responses WebSocket request is invalid",
    )
    .with_provider("openai-responses")
}
/// A typed request could not become a `response.create` frame.
pub(super) fn serialize(error: serde_json::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::InvalidInput,
        "model.openai_responses.ws_serialize",
        "Responses WebSocket request serialization failed",
    )
    .with_provider("openai-responses")
    .with_source(error)
}
fn timeout() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Timeout,
        "model.openai_responses.timeout",
        "Responses WebSocket timed out",
    )
    .with_provider("openai-responses")
}
fn closed() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.unfinished_stream",
        "Responses WebSocket closed before completion",
    )
    .with_provider("openai-responses")
}
const WEBSOCKET_ERROR_CODES: &[&str] = &[
    "previous_response_not_found",
    "invalid_stream_id",
    "websocket_connection_limit_reached",
    "websocket_stream_limit_reached",
];
fn error_event(value: &Value) -> AdkError {
    let code =
        value.pointer("/error/code").or_else(|| value.get("code")).and_then(Value::as_str).filter(
            |code| {
                WEBSOCKET_ERROR_CODES.contains(code) || wire::DOCUMENTED_ERROR_CODES.contains(code)
            },
        );
    let category = match code {
        Some("previous_response_not_found" | "invalid_stream_id") => ErrorCategory::NotFound,
        Some("websocket_connection_limit_reached" | "server_error") => ErrorCategory::Unavailable,
        Some("rate_limit_exceeded") => ErrorCategory::RateLimited,
        _ => value
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok())
            .map_or(ErrorCategory::Internal, category_for_status_code),
    };
    crate::error_metadata::with_code(
        AdkError::new(
            ErrorComponent::Model,
            category,
            "model.openai_responses.ws_error",
            "Responses WebSocket request failed",
        )
        .with_provider("openai-responses"),
        code,
    )
}
fn derive_ws_url(base: &str) -> String {
    let base = if base.contains("://") { base.to_owned() } else { format!("wss://{base}") };
    let Ok(mut url) = reqwest_openai::Url::parse(&base) else {
        return base;
    };
    match url.scheme() {
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "http" => {
            let _ = url.set_scheme("ws");
        }
        _ => {}
    }
    let path = url.path().trim_end_matches('/');
    let path = if path.is_empty() {
        "/v1/responses".to_owned()
    } else if path.ends_with("/responses") {
        path.to_owned()
    } else {
        format!("{path}/responses")
    };
    url.set_path(&path);
    url.to_string()
}
fn upgrade_headers(mut headers: HeaderMap) -> Result<HeaderMap, AdkError> {
    let values = headers.get_all("openai-beta");
    if !values.iter().any(|value| {
        value
            .as_bytes()
            .split(|byte| *byte == b',')
            .any(|token| token.trim_ascii() == WS_BETA.as_bytes())
    }) {
        let mut bytes = Vec::new();
        for value in values.iter() {
            if !bytes.is_empty() {
                bytes.push(b',');
            }
            bytes.extend_from_slice(value.as_bytes());
        }
        if !bytes.is_empty() {
            bytes.push(b',');
        }
        bytes.extend_from_slice(WS_BETA.as_bytes());
        let mut beta = HeaderValue::from_bytes(&bytes).map_err(|_| invalid())?;
        beta.set_sensitive(values.iter().any(HeaderValue::is_sensitive));
        headers.insert("openai-beta", beta);
    }
    // Content framing belongs to response.create, not the upgrade request.
    for name in ["content-type", "content-length", "accept"] {
        headers.remove(name);
    }
    Ok(headers)
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, ()> {
    use rustls_platform_verifier::BuilderVerifierExt;

    adk_core::ensure_crypto_provider();
    // Match reqwest's default verifier and respect a caller-installed crypto provider.
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| ())?
            .with_platform_verifier()
            .map_err(|_| ())?
            .with_no_client_auth(),
    ))
}
fn refusal(error: &AdkError) -> WsRefusal {
    if error.code == "model.openai_responses.ws_tls"
        || error.details.upstream_status_code.is_some_and(refused)
    {
        WsRefusal::Definitive
    } else {
        WsRefusal::Transient
    }
}
fn classify(error: tungstenite::Error) -> BeforeEvent {
    let certificate = match &error {
        tungstenite::Error::Io(error) => error
            .get_ref()
            .and_then(|source| source.downcast_ref::<rustls::Error>())
            .is_some_and(invalid_certificate),
        tungstenite::Error::Tls(tungstenite::error::TlsError::Rustls(error)) => {
            invalid_certificate(error)
        }
        tungstenite::Error::Tls(tungstenite::error::TlsError::InvalidDnsName) => true,
        _ => false,
    };
    if certificate {
        return before(
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Unsupported,
                "model.openai_responses.ws_tls",
                "Responses WebSocket TLS certificate verification failed",
            )
            .with_provider("openai-responses"),
            WsRefusal::Definitive,
        );
    }
    if let tungstenite::Error::Http(response) = error {
        let status = response.status().as_u16();
        let delay = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        return before(
            with_retry_after(
                AdkError::new(
                    ErrorComponent::Model,
                    category_for_status_code(status),
                    "model.openai_responses.ws_handshake",
                    format!("WebSocket handshake rejected with HTTP {status}"),
                )
                .with_provider("openai-responses")
                .with_upstream_status(status),
                delay,
            ),
            if refused(status) { WsRefusal::Definitive } else { WsRefusal::Transient },
        );
    }
    let (category, code) = match error {
        tungstenite::Error::Url(_) | tungstenite::Error::HttpFormat(_) => {
            (ErrorCategory::InvalidInput, "model.openai_responses.ws_invalid_url")
        }
        tungstenite::Error::Capacity(_)
        | tungstenite::Error::WriteBufferFull(_)
        | tungstenite::Error::Utf8(_)
        | tungstenite::Error::AttackAttempt => {
            (ErrorCategory::Internal, "model.openai_responses.ws_connect")
        }
        _ => (ErrorCategory::Unavailable, "model.openai_responses.ws_connect"),
    };
    before(
        AdkError::new(
            ErrorComponent::Model,
            category,
            code,
            "Responses WebSocket connection failed",
        )
        .with_provider("openai-responses"),
        WsRefusal::Transient,
    )
}
fn refused(status: u16) -> bool {
    matches!(status, 400 | 401 | 403 | 404 | 405 | 426 | 501 | 200..=399)
}
fn invalid_certificate(error: &rustls::Error) -> bool {
    matches!(error, rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented)
}
fn proxy_in_effect(url: &str) -> bool {
    uses_proxy(url, &hyper_util::client::proxy::matcher::Matcher::from_system())
}
fn uses_proxy(url: &str, matcher: &hyper_util::client::proxy::matcher::Matcher) -> bool {
    // Match reqwest's environment/system policy, including NO_PROXY. WebSocket
    // itself does not implement proxy tunnelling, so a matched proxy selects HTTP.
    let url = url.replacen("wss://", "https://", 1).replacen("ws://", "http://", 1);
    url.parse::<http::Uri>().is_ok_and(|uri| matcher.intercept(&uri).is_some())
}

#[cfg(test)]
mod tests;
