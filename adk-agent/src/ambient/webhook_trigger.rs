use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::event_source::{EventSource, TriggerEvent};

/// Decides whether an inbound webhook request is allowed to trigger agent work.
///
/// A webhook that reaches an agent is a remote code path into application logic, so the
/// decision needs to be explicit. Implement this to check a provider signature, validate a
/// bearer token, or consult an allowlist, and return the principal the request speaks for.
///
/// # Example
///
/// ```rust
/// use adk_agent::ambient::{WebhookRequest, WebhookVerifier};
///
/// #[derive(Debug)]
/// struct SharedToken(String);
///
/// impl WebhookVerifier for SharedToken {
///     fn verify(&self, request: &WebhookRequest<'_>) -> Result<String, String> {
///         match request.header("authorization") {
///             Some(value) if value == format!("Bearer {}", self.0) => Ok("ci".to_string()),
///             Some(_) => Err("token mismatch".to_string()),
///             None => Err("missing authorization header".to_string()),
///         }
///     }
/// }
/// ```
pub trait WebhookVerifier: Send + Sync + std::fmt::Debug {
    /// Returns the verified principal, or the reason the request is rejected.
    ///
    /// The reason is logged, never returned to the caller: a rejected request receives a
    /// bare `401` so it cannot be used to probe which part of a credential was wrong.
    fn verify(&self, request: &WebhookRequest<'_>) -> std::result::Result<String, String>;
}

/// An inbound webhook request, as presented to a [`WebhookVerifier`].
pub struct WebhookRequest<'a> {
    headers: &'a axum::http::HeaderMap,
    body: &'a [u8],
}

impl<'a> WebhookRequest<'a> {
    /// A header value as UTF-8, or `None` when absent or not UTF-8.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The raw request body.
    ///
    /// Signature schemes are computed over the exact bytes received, so this is deliberately
    /// the unparsed body.
    pub fn body(&self) -> &'a [u8] {
        self.body
    }
}

impl std::fmt::Debug for WebhookRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Headers and body carry credentials and caller data; report only shape.
        f.debug_struct("WebhookRequest")
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Emits trigger events when authorized HTTP POST requests arrive on the configured path.
///
/// Binds loopback by default. Serving a non-loopback address requires a
/// [`WebhookVerifier`], because a reachable unauthenticated webhook lets any caller start
/// application-defined agent work.
///
/// # Example
///
/// ```rust
/// use adk_agent::ambient::WebhookTrigger;
///
/// // Local development: loopback, no verifier required.
/// let trigger = WebhookTrigger::new(8080, "/webhook");
/// assert!(trigger.bind_address().ip().is_loopback());
/// ```
pub struct WebhookTrigger {
    path: String,
    name: String,
    bind_address: SocketAddr,
    verifier: Option<Arc<dyn WebhookVerifier>>,
    accept_non_json: bool,
    max_body_bytes: usize,
}

/// Bodies above this size are rejected before parsing.
const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

impl WebhookTrigger {
    /// Creates a webhook trigger on loopback at the given port and path.
    ///
    /// The path is prefixed with `/` when it is not already.
    pub fn new(port: u16, path: &str) -> Self {
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };

        Self {
            name: format!("webhook:{path}"),
            path,
            // Loopback, not `0.0.0.0`: binding every interface is a decision the caller
            // should have to make, alongside supplying a verifier.
            bind_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            verifier: None,
            accept_non_json: false,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    /// Serves on `address` instead of loopback.
    ///
    /// A non-loopback address requires [`WebhookTrigger::with_verifier`]; without one,
    /// [`EventSource::subscribe`] fails rather than exposing an open trigger.
    pub fn with_bind_address(mut self, address: SocketAddr) -> Self {
        self.bind_address = address;
        self
    }

    /// Requires every request to be authorized by `verifier`.
    pub fn with_verifier(mut self, verifier: Arc<dyn WebhookVerifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Accepts bodies that are not valid JSON, wrapping them as a JSON string.
    ///
    /// Off by default. Coercing a malformed body into a string produces a trigger event
    /// indistinguishable from a deliberate one, so unparseable input is rejected with `400`
    /// unless a caller opts in.
    pub fn accept_non_json(mut self) -> Self {
        self.accept_non_json = true;
        self
    }

    /// Sets the largest accepted body, in bytes. Defaults to 1 MiB.
    pub fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }

    /// The address this trigger serves on.
    pub fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }
}

#[async_trait]
impl EventSource for WebhookTrigger {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        // Fail closed. An exposed webhook with no verifier is a remote trigger for anyone
        // who can reach the port, and refusing at subscribe time is the only point where
        // the mistake is still cheap.
        if !self.bind_address.ip().is_loopback() && self.verifier.is_none() {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "agent.ambient.webhook_unauthenticated",
                format!(
                    "WebhookTrigger is set to serve {}, which is reachable beyond this host, \
                     with no verifier. Any caller able to reach the port could start agent \
                     work. Call `with_verifier`, or leave the default loopback bind.",
                    self.bind_address
                ),
            ));
        }

        let (tx, mut rx) = mpsc::channel::<TriggerEvent>(256);
        let source_name = self.name.clone();
        let path = self.path.clone();
        let bind_address = self.bind_address;
        let verifier = self.verifier.clone();
        let accept_non_json = self.accept_non_json;
        let max_body_bytes = self.max_body_bytes;

        // Bind before spawning so a port conflict is reported to the caller instead of
        // vanishing into a background log line.
        let listener = tokio::net::TcpListener::bind(bind_address).await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::Unavailable,
                "agent.ambient.webhook_bind_failed",
                format!("WebhookTrigger could not bind {bind_address}: {e}"),
            )
        })?;

        // The server's lifetime is tied to the stream the caller holds. Dropping the stream
        // fires this token, `axum` shuts down gracefully, and the port is released — before,
        // the server stayed bound after the consumer went away, so a restart on the same
        // port failed while requests kept being accepted and discarded.
        let shutdown = CancellationToken::new();
        let server_shutdown = shutdown.clone();

        tokio::spawn(async move {
            use axum::Router;
            use axum::body::Bytes;
            use axum::http::{HeaderMap, StatusCode};
            use axum::routing::post;

            let app = Router::new().route(
                &path,
                post(move |headers: HeaderMap, body: Bytes| {
                    let tx = tx.clone();
                    let source = source_name.clone();
                    let verifier = verifier.clone();
                    async move {
                        if body.len() > max_body_bytes {
                            tracing::warn!(
                                body.bytes = body.len(),
                                limit = max_body_bytes,
                                "webhook body rejected as oversized"
                            );
                            return StatusCode::PAYLOAD_TOO_LARGE;
                        }

                        let principal = match &verifier {
                            Some(verifier) => {
                                let request =
                                    WebhookRequest { headers: &headers, body: body.as_ref() };
                                match verifier.verify(&request) {
                                    Ok(principal) => Some(principal),
                                    Err(reason) => {
                                        // Logged, not returned: the response must not reveal
                                        // which part of a credential failed.
                                        tracing::warn!(
                                            reason = %reason,
                                            "webhook request rejected by verifier"
                                        );
                                        return StatusCode::UNAUTHORIZED;
                                    }
                                }
                            }
                            None => None,
                        };

                        let payload = match serde_json::from_slice::<serde_json::Value>(&body) {
                            Ok(value) => value,
                            Err(_) if accept_non_json => serde_json::Value::String(
                                String::from_utf8_lossy(&body).to_string(),
                            ),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "webhook body rejected as malformed JSON"
                                );
                                return StatusCode::BAD_REQUEST;
                            }
                        };

                        let event = TriggerEvent { source, payload, principal };

                        if tx.send(event).await.is_err() {
                            tracing::debug!("webhook subscriber dropped, refusing the request");
                            return StatusCode::SERVICE_UNAVAILABLE;
                        }

                        StatusCode::OK
                    }
                }),
            );

            tracing::info!(address = %bind_address, path = %path, "webhook trigger listening");

            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
                .await;

            if let Err(e) = served {
                tracing::warn!(error = %e, "webhook trigger server error");
            }
            tracing::debug!(address = %bind_address, "webhook trigger stopped");
        });

        // Cancels on drop, which is what releases the port.
        let guard = shutdown.drop_guard();
        let stream = async_stream::stream! {
            let _guard = guard;
            while let Some(event) = rx.recv().await {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for WebhookTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookTrigger")
            .field("bind_address", &self.bind_address)
            .field("path", &self.path)
            .field("verifier", &self.verifier)
            .field("accept_non_json", &self.accept_non_json)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish()
    }
}
