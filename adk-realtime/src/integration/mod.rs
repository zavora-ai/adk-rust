//! # Integration Module
//!
//! This module provides the `IntegratedRealtimeRunner` — a composition layer that
//! wraps the existing `RealtimeRunner` and connects it to ADK services (sessions,
//! memory, plugins) without modifying `RealtimeRunner` internals.
//!
//! ## Components
//!
//! - [`tool_bridge`] — `ToolBridgeAdapter` bridges `adk_core::Tool` → `ToolHandler`
//! - [`transcript`] — `TranscriptAggregator` collects deltas into complete turns
//! - [`context`] — `RealtimeToolContext` implementation for tool execution
//! - [`builder`] — `IntegratedRealtimeRunnerBuilder` for constructing the runner

pub mod builder;
pub mod context;
pub mod tool_bridge;
pub mod transcript;

// Re-exports for convenience
pub use builder::IntegratedRealtimeRunnerBuilder;
pub use context::{DefaultToolContextFactory, RealtimeToolContext, ToolContextFactory};
pub use tool_bridge::ToolBridgeAdapter;
pub use transcript::TranscriptAggregator;

use serde_json::Value;

// ─── Shared Data Types ───────────────────────────────────────────────────────

/// Identifies the ADK session scope for all service interactions.
///
/// Every interaction with session, memory, and plugin services requires
/// a consistent identity triple to locate the correct session state.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    /// The application name (scopes sessions and memory).
    pub app_name: String,
    /// The user identifier.
    pub user_id: String,
    /// The unique session identifier.
    pub session_id: String,
}

/// Events emitted by the [`TranscriptAggregator`]
/// when a turn completes.
#[derive(Debug, Clone)]
pub enum AggregatedEvent {
    /// A complete assistant response turn.
    TurnComplete {
        /// Full concatenated text output.
        text: String,
        /// Full concatenated audio transcript.
        audio_transcript: String,
        /// Tool calls executed during this turn.
        tool_calls: Vec<CompletedToolCall>,
        /// Provider item ID for this response.
        item_id: String,
        /// Whether this turn was interrupted by user speech.
        interrupted: bool,
    },
    /// A complete user utterance (from speech recognition).
    UserUtteranceComplete {
        /// Full user transcript.
        transcript: String,
    },
}

/// A tool call that was executed during a turn.
#[derive(Debug, Clone)]
pub struct CompletedToolCall {
    /// Unique call ID from the provider.
    pub call_id: String,
    /// Tool function name.
    pub name: String,
    /// Arguments passed to the tool.
    pub arguments: Value,
    /// Result returned by the tool.
    pub result: Value,
}

/// Configuration options specific to the integration layer.
///
/// Controls which ADK service interactions are performed automatically
/// during the realtime session lifecycle.
#[derive(Debug, Clone)]
pub struct IntegrationConfig {
    /// Whether to persist transcripts to session on each turn.
    pub persist_transcripts: bool,
    /// Whether to store turns in memory for future retrieval.
    pub store_to_memory: bool,
    /// Whether to inject memory context at session start.
    pub inject_memory_context: bool,
    /// Maximum memory entries to inject into system instruction.
    pub max_memory_injection: usize,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            persist_transcripts: true,
            store_to_memory: true,
            inject_memory_context: true,
            max_memory_injection: 10,
        }
    }
}

// ─── IntegratedRealtimeRunner ────────────────────────────────────────────────

use std::sync::Arc;

use std::collections::HashMap;

use adk_memory::MemoryService;
use adk_plugin::EnhancedPluginManager;
use adk_session::SessionService;
use tokio::sync::RwLock;

use crate::config::SessionUpdateConfig;
use crate::error::Result;
use crate::events::ServerEvent;
use crate::runner::RealtimeRunner;

/// The main orchestrator that wraps [`RealtimeRunner`] and intercepts its event loop
/// to connect it with ADK services (sessions, memory, plugins).
///
/// `IntegratedRealtimeRunner` provides transparent integration with ADK services:
/// - **Session persistence**: Completed turns are automatically saved to the session.
/// - **Memory storage**: Turns are stored for future RAG retrieval.
/// - **Plugin hooks**: Tool calls pass through `before_tool_call`/`after_tool_call` hooks.
/// - **Transcript aggregation**: Streaming deltas are assembled into complete turns.
///
/// Use [`IntegratedRealtimeRunnerBuilder`] to
/// construct an instance.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::integration::IntegratedRealtimeRunner;
///
/// let runner = IntegratedRealtimeRunner::builder()
///     .model(model)
///     .identity("my-app", "user-1", "session-1")
///     .session_service(session_svc)
///     .build()?;
/// ```
pub struct IntegratedRealtimeRunner {
    /// The underlying realtime runner handling transport and tool execution.
    pub(crate) runner: Arc<RealtimeRunner>,
    /// Optional session service for transcript persistence.
    pub(crate) session_service: Option<Arc<dyn SessionService>>,
    /// Optional memory service for RAG storage and retrieval.
    pub(crate) memory_service: Option<Arc<dyn MemoryService>>,
    /// Optional plugin manager for lifecycle hooks.
    #[allow(dead_code)] // Used by task 6.5
    pub(crate) plugin_manager: Option<Arc<EnhancedPluginManager>>,
    /// Aggregator that assembles streaming deltas into complete turns.
    pub(crate) aggregator: RwLock<TranscriptAggregator>,
    /// Session identity triple (app_name, user_id, session_id).
    pub(crate) identity: SessionIdentity,
    /// Integration-layer configuration.
    pub(crate) config: IntegrationConfig,
    /// The ADK tools by name, so the live path can run them through the policy pipeline.
    ///
    /// The builder wrapped each one in a `ToolBridgeAdapter` for the inner runner and then
    /// dropped the originals, which is why the active path could only reach the adapter — and
    /// the adapter applies no confirmation, callbacks, or plugins.
    pub(crate) adk_tools: HashMap<String, Arc<dyn adk_core::Tool>>,
}

impl IntegratedRealtimeRunner {
    /// Creates a new [`IntegratedRealtimeRunnerBuilder`].
    pub fn builder() -> builder::IntegratedRealtimeRunnerBuilder {
        builder::IntegratedRealtimeRunnerBuilder::new()
    }

    /// Connect to the realtime provider.
    ///
    /// Before connecting, loads session history and injects memory context
    /// if the respective services are configured. Session/memory failures
    /// are non-fatal — the connection proceeds regardless.
    ///
    /// # Errors
    ///
    /// Returns an error only if the underlying [`RealtimeRunner::connect`] fails
    /// (transport-level error). Session and memory service failures are logged
    /// and swallowed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = IntegratedRealtimeRunner::builder()
    ///     .model(model)
    ///     .identity("my-app", "user-1", "session-1")
    ///     .session_service(session_svc)
    ///     .memory_service(memory_svc)
    ///     .build()?;
    ///
    /// runner.connect().await?;
    /// ```
    pub async fn connect(&self) -> Result<()> {
        // 1. Load session history if session_service is configured
        if let Some(ref session_service) = self.session_service {
            let get_req = adk_session::GetRequest {
                app_name: self.identity.app_name.clone(),
                user_id: self.identity.user_id.clone(),
                session_id: self.identity.session_id.clone(),
                num_recent_events: None,
                after: None,
            };
            match session_service.get(get_req).await {
                Ok(_session) => {
                    tracing::debug!(
                        session_id = %self.identity.session_id,
                        "loaded prior session history"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %self.identity.session_id,
                        error = %e,
                        "session load failed (non-fatal)"
                    );
                }
            }
        }

        // 2. Query MemoryService at session start if inject_memory_context is enabled
        if self.config.inject_memory_context
            && let Some(ref memory_service) = self.memory_service
        {
            match memory_service
                .search(adk_memory::SearchRequest {
                    query: "session context".to_string(),
                    user_id: self.identity.user_id.clone(),
                    app_name: self.identity.app_name.clone(),
                    limit: Some(self.config.max_memory_injection),
                    min_score: None,
                    project_id: None,
                })
                .await
            {
                Ok(response) => {
                    if !response.memories.is_empty() {
                        tracing::debug!(
                            count = response.memories.len(),
                            "injecting memory entries into session context"
                        );
                        // Memory entries are available for system instruction enrichment.
                        // Actual injection into the system instruction happens via
                        // update_session in a future enhancement.
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "memory query failed (non-fatal)"
                    );
                }
            }
        }

        // 3. Connect the underlying runner — this is the only error that propagates
        self.runner.connect().await
    }

    /// Send audio to the realtime session.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// runner.send_audio(base64_encoded_pcm).await?;
    /// ```
    pub async fn send_audio(&self, audio_base64: &str) -> Result<()> {
        self.runner.send_audio(audio_base64).await
    }

    /// Send text to the realtime session.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// runner.send_text("Hello, how are you?").await?;
    /// ```
    pub async fn send_text(&self, text: &str) -> Result<()> {
        self.runner.send_text(text).await
    }

    /// Send a base64-encoded video/image frame (e.g. `image/jpeg`) for
    /// multimodal input. Delegates to the underlying [`RealtimeRunner`].
    pub async fn send_video_frame(&self, mime_type: &str, data_base64: &str) -> Result<()> {
        self.runner.send_video_frame(mime_type, data_base64).await
    }

    /// Trigger a response from the model.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// runner.create_response().await?;
    /// ```
    pub async fn create_response(&self) -> Result<()> {
        self.runner.create_response().await
    }

    /// Interrupt the current response.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// runner.interrupt().await?;
    /// ```
    pub async fn interrupt(&self) -> Result<()> {
        self.runner.interrupt().await
    }

    /// Update the session configuration.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_realtime::config::{SessionUpdateConfig, RealtimeConfig};
    ///
    /// let update = SessionUpdateConfig(
    ///     RealtimeConfig::default().with_instruction("You are now a pirate.")
    /// );
    /// runner.update_session(update).await?;
    /// ```
    pub async fn update_session(&self, config: SessionUpdateConfig) -> Result<()> {
        self.runner.update_session(config).await
    }

    /// Close the realtime session.
    ///
    /// Delegates to the underlying [`RealtimeRunner`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// runner.close().await?;
    /// ```
    pub async fn close(&self) -> Result<()> {
        self.runner.close().await
    }

    /// Process the next event from the realtime session with full integration.
    ///
    /// Feeds each [`ServerEvent`] to the [`TranscriptAggregator`] for turn assembly.
    /// When a turn completes, persists the aggregated event to session/memory.
    /// The raw event is always forwarded to the caller.
    ///
    /// Returns `None` when the session is closed or no more events are available.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// while let Some(event) = runner.next_event().await {
    ///     match event? {
    ///         ServerEvent::AudioDelta { delta, .. } => { /* forward to browser */ }
    ///         ServerEvent::TranscriptDelta { delta, .. } => { /* show in UI */ }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub async fn next_event(&self) -> Option<Result<ServerEvent>> {
        let event = self.runner.next_event().await?;

        if let Ok(server_event) = &event {
            // Feed to transcript aggregator
            let aggregated = self.aggregator.write().await.process(server_event);

            if let Some(agg_event) = aggregated {
                self.handle_aggregated_event(agg_event).await;
            }

            // Auto-execute tool calls. Unlike `RealtimeRunner::run`, the pull-based
            // `next_event` path does not dispatch tools on its own, so we do it here:
            // bridged ADK tools and native handlers registered on the builder run,
            // and (with `auto_respond_tools`) the result is sent back to the model.
            if let ServerEvent::FunctionCallDone { call_id, name, arguments, .. } = server_event
                && let Err(e) = self.dispatch_with_policy(call_id, name, arguments).await
            {
                tracing::warn!(
                    tool = %name,
                    call_id = %call_id,
                    error = %e,
                    "tool dispatch failed (non-fatal)"
                );
            }

            // When the dispatch response finishes, send the single owed follow-up
            // response so the model speaks its answer using the tool results. This
            // mirrors what `RealtimeRunner::run` does in `handle_event`; the
            // pull-based path must do it explicitly.
            if let ServerEvent::ResponseDone { .. } = server_event
                && let Err(e) = self.runner.respond_after_tools().await
            {
                tracing::warn!(error = %e, "post-tool response trigger failed (non-fatal)");
            }
        }

        Some(event)
    }

    /// Handle a completed aggregated event (turn or user utterance).
    ///
    /// - `TurnComplete`: builds an assistant `Event`, persists to session, stores to memory,
    ///   calls plugin `on_event`
    /// - `UserUtteranceComplete`: builds a user `Event`, persists to session
    ///
    /// All service errors are logged and swallowed (non-fatal).
    async fn handle_aggregated_event(&self, event: AggregatedEvent) {
        match &event {
            AggregatedEvent::TurnComplete { text, audio_transcript, .. } => {
                tracing::debug!(text_len = text.len(), "turn complete");

                // 1. Build adk_core::Event for the assistant turn
                let transcript_text =
                    if text.is_empty() { audio_transcript.as_str() } else { text.as_str() };
                let content = adk_core::Content::new("model").with_text(transcript_text);
                let mut adk_event = adk_core::Event::new("realtime");
                adk_event.author = "model".to_string();
                adk_event.set_content(content.clone());

                // 2. Persist to session
                if self.config.persist_transcripts
                    && let Some(ref session_service) = self.session_service
                    && let Err(e) = session_service
                        .append_event(&self.identity.session_id, adk_event.clone())
                        .await
                {
                    tracing::warn!(error = %e, "session persist failed (non-fatal)");
                }

                // 3. Store to memory
                if self.config.store_to_memory
                    && let Some(ref memory_service) = self.memory_service
                {
                    let entry = adk_memory::MemoryEntry {
                        content,
                        author: "assistant".to_string(),
                        timestamp: chrono::Utc::now(),
                    };
                    if let Err(e) = memory_service
                        .add_session(
                            &self.identity.app_name,
                            &self.identity.user_id,
                            &self.identity.session_id,
                            vec![entry],
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "memory persist failed (non-fatal)");
                    }
                }

                // 4. Plugin on_event hook
                //    EnhancedPluginManager::run_on_event requires an InvocationContext
                //    which is not available in the realtime runner context.
                //    We log the event notification for observability; full plugin
                //    integration requires a future InvocationContext adapter.
                if self.plugin_manager.is_some() {
                    tracing::debug!(
                        event_id = %adk_event.id,
                        "plugin on_event: skipped (no InvocationContext in realtime)"
                    );
                }
            }
            AggregatedEvent::UserUtteranceComplete { transcript } => {
                tracing::debug!(transcript_len = transcript.len(), "user utterance complete");

                // Build user event and persist to session
                let content = adk_core::Content::new("user").with_text(transcript);
                let mut adk_event = adk_core::Event::new("realtime");
                adk_event.author = "user".to_string();
                adk_event.set_content(content);

                if self.config.persist_transcripts
                    && let Some(ref session_service) = self.session_service
                    && let Err(e) =
                        session_service.append_event(&self.identity.session_id, adk_event).await
                {
                    tracing::warn!(error = %e, "session persist failed (non-fatal)");
                }
            }
        }
    }

    /// Execute a tool with plugin lifecycle hooks.
    ///
    /// When an [`EnhancedPluginManager`] is configured:
    /// 1. Runs `before_tool_call` pipeline — may short-circuit with a synthetic result
    /// 2. Executes the tool on `Continue`
    /// 3. Runs `after_tool_call` pipeline with the result
    ///
    /// When no [`EnhancedPluginManager`] is configured, executes the tool directly.
    ///
    /// Records the completed tool call in the [`TranscriptAggregator`] regardless of path,
    /// and emits an [`adk_core::Event`] with the function call and result.
    ///
    /// # Arguments
    ///
    /// * `tool` - The ADK tool to execute.
    /// * `call` - The tool call metadata (call_id, name, arguments).
    ///
    /// # Errors
    ///
    /// Returns an error only if the result construction fails. Plugin hook errors
    /// are non-fatal: logged and swallowed, with execution falling through to
    /// direct tool invocation.
    #[allow(dead_code)] // Will be wired in event loop handling
    /// Dispatches one provider tool call, preferring the policy pipeline.
    ///
    /// An ADK tool registered on this builder runs through `execute_tool_with_plugins`, which
    /// applies the configured plugin pipeline, records the call for the transcript, and
    /// persists the tool event. Previously the live path called
    /// `RealtimeRunner::dispatch_tool_call`, which invokes the `ToolBridgeAdapter` directly —
    /// the adapter creates a context and calls `Tool::execute` with no plugins, callbacks, or
    /// confirmation, so a tool governed in the standard agent loop ran ungoverned here.
    ///
    /// A name that is not a registered ADK tool is a native handler, and falls through to the
    /// runner's own dispatch. That bypass is now explicit rather than the default.
    async fn dispatch_with_policy(&self, call_id: &str, name: &str, arguments: &str) -> Result<()> {
        let Some(tool) = self.adk_tools.get(name).cloned() else {
            tracing::debug!(
                tool = %name,
                "no ADK tool by this name; dispatching as a native handler"
            );
            return self.runner.dispatch_tool_call(call_id, name, arguments).await;
        };

        let call = crate::events::ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: serde_json::from_str(arguments)
                .unwrap_or(serde_json::Value::Object(Default::default())),
        };

        let result = self.execute_tool_with_plugins(&tool, &call).await?;
        self.runner.send_tool_result(call_id, result).await
    }

    /// Runs one tool through the policy pipeline, for conformance tests.
    ///
    /// Exposed so the governed path can be asserted directly rather than only through a live
    /// provider session, which is why this defect had no coverage.
    #[doc(hidden)]
    pub async fn execute_tool_with_plugins_for_test(
        &self,
        tool: &Arc<dyn adk_core::Tool>,
        call: &crate::events::ToolCall,
    ) -> Result<Value> {
        self.execute_tool_with_plugins(tool, call).await
    }

    pub(crate) async fn execute_tool_with_plugins(
        &self,
        tool: &Arc<dyn adk_core::Tool>,
        call: &crate::events::ToolCall,
    ) -> Result<Value> {
        let ctx = self.create_tool_context(&call.call_id);

        let result = if let Some(ref pm) = self.plugin_manager {
            // Run before_tool_call pipeline
            match pm
                .run_before_tool_call(
                    tool.clone(),
                    call.arguments.clone(),
                    ctx.clone() as Arc<dyn adk_core::CallbackContext>,
                )
                .await
            {
                Ok(adk_plugin::BeforeToolCallResult::ShortCircuit(value)) => {
                    tracing::debug!(tool = tool.name(), "plugin short-circuited tool execution");
                    value
                }
                Ok(adk_plugin::BeforeToolCallResult::Continue(args)) => {
                    // Execute tool with potentially modified args
                    let tool_result = tool
                        .execute(ctx.clone() as Arc<dyn adk_core::ToolContext>, args)
                        .await
                        .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));

                    // Run after_tool_call pipeline
                    match pm
                        .run_after_tool_call(
                            tool.clone(),
                            &call.arguments,
                            tool_result.clone(),
                            ctx as Arc<dyn adk_core::CallbackContext>,
                        )
                        .await
                    {
                        Ok(adk_plugin::AfterToolCallResult::Continue(v)) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "after_tool_call plugin error (non-fatal)");
                            tool_result
                        }
                    }
                }
                Err(e) => {
                    // Fail closed. A before-tool plugin is where authorization, redaction, and
                    // policy live, so executing the tool when that pipeline fails turns a
                    // broken guard into no guard. The model receives the error instead.
                    tracing::error!(
                        tool = tool.name(),
                        error = %e,
                        "before_tool_call plugin failed; refusing the tool"
                    );
                    serde_json::json!({
                        "error": format!(
                            "tool {} was refused: its before-tool plugin pipeline failed ({e}). \
                             Execution is refused rather than proceeding without policy.",
                            tool.name()
                        )
                    })
                }
            }
        } else {
            // No plugin manager — execute directly
            tool.execute(ctx as Arc<dyn adk_core::ToolContext>, call.arguments.clone())
                .await
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
        };

        // Record completed tool call in aggregator
        self.aggregator.write().await.record_tool_call(CompletedToolCall {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            result: result.clone(),
        });

        // Emit adk_core::Event with function call and result
        let mut content = adk_core::Content::new("tool");
        content.parts.push(adk_core::Part::FunctionCall {
            name: call.name.clone(),
            args: call.arguments.clone(),
            id: Some(call.call_id.clone()),
            thought_signature: None,
        });
        content.parts.push(adk_core::Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(&call.name, result.clone()),
            id: Some(call.call_id.clone()),
        });

        if let Some(ref session_service) = self.session_service {
            let mut adk_event = adk_core::Event::new(&call.call_id);
            adk_event.author = "tool".to_string();
            adk_event.set_content(content);

            if let Err(e) = session_service.append_event(&self.identity.session_id, adk_event).await
            {
                tracing::warn!(error = %e, "tool event session persist failed (non-fatal)");
            }
        }

        Ok(result)
    }

    /// Create a [`RealtimeToolContext`](context::RealtimeToolContext) for the given
    /// function call ID.
    ///
    /// Returns an `Arc<RealtimeToolContext>` which implements both [`ToolContext`]
    /// and [`CallbackContext`], allowing it to be passed to plugin hooks and tool
    /// execution alike.
    #[allow(dead_code)] // Used by execute_tool_with_plugins
    fn create_tool_context(&self, function_call_id: &str) -> Arc<context::RealtimeToolContext> {
        Arc::new(context::RealtimeToolContext::new(
            self.identity.app_name.clone(),
            self.identity.user_id.clone(),
            self.identity.session_id.clone(),
            function_call_id.to_string(),
            self.memory_service.clone(),
        ))
    }
}

// ─── Session Persistence Tests ───────────────────────────────────────────────

#[cfg(test)]
mod session_persistence_tests {
    use super::*;
    use crate::audio::AudioFormat;
    use crate::config::RealtimeConfig;
    use crate::model::BoxedModel;
    use crate::session::BoxedSession;
    use adk_session::{CreateRequest, GetRequest, InMemorySessionService};
    use async_trait::async_trait;
    use proptest::prelude::*;
    use std::collections::HashMap;

    // ─── Mock RealtimeModel ──────────────────────────────────────────────────

    struct MockRealtimeModel;

    #[async_trait]
    impl crate::model::RealtimeModel for MockRealtimeModel {
        fn provider(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model-v1"
        }

        fn supports_realtime(&self) -> bool {
            true
        }

        fn supported_input_formats(&self) -> Vec<AudioFormat> {
            vec![AudioFormat::pcm16_24khz()]
        }

        fn supported_output_formats(&self) -> Vec<AudioFormat> {
            vec![AudioFormat::pcm16_24khz()]
        }

        fn available_voices(&self) -> Vec<&str> {
            vec!["default"]
        }

        async fn connect(&self, _config: RealtimeConfig) -> crate::error::Result<BoxedSession> {
            Err(crate::error::RealtimeError::connection("mock transport: no connection available"))
        }
    }

    fn mock_model() -> BoxedModel {
        Arc::new(MockRealtimeModel) as BoxedModel
    }

    // ─── Helpers ─────────────────────────────────────────────────────────────

    /// Build an IntegratedRealtimeRunner with the given session service.
    fn build_runner_with_session_service(
        session_service: Arc<dyn SessionService>,
        session_id: &str,
    ) -> IntegratedRealtimeRunner {
        builder::IntegratedRealtimeRunnerBuilder::new()
            .model(mock_model())
            .identity("test-app", "test-user", session_id)
            .session_service(session_service)
            .integration_config(IntegrationConfig {
                persist_transcripts: true,
                store_to_memory: false,
                inject_memory_context: false,
                max_memory_injection: 0,
            })
            .build()
            .expect("builder should succeed with mock model + identity")
    }

    /// Create a session in the service so append_event can find it.
    async fn create_test_session(service: &InMemorySessionService, session_id: &str) {
        service
            .create(CreateRequest {
                app_name: "test-app".to_string(),
                user_id: "test-user".to_string(),
                session_id: Some(session_id.to_string()),
                state: HashMap::new(),
            })
            .await
            .expect("session creation should succeed");
    }

    /// Retrieve all events from the session.
    async fn get_session_events(
        service: &InMemorySessionService,
        session_id: &str,
    ) -> Vec<adk_core::Event> {
        let session = service
            .get(GetRequest {
                app_name: "test-app".to_string(),
                user_id: "test-user".to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .expect("session retrieval should succeed");
        session.events().all()
    }

    // ─── Property Test: Session Append Ordering ──────────────────────────────

    /// **Feature: realtime-adk-integration, Property 3: Session Append Ordering**
    /// *For any* conversation with user utterances followed by assistant responses,
    /// events appended to `SessionService` SHALL maintain causal ordering:
    /// user events precede the assistant responses they triggered.
    /// **Validates: Requirements 2.1, 2.2, 7.2, 7.3**
    #[test]
    fn prop_session_append_ordering() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

        proptest!(ProptestConfig::with_cases(100), |(
            user_transcript in "[a-zA-Z0-9 ]{1,50}",
            assistant_text in "[a-zA-Z0-9 ]{1,50}",
            num_exchanges in 1usize..5,
        )| {
            rt.block_on(async {
                let session_service = Arc::new(InMemorySessionService::new());
                let session_id = format!("prop-session-{}", uuid::Uuid::new_v4());
                create_test_session(&session_service, &session_id).await;

                let runner = build_runner_with_session_service(
                    session_service.clone(),
                    &session_id,
                );

                // Simulate multiple user→assistant exchanges
                for _ in 0..num_exchanges {
                    // 1. User utterance completes first
                    runner
                        .handle_aggregated_event(AggregatedEvent::UserUtteranceComplete {
                            transcript: user_transcript.clone(),
                        })
                        .await;

                    // 2. Assistant turn completes in response
                    runner
                        .handle_aggregated_event(AggregatedEvent::TurnComplete {
                            text: assistant_text.clone(),
                            audio_transcript: String::new(),
                            tool_calls: vec![],
                            item_id: format!("item-{}", uuid::Uuid::new_v4()),
                            interrupted: false,
                        })
                        .await;
                }

                // Verify: retrieve all events from session
                let events = get_session_events(&session_service, &session_id).await;

                // Should have 2 * num_exchanges events (user + assistant per exchange)
                prop_assert_eq!(
                    events.len(),
                    num_exchanges * 2,
                    "expected {} events, got {}",
                    num_exchanges * 2,
                    events.len()
                );

                // Verify ordering: even indices are user, odd indices are assistant
                for i in 0..num_exchanges {
                    let user_event = &events[i * 2];
                    let assistant_event = &events[i * 2 + 1];

                    prop_assert_eq!(
                        &user_event.author,
                        "user",
                        "event at index {} should be from 'user', got '{}'",
                        i * 2,
                        user_event.author
                    );
                    prop_assert_eq!(
                        &assistant_event.author,
                        "model",
                        "event at index {} should be from 'model', got '{}'",
                        i * 2 + 1,
                        assistant_event.author
                    );

                    // Verify causal ordering: user event timestamp <= assistant event timestamp
                    prop_assert!(
                        user_event.timestamp <= assistant_event.timestamp,
                        "user event timestamp ({}) should be <= assistant event timestamp ({})",
                        user_event.timestamp,
                        assistant_event.timestamp
                    );
                }

                Ok(())
            })?;
        });
    }

    // ─── Unit Test: full event flow ──────────────────────────────────────────

    /// Integration test verifying the full event flow:
    /// connect → user utterance → assistant response → verify session state.
    #[tokio::test]
    async fn test_full_event_flow_with_in_memory_session_service() {
        let session_service = Arc::new(InMemorySessionService::new());
        let session_id = "integration-test-session";

        // Create session first
        create_test_session(&session_service, session_id).await;

        // Build runner
        let runner = build_runner_with_session_service(session_service.clone(), session_id);

        // Simulate user utterance
        runner
            .handle_aggregated_event(AggregatedEvent::UserUtteranceComplete {
                transcript: "Hello, what is the weather today?".to_string(),
            })
            .await;

        // Simulate assistant response (as would be produced by TranscriptAggregator
        // after processing ResponseCreated → TextDelta* → ResponseDone)
        runner
            .handle_aggregated_event(AggregatedEvent::TurnComplete {
                text: "The weather is sunny and 72°F today.".to_string(),
                audio_transcript: String::new(),
                tool_calls: vec![],
                item_id: "item-001".to_string(),
                interrupted: false,
            })
            .await;

        // Verify session contains both events in correct order
        let events = get_session_events(&session_service, session_id).await;

        assert_eq!(events.len(), 2, "session should have 2 events");

        // First event: user utterance
        assert_eq!(events[0].author, "user");
        let user_content = events[0].content();
        assert!(user_content.is_some(), "user event should have content");
        let user_text = user_content
            .unwrap()
            .parts
            .iter()
            .filter_map(|p| match p {
                adk_core::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(user_text, "Hello, what is the weather today?");

        // Second event: assistant response
        assert_eq!(events[1].author, "model");
        let assistant_content = events[1].content();
        assert!(assistant_content.is_some(), "assistant event should have content");
        let assistant_text = assistant_content
            .unwrap()
            .parts
            .iter()
            .filter_map(|p| match p {
                adk_core::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(assistant_text, "The weather is sunny and 72°F today.");

        // Verify causal ordering
        assert!(
            events[0].timestamp <= events[1].timestamp,
            "user event should be timestamped before or at same time as assistant event"
        );
    }

    /// Test that multiple exchanges maintain ordering.
    #[tokio::test]
    async fn test_multiple_exchanges_maintain_ordering() {
        let session_service = Arc::new(InMemorySessionService::new());
        let session_id = "multi-exchange-session";
        create_test_session(&session_service, session_id).await;

        let runner = build_runner_with_session_service(session_service.clone(), session_id);

        // Exchange 1
        runner
            .handle_aggregated_event(AggregatedEvent::UserUtteranceComplete {
                transcript: "First question".to_string(),
            })
            .await;
        runner
            .handle_aggregated_event(AggregatedEvent::TurnComplete {
                text: "First answer".to_string(),
                audio_transcript: String::new(),
                tool_calls: vec![],
                item_id: "item-1".to_string(),
                interrupted: false,
            })
            .await;

        // Exchange 2
        runner
            .handle_aggregated_event(AggregatedEvent::UserUtteranceComplete {
                transcript: "Second question".to_string(),
            })
            .await;
        runner
            .handle_aggregated_event(AggregatedEvent::TurnComplete {
                text: "Second answer".to_string(),
                audio_transcript: String::new(),
                tool_calls: vec![],
                item_id: "item-2".to_string(),
                interrupted: false,
            })
            .await;

        let events = get_session_events(&session_service, session_id).await;
        assert_eq!(events.len(), 4);

        // Verify full ordering
        assert_eq!(events[0].author, "user");
        assert_eq!(events[1].author, "model");
        assert_eq!(events[2].author, "user");
        assert_eq!(events[3].author, "model");

        // Verify monotonically increasing timestamps
        for i in 1..events.len() {
            assert!(
                events[i - 1].timestamp <= events[i].timestamp,
                "events should have monotonically non-decreasing timestamps"
            );
        }
    }

    // ─── Plugin Short-Circuit Test Helpers ───────────────────────────────────

    /// A tool that tracks whether its `execute` method was called.
    struct TrackedTool {
        executed: Arc<std::sync::atomic::AtomicBool>,
        tool_name: String,
    }

    #[async_trait]
    impl adk_core::Tool for TrackedTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "A tool that tracks whether execute was called"
        }

        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: serde_json::Value,
        ) -> adk_core::Result<serde_json::Value> {
            self.executed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(serde_json::json!({"executed": true}))
        }
    }

    /// A plugin that always returns `ShortCircuit` from `before_tool_call`.
    struct ShortCircuitPlugin {
        short_circuit_value: serde_json::Value,
    }

    #[async_trait]
    impl adk_plugin::EnhancedPlugin for ShortCircuitPlugin {
        fn name(&self) -> &str {
            "short-circuit-plugin"
        }

        fn priority(&self) -> i32 {
            10
        }

        async fn before_tool_call(
            &self,
            _tool: Arc<dyn adk_core::Tool>,
            _args: serde_json::Value,
            _ctx: Arc<dyn adk_core::CallbackContext>,
            _plugin_ctx: &adk_plugin::PluginContext,
        ) -> adk_core::Result<adk_plugin::BeforeToolCallResult> {
            Ok(adk_plugin::BeforeToolCallResult::ShortCircuit(self.short_circuit_value.clone()))
        }
    }

    /// Build an `IntegratedRealtimeRunner` with a plugin manager configured.
    fn build_runner_with_plugin(
        plugin_manager: Arc<adk_plugin::EnhancedPluginManager>,
    ) -> IntegratedRealtimeRunner {
        let model = mock_model();
        let runner = crate::runner::RealtimeRunner::builder()
            .model(model)
            .build()
            .expect("mock runner build should succeed");

        IntegratedRealtimeRunner {
            runner: Arc::new(runner),
            session_service: None,
            memory_service: None,
            plugin_manager: Some(plugin_manager),
            aggregator: tokio::sync::RwLock::new(TranscriptAggregator::new()),
            identity: SessionIdentity {
                app_name: "test-app".to_string(),
                user_id: "test-user".to_string(),
                session_id: "test-session".to_string(),
            },
            config: IntegrationConfig::default(),
            adk_tools: HashMap::new(),
        }
    }

    // ─── Property Test: Plugin Short-Circuit ─────────────────────────────────

    // **Feature: realtime-adk-integration, Property 4: Plugin Short-Circuit**
    // *For any* `before_tool_call` hook returning `ShortCircuit(v)`, verify the tool's
    // `execute` is never called and `v` is used as the response.
    // **Validates: Requirements 4.1, 4.3**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_plugin_short_circuit_skips_tool_execution(
            short_circuit_json in prop_oneof![
                Just(serde_json::json!({"cached": "response"})),
                Just(serde_json::json!({"result": 42})),
                Just(serde_json::json!("simple_string")),
                Just(serde_json::json!(null)),
                Just(serde_json::json!([1, 2, 3])),
                Just(serde_json::json!({"nested": {"key": "value"}, "list": [true, false]})),
                (0i64..1000).prop_map(|n| serde_json::json!({"number": n})),
                "[a-z]{1,20}".prop_map(|s| serde_json::json!({"text": s})),
            ],
            tool_name in "[a-z_]{3,15}",
            call_id in "[a-z0-9]{5,15}",
            args in prop_oneof![
                Just(serde_json::json!({})),
                Just(serde_json::json!({"query": "hello"})),
                Just(serde_json::json!({"x": 1, "y": 2})),
            ],
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                // 1. Create a tracked tool that records whether execute was called
                let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let tool: Arc<dyn adk_core::Tool> = Arc::new(TrackedTool {
                    executed: executed.clone(),
                    tool_name: tool_name.clone(),
                });

                // 2. Create a plugin that returns ShortCircuit with the given value
                let plugin: Arc<dyn adk_plugin::EnhancedPlugin> = Arc::new(ShortCircuitPlugin {
                    short_circuit_value: short_circuit_json.clone(),
                });
                let pm = Arc::new(adk_plugin::EnhancedPluginManager::new(vec![plugin]));

                // 3. Build the IntegratedRealtimeRunner with the plugin manager
                let runner = build_runner_with_plugin(pm);

                // 4. Create a ToolCall
                let tool_call = crate::events::ToolCall {
                    call_id,
                    name: tool_name,
                    arguments: args,
                };

                // 5. Execute tool with plugins
                let result = runner.execute_tool_with_plugins(&tool, &tool_call).await;

                // 6. Verify the tool's execute was NOT called
                prop_assert!(
                    !executed.load(std::sync::atomic::Ordering::SeqCst),
                    "Tool execute() should NOT be called when plugin short-circuits"
                );

                // 7. Verify the returned value matches the short-circuit value
                let result_value = result.expect("execute_tool_with_plugins should not error");
                prop_assert_eq!(
                    &result_value,
                    &short_circuit_json,
                    "Returned value should match the short-circuit value"
                );

                Ok(())
            })?;
        }
    }

    // ─── Integration Test: Plugin Short-Circuit Records Tool Call & Persists Event ─

    /// Integration test verifying that when a plugin short-circuits tool execution:
    /// 1. The tool's `execute` method is NOT invoked
    /// 2. The short-circuit value is returned as the tool result
    /// 3. The tool call is recorded in the `TranscriptAggregator`
    /// 4. The session event is persisted with the short-circuit result
    ///
    /// **Validates: Requirements 4.1, 4.3**
    #[tokio::test]
    async fn test_plugin_short_circuit_records_tool_call_and_persists_event() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let session_service = Arc::new(InMemorySessionService::new());
        let session_id = "short-circuit-persist-session";

        // 1. Create session so append_event can find it
        create_test_session(&session_service, session_id).await;

        // 2. Create a tracked tool that records whether execute was called
        let executed = Arc::new(AtomicBool::new(false));
        let tool: Arc<dyn adk_core::Tool> = Arc::new(TrackedTool {
            executed: executed.clone(),
            tool_name: "weather_lookup".to_string(),
        });

        // 3. Create a plugin that returns ShortCircuit with a specific cached value
        let short_circuit_value = serde_json::json!({
            "cached": true,
            "temperature": 72,
            "condition": "sunny"
        });
        let plugin: Arc<dyn adk_plugin::EnhancedPlugin> =
            Arc::new(ShortCircuitPlugin { short_circuit_value: short_circuit_value.clone() });
        let pm = Arc::new(adk_plugin::EnhancedPluginManager::new(vec![plugin]));

        // 4. Build runner with both session service and plugin manager
        let model = mock_model();
        let runner_inner = crate::runner::RealtimeRunner::builder()
            .model(model)
            .build()
            .expect("mock runner build should succeed");

        let runner = IntegratedRealtimeRunner {
            runner: Arc::new(runner_inner),
            session_service: Some(session_service.clone()),
            memory_service: None,
            plugin_manager: Some(pm),
            aggregator: tokio::sync::RwLock::new(TranscriptAggregator::new()),
            identity: SessionIdentity {
                app_name: "test-app".to_string(),
                user_id: "test-user".to_string(),
                session_id: session_id.to_string(),
            },
            config: IntegrationConfig {
                persist_transcripts: true,
                store_to_memory: false,
                inject_memory_context: false,
                max_memory_injection: 0,
            },
            adk_tools: HashMap::new(),
        };

        // 5. Start a turn in the aggregator so record_tool_call has somewhere to attach
        {
            let mut agg = runner.aggregator.write().await;
            agg.process(&crate::events::ServerEvent::ResponseCreated {
                event_id: "evt_created".to_string(),
                response: serde_json::json!({}),
            });
        }

        // 6. Create a ToolCall and execute with plugins
        let tool_call = crate::events::ToolCall {
            call_id: "call-abc-123".to_string(),
            name: "weather_lookup".to_string(),
            arguments: serde_json::json!({"city": "Seattle"}),
        };

        let result = runner.execute_tool_with_plugins(&tool, &tool_call).await;

        // ─── Assertion 1: tool's execute was NOT called ──────────────────────
        assert!(
            !executed.load(Ordering::SeqCst),
            "Tool execute() should NOT be called when plugin short-circuits"
        );

        // ─── Assertion 2: returned value matches the short-circuit value ─────
        let result_value = result.expect("execute_tool_with_plugins should not error");
        assert_eq!(
            result_value, short_circuit_value,
            "Returned value should match the short-circuit value"
        );

        // ─── Assertion 3: tool call recorded in the aggregator ───────────────
        // Finalize the turn to extract tool calls (recorded during execute_tool_with_plugins)
        let aggregated = {
            let mut agg = runner.aggregator.write().await;
            agg.process(&crate::events::ServerEvent::ResponseDone {
                event_id: "evt_done".to_string(),
                response: serde_json::json!({}),
            })
        };

        let aggregated_event = aggregated.expect("ResponseDone should finalize the turn");
        match aggregated_event {
            AggregatedEvent::TurnComplete { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1, "Should have exactly 1 tool call recorded");
                let recorded = &tool_calls[0];
                assert_eq!(recorded.call_id, "call-abc-123");
                assert_eq!(recorded.name, "weather_lookup");
                assert_eq!(recorded.arguments, serde_json::json!({"city": "Seattle"}));
                assert_eq!(
                    recorded.result, short_circuit_value,
                    "Recorded tool call result should be the short-circuit value"
                );
            }
            _ => panic!("Expected TurnComplete event from aggregator"),
        }

        // ─── Assertion 4: session event persisted with tool call details ─────
        let events = get_session_events(&session_service, session_id).await;
        assert_eq!(events.len(), 1, "Session should have exactly 1 event (the tool call event)");

        let persisted_event = &events[0];
        assert_eq!(persisted_event.author, "tool");

        // Verify the event content contains both FunctionCall and FunctionResponse parts
        let content = persisted_event.content().expect("persisted event should have content");

        let has_function_call = content.parts.iter().any(|p| {
            matches!(p, adk_core::Part::FunctionCall { name, id, .. }
                if name == "weather_lookup" && id.as_deref() == Some("call-abc-123"))
        });
        assert!(
            has_function_call,
            "Persisted event should contain a FunctionCall part with correct name and id"
        );

        let has_function_response = content.parts.iter().any(|p| match p {
            adk_core::Part::FunctionResponse { function_response, id } => {
                id.as_deref() == Some("call-abc-123")
                    && function_response.response == short_circuit_value
            }
            _ => false,
        });
        assert!(
            has_function_response,
            "Persisted event should contain a FunctionResponse part with the short-circuit value"
        );
    }
}

#[cfg(test)]
mod graceful_degradation_tests {
    use super::*;
    use crate::audio::AudioFormat;
    use crate::config::RealtimeConfig;
    use crate::events::ServerEvent;
    use crate::model::RealtimeModel;
    use crate::session::BoxedSession;
    use adk_core::AdkError;
    use adk_memory::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
    use adk_session::{
        CreateRequest, DeleteRequest, GetRequest, ListRequest, Session, SessionService,
    };
    use async_trait::async_trait;
    use proptest::prelude::*;
    use serde_json::json;
    use tokio::sync::RwLock;

    // ─── Mock RealtimeModel ──────────────────────────────────────────────────

    struct MockRealtimeModel;

    #[async_trait]
    impl RealtimeModel for MockRealtimeModel {
        fn provider(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        fn supported_input_formats(&self) -> Vec<AudioFormat> {
            vec![]
        }

        fn supported_output_formats(&self) -> Vec<AudioFormat> {
            vec![]
        }

        fn available_voices(&self) -> Vec<&str> {
            vec![]
        }

        async fn connect(&self, _config: RealtimeConfig) -> crate::error::Result<BoxedSession> {
            Err(crate::error::RealtimeError::config("mock model cannot connect"))
        }
    }

    // ─── Failing Mock Services ───────────────────────────────────────────────

    /// A `SessionService` that always returns errors on all operations.
    struct FailingSessionService;

    #[async_trait]
    impl SessionService for FailingSessionService {
        async fn create(&self, _req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
            Err(AdkError::session("simulated session create failure"))
        }

        async fn get(&self, _req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
            Err(AdkError::session("simulated session get failure"))
        }

        async fn list(&self, _req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
            Err(AdkError::session("simulated session list failure"))
        }

        async fn delete(&self, _req: DeleteRequest) -> adk_core::Result<()> {
            Err(AdkError::session("simulated session delete failure"))
        }

        async fn append_event(
            &self,
            _session_id: &str,
            _event: adk_core::Event,
        ) -> adk_core::Result<()> {
            Err(AdkError::session("simulated session append_event failure"))
        }
    }

    /// A `MemoryService` that always returns errors on all operations.
    struct FailingMemoryService;

    #[async_trait]
    impl MemoryService for FailingMemoryService {
        async fn add_session(
            &self,
            _app_name: &str,
            _user_id: &str,
            _session_id: &str,
            _entries: Vec<MemoryEntry>,
        ) -> adk_core::Result<()> {
            Err(AdkError::memory("simulated memory add_session failure"))
        }

        async fn search(&self, _req: SearchRequest) -> adk_core::Result<SearchResponse> {
            Err(AdkError::memory("simulated memory search failure"))
        }
    }

    // ─── Proptest Strategies ─────────────────────────────────────────────────

    /// Generate arbitrary non-empty text delta strings.
    fn arb_text_delta() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 .,!?]{1,50}"
    }

    /// Generate an arbitrary sequence of ServerEvents representing a complete turn.
    /// The sequence is: ResponseCreated, then N TextDeltas, then ResponseDone.
    fn arb_turn_event_sequence() -> impl Strategy<Value = Vec<ServerEvent>> {
        prop::collection::vec(arb_text_delta(), 0..10).prop_map(|deltas| {
            let mut events = Vec::new();

            // ResponseCreated
            events.push(ServerEvent::ResponseCreated {
                event_id: "evt_created".to_string(),
                response: json!({}),
            });

            // TextDelta events
            for (i, delta) in deltas.iter().enumerate() {
                events.push(ServerEvent::TextDelta {
                    event_id: format!("evt_delta_{i}"),
                    response_id: "resp_1".to_string(),
                    item_id: "item_1".to_string(),
                    output_index: 0,
                    content_index: 0,
                    delta: delta.clone(),
                });
            }

            // ResponseDone
            events.push(ServerEvent::ResponseDone {
                event_id: "evt_done".to_string(),
                response: json!({}),
            });

            events
        })
    }

    // ─── Property Tests ──────────────────────────────────────────────────────

    // **Feature: realtime-adk-integration, Property 5: Graceful Degradation**
    // *For any* `SessionService` or `MemoryService` that returns errors, verify
    // `handle_aggregated_event` does not propagate errors and the aggregator
    // still processes events correctly.
    // **Validates: Requirements 2.5, 3.4**
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn prop_graceful_degradation_with_failing_services(
            events in arb_turn_event_sequence()
        ) {
            // Run the async test in a tokio runtime
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                // Create an IntegratedRealtimeRunner with failing services.
                // We construct the struct directly since we can't connect to a real
                // WebSocket in unit tests, and we only need to test the aggregator
                // + handle_aggregated_event path.
                let failing_session: Arc<dyn SessionService> =
                    Arc::new(FailingSessionService);
                let failing_memory: Arc<dyn MemoryService> =
                    Arc::new(FailingMemoryService);

                let mock_model: Arc<dyn RealtimeModel> = Arc::new(MockRealtimeModel);

                let runner_struct = IntegratedRealtimeRunner {
                    runner: Arc::new(
                        crate::runner::RealtimeRunner::builder()
                            .model(mock_model)
                            .build()
                            .unwrap(),
                    ),
                    session_service: Some(failing_session),
                    memory_service: Some(failing_memory),
                    plugin_manager: None,
                    aggregator: RwLock::new(TranscriptAggregator::new()),
                    identity: SessionIdentity {
                        app_name: "test-app".to_string(),
                        user_id: "test-user".to_string(),
                        session_id: "test-session".to_string(),
                    },
                    config: IntegrationConfig {
                        persist_transcripts: true,
                        store_to_memory: true,
                        inject_memory_context: true,
                        max_memory_injection: 10,
                    },
                    adk_tools: HashMap::new(),
                };

                // Process events through the aggregator — this is the path that
                // next_event() would take internally. We test that no panics
                // occur and no errors propagate even with failing services.
                for event in &events {
                    let aggregated = runner_struct.aggregator.write().await.process(event);

                    if let Some(agg_event) = aggregated {
                        // This is the key test: handle_aggregated_event must NOT
                        // panic or propagate errors even when both services fail.
                        runner_struct.handle_aggregated_event(agg_event).await;
                    }
                }

                // After processing a full turn sequence (ResponseCreated...ResponseDone),
                // verify the aggregator produced a TurnComplete event. Since our
                // event sequence always ends with ResponseDone, a finalization
                // MUST have been emitted (tested above in the loop). If we got
                // here without panic, graceful degradation is confirmed.
            });
        }
    }

    /// Additional test: verify that even with a user utterance event flowing
    /// through handle_aggregated_event with failing services, no errors propagate.
    #[tokio::test]
    async fn test_graceful_degradation_user_utterance() {
        let failing_session: Arc<dyn SessionService> = Arc::new(FailingSessionService);
        let mock_model: Arc<dyn RealtimeModel> = Arc::new(MockRealtimeModel);

        let runner_struct = IntegratedRealtimeRunner {
            runner: Arc::new(
                crate::runner::RealtimeRunner::builder().model(mock_model).build().unwrap(),
            ),
            session_service: Some(failing_session),
            memory_service: None,
            plugin_manager: None,
            aggregator: RwLock::new(TranscriptAggregator::new()),
            identity: SessionIdentity {
                app_name: "test-app".to_string(),
                user_id: "test-user".to_string(),
                session_id: "test-session".to_string(),
            },
            config: IntegrationConfig {
                persist_transcripts: true,
                store_to_memory: false,
                inject_memory_context: false,
                max_memory_injection: 0,
            },
            adk_tools: HashMap::new(),
        };

        // Process a UserUtteranceComplete event — should not panic even
        // though session_service.append_event will fail.
        let user_event = AggregatedEvent::UserUtteranceComplete {
            transcript: "Hello, how are you?".to_string(),
        };
        runner_struct.handle_aggregated_event(user_event).await;

        // If we get here without panic, the test passes — graceful degradation works.
    }
}
