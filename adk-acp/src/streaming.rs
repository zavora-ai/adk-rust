//! Streaming output from ACP agent sessions.
//!
//! Instead of collecting the full response into a string, streaming mode
//! yields chunks as they arrive from the agent — enabling real-time display
//! and lower time-to-first-token.

use std::sync::Arc;

use agent_client_protocol::v1::schema::{
    AvailableCommand, CompactionUpdate, ContentBlock, NoticeSeverity, Plan, PlanRemoved,
    PlanUpdate, SessionConfigOption, SessionId, SessionInfoUpdate, SessionMessage,
    SessionMessageChunk, SessionUpdate, StopReason, SubagentUpdate, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolKind, Usage,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::client::{AcpConnection, Observer, RoutedUpdate};
use crate::connection::AcpAgentConfig;
use crate::error::Result;
use crate::permissions::PermissionPolicy;
use crate::status::{AgentStatus, StatusTracker};

/// A chunk of output from the ACP agent.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputChunk {
    /// A text chunk from the agent's response.
    Text(String),
    /// The agent is thinking (internal reasoning, not shown to user by default).
    Thought(String),
    /// A chunk of a user message, as the agent replays history on `session/load`.
    UserText(String),
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
    /// An update on the status or results of a previously initiated tool call.
    ///
    /// Surfaces the External_Agent's `SessionUpdate::ToolCallUpdate`. The `id`
    /// correlates this update back to its originating [`OutputChunk::ToolCall`].
    ToolUpdate {
        /// The tool-call identifier this update correlates to.
        id: String,
        /// Execution status if reported (`pending`, `in_progress`, `completed`, `failed`).
        status: Option<String>,
        /// Tool kind if reported (`read`, `edit`, `execute`, ...).
        kind: Option<String>,
        /// Updated human-readable title if reported.
        title: Option<String>,
        /// Human-readable text summary extracted from the update content, if any.
        content: Option<String>,
        /// Absolute file paths the tool reported affecting.
        locations: Vec<String>,
    },
    /// A usage update reporting context-window consumption and (optionally) cost.
    ///
    /// Surfaces the External_Agent's `SessionUpdate::UsageUpdate`.
    Usage {
        /// Tokens currently in context.
        used: u64,
        /// Total context window size in tokens.
        size: u64,
        /// Cumulative session cost amount, if reported.
        cost: Option<f64>,
        /// ISO 4217 currency code accompanying `cost`, if reported.
        currency: Option<String>,
    },
    /// The agent's full execution plan, replacing any earlier one.
    Plan(Plan),
    /// One plan created or changed (unstable plan operations).
    PlanUpdate(PlanUpdate),
    /// One plan removed (unstable plan operations).
    PlanRemoved(PlanRemoved),
    /// Information for the user that is not part of the session history.
    Notice {
        /// How serious the notice is.
        severity: NoticeSeverity,
        /// Short summary.
        title: String,
        /// Optional detail.
        description: Option<String>,
    },
    /// A context compaction was created or changed.
    Compaction(CompactionUpdate),
    /// Text appended to a context compaction's retained summary.
    CompactionSummary {
        /// The compaction the text belongs to.
        compaction_id: String,
        /// The appended text.
        text: String,
    },
    /// The session switched to another mode.
    ModeChanged(String),
    /// The session's configuration options, with their current values.
    ConfigOptions(Vec<SessionConfigOption>),
    /// The slash commands the agent offers in the session.
    AvailableCommands(Vec<AvailableCommand>),
    /// The session's title or last-update time changed.
    SessionInfo(SessionInfoUpdate),
    /// A child session the agent created for this session, or a change to it
    /// (unstable subagents).
    Subagent(SubagentUpdate),
    /// A message sent to or received from another session (unstable subagents).
    SessionMessage(SessionMessage),
    /// Content appended to a session message (unstable subagents).
    SessionMessageChunk(SessionMessageChunk),
    /// Output of a subagent session, addressed to the child's session id.
    SubagentOutput {
        /// The subagent's session id.
        session_id: String,
        /// What the subagent produced.
        chunk: Box<OutputChunk>,
    },
    /// The agent requested permission (informational — decision already made by policy).
    PermissionRequested {
        /// What the agent wanted to do.
        title: String,
        /// Whether it was approved.
        approved: bool,
    },
    /// The agent finished responding.
    Done {
        /// Why the agent ended the turn.
        stop_reason: StopReason,
        /// The tokens the turn used, when the agent reported them.
        usage: Option<Usage>,
    },
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
///         OutputChunk::Done { .. } => break,
///         _ => {}
///     }
/// }
/// ```
pub type OutputStream = mpsc::Receiver<OutputChunk>;

/// Send a prompt and stream the response chunks.
///
/// Returns a receiver that yields [`OutputChunk`]s as they arrive, ending with
/// [`OutputChunk::Done`] (or [`OutputChunk::Error`]). The agent process is
/// terminated when the stream completes.
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
    let config = config.clone();

    status.set(AgentStatus::Starting);

    tokio::spawn(async move {
        let observer = Observer { status: status.clone(), events: chunk_tx.clone() };
        let outcome = async {
            let connection =
                AcpConnection::open_agent(agent, config, policy, Some(observer)).await?;
            status.set(AgentStatus::Running);
            let result = async {
                let mut session = connection.new_session().await?;
                // Forward each chunk as it arrives: a bounded channel would stall the
                // prompt loop, so chunks go through an unbounded hop.
                let (forward_tx, mut forward_rx) = mpsc::unbounded_channel::<OutputChunk>();
                let forwarder = {
                    let chunk_tx = chunk_tx.clone();
                    tokio::spawn(async move {
                        while let Some(chunk) = forward_rx.recv().await {
                            if chunk_tx.send(chunk).await.is_err() {
                                break;
                            }
                        }
                    })
                };
                let reply = session
                    .prompt_blocks(vec![ContentBlock::from(prompt_text)], move |chunk| {
                        let _ = forward_tx.send(chunk);
                    })
                    .await;
                let _ = forwarder.await;
                let _ = session.close().await;
                reply
            }
            .await;
            let _ = connection.close().await;
            result
        }
        .await;

        match outcome {
            Ok(reply) => {
                status.set(AgentStatus::Idle);
                let _ = chunk_tx
                    .send(OutputChunk::Done { stop_reason: reply.stop_reason, usage: reply.usage })
                    .await;
            }
            Err(error) => {
                warn!(%error, "streaming ACP session ended with error");
                let _ = chunk_tx.send(OutputChunk::Error(error.to_string())).await;
            }
        }
        status.set(AgentStatus::Stopped);
    });

    Ok(chunk_rx)
}

/// Map an update routed to `session_id`'s stream to an [`OutputChunk`],
/// wrapping a subagent's output in [`OutputChunk::SubagentOutput`].
pub(crate) fn map_routed_update(
    session_id: &SessionId,
    routed: RoutedUpdate,
) -> Option<OutputChunk> {
    let chunk = map_update(routed.update)?;
    if routed.session_id == *session_id {
        Some(chunk)
    } else {
        Some(OutputChunk::SubagentOutput {
            session_id: routed.session_id.to_string(),
            chunk: Box::new(chunk),
        })
    }
}

/// Map an incoming ACP [`SessionUpdate`] from an External_Agent to an
/// [`OutputChunk`] for the client's streaming surface.
///
/// This is a pure function so the mapping can be unit-tested without spawning a
/// subprocess or driving a live connection. Returns `None` for updates that
/// have no client-facing representation.
///
/// The text paths (`AgentMessageChunk` → [`OutputChunk::Text`],
/// `AgentThoughtChunk` → [`OutputChunk::Thought`]) are preserved exactly as they
/// were before richer updates were surfaced (see design property P12).
pub fn map_update(update: SessionUpdate) -> Option<OutputChunk> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text_content) => {
                Some(OutputChunk::Text(text_content.text.to_string()))
            }
            _ => None,
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match chunk.content {
            ContentBlock::Text(text_content) => {
                Some(OutputChunk::Thought(text_content.text.to_string()))
            }
            _ => None,
        },
        SessionUpdate::UserMessageChunk(chunk) => match chunk.content {
            ContentBlock::Text(text_content) => Some(OutputChunk::UserText(text_content.text)),
            _ => None,
        },
        SessionUpdate::ToolCall(tool_call) => {
            Some(OutputChunk::ToolCall { title: tool_call.title.to_string() })
        }
        SessionUpdate::ToolCallUpdate(update) => Some(map_tool_call_update(update)),
        SessionUpdate::UsageUpdate(usage) => Some(OutputChunk::Usage {
            used: usage.used,
            size: usage.size,
            cost: usage.cost.as_ref().map(|cost| cost.amount),
            currency: usage.cost.as_ref().map(|cost| cost.currency.clone()),
        }),
        SessionUpdate::Plan(plan) => Some(OutputChunk::Plan(plan)),
        SessionUpdate::PlanUpdate(update) => Some(OutputChunk::PlanUpdate(update)),
        SessionUpdate::PlanRemoved(removed) => Some(OutputChunk::PlanRemoved(removed)),
        SessionUpdate::Notice(notice) => Some(OutputChunk::Notice {
            severity: notice.severity,
            title: notice.title,
            description: notice.description,
        }),
        SessionUpdate::CompactionUpdate(update) => Some(OutputChunk::Compaction(update)),
        SessionUpdate::CompactionSummaryChunk(chunk) => match chunk.content {
            ContentBlock::Text(text_content) => Some(OutputChunk::CompactionSummary {
                compaction_id: chunk.compaction_id.to_string(),
                text: text_content.text,
            }),
            _ => None,
        },
        SessionUpdate::CurrentModeUpdate(update) => {
            Some(OutputChunk::ModeChanged(update.current_mode_id.to_string()))
        }
        SessionUpdate::ConfigOptionUpdate(update) => {
            Some(OutputChunk::ConfigOptions(update.config_options))
        }
        SessionUpdate::AvailableCommandsUpdate(update) => {
            Some(OutputChunk::AvailableCommands(update.available_commands))
        }
        SessionUpdate::SessionInfoUpdate(update) => Some(OutputChunk::SessionInfo(update)),
        SessionUpdate::SubagentUpdate(update) => Some(OutputChunk::Subagent(update)),
        SessionUpdate::SessionMessage(message) => Some(OutputChunk::SessionMessage(message)),
        SessionUpdate::SessionMessageChunk(chunk) => Some(OutputChunk::SessionMessageChunk(chunk)),
        _ => None,
    }
}

/// Build an [`OutputChunk::ToolUpdate`] from an ACP [`ToolCallUpdate`],
/// surfacing the status, kind, title, extracted content text, and affected
/// file locations while preserving the tool-call id correlation.
fn map_tool_call_update(update: ToolCallUpdate) -> OutputChunk {
    let ToolCallUpdate { tool_call_id, fields, .. } = update;

    let locations = fields
        .locations
        .unwrap_or_default()
        .into_iter()
        .map(|location| location.path.display().to_string())
        .collect();

    OutputChunk::ToolUpdate {
        id: tool_call_id.to_string(),
        status: fields.status.map(tool_status_str).map(str::to_string),
        kind: fields.kind.map(tool_kind_str).map(str::to_string),
        title: fields.title,
        content: extract_tool_content_text(fields.content.as_deref()),
        locations,
    }
}

/// Extract a human-readable text summary from tool-call update content blocks.
///
/// Joins the text of any standard text content blocks. Non-text content
/// (diffs, terminals, images) is ignored for the summary.
fn extract_tool_content_text(content: Option<&[ToolCallContent]>) -> Option<String> {
    let content = content?;
    let mut parts = Vec::new();
    for item in content {
        if let ToolCallContent::Content(block) = item
            && let ContentBlock::Text(text_content) = &block.content
        {
            parts.push(text_content.text.to_string());
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("\n")) }
}

/// Stable wire string for a [`ToolCallStatus`].
fn tool_status_str(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        _ => "unknown",
    }
}

/// Stable wire string for a [`ToolKind`].
fn tool_kind_str(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        ToolKind::Other => "other",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::v1::schema::{
        ContentChunk, Cost, ToolCall, ToolCallContent, ToolCallLocation, ToolCallUpdate,
        ToolCallUpdateFields, UsageUpdate,
    };

    /// Every stable and unstable v1 session update adk-acp advertises reaches the
    /// stream as its own chunk.
    #[test]
    fn session_updates_beyond_text_and_tools_are_surfaced() {
        use agent_client_protocol::v1::schema::{
            AvailableCommand, AvailableCommandsUpdate, CompactionStatus, CompactionSummaryChunk,
            CompactionUpdate, ConfigOptionUpdate, CurrentModeUpdate, Notice, Plan, PlanEntry,
            PlanEntryPriority, PlanEntryStatus, PlanRemoved, SessionConfigOption,
            SessionInfoUpdate, SubagentUpdate,
        };

        let plan = Plan::new(vec![PlanEntry::new(
            "write tests",
            PlanEntryPriority::High,
            PlanEntryStatus::Pending,
        )]);
        assert!(matches!(map_update(SessionUpdate::Plan(plan)), Some(OutputChunk::Plan(_))));
        assert!(matches!(
            map_update(SessionUpdate::PlanRemoved(PlanRemoved::new("plan-1"))),
            Some(OutputChunk::PlanRemoved(_))
        ));
        assert!(matches!(
            map_update(SessionUpdate::Notice(Notice::new(NoticeSeverity::Info, "fyi").description("more".to_string()))),
            Some(OutputChunk::Notice { severity: NoticeSeverity::Info, title, description: Some(description) })
                if title == "fyi" && description == "more"
        ));
        assert!(matches!(
            map_update(SessionUpdate::CompactionUpdate(CompactionUpdate::new(
                "c",
                CompactionStatus::Failed
            ))),
            Some(OutputChunk::Compaction(_))
        ));
        assert!(matches!(
            map_update(SessionUpdate::CompactionSummaryChunk(CompactionSummaryChunk::new(
                "c",
                ContentBlock::from("kept")
            ))),
            Some(OutputChunk::CompactionSummary { compaction_id, text }) if compaction_id == "c" && text == "kept"
        ));
        assert!(matches!(
            map_update(SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new("code"))),
            Some(OutputChunk::ModeChanged(mode)) if mode == "code"
        ));
        assert!(matches!(
            map_update(SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(vec![
                SessionConfigOption::boolean("verbose", "Verbose", true)
            ]))),
            Some(OutputChunk::ConfigOptions(options)) if options.len() == 1
        ));
        assert!(matches!(
            map_update(SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
                AvailableCommand::new("review", "Review")
            ]))),
            Some(OutputChunk::AvailableCommands(commands)) if commands.len() == 1
        ));
        assert!(matches!(
            map_update(SessionUpdate::SessionInfoUpdate(
                SessionInfoUpdate::new().title("t".to_string())
            )),
            Some(OutputChunk::SessionInfo(_))
        ));
        assert!(matches!(
            map_update(SessionUpdate::SubagentUpdate(SubagentUpdate::new("child"))),
            Some(OutputChunk::Subagent(_))
        ));
        assert!(matches!(
            map_update(SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::from("hi")))),
            Some(OutputChunk::UserText(text)) if text == "hi"
        ));
    }

    /// A subagent's output is wrapped with the child's session id; the
    /// session's own output is not.
    #[test]
    fn routed_updates_from_subagents_are_wrapped() {
        let own = SessionId::new("parent");
        let text = |value: &str| {
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(value)))
        };
        let mine =
            map_routed_update(&own, RoutedUpdate { session_id: own.clone(), update: text("a") });
        assert!(matches!(mine, Some(OutputChunk::Text(text)) if text == "a"));
        let child = map_routed_update(
            &own,
            RoutedUpdate { session_id: SessionId::new("child"), update: text("b") },
        );
        assert!(matches!(
            child,
            Some(OutputChunk::SubagentOutput { session_id, chunk })
                if session_id == "child" && matches!(chunk.as_ref(), OutputChunk::Text(text) if text == "b")
        ));
    }

    /// P12: agent message text must be surfaced exactly as before — the raw
    /// text payload of an `AgentMessageChunk`, byte-for-byte.
    #[test]
    fn agent_message_chunk_surfaces_text_unchanged() {
        let update =
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from("hello world")));

        match map_update(update) {
            Some(OutputChunk::Text(text)) => assert_eq!(text, "hello world"),
            other => panic!("expected Text chunk, got {other:?}"),
        }
    }

    /// Thought chunks continue to surface as before.
    #[test]
    fn agent_thought_chunk_surfaces_thought() {
        let update =
            SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::from("let me think")));

        match map_update(update) {
            Some(OutputChunk::Thought(text)) => assert_eq!(text, "let me think"),
            other => panic!("expected Thought chunk, got {other:?}"),
        }
    }

    /// A non-text agent message block produces no chunk (unchanged behavior).
    #[test]
    fn non_text_agent_message_is_ignored() {
        use agent_client_protocol::v1::schema::ImageContent;
        let update = SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Image(
            ImageContent::new("AAAA", "image/png"),
        )));

        assert!(map_update(update).is_none());
    }

    /// Tool calls continue to surface their title.
    #[test]
    fn tool_call_surfaces_title() {
        let update = SessionUpdate::ToolCall(ToolCall::new("tc-1", "Reading app.rs"));

        match map_update(update) {
            Some(OutputChunk::ToolCall { title }) => assert_eq!(title, "Reading app.rs"),
            other => panic!("expected ToolCall chunk, got {other:?}"),
        }
    }

    /// Tool-call updates now surface status, kind, title, content, and
    /// locations, preserving the tool-call id correlation.
    #[test]
    fn tool_call_update_surfaces_status_and_summary() {
        let fields = ToolCallUpdateFields::new()
            .status(ToolCallStatus::Completed)
            .kind(ToolKind::Edit)
            .title("Edited app.rs".to_string())
            .content(vec![ToolCallContent::from(ContentBlock::from("done"))])
            .locations(vec![ToolCallLocation::new("/tmp/app.rs")]);
        let update = SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("tc-1", fields));

        match map_update(update) {
            Some(OutputChunk::ToolUpdate { id, status, kind, title, content, locations }) => {
                assert_eq!(id, "tc-1");
                assert_eq!(status.as_deref(), Some("completed"));
                assert_eq!(kind.as_deref(), Some("edit"));
                assert_eq!(title.as_deref(), Some("Edited app.rs"));
                assert_eq!(content.as_deref(), Some("done"));
                assert_eq!(locations, vec!["/tmp/app.rs".to_string()]);
            }
            other => panic!("expected ToolUpdate chunk, got {other:?}"),
        }
    }

    /// A minimal tool-call update (id only) still surfaces with empty optionals.
    #[test]
    fn tool_call_update_minimal_surfaces_id() {
        let update =
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new("tc-9", ToolCallUpdateFields::new()));

        match map_update(update) {
            Some(OutputChunk::ToolUpdate { id, status, kind, title, content, locations }) => {
                assert_eq!(id, "tc-9");
                assert!(status.is_none());
                assert!(kind.is_none());
                assert!(title.is_none());
                assert!(content.is_none());
                assert!(locations.is_empty());
            }
            other => panic!("expected ToolUpdate chunk, got {other:?}"),
        }
    }

    /// Usage updates surface token counts and cost when present.
    #[test]
    fn usage_update_surfaces_tokens_and_cost() {
        let update = SessionUpdate::UsageUpdate(
            UsageUpdate::new(53_000, 200_000).cost(Cost::new(0.045, "USD")),
        );

        match map_update(update) {
            Some(OutputChunk::Usage { used, size, cost, currency }) => {
                assert_eq!(used, 53_000);
                assert_eq!(size, 200_000);
                assert_eq!(cost, Some(0.045));
                assert_eq!(currency.as_deref(), Some("USD"));
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }
    }

    /// Usage updates without cost surface token counts and no cost.
    #[test]
    fn usage_update_without_cost() {
        let update = SessionUpdate::UsageUpdate(UsageUpdate::new(10, 100));

        match map_update(update) {
            Some(OutputChunk::Usage { used, size, cost, currency }) => {
                assert_eq!(used, 10);
                assert_eq!(size, 100);
                assert!(cost.is_none());
                assert!(currency.is_none());
            }
            other => panic!("expected Usage chunk, got {other:?}"),
        }
    }
}
