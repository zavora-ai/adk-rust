//! Tasks a server keeps for calls that outlive their request (SEP-2663), and the client end of
//! a request.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use rmcp::RoleServer;
use rmcp::model::{
    DetailedTask, ElicitRequestParams, ElicitResult, InputResponses, ProgressNotificationParam,
    ProgressToken,
};
use rmcp::service::Peer;

use super::McpCaller;

/// Follows the tasks an [`McpCallExecutor`](super::McpCallExecutor) starts with
/// [`McpToolOutcome::Task`](super::McpToolOutcome::Task) (SEP-2663).
///
/// Setting one with [`McpServerBuilder::task_host`](super::McpServerBuilder::task_host)
/// declares the `io.modelcontextprotocol/tasks` extension and routes `tasks/get`,
/// `tasks/update` and `tasks/cancel` here, with the caller the router resolved. A task belongs
/// to the caller that started it: a host answers another caller's task id as unknown, with a
/// `NotFound` error, which reaches the client as JSON-RPC `-32602`.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
/// use adk_tool::mcp::rmcp::model::{DetailedTask, InputResponses, Task, TaskPayload, TaskStatus};
/// use adk_tool::mcp::server::{McpCaller, McpTaskHost};
///
/// /// Every task is still working.
/// struct AlwaysWorking;
///
/// #[async_trait::async_trait]
/// impl McpTaskHost for AlwaysWorking {
///     async fn get(&self, task_id: &str, _caller: &McpCaller) -> Result<DetailedTask> {
///         let at = "2026-11-17T09:00:00Z";
///         let task = Task::new(task_id, TaskStatus::Working, at, at);
///         Ok(DetailedTask::new(task, TaskPayload::Working))
///     }
///
///     async fn update(&self, _: &str, _: &McpCaller, _: InputResponses) -> Result<()> {
///         Err(AdkError::new(
///             ErrorComponent::Tool,
///             ErrorCategory::InvalidInput,
///             "example.no_input",
///             "this task asks nothing",
///         ))
///     }
///
///     async fn cancel(&self, _: &str, _: &McpCaller) -> Result<()> {
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait McpTaskHost: Send + Sync {
    /// The task `task_id` as it stands, for `tasks/get`.
    ///
    /// # Errors
    ///
    /// Returns a `NotFound` error for a task that does not exist or is not `caller`'s.
    async fn get(&self, task_id: &str, caller: &McpCaller) -> Result<DetailedTask>;

    /// Takes `responses` to the task's outstanding input requests, for `tasks/update`.
    ///
    /// # Errors
    ///
    /// Returns a `NotFound` error for an unknown task, and an `InvalidInput` error for an answer
    /// to a request the task is not waiting on, such as one already answered.
    async fn update(
        &self,
        task_id: &str,
        caller: &McpCaller,
        responses: InputResponses,
    ) -> Result<()>;

    /// Asks the task to stop, for `tasks/cancel`. The status may lag the acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a `NotFound` error for an unknown task, and an `InvalidInput` error for one that
    /// already finished.
    async fn cancel(&self, task_id: &str, caller: &McpCaller) -> Result<()>;
}

/// The client end of one `tools/call`: progress notifications for it, and elicitation on the
/// live connection of a client before MCP `2026-07-28`.
///
/// Progress reaches the client only while it listens on the request's stream, or on the
/// session's stream for a stateful session; a notification nobody receives is dropped.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::server::McpRequestPeer;
///
/// async fn report(peer: &McpRequestPeer) {
///     peer.progress("calling send_email").await;
/// }
/// ```
#[derive(Clone)]
pub struct McpRequestPeer {
    peer: Peer<RoleServer>,
    progress_token: Option<ProgressToken>,
    sent: Arc<AtomicU64>,
    can_elicit: bool,
}

impl std::fmt::Debug for McpRequestPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRequestPeer")
            .field("progress", &self.progress_token.is_some())
            .field("can_elicit", &self.can_elicit)
            .finish_non_exhaustive()
    }
}

impl McpRequestPeer {
    pub(crate) fn new(
        peer: Peer<RoleServer>,
        progress_token: Option<ProgressToken>,
        can_elicit: bool,
    ) -> Self {
        Self { peer, progress_token, sent: Arc::new(AtomicU64::new(0)), can_elicit }
    }

    /// Whether the client asked for progress with a `progressToken`.
    pub fn wants_progress(&self) -> bool {
        self.progress_token.is_some()
    }

    /// Sends `notifications/progress` with `message`, counting up from 1, when the client asked
    /// for progress. Returns whether the notification was sent.
    pub async fn progress(&self, message: impl Into<String>) -> bool {
        let Some(token) = &self.progress_token else {
            return false;
        };
        let progress = self.sent.fetch_add(1, Ordering::SeqCst) + 1;
        let params = ProgressNotificationParam::new(token.clone(), progress as f64)
            .with_message(message.into());
        self.peer.notify_progress(params).await.is_ok()
    }

    /// Whether [`elicit`](Self::elicit) can ask the client: a client before MCP `2026-07-28`
    /// that declared form elicitation. A later client is asked with an `input_required` result
    /// instead.
    pub fn can_elicit(&self) -> bool {
        self.can_elicit
    }

    /// Asks the client with `elicitation/create` on the live connection and waits for the
    /// answer.
    ///
    /// # Errors
    ///
    /// Returns an `Unsupported` error when the client cannot be asked this way, and an
    /// `Unavailable` error when the request fails or the connection closes.
    pub async fn elicit(&self, params: ElicitRequestParams) -> Result<ElicitResult> {
        if !self.can_elicit {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unsupported,
                "mcp.server.elicitation_unsupported",
                "the client declared no form elicitation on this connection",
            ));
        }
        self.peer.create_elicitation(params).await.map_err(|error| {
            AdkError::unavailable(
                ErrorComponent::Tool,
                "mcp.server.elicitation_failed",
                format!("the client did not answer the elicitation: {error}"),
            )
        })
    }
}
