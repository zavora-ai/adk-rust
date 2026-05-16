//! Stdio transport using newline-delimited JSON on stdin/stdout.
//!
//! Each protocol message is a single JSON object followed by `\n`.
//! Supports multiple sequential sessions over the same connection.
//!
//! # Protocol Messages
//!
//! Incoming (client → server):
//! - `{"method": "initialize", "params": {"protocol_version": "1.0"}}`
//! - `{"method": "session/create", "params": {}}`
//! - `{"method": "session/prompt", "params": {"session_id": "...", "text": "..."}}`
//! - `{"method": "session/close", "params": {"session_id": "..."}}`
//! - `{"method": "permission/respond", "params": {"function_call_id": "...", "approved": true}}`
//!
//! Outgoing (server → client):
//! - JSON responses and notifications, one per line.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::super::capabilities::{AgentCapabilities, CapabilitiesBuilder};
use super::super::config::AcpServerConfig;
use super::super::error::AcpServerError;
use super::super::handler::AcpSessionHandler;
use super::Transport;

/// Stdio transport for local IDE connections.
///
/// Reads newline-delimited JSON from stdin, routes messages to the handler,
/// and writes JSON responses to stdout.
pub struct StdioTransport {
    capabilities: AgentCapabilities,
}

impl StdioTransport {
    /// Create a new stdio transport with capabilities derived from config.
    pub fn new(config: &AcpServerConfig) -> Self {
        Self { capabilities: CapabilitiesBuilder::build(config) }
    }
}

/// An incoming protocol message from the client.
#[derive(Debug, Deserialize)]
struct IncomingMessage {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// A response sent back to the client.
#[derive(Debug, Serialize)]
struct OutgoingResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<super::super::error::ErrorResponse>,
}

#[async_trait]
impl Transport for StdioTransport {
    async fn serve(
        &self,
        handler: Arc<AcpSessionHandler>,
        shutdown: CancellationToken,
    ) -> Result<(), AcpServerError> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        info!("stdio transport started, waiting for messages");

        loop {
            line.clear();

            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("stdio transport shutting down");
                    break;
                }
                result = reader.read_line(&mut line) => {
                    match result {
                        Ok(0) => {
                            // EOF — stdin closed
                            info!("stdin closed, stopping transport");
                            break;
                        }
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            let response = match serde_json::from_str::<IncomingMessage>(trimmed) {
                                Ok(msg) => self.handle_message(msg, &handler).await,
                                Err(e) => {
                                    warn!(error = %e, "malformed message received");
                                    OutgoingResponse {
                                        result: None,
                                        error: Some(AcpServerError::MalformedMessage(e.to_string()).to_error_response()),
                                    }
                                }
                            };

                            let json = serde_json::to_string(&response)
                                .map_err(|e| AcpServerError::Transport(format!("serialization failed: {e}")))?;

                            stdout.write_all(json.as_bytes()).await
                                .map_err(|e| AcpServerError::Transport(format!("stdout write failed: {e}")))?;
                            stdout.write_all(b"\n").await
                                .map_err(|e| AcpServerError::Transport(format!("stdout write failed: {e}")))?;
                            stdout.flush().await
                                .map_err(|e| AcpServerError::Transport(format!("stdout flush failed: {e}")))?;
                        }
                        Err(e) => {
                            warn!(error = %e, "stdin read error");
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl StdioTransport {
    async fn handle_message(
        &self,
        msg: IncomingMessage,
        handler: &Arc<AcpSessionHandler>,
    ) -> OutgoingResponse {
        match msg.method.as_str() {
            "initialize" => self.handle_initialize(&msg.params),
            "session/create" => self.handle_session_create(handler).await,
            "session/prompt" => self.handle_session_prompt(handler, &msg.params).await,
            "session/close" => self.handle_session_close(handler, &msg.params).await,
            other => OutgoingResponse {
                result: None,
                error: Some(
                    AcpServerError::MalformedMessage(format!("unknown method: {other}"))
                        .to_error_response(),
                ),
            },
        }
    }

    fn handle_initialize(&self, params: &serde_json::Value) -> OutgoingResponse {
        let version = params.get("protocol_version").and_then(|v| v.as_str()).unwrap_or("1.0");

        if version != "1.0" {
            return OutgoingResponse {
                result: None,
                error: Some(
                    AcpServerError::UnsupportedVersion {
                        requested: version.to_string(),
                        supported: vec!["1.0".to_string()],
                    }
                    .to_error_response(),
                ),
            };
        }

        OutgoingResponse {
            result: Some(serde_json::json!({
                "protocol_version": "1.0",
                "capabilities": self.capabilities,
            })),
            error: None,
        }
    }

    async fn handle_session_create(&self, handler: &Arc<AcpSessionHandler>) -> OutgoingResponse {
        match handler.create_session().await {
            Ok(session_id) => OutgoingResponse {
                result: Some(serde_json::json!({ "session_id": session_id })),
                error: None,
            },
            Err(e) => OutgoingResponse { result: None, error: Some(e.to_error_response()) },
        }
    }

    async fn handle_session_prompt(
        &self,
        handler: &Arc<AcpSessionHandler>,
        params: &serde_json::Value,
    ) -> OutgoingResponse {
        let session_id = match params.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => {
                return OutgoingResponse {
                    result: None,
                    error: Some(
                        AcpServerError::MalformedMessage("missing session_id".to_string())
                            .to_error_response(),
                    ),
                };
            }
        };

        let text = match params.get("text").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => {
                return OutgoingResponse {
                    result: None,
                    error: Some(
                        AcpServerError::MalformedMessage("missing text".to_string())
                            .to_error_response(),
                    ),
                };
            }
        };

        match handler.handle_prompt(session_id, text).await {
            Ok(notifications) => OutgoingResponse {
                result: Some(serde_json::json!({ "notifications": notifications })),
                error: None,
            },
            Err(e) => OutgoingResponse { result: None, error: Some(e.to_error_response()) },
        }
    }

    async fn handle_session_close(
        &self,
        handler: &Arc<AcpSessionHandler>,
        params: &serde_json::Value,
    ) -> OutgoingResponse {
        let session_id = match params.get("session_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => {
                return OutgoingResponse {
                    result: None,
                    error: Some(
                        AcpServerError::MalformedMessage("missing session_id".to_string())
                            .to_error_response(),
                    ),
                };
            }
        };

        match handler.close_session(session_id).await {
            Ok(()) => {
                OutgoingResponse { result: Some(serde_json::json!({ "ok": true })), error: None }
            }
            Err(e) => OutgoingResponse { result: None, error: Some(e.to_error_response()) },
        }
    }
}
