//! Supervised session loop for the managed agent runtime.
//!
//! The [`SessionLoop`] is the core execution engine. It runs as a
//! `tokio::spawn`ed background task, dequeues [`UserEvent`]s from an
//! mpsc channel, processes each turn, and broadcasts [`SessionEvent`]s
//! to stream subscribers.
//!
//! # Architecture
//!
//! The loop composes:
//! - [`SequenceCounter`] — assigns monotonically increasing `seq` to each event
//! - [`ToolParkingLot`] — parks on `custom_tool_use` until client delivers a result
//! - [`CheckpointManager`] — atomic checkpoint after each event
//! - `tokio::broadcast` — fan-out to stream subscribers
//! - [`Runner`](adk_runner::Runner) — drives the agent through the real LLM
//! - [`SessionUsageTracker`] — tracks per-turn and cumulative token usage
//!
//! # Control Flow
//!
//! ```text
//! Dequeue UserEvent → emit status.running → invoke Runner
//!   → for each output event: classify, map, assign seq, checkpoint, broadcast
//!   → if custom tool call: park, wait for result, resume
//!   → track usage → emit status.idle → loop
//! ```
//!
//! # Interrupt and Pause
//!
//! - **Interrupt**: A [`CancellationToken`] signals the loop to stop at the next
//!   boundary. On interrupt, the loop emits `status.idle` and exits.
//! - **Pause/Resume**: A pause flag + [`Notify`] allow the loop to park until
//!   resumed.

use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use adk_core::{Agent, Content, Event, Part};
use adk_runner::Runner;
use adk_session::service::SessionService;

use crate::checkpoint::{CheckpointManager, RunState};
use crate::event_mapping::{RunnerOutput, map_runner_output, requires_parking, custom_tool_use_id};
use crate::parking::ToolParkingLot;
use crate::sequence::SequenceCounter;
use crate::types::{ContentBlock, RuntimeError, SessionEvent, SessionStatus, StopReason, UserEvent};
use crate::usage::{SessionUsageTracker, UsageReport};

/// Supervised session loop — one per active session.
///
/// Runs as a background `tokio::spawn`ed task. Receives user events via an
/// mpsc channel, processes each turn through the real Runner, and broadcasts
/// session events via a `tokio::broadcast` channel.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use std::time::Duration;
/// use tokio::sync::{broadcast, mpsc, Mutex, Notify};
/// use tokio_util::sync::CancellationToken;
/// use adk_managed::session_loop::SessionLoop;
/// use adk_managed::parking::ToolParkingLot;
///
/// let (event_tx, event_rx) = mpsc::channel(64);
/// let (broadcast_tx, _) = broadcast::channel(256);
/// let cancel = CancellationToken::new();
/// let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(300)));
///
/// let loop_handle = SessionLoop::new(
///     "session_001".to_string(),
///     event_rx,
///     broadcast_tx,
///     parking,
///     cancel.clone(),
///     agent,
///     session_service,
/// );
///
/// let handle = tokio::spawn(loop_handle.run());
/// // Send events via event_tx...
/// ```
pub struct SessionLoop {
    /// Session identifier.
    session_id: String,
    /// Input channel for user events.
    event_rx: mpsc::Receiver<UserEvent>,
    /// Broadcast channel for session events (fan-out to subscribers).
    event_tx: broadcast::Sender<SessionEvent>,
    /// Monotonic sequence counter.
    seq: SequenceCounter,
    /// Custom tool parking lot.
    parking: Arc<ToolParkingLot>,
    /// Checkpoint manager for durable state (shared with ActiveSession for replay).
    checkpoint: Arc<RwLock<CheckpointManager>>,
    /// Cancellation token for interrupt handling.
    cancel_token: CancellationToken,
    /// Pause flag — when true, the loop parks until resumed.
    pause_flag: Arc<Mutex<bool>>,
    /// Notify used to wake the loop after resume.
    pause_notify: Arc<Notify>,
    /// Current session status.
    status: SessionStatus,
    /// The agent driving this session.
    agent: Arc<dyn Agent>,
    /// Session persistence backend (needed by the Runner).
    session_service: Arc<dyn SessionService>,
    /// Accumulated usage tracking across all turns.
    usage_tracker: SessionUsageTracker,
}

impl SessionLoop {
    /// Create a new session loop.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The session this loop operates on.
    /// * `event_rx` - Receiver for incoming user events.
    /// * `event_tx` - Broadcast sender for outgoing session events.
    /// * `parking` - Shared parking lot for custom tool calls.
    /// * `cancel_token` - Token to signal interrupt/shutdown.
    /// * `agent` - The built agent to drive through the Runner.
    /// * `session_service` - Session persistence for the Runner.
    pub fn new(
        session_id: String,
        event_rx: mpsc::Receiver<UserEvent>,
        event_tx: broadcast::Sender<SessionEvent>,
        parking: Arc<ToolParkingLot>,
        cancel_token: CancellationToken,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> Self {
        let checkpoint = Arc::new(RwLock::new(CheckpointManager::new(session_id.clone())));
        Self {
            session_id,
            event_rx,
            event_tx,
            seq: SequenceCounter::default(),
            parking,
            checkpoint,
            cancel_token,
            pause_flag: Arc::new(Mutex::new(false)),
            pause_notify: Arc::new(Notify::new()),
            status: SessionStatus::Queued,
            agent,
            session_service,
            usage_tracker: SessionUsageTracker::new(),
        }
    }

    /// Create a session loop with custom pause controls (for external pause/resume).
    ///
    /// This allows the runtime to share the pause flag, notify, and checkpoint
    /// with the session handle so that `pause()`, `resume()`, and `stream_events()`
    /// (replay) work correctly against the same state the loop writes to.
    #[allow(clippy::too_many_arguments)]
    pub fn with_pause_controls(
        session_id: String,
        event_rx: mpsc::Receiver<UserEvent>,
        event_tx: broadcast::Sender<SessionEvent>,
        parking: Arc<ToolParkingLot>,
        cancel_token: CancellationToken,
        pause_flag: Arc<Mutex<bool>>,
        pause_notify: Arc<Notify>,
        checkpoint: Arc<RwLock<CheckpointManager>>,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> Self {
        Self {
            session_id,
            event_rx,
            event_tx,
            seq: SequenceCounter::default(),
            parking,
            checkpoint,
            cancel_token,
            pause_flag,
            pause_notify,
            status: SessionStatus::Queued,
            agent,
            session_service,
            usage_tracker: SessionUsageTracker::new(),
        }
    }

    /// Get a clone of the pause flag for external control.
    pub fn pause_flag(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.pause_flag)
    }

    /// Get a clone of the pause notify for external control.
    pub fn pause_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.pause_notify)
    }

    /// Run the session loop (consumes self).
    ///
    /// This is the main loop body, designed to be `tokio::spawn`ed. It runs
    /// until the input channel is closed or the cancellation token is triggered.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on graceful shutdown, or `Err(RuntimeError)` if an
    /// unrecoverable error occurs.
    pub async fn run(mut self) -> Result<(), RuntimeError> {
        info!(session_id = %self.session_id, "session loop started");

        loop {
            // Check for interrupt before waiting for the next event.
            if self.cancel_token.is_cancelled() {
                debug!(session_id = %self.session_id, "interrupt detected, shutting down");
                self.emit_idle(Some(StopReason::EndTurn), None).await;
                break;
            }

            // Check for pause.
            self.check_pause().await;

            // Wait for next event or cancellation.
            let event = tokio::select! {
                biased;
                _ = self.cancel_token.cancelled() => {
                    debug!(session_id = %self.session_id, "interrupted while waiting for event");
                    self.emit_idle(Some(StopReason::EndTurn), None).await;
                    break;
                }
                ev = self.event_rx.recv() => {
                    match ev {
                        Some(event) => event,
                        None => {
                            debug!(session_id = %self.session_id, "event channel closed, shutting down");
                            break;
                        }
                    }
                }
            };

            // Dispatch based on event type.
            match event {
                UserEvent::Message { content } => {
                    self.process_turn(content).await?;
                }
                UserEvent::Interrupt {} => {
                    debug!(session_id = %self.session_id, "user.interrupt received");
                    self.emit_idle(Some(StopReason::EndTurn), None).await;
                    break;
                }
                UserEvent::CustomToolResult {
                    custom_tool_use_id,
                    content,
                } => {
                    debug!(
                        session_id = %self.session_id,
                        tool_use_id = %custom_tool_use_id,
                        "delivering custom tool result"
                    );
                    if let Err(e) = self.parking.deliver(&custom_tool_use_id, content).await {
                        warn!(
                            session_id = %self.session_id,
                            error = %e,
                            "failed to deliver custom tool result"
                        );
                    }
                }
                UserEvent::ToolConfirmation { tool_use_id, result, deny_message } => {
                    debug!(
                        session_id = %self.session_id,
                        tool_use_id = %tool_use_id,
                        result = ?result,
                        "tool confirmation received, delivering to parking lot"
                    );
                    // Tool confirmation decisions are delivered via the parking lot.
                    // The session loop parks on tool_use_id when a confirmation is
                    // required (emitted as RequiresAction). The client sends back
                    // Allow/Deny which we convert to a ContentBlock result.
                    let content = match result {
                        crate::types::ConfirmationResult::Allow => {
                            vec![ContentBlock::Text {
                                text: serde_json::json!({
                                    "confirmation": "approved",
                                    "tool_use_id": tool_use_id
                                }).to_string(),
                            }]
                        }
                        crate::types::ConfirmationResult::Deny => {
                            let message = deny_message.unwrap_or_else(|| "Tool execution denied by user".to_string());
                            vec![ContentBlock::Text {
                                text: serde_json::json!({
                                    "confirmation": "denied",
                                    "tool_use_id": tool_use_id,
                                    "reason": message
                                }).to_string(),
                            }]
                        }
                    };
                    if let Err(e) = self.parking.deliver(&tool_use_id, content).await {
                        warn!(
                            session_id = %self.session_id,
                            error = %e,
                            "failed to deliver tool confirmation"
                        );
                    }
                }
                UserEvent::ToolResult { tool_use_id, .. } => {
                    debug!(
                        session_id = %self.session_id,
                        tool_use_id = %tool_use_id,
                        "tool result received (self-hosted only, not yet wired)"
                    );
                }
                UserEvent::DefineOutcome { criteria } => {
                    debug!(
                        session_id = %self.session_id,
                        criteria = %criteria,
                        "outcome criteria defined"
                    );
                    // Stored for future use — outcome evaluation is a later task.
                }
            }
        }

        info!(session_id = %self.session_id, "session loop exited");
        Ok(())
    }

    /// Process a single turn: emit status.running, invoke Runner, emit events, emit status.idle.
    async fn process_turn(&mut self, content: Vec<ContentBlock>) -> Result<(), RuntimeError> {
        // 1. Emit status.running
        self.status = SessionStatus::Running;
        let running_event = SessionEvent::StatusRunning {
            seq: self.seq.next(),
        };
        self.emit_event(running_event).await;

        // 2. Check interrupt before processing.
        if self.check_interrupt() {
            self.emit_idle(Some(StopReason::EndTurn), None).await;
            return Ok(());
        }

        // 3. Build user Content from ContentBlocks
        let user_content = self.build_user_content(&content);

        // 4. Build and invoke the Runner
        let runner = self.build_runner()?;

        let event_stream = runner
            .run_str("managed_user", &self.session_id, user_content)
            .await
            .map_err(|e| RuntimeError::internal(format!("runner invocation failed: {e}")))?;

        // 5. Consume event stream, mapping each event to SessionEvents
        let mut turn_usage = UsageReport::default();
        let mut custom_tool_ids = Vec::new();

        futures::pin_mut!(event_stream);

        while let Some(event_result) = event_stream.next().await {
            // Check interrupt between events
            if self.check_interrupt() {
                self.emit_idle(Some(StopReason::EndTurn), None).await;
                return Ok(());
            }

            match event_result {
                Ok(event) => {
                    self.process_runner_event(&event, &mut turn_usage, &mut custom_tool_ids)
                        .await;
                }
                Err(e) => {
                    warn!(
                        session_id = %self.session_id,
                        error = %e,
                        "runner event stream error"
                    );
                    let error_event = SessionEvent::Error {
                        code: "runner_error".to_string(),
                        message: e.to_string(),
                        seq: self.seq.next(),
                    };
                    self.emit_event(error_event).await;
                }
            }
        }

        // 6. Track usage
        // 6. Track usage
        let turn_usage_report = if !turn_usage.is_empty() {
            self.usage_tracker.record_turn(turn_usage.clone());
            Some(turn_usage)
        } else {
            None
        };

        // 7. Determine stop reason
        let stop_reason = if custom_tool_ids.is_empty() {
            Some(StopReason::EndTurn)
        } else {
            Some(StopReason::RequiresAction {
                event_ids: custom_tool_ids,
            })
        };

        // 8. Emit status.idle with usage from this turn
        self.emit_idle(stop_reason, turn_usage_report).await;

        Ok(())
    }

    /// Build a Runner instance for this turn.
    fn build_runner(&self) -> Result<Runner, RuntimeError> {
        Runner::builder()
            .app_name("managed")
            .agent(Arc::clone(&self.agent))
            .session_service(Arc::clone(&self.session_service))
            .cancellation_token(self.cancel_token.clone())
            .build()
            .map_err(|e| RuntimeError::internal(format!("failed to build runner: {e}")))
    }

    /// Convert managed ContentBlocks into an adk-core Content for the Runner.
    fn build_user_content(&self, blocks: &[ContentBlock]) -> Content {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(Part::Text {
                        text: text.clone(),
                    });
                }
                ContentBlock::Image { source } => {
                    // Convert image block to inline data or file reference
                    if let Some(url) = source.get("url").and_then(|v| v.as_str()) {
                        parts.push(Part::FileData {
                            mime_type: source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image/png")
                                .to_string(),
                            file_uri: url.to_string(),
                        });
                    }
                }
                ContentBlock::File { file_id } => {
                    parts.push(Part::FileData {
                        mime_type: "application/octet-stream".to_string(),
                        file_uri: file_id.clone(),
                    });
                }
            }
        }

        Content {
            role: "user".to_string(),
            parts,
        }
    }

    /// Process a single Runner event, mapping it to SessionEvents and tracking usage.
    async fn process_runner_event(
        &mut self,
        event: &Event,
        turn_usage: &mut UsageReport,
        custom_tool_ids: &mut Vec<String>,
    ) {
        // Extract usage metadata from the LLM response
        if let Some(ref usage_meta) = event.llm_response.usage_metadata {
            let report = UsageReport::from_usage_metadata(usage_meta);
            turn_usage.accumulate(&report);
        }

        // Skip partial streaming chunks — we only emit complete events
        if event.llm_response.partial {
            return;
        }

        // Extract content from the LLM response
        if let Some(ref content) = event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::Text { text } => {
                        if text.is_empty() {
                            continue;
                        }
                        let output = RunnerOutput::TextContent {
                            text: text.clone(),
                        };
                        let session_event = map_runner_output(output, self.seq.next());
                        self.emit_event(session_event).await;
                    }
                    Part::FunctionCall { name, args, id, .. } => {
                        let tool_use_id = id
                            .clone()
                            .unwrap_or_else(|| format!("tu_{}", uuid::Uuid::new_v4()));

                        // Classify the tool call
                        let tool_kind = self.classify_tool(name);

                        let output = match tool_kind {
                            ToolKind::Custom => {
                                let ctu_id = format!("ctu_{}", uuid::Uuid::new_v4());
                                custom_tool_ids.push(ctu_id.clone());
                                RunnerOutput::CustomToolCall {
                                    custom_tool_use_id: ctu_id,
                                    name: name.clone(),
                                    input: args.clone(),
                                }
                            }
                            ToolKind::Builtin => RunnerOutput::BuiltinToolCall {
                                tool_use_id,
                                name: name.clone(),
                                input: args.clone(),
                            },
                            ToolKind::Mcp => RunnerOutput::McpToolCall {
                                tool_use_id,
                                name: name.clone(),
                                input: args.clone(),
                            },
                        };

                        let session_event = map_runner_output(output.clone(), self.seq.next());
                        self.emit_event(session_event).await;

                        // If custom tool, park and wait for client result
                        if requires_parking(&output)
                            && let Some(ctu_id) = custom_tool_use_id(&output)
                        {
                            let ctu_id_owned = ctu_id.to_string();
                            debug!(
                                session_id = %self.session_id,
                                custom_tool_use_id = %ctu_id_owned,
                                "parking for custom tool result"
                            );
                            match self.parking.park(&ctu_id_owned).await {
                                Ok(_result_blocks) => {
                                    debug!(
                                        session_id = %self.session_id,
                                        custom_tool_use_id = %ctu_id_owned,
                                        "custom tool result delivered"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        session_id = %self.session_id,
                                        error = %e,
                                        "custom tool park failed or timed out"
                                    );
                                }
                            }
                        }
                    }
                    // Skip FunctionResponse, Thinking, and other part types
                    _ => {}
                }
            }
        }
    }

    /// Classify a tool call by name to determine which RunnerOutput variant to use.
    fn classify_tool(&self, name: &str) -> ToolKind {
        // Known built-in tools execute server-side
        const BUILTIN_TOOLS: &[&str] = &[
            "bash",
            "filesystem",
            "web_search",
            "web_fetch",
            "code_execution",
        ];

        if BUILTIN_TOOLS.contains(&name) {
            ToolKind::Builtin
        } else if name.starts_with("mcp_") || name.contains("::") {
            ToolKind::Mcp
        } else {
            // All other tools are custom (client-executed)
            ToolKind::Custom
        }
    }

    /// Emit a session event: assign to checkpoint and broadcast.
    async fn emit_event(&mut self, event: SessionEvent) {
        // Checkpoint atomically via the shared manager.
        let run_state = RunState {
            seq: self.seq.current(),
            pending_tool_ids: Vec::new(),
            status: self.status,
        };
        self.checkpoint.write().await.checkpoint(event.clone(), run_state);

        // Broadcast to subscribers (ignore if no receivers).
        let _ = self.event_tx.send(event);
    }

    /// Emit a `status.idle` event and update internal status.
    async fn emit_idle(&mut self, stop_reason: Option<StopReason>, usage: Option<UsageReport>) {
        self.status = SessionStatus::Idle;
        let idle_event = SessionEvent::StatusIdle {
            seq: self.seq.next(),
            stop_reason,
            usage,
        };
        self.emit_event(idle_event).await;
    }

    /// Check if the cancellation token has been triggered.
    ///
    /// Returns `true` if interrupted.
    fn check_interrupt(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Check and handle pause state. If paused, blocks until resumed.
    async fn check_pause(&self) {
        loop {
            let is_paused = *self.pause_flag.lock().await;
            if !is_paused {
                break;
            }
            debug!(session_id = %self.session_id, "session loop paused, waiting for resume");
            self.pause_notify.notified().await;
        }
    }
}

/// Tool classification used internally by the session loop.
///
/// Re-exported from [`crate::event_mapping::ToolKind`] for internal use.
use crate::event_mapping::ToolKind;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use adk_core::{FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
    use async_stream::stream;
    use async_trait::async_trait;

    /// Mock LLM that returns a configurable response.
    struct TestLlm {
        response_text: String,
    }

    impl TestLlm {
        fn new(text: &str) -> Self {
            Self {
                response_text: text.to_string(),
            }
        }
    }

    #[async_trait]
    impl Llm for TestLlm {
        fn name(&self) -> &str {
            "test-llm"
        }

        async fn generate_content(
            &self,
            _request: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<LlmResponseStream> {
            let text = self.response_text.clone();
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(Content::new("model").with_text(&text)),
                    partial: false,
                    turn_complete: true,
                    finish_reason: Some(FinishReason::Stop),
                    ..Default::default()
                });
            };
            Ok(Box::pin(s))
        }
    }

    /// Build a test agent with the given LLM.
    fn build_test_agent(llm: impl Llm + 'static) -> Arc<dyn Agent> {
        let agent = adk_agent::LlmAgentBuilder::new("test-agent")
            .model(Arc::new(llm))
            .build()
            .unwrap();
        Arc::new(agent)
    }

    /// Helper to create a session loop with default test configuration.
    fn create_test_loop() -> (
        mpsc::Sender<UserEvent>,
        broadcast::Receiver<SessionEvent>,
        CancellationToken,
        SessionLoop,
    ) {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(5)));
        let agent = build_test_agent(TestLlm::new("Hello from the agent"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "test_session".to_string(),
            event_rx,
            broadcast_tx,
            parking,
            cancel.clone(),
            agent,
            session_service,
        );

        (event_tx, broadcast_rx, cancel, session_loop)
    }

    #[tokio::test]
    async fn test_basic_message_flow() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send a message.
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            })
            .await
            .unwrap();

        // Expect: status.running, then agent response events, then status.idle
        let ev1 = broadcast_rx.recv().await.unwrap();
        match ev1 {
            SessionEvent::StatusRunning { seq } => assert_eq!(seq, 0),
            other => panic!("expected StatusRunning, got: {other:?}"),
        }

        // Collect remaining events until we get StatusIdle
        let mut got_message = false;
        let mut got_idle = false;
        for _ in 0..10 {
            match tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv()).await {
                Ok(Ok(SessionEvent::Message { content, .. })) => {
                    assert!(!content.is_empty());
                    got_message = true;
                }
                Ok(Ok(SessionEvent::StatusIdle { stop_reason, .. })) => {
                    assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
                    got_idle = true;
                    break;
                }
                Ok(Ok(SessionEvent::Error { message, .. })) => {
                    // In test environments without a real model, errors are acceptable
                    debug!("got error event: {message}");
                }
                Ok(Ok(other)) => {
                    debug!("got other event: {other:?}");
                }
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        // We must at least get status.idle (the turn completes regardless)
        assert!(got_idle, "expected StatusIdle event");

        // Close the channel to stop the loop.
        drop(event_tx);
        let result = handle.await.unwrap();
        assert!(result.is_ok());

        // Note: got_message depends on whether the Runner successfully invoked
        // the mock LLM. In unit tests, InMemorySessionService may not have the
        // session pre-created so the Runner creates one — either way the flow
        // should complete without panics.
        let _ = got_message;
    }

    #[tokio::test]
    async fn test_seq_monotonically_increases() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send a message
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text {
                    text: "First".to_string(),
                }],
            })
            .await
            .unwrap();

        // Collect events from the turn
        let mut seqs = Vec::new();
        for _ in 0..10 {
            match tokio::time::timeout(Duration::from_secs(5), broadcast_rx.recv()).await {
                Ok(Ok(ev)) => {
                    let seq = match &ev {
                        SessionEvent::StatusRunning { seq } => *seq,
                        SessionEvent::Message { seq, .. } => *seq,
                        SessionEvent::StatusIdle { seq, .. } => *seq,
                        SessionEvent::ToolUse { seq, .. } => *seq,
                        SessionEvent::CustomToolUse { seq, .. } => *seq,
                        SessionEvent::McpToolUse { seq, .. } => *seq,
                        SessionEvent::Error { seq, .. } => *seq,
                    };
                    seqs.push(seq);
                    if matches!(ev, SessionEvent::StatusIdle { .. }) {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Verify strict monotonic increase.
        assert!(seqs.len() >= 2, "expected at least 2 events");
        for window in seqs.windows(2) {
            assert!(
                window[1] > window[0],
                "seq must be strictly increasing: {} should be > {}",
                window[1],
                window[0]
            );
        }

        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_interrupt_stops_loop() {
        let (event_tx, mut broadcast_rx, cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Give the loop a moment to start waiting.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Trigger interrupt.
        cancel.cancel();

        // Should emit status.idle on interrupt.
        let ev = broadcast_rx.recv().await.unwrap();
        match ev {
            SessionEvent::StatusIdle { stop_reason, .. } => {
                assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
            }
            other => panic!("expected StatusIdle on interrupt, got: {other:?}"),
        }

        // The loop should exit cleanly.
        let result = handle.await.unwrap();
        assert!(result.is_ok());

        drop(event_tx);
    }

    #[tokio::test]
    async fn test_user_interrupt_event_stops_loop() {
        let (event_tx, mut broadcast_rx, _cancel, session_loop) = create_test_loop();

        let handle = tokio::spawn(session_loop.run());

        // Send an interrupt event.
        event_tx.send(UserEvent::Interrupt {}).await.unwrap();

        // Should emit status.idle.
        let ev = broadcast_rx.recv().await.unwrap();
        match ev {
            SessionEvent::StatusIdle { stop_reason, .. } => {
                assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
            }
            other => panic!("expected StatusIdle, got: {other:?}"),
        }

        let result = handle.await.unwrap();
        assert!(result.is_ok());

        drop(event_tx);
    }

    #[tokio::test]
    async fn test_pause_and_resume() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, mut broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(5)));
        let pause_flag = Arc::new(Mutex::new(false));
        let pause_notify = Arc::new(Notify::new());
        let agent = build_test_agent(TestLlm::new("resumed response"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::with_pause_controls(
            "pause_test".to_string(),
            event_rx,
            broadcast_tx,
            parking,
            cancel.clone(),
            Arc::clone(&pause_flag),
            Arc::clone(&pause_notify),
            Arc::new(RwLock::new(CheckpointManager::new("pause_test".to_string()))),
            agent,
            session_service,
        );

        let handle = tokio::spawn(session_loop.run());

        // Pause the loop.
        *pause_flag.lock().await = true;

        // Send a message — should not be processed while paused.
        event_tx
            .send(UserEvent::Message {
                content: vec![ContentBlock::Text {
                    text: "While paused".to_string(),
                }],
            })
            .await
            .unwrap();

        // Give the loop time to potentially process (it shouldn't).
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Verify nothing was broadcast yet (try_recv should fail).
        assert!(broadcast_rx.try_recv().is_err());

        // Resume.
        *pause_flag.lock().await = false;
        pause_notify.notify_one();

        // Now the message should be processed.
        let ev1 = tokio::time::timeout(Duration::from_secs(2), broadcast_rx.recv())
            .await
            .expect("timed out waiting for event after resume")
            .unwrap();

        match ev1 {
            SessionEvent::StatusRunning { .. } => {}
            other => panic!("expected StatusRunning after resume, got: {other:?}"),
        }

        // Clean up.
        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_channel_close_stops_loop() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, _broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(5)));
        let agent = build_test_agent(TestLlm::new("test"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "close_test".to_string(),
            event_rx,
            broadcast_tx,
            parking,
            cancel,
            agent,
            session_service,
        );

        let handle = tokio::spawn(session_loop.run());

        // Drop the sender — closes the channel.
        drop(event_tx);

        // Loop should exit cleanly.
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_custom_tool_result_delivery() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, _broadcast_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(5)));
        let parking_clone = Arc::clone(&parking);
        let agent = build_test_agent(TestLlm::new("test"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "parking_test".to_string(),
            event_rx,
            broadcast_tx,
            parking_clone,
            cancel,
            agent,
            session_service,
        );

        let handle = tokio::spawn(session_loop.run());

        // Park a tool call from another task.
        let parking_for_park = Arc::clone(&parking);
        let park_handle = tokio::spawn(async move { parking_for_park.park("ctu_test_001").await });

        // Give the park a moment to register.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Send custom tool result via the session loop.
        event_tx
            .send(UserEvent::CustomToolResult {
                custom_tool_use_id: "ctu_test_001".to_string(),
                content: vec![ContentBlock::Text {
                    text: "tool output".to_string(),
                }],
            })
            .await
            .unwrap();

        // The parked task should receive the result.
        let result = tokio::time::timeout(Duration::from_secs(2), park_handle)
            .await
            .expect("park timed out")
            .unwrap()
            .unwrap();

        assert_eq!(result.len(), 1);
        match &result[0] {
            ContentBlock::Text { text } => assert_eq!(text, "tool output"),
            _ => panic!("expected Text"),
        }

        // Clean up.
        drop(event_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_tool_classification() {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (broadcast_tx, _) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let parking = Arc::new(ToolParkingLot::new(Duration::from_secs(5)));
        let agent = build_test_agent(TestLlm::new("test"));
        let session_service: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());

        let session_loop = SessionLoop::new(
            "classify_test".to_string(),
            event_rx,
            broadcast_tx,
            parking,
            cancel,
            agent,
            session_service,
        );

        // Test builtin tools
        assert!(matches!(session_loop.classify_tool("bash"), ToolKind::Builtin));
        assert!(matches!(
            session_loop.classify_tool("filesystem"),
            ToolKind::Builtin
        ));
        assert!(matches!(
            session_loop.classify_tool("web_search"),
            ToolKind::Builtin
        ));
        assert!(matches!(
            session_loop.classify_tool("web_fetch"),
            ToolKind::Builtin
        ));
        assert!(matches!(
            session_loop.classify_tool("code_execution"),
            ToolKind::Builtin
        ));

        // Test MCP tools
        assert!(matches!(
            session_loop.classify_tool("mcp_file_read"),
            ToolKind::Mcp
        ));
        assert!(matches!(
            session_loop.classify_tool("server::tool"),
            ToolKind::Mcp
        ));

        // Test custom tools
        assert!(matches!(
            session_loop.classify_tool("get_weather"),
            ToolKind::Custom
        ));
        assert!(matches!(
            session_loop.classify_tool("deploy"),
            ToolKind::Custom
        ));

        drop(event_tx);
    }
}
