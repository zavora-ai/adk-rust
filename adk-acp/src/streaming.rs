//! Streaming output from ACP agent sessions.
//!
//! Instead of collecting the full response into a string, streaming mode
//! yields chunks as they arrive from the agent — enabling real-time display
//! and lower time-to-first-token.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification, SessionUpdate,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::connection::AcpAgentConfig;
use crate::error::Result;
use crate::host::{AcpHostHandler, capabilities};
use crate::permissions::{PermissionPolicy, PermissionRequest, outcome_for};
use crate::status::{AgentStatus, StatusTracker};

/// A chunk of output from the ACP agent.
#[derive(Debug, Clone)]
pub enum OutputChunk {
    /// A text chunk from the agent's response.
    Text(String),
    /// The agent is thinking (internal reasoning, not shown to user by default).
    Thought(String),
    /// A tool call was initiated (e.g., "Creating file app.rs").
    ToolCall {
        /// Human-readable title of the operation.
        title: String,
    },
    /// A tool call completed.
    ToolCallComplete {
        /// Human-readable title.
        title: String,
    },
    /// The agent requested permission (informational — decision already made by policy).
    PermissionRequested {
        /// What the agent wanted to do.
        title: String,
        /// Whether it was approved.
        approved: bool,
    },
    /// The agent finished responding.
    Done,
    /// An error occurred.
    Error(String),
}

/// A streaming receiver for ACP agent output.
///
/// Yields [`OutputChunk`]s as they arrive from the agent.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::streaming::stream_prompt;
///
/// let mut stream = stream_prompt(&config, "Write a hello world", policy, status).await?;
/// while let Some(chunk) = stream.recv().await {
///     match chunk {
///         OutputChunk::Text(t) => print!("{t}"),
///         OutputChunk::ToolCall { title } => println!("\n[tool] {title}"),
///         OutputChunk::Done => break,
///         _ => {}
///     }
/// }
/// ```
pub type OutputStream = mpsc::Receiver<OutputChunk>;

/// Send a prompt and stream the response chunks.
///
/// Returns a receiver that yields [`OutputChunk`]s as they arrive.
/// The agent process is terminated when the stream completes.
pub async fn stream_prompt(
    config: &AcpAgentConfig,
    prompt: &str,
    policy: Arc<PermissionPolicy>,
    status: StatusTracker,
) -> Result<OutputStream> {
    info!(command = %config.command, "starting streaming ACP prompt");

    let agent = crate::connection::build_agent(config)?;

    let (chunk_tx, chunk_rx) = mpsc::channel::<OutputChunk>(64);
    let prompt_text = prompt.to_string();
    let working_dir = config.working_dir.clone();
    let mcp_servers = config.mcp_servers.clone();
    let filesystem = config.filesystem.clone();
    let terminal = config.terminal.clone();
    let client_capabilities = capabilities(filesystem.as_ref(), terminal.as_ref());

    status.set(AgentStatus::Starting);

    tokio::spawn(async move {
        let chunk_tx_err = chunk_tx.clone();
        let status_inner = status.clone();
        let policy_clone = policy.clone();
        let chunk_tx_perm = chunk_tx.clone();

        let outcome = Client
            .builder()
            .on_receive_notification(
                {
                    let tx = chunk_tx.clone();
                    async move |notif: SessionNotification, _cx: ConnectionTo<Agent>| {
                        match notif.update {
                            SessionUpdate::AgentMessageChunk(chunk) => {
                                if let ContentBlock::Text(text_content) = chunk.content {
                                    let _ = tx
                                        .send(OutputChunk::Text(text_content.text.to_string()))
                                        .await;
                                }
                            }
                            SessionUpdate::AgentThoughtChunk(chunk) => {
                                if let ContentBlock::Text(text_content) = chunk.content {
                                    let _ = tx
                                        .send(OutputChunk::Thought(text_content.text.to_string()))
                                        .await;
                                }
                            }
                            SessionUpdate::ToolCall(tool_call) => {
                                let _ = tx
                                    .send(OutputChunk::ToolCall {
                                        title: tool_call.title.to_string(),
                                    })
                                    .await;
                            }
                            _ => {}
                        }
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let status = status_inner.clone();
                    move |request: RequestPermissionRequest,
                          responder: Responder<RequestPermissionResponse>,
                          cx: ConnectionTo<Agent>| {
                        let status = status.clone();
                        let policy = policy_clone.clone();
                        let permission_tx = chunk_tx_perm.clone();
                        async move {
                            status.set(AgentStatus::WaitingPermission);
                            cx.spawn(async move {
                            let cancellation = responder.cancellation();
                            let perm_request = PermissionRequest::from_acp(&request);
                            let outcome = tokio::select! {
                                decision = policy.decide(&perm_request) => {
                                    outcome_for(&perm_request, &decision)
                                }
                                _ = cancellation.cancelled() => RequestPermissionOutcome::Cancelled,
                            };
                            let approved = match &outcome {
                                RequestPermissionOutcome::Selected(selected) => perm_request
                                    .options
                                    .iter()
                                    .find(|option| option.id == selected.option_id.to_string())
                                    .is_some_and(|option| {
                                        matches!(
                                            option.kind,
                                            PermissionOptionKind::AllowOnce
                                                | PermissionOptionKind::AllowAlways
                                        )
                                    }),
                                RequestPermissionOutcome::Cancelled => false,
                                _ => false,
                            };

                            let _ = permission_tx
                                .send(OutputChunk::PermissionRequested {
                                    title: perm_request.title.clone(),
                                    approved,
                                })
                                .await;
                            status.set(AgentStatus::Running);
                            responder.respond(RequestPermissionResponse::new(outcome))
                        })?;
                            Ok(())
                        }
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .with_handler(AcpHostHandler::new(filesystem, terminal))
            .connect_with(agent, {
                let status = status_inner.clone();
                let tx = chunk_tx.clone();
                |connection: ConnectionTo<Agent>| async move {
                    status.set(AgentStatus::Starting);

                    let initialization = connection
                        .send_request(
                            InitializeRequest::new(ProtocolVersion::V1)
                                .client_capabilities(client_capabilities),
                        )
                        .block_task()
                        .await?;
                    crate::connection::validate_initialization(&initialization, &mcp_servers)?;

                    status.set(AgentStatus::Running);

                    connection
                        .build_session_from(
                            NewSessionRequest::new(&working_dir).mcp_servers(mcp_servers),
                        )
                        .block_task()
                        .run_until(async |mut session| {
                            session.send_prompt(&prompt_text)?;
                            // read_to_string collects internally; notifications stream via callback
                            let _ = session.read_to_string().await?;
                            let _ = tx.send(OutputChunk::Done).await;
                            Ok(())
                        })
                        .await?;

                    status.set(AgentStatus::Idle);
                    Ok(())
                }
            })
            .await;

        if let Err(e) = outcome {
            warn!(error = %e, "streaming ACP session ended with error");
            let _ = chunk_tx_err.send(OutputChunk::Error(e.to_string())).await;
        }

        status_inner.set(AgentStatus::Stopped);
    });

    Ok(chunk_rx)
}
