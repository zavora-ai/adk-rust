//! WebSocket transport for the OpenAI Responses API.
//!
//! Provides a persistent WebSocket connection to `/v1/responses` (WebSocket mode) for
//! lower-latency agentic workflows. Each request is sent as a `response.create` event and
//! its server events are read with the same projection and stream recovery as the HTTP
//! SSE path, so a WebSocket stream ends with the same outcomes as an HTTP stream.
//!
//! Gated behind the `openai-ws` feature flag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, LlmResponseStream};
use async_openai::types::responses::{CreateResponse, Tool};
use async_stream::try_stream;
use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        self, Message,
        client::IntoClientRequest,
        http::{HeaderValue, header},
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

/// Persistent WebSocket connection to the OpenAI Responses API.
///
/// Maintains a single WebSocket connection that is reused across
/// [`send_request`](Self::send_request) calls, reducing per-request connection overhead
/// for latency-sensitive agentic workflows.
///
/// The connection carries one response at a time: a request waits until the previous
/// response stream has ended or been dropped. A stream dropped before its terminal event
/// discards the connection, so the rest of that response never reaches the next request;
/// the next request opens a new connection. A connection the server closed while idle is
/// replaced the same way. A response that continues with `previous_response_id` on a new
/// connection needs `store = true`, because the connection-local cache of an unstored
/// response is lost with the old connection.
///
/// Handshakes are retried under the [`RetryConfig`] for transient failures (network
/// errors, HTTP 408, 429, and 5xx); an invalid URL or an HTTP 4xx rejection such as 401
/// fails at once.
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
    /// The connection, held by the response stream that is reading from it.
    connection: Arc<Mutex<Connection>>,
    /// Connection URL (derived from base_url).
    url: String,
    /// API key for reconnection.
    api_key: String,
    /// Retry configuration for handshakes.
    retry_config: RetryConfig,
    /// Whether an open connection is held.
    connected: Arc<AtomicBool>,
}

struct Connection {
    socket: Option<WsStream>,
}

/// Exclusive use of the connection for one response.
///
/// Dropped before the response's terminal event, it discards the socket: frames of that
/// response may still be in flight.
struct Lease {
    guard: OwnedMutexGuard<Connection>,
    connected: Arc<AtomicBool>,
    finished: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.finished {
            self.guard.socket = None;
        }
        self.connected.store(self.guard.socket.is_some(), Ordering::Relaxed);
    }
}

impl WsTransport {
    /// Establish a new WebSocket connection to the OpenAI Responses API.
    ///
    /// The WebSocket URL is derived from the configured base URL by replacing
    /// `https://` with `wss://` and appending `/v1/responses`. The API key is
    /// sent as a bearer token in the handshake headers. Transient handshake failures
    /// are retried under [`RetryConfig::default`]; use
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
    /// `retry_config`, which also governs later reconnections.
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
        let base_url = config.base_url.as_deref().unwrap_or("https://api.openai.com/v1");
        let url = derive_ws_url(base_url);
        let socket = handshake(&url, &config.api_key, &retry_config).await?;
        Ok(Self {
            connection: Arc::new(Mutex::new(Connection { socket: Some(socket) })),
            url,
            api_key: config.api_key.clone(),
            retry_config,
            connected: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Set the retry configuration for reconnection handshakes.
    #[must_use]
    pub fn with_retry_config(mut self, retry_config: RetryConfig) -> Self {
        self.retry_config = retry_config;
        self
    }

    /// Returns whether the transport holds an open WebSocket connection.
    ///
    /// `false` after the server closed the connection or a stream was dropped before its
    /// terminal event; the next [`send_request`](Self::send_request) reconnects.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Send a request over the WebSocket and return a response stream.
    ///
    /// The request is sent as a `response.create` event; `stream` and `background` are
    /// HTTP transport fields and are not sent. The stream yields partial deltas and then
    /// one final chunk carrying the rest of the response, exactly as the HTTP SSE path
    /// does, and ends after the response's terminal event.
    ///
    /// The call waits while a previous response stream from this transport is still
    /// open. It reconnects first when the connection was closed or abandoned, and once
    /// more if sending on a reused connection fails.
    ///
    /// The stream fails with:
    /// - `model.openai_responses.unfinished_stream` (`Unavailable`) when the connection
    ///   closes before a terminal event;
    /// - `model.openai_responses.response_failed` for `response.failed`;
    /// - `model.openai_responses.ws_error` for an `error` event, categorized by its status;
    /// - `model.openai_responses.incomplete` or `model.openai_responses.cancelled` for a
    ///   response that ended incomplete (other than at `max_output_tokens` or by the
    ///   content filter, which end with a finish reason) or cancelled;
    /// - `model.openai_responses.parse` when an event does not match the Responses schema.
    ///
    /// # Errors
    ///
    /// Returns `AdkError` with category `Unavailable` if the request cannot be sent after
    /// reconnecting, or the handshake error when reconnecting fails.
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

        let mut lease = Lease {
            guard: Arc::clone(&self.connection).lock_owned().await,
            connected: Arc::clone(&self.connected),
            finished: true,
        };
        if !lease.guard.socket.as_mut().is_some_and(idle_and_open) {
            lease.guard.socket = None;
        }
        // A failed send delivers no complete frame, so the request goes once more over a
        // new connection.
        let mut sent = match lease.guard.socket.as_mut() {
            Some(socket) => send_frame(socket, &frame).await,
            None => Err(not_connected()),
        };
        if sent.is_err() {
            lease.guard.socket = None;
            let socket = lease
                .guard
                .socket
                .insert(handshake(&self.url, &self.api_key, &self.retry_config).await?);
            sent = send_frame(socket, &frame).await;
        }
        if let Err(error) = sent {
            lease.guard.socket = None;
            return Err(error);
        }
        self.connected.store(true, Ordering::Relaxed);
        lease.finished = false;

        let stream = try_stream! {
            let mut lease = lease;
            let mut state = StreamState::new(native_tools);
            loop {
                let frame = match lease.guard.socket.as_mut() {
                    Some(socket) => socket.next().await,
                    None => None,
                };
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
                // After a terminal event the connection is clean, whatever its outcome.
                lease.finished = is_terminal(&value);
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
                    wire::Event::Activity if lease.finished => {
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

async fn send_frame(socket: &mut WsStream, frame: &str) -> Result<(), AdkError> {
    socket.send(Message::Text(frame.into())).await.map_err(|e| {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.openai_responses.ws_send",
            format!("Failed to send WebSocket message: {e}"),
        )
        .with_provider("openai-responses")
    })
}

fn not_connected() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unavailable,
        "model.openai_responses.ws_send",
        "WebSocket connection is closed",
    )
    .with_provider("openai-responses")
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

/// Opens a WebSocket connection with bearer authentication, retrying transient failures.
async fn handshake(
    url: &str,
    api_key: &str,
    retry_config: &RetryConfig,
) -> Result<WsStream, AdkError> {
    execute_with_retry(retry_config, is_retryable_model_error, || async move {
        let mut request = url.into_client_request().map_err(|e| handshake_error(&e))?;
        let token = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|_| {
            AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::InvalidInput,
                "model.openai_responses.ws_invalid_api_key",
                "OpenAI API key is not a valid HTTP header value",
            )
            .with_provider("openai-responses")
        })?;
        request.headers_mut().insert(header::AUTHORIZATION, token);
        let (socket, _response) = connect_async(request).await.map_err(|e| handshake_error(&e))?;
        Ok(socket)
    })
    .await
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
