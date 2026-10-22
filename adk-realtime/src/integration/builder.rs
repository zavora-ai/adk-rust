//! # Integrated Realtime Runner Builder
//!
//! Provides `IntegratedRealtimeRunnerBuilder` — a builder API for constructing
//! `IntegratedRealtimeRunner` with optional ADK services (session, memory, plugins),
//! ADK tools (auto-bridged via `ToolBridgeAdapter`), and native `ToolHandler` tools.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::Tool;
use adk_memory::MemoryService;
use adk_plugin::EnhancedPluginManager;
use adk_session::SessionService;

use crate::config::{RealtimeConfig, ToolDefinition};
use crate::error::RealtimeError;
use crate::metering::RealtimePricing;
use crate::model::BoxedModel;
use crate::runner::{EventHandler, RealtimeRunner, RunnerConfig, ToolHandler};

use super::context::IdentityToolContextFactory;
use super::tool_bridge::ToolBridgeAdapter;
use super::{IntegrationConfig, SessionIdentity};

/// Builder for constructing an [`IntegratedRealtimeRunner`](super::IntegratedRealtimeRunner).
///
/// Provides a fluent API for configuring the realtime runner with optional ADK
/// services (session, memory, plugins), ADK tools (auto-bridged via
/// [`ToolBridgeAdapter`]), and native
/// [`ToolHandler`] tools.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::integration::builder::IntegratedRealtimeRunnerBuilder;
///
/// let runner = IntegratedRealtimeRunnerBuilder::new()
///     .model(model)
///     .identity("my-app", "user-1", "session-1")
///     .session_service(session_svc)
///     .memory_service(memory_svc)
///     .adk_tool(rag_tool)
///     .build()?;
/// ```
pub struct IntegratedRealtimeRunnerBuilder {
    /// The realtime model (required for build).
    pub(crate) model: Option<BoxedModel>,
    /// Realtime session configuration.
    pub(crate) config: RealtimeConfig,
    /// Runner execution configuration.
    pub(crate) runner_config: RunnerConfig,
    /// Optional session service for persistence.
    pub(crate) session_service: Option<Arc<dyn SessionService>>,
    /// Optional memory service for RAG/search.
    pub(crate) memory_service: Option<Arc<dyn MemoryService>>,
    /// Optional plugin manager for lifecycle hooks.
    pub(crate) plugin_manager: Option<Arc<EnhancedPluginManager>>,
    /// ADK tools to be auto-bridged via `ToolBridgeAdapter`.
    pub(crate) adk_tools: Vec<Arc<dyn Tool>>,
    /// Native tools registered directly as `ToolHandler` implementations.
    pub(crate) native_tools: HashMap<String, (ToolDefinition, Arc<dyn ToolHandler>)>,
    /// The first native tool name registered twice; `build` rejects it.
    pub(crate) duplicate_tool: Option<String>,
    /// Optional event handler for raw realtime events.
    pub(crate) event_handler: Option<Arc<dyn EventHandler>>,
    /// Session identity triple (required for build).
    pub(crate) identity: Option<SessionIdentity>,
    /// Integration-layer configuration (transcript persistence, memory injection, etc.).
    pub(crate) integration_config: IntegrationConfig,
    /// Policy, kill switch, and confirmation applied to every dispatched tool call.
    pub(crate) tool_governance: Option<adk_core::RunConfig>,
    /// How long one tool call may run.
    pub(crate) tool_timeout: std::time::Duration,
    /// Screens every tool result before the model and the session see it.
    pub(crate) tool_result_screen: Option<Arc<dyn adk_core::ToolResultScreen>>,
    /// Whether tool and plugin error messages reach the model.
    pub(crate) expose_tool_error_messages: bool,
    /// The identity the server authenticated for the session, if any.
    pub(crate) request_context: Option<adk_core::RequestContext>,
    /// Output guardrails on the model's text and transcripts.
    #[cfg(feature = "guardrails")]
    pub(crate) output_guardrails: Option<adk_guardrail::GuardrailSet>,
    /// Prices each response's usage for the spend ledger.
    pub(crate) pricing: Option<RealtimePricing>,
    /// Whether `next_event` runs tool dispatch on its own task.
    pub(crate) concurrent_tool_dispatch: bool,
    /// The agent tool calls are governed, metered and held under.
    pub(crate) agent_name: String,
}

impl Default for IntegratedRealtimeRunnerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IntegratedRealtimeRunnerBuilder {
    /// Creates a new builder with all fields set to defaults.
    ///
    /// The `model` and `identity` fields must be set before calling `build()`.
    pub fn new() -> Self {
        Self {
            model: None,
            config: RealtimeConfig::default(),
            runner_config: RunnerConfig::default(),
            session_service: None,
            memory_service: None,
            plugin_manager: None,
            adk_tools: Vec::new(),
            native_tools: HashMap::new(),
            duplicate_tool: None,
            event_handler: None,
            identity: None,
            integration_config: IntegrationConfig::default(),
            tool_governance: None,
            tool_timeout: crate::runner::DEFAULT_REALTIME_TOOL_TIMEOUT,
            tool_result_screen: None,
            expose_tool_error_messages: false,
            request_context: None,
            #[cfg(feature = "guardrails")]
            output_guardrails: None,
            pricing: None,
            concurrent_tool_dispatch: false,
            agent_name: crate::runner::DEFAULT_REALTIME_AGENT_NAME.to_string(),
        }
    }

    /// Sets the realtime model to use for the session.
    ///
    /// This is required — `build()` will return an error if no model is set.
    ///
    /// # Arguments
    ///
    /// * `model` - A shared realtime model instance.
    pub fn model(mut self, model: BoxedModel) -> Self {
        self.model = Some(model);
        self
    }

    /// Sets the realtime session configuration (instructions, voice, VAD, etc.).
    ///
    /// # Arguments
    ///
    /// * `config` - The realtime configuration.
    pub fn config(mut self, config: RealtimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets the runner execution configuration.
    ///
    /// # Arguments
    ///
    /// * `runner_config` - Controls auto-execution, concurrency, etc.
    pub fn runner_config(mut self, runner_config: RunnerConfig) -> Self {
        self.runner_config = runner_config;
        self
    }

    /// Sets the session service for transcript persistence.
    ///
    /// When configured, completed turns are automatically persisted to the session
    /// history, enabling cross-session continuity.
    ///
    /// # Arguments
    ///
    /// * `service` - A shared session service instance.
    pub fn session_service(mut self, service: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(service);
        self
    }

    /// Sets the memory service for RAG search and memory injection.
    ///
    /// When configured, memory entries can be injected into the system instruction
    /// at session start, and completed turns can be stored for future retrieval.
    ///
    /// # Arguments
    ///
    /// * `service` - A shared memory service instance.
    pub fn memory_service(mut self, service: Arc<dyn MemoryService>) -> Self {
        self.memory_service = Some(service);
        self
    }

    /// Sets the plugin manager for lifecycle hooks (before/after tool calls, etc.).
    ///
    /// When configured, tool calls are intercepted by the plugin pipeline,
    /// enabling short-circuit responses, argument modification, and result
    /// transformation.
    ///
    /// # Arguments
    ///
    /// * `pm` - A shared enhanced plugin manager instance.
    pub fn plugin_manager(mut self, pm: Arc<EnhancedPluginManager>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Sets the session identity triple (`app_name`, `user_id`, `session_id`).
    ///
    /// This is required — `build()` will return an error if no identity is set.
    /// The identity scopes all interactions with session, memory, and plugin services.
    ///
    /// # Arguments
    ///
    /// * `app_name` - The application name.
    /// * `user_id` - The user identifier.
    /// * `session_id` - The unique session identifier.
    pub fn identity(
        mut self,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        self.identity = Some(SessionIdentity {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
        });
        self
    }

    /// Registers an ADK tool for automatic bridging via [`ToolBridgeAdapter`].
    ///
    /// The tool's name, description, and parameter schema are automatically
    /// extracted and registered with the realtime provider. At execution time,
    /// the bridge creates a `ToolContext` scoped to the current session.
    ///
    /// # Arguments
    ///
    /// * `tool` - A shared ADK tool instance.
    pub fn adk_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.adk_tools.push(tool);
        self
    }

    /// Registers a native tool with its definition and handler.
    ///
    /// Native tools bypass the `ToolBridgeAdapter` and are executed directly
    /// via their [`ToolHandler`] implementation. Use this for tools that are
    /// already implemented against the realtime `ToolHandler` interface.
    ///
    /// # Arguments
    ///
    /// * `definition` - The tool definition (name, description, parameters schema).
    /// * `handler` - The tool handler implementation.
    pub fn tool(mut self, definition: ToolDefinition, handler: impl ToolHandler + 'static) -> Self {
        let name = definition.name.clone();
        if self.native_tools.insert(name.clone(), (definition, Arc::new(handler))).is_some() {
            self.duplicate_tool.get_or_insert(name);
        }
        self
    }

    /// Sets the event handler for raw realtime server events.
    ///
    /// The event handler receives audio deltas, text deltas, transcripts,
    /// and other events from the realtime provider.
    ///
    /// # Arguments
    ///
    /// * `handler` - A shared event handler instance.
    pub fn event_handler(mut self, handler: Arc<dyn EventHandler>) -> Self {
        self.event_handler = Some(handler);
        self
    }

    /// Sets the integration-layer configuration.
    ///
    /// Controls which ADK service interactions are performed automatically
    /// (transcript persistence, memory storage, memory injection).
    ///
    /// # Arguments
    ///
    /// * `config` - The integration configuration.
    pub fn integration_config(mut self, config: IntegrationConfig) -> Self {
        self.integration_config = config;
        self
    }

    /// Governs every tool call with `config`'s tool policy, kill switches, confirmation
    /// handler, approvals, and approval store, scoped to this runner's identity. ADK tools
    /// are authorized after the plugin pipeline rewrites their arguments, and every tool
    /// context answers [`run_config`](adk_core::ToolContext::run_config) with `config`.
    /// A call the governed path holds, or that a tool's delegate holds, is surfaced as
    /// [`ServerEvent::ToolConfirmationRequested`](crate::events::ServerEvent::ToolConfirmationRequested)
    /// and recorded in the session; see
    /// [`IntegratedRealtimeRunner::resume_held_calls`](super::IntegratedRealtimeRunner::resume_held_calls).
    /// With a `spend_ledger`, every response is metered under the identity's app name.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::{DeclarativePolicy, RunConfig};
    ///
    /// let runner = IntegratedRealtimeRunner::builder()
    ///     .model(model)
    ///     .identity("voice", "user-1", "session-1")
    ///     .tool_governance(
    ///         RunConfig::builder()
    ///             .tool_policy(Arc::new(DeclarativePolicy::builder().allow_read_only().build()))
    ///             .build(),
    ///     )
    ///     .build()?;
    /// ```
    pub fn tool_governance(mut self, config: adk_core::RunConfig) -> Self {
        self.tool_governance = Some(config);
        self
    }

    /// Sets how long a tool may run before its call is answered with a timeout error.
    pub fn tool_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Screens every tool result before the model and the session see it.
    ///
    /// For an ADK tool the screen runs after the plugin pipeline, before the tool event is
    /// persisted and the result is sent; for a native handler it runs as
    /// [`RealtimeRunnerBuilder::tool_result_screen`](crate::runner::RealtimeRunnerBuilder::tool_result_screen)
    /// describes. A result it withholds, or cannot decide on, is replaced by
    /// [`result_blocked_response`](adk_core::result_blocked_response).
    pub fn tool_result_screen(mut self, screen: Arc<dyn adk_core::ToolResultScreen>) -> Self {
        self.tool_result_screen = Some(screen);
        self
    }

    /// Sends the full message of tool and plugin errors to the model, which by default sees
    /// their component, category, and code only.
    pub fn expose_tool_error_messages(mut self, expose: bool) -> Self {
        self.expose_tool_error_messages = expose;
        self
    }

    /// Runs every tool call as the identity the server authenticated for this session (spec
    /// 3.1 §7.17): tool contexts answer
    /// [`is_authenticated`](adk_core::ReadonlyContext::is_authenticated) with `true` and
    /// [`user_scopes`](adk_core::ToolContext::user_scopes) with the context's scopes, so a
    /// policy that trusts only authenticated users (such as a harness's standing approvals)
    /// treats a voice session as the server does. Without it, tool contexts report an
    /// unauthenticated user. The context's `user_id` must be the [`identity`](Self::identity)
    /// user; [`build`](Self::build) refuses a mismatch.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = IntegratedRealtimeRunner::builder()
    ///     .model(model)
    ///     .identity("voice", "user-1", "session-1")
    ///     .request_context(authenticated) // from the server's RequestContextExtractor
    ///     .build()?;
    /// ```
    pub fn request_context(mut self, request_context: adk_core::RequestContext) -> Self {
        self.request_context = Some(request_context);
        self
    }

    /// Screens every completed model turn — its text, or its audio transcript when it
    /// produced no text — with `guardrails` before it reaches the session and memory, as
    /// `LlmAgent` screens final text. A transform replaces the persisted text; a block
    /// interrupts the response, persists nothing of the turn, and makes
    /// [`next_event`](super::IntegratedRealtimeRunner::next_event) return
    /// [`crate::error::RealtimeError::OutputBlocked`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_guardrail::{GuardrailSet, PiiRedactor};
    ///
    /// let runner = IntegratedRealtimeRunner::builder()
    ///     .model(model)
    ///     .identity("voice", "user-1", "session-1")
    ///     .output_guardrails(GuardrailSet::new().with(PiiRedactor::new()))
    ///     .build()?;
    /// ```
    #[cfg(feature = "guardrails")]
    pub fn output_guardrails(mut self, guardrails: adk_guardrail::GuardrailSet) -> Self {
        self.output_guardrails = Some(guardrails);
        self
    }

    /// Prices each response's reported usage, so the run's spend ledger is charged the
    /// usage's price rather than the per-call estimate. Needs
    /// [`tool_governance`](Self::tool_governance) with a `spend_ledger`.
    pub fn realtime_pricing(mut self, pricing: RealtimePricing) -> Self {
        self.pricing = Some(pricing);
        self
    }

    /// Runs each tool dispatch on its own task, so
    /// [`next_event`](super::IntegratedRealtimeRunner::next_event) keeps reading provider
    /// events — the user's transcript included — while a tool runs or waits for a
    /// confirmation handler. The follow-up response is sent once the dispatching response
    /// has closed and every tool is in, as [`RealtimeRunner::run`] does. Off by default:
    /// `next_event` then dispatches inline and returns after the result is sent.
    pub fn concurrent_tool_dispatch(mut self, concurrent: bool) -> Self {
        self.concurrent_tool_dispatch = concurrent;
        self
    }

    /// Names the agent every tool call is governed, metered and held under: the gate's
    /// agent name, the acting agent of each call, the agent tool contexts report, and the
    /// author of the session events that record a hold. Defaults to
    /// [`DEFAULT_REALTIME_AGENT_NAME`](crate::runner::DEFAULT_REALTIME_AGENT_NAME). A
    /// surface that runs an existing agent's tools sets that agent's name, so the agent's
    /// tool policy classifies its own tools (spec 3.1 §7.17).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = IntegratedRealtimeRunner::builder()
    ///     .model(model)
    ///     .identity("adk-rust", "ana", "voice-1")
    ///     .agent_name("harness")
    ///     .tool_governance(harness.run_config())
    ///     .build()?;
    /// ```
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    /// Builds the [`IntegratedRealtimeRunner`](super::IntegratedRealtimeRunner).
    ///
    /// Validates required fields, bridges ADK tools via
    /// [`ToolBridgeAdapter`], builds the underlying
    /// [`RealtimeRunner`], and constructs the integrated runner
    /// with all configured services.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`](crate::error::RealtimeError::ConfigError) if:
    /// - `model` was not set
    /// - `identity` was not set
    /// - a [`request_context`](Self::request_context) names a user other than the identity's
    /// - two tools, native or bridged, share a name
    ///
    /// Also propagates any errors from the underlying `RealtimeRunner::builder().build()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = IntegratedRealtimeRunnerBuilder::new()
    ///     .model(model)
    ///     .identity("my-app", "user-1", "session-1")
    ///     .build()?;
    /// ```
    pub fn build(self) -> crate::error::Result<super::IntegratedRealtimeRunner> {
        let model = self.model.ok_or_else(|| RealtimeError::config("Model is required"))?;
        let identity = self.identity.ok_or_else(|| {
            RealtimeError::config("Identity (app_name, user_id, session_id) is required")
        })?;

        if let Some(request_context) = &self.request_context
            && request_context.user_id != identity.user_id
        {
            return Err(RealtimeError::config(format!(
                "the request context authenticates user '{}' but the session identity is user \
                 '{}'; pass the authenticated user to identity()",
                request_context.user_id, identity.user_id
            )));
        }

        // Create context factory for ADK tools
        let context_factory = Arc::new(IdentityToolContextFactory {
            identity: identity.clone(),
            memory_service: self.memory_service.clone(),
            request_context: self.request_context.clone(),
            run_config: self.tool_governance.clone(),
            agent_name: self.agent_name.clone(),
        });

        // Bridge ADK tools via ToolBridgeAdapter
        let mut all_tools = self.native_tools;
        let mut duplicate_tool = self.duplicate_tool;
        for tool in &self.adk_tools {
            let def = ToolBridgeAdapter::definition(tool.as_ref());
            let adapter = Arc::new(
                ToolBridgeAdapter::new(tool.clone(), context_factory.clone())
                    .with_exposed_error_messages(self.expose_tool_error_messages),
            );
            let name = def.name.clone();
            if all_tools.insert(name.clone(), (def, adapter as Arc<dyn ToolHandler>)).is_some() {
                duplicate_tool.get_or_insert(name);
            }
        }
        if let Some(name) = duplicate_tool {
            return Err(RealtimeError::config(format!(
                "two tools are registered as '{name}'; the model calls tools by name, so only \
                 one could ever run. Rename one of them or register it once."
            )));
        }

        // Build underlying RealtimeRunner
        let mut runner_builder = RealtimeRunner::builder()
            .model(model)
            .config(self.config)
            .runner_config(self.runner_config)
            .tool_timeout(self.tool_timeout)
            .expose_tool_error_messages(self.expose_tool_error_messages)
            .agent_name(self.agent_name);
        if let Some(screen) = self.tool_result_screen {
            runner_builder = runner_builder.tool_result_screen(screen);
        }
        if let Some(pricing) = self.pricing {
            runner_builder = runner_builder.realtime_pricing(pricing);
        }
        if let Some(config) = self.tool_governance {
            let scope = adk_core::ApprovalScope::new(
                &identity.app_name,
                &identity.user_id,
                &identity.session_id,
            );
            runner_builder = runner_builder.tool_governance(config, scope);
        }

        for (_, (def, handler)) in all_tools {
            runner_builder = runner_builder.tool_arc(def, handler);
        }

        if let Some(handler) = self.event_handler {
            runner_builder = runner_builder.event_handler_arc(handler);
        }

        let runner = Arc::new(runner_builder.build()?);

        // Kept so the live event path can run them through the policy pipeline rather than
        // only through the bridge adapter.
        let adk_tools_by_name =
            self.adk_tools.iter().map(|tool| (tool.name().to_string(), Arc::clone(tool))).collect();

        Ok(super::IntegratedRealtimeRunner {
            inner: Arc::new(super::Inner {
                runner,
                session_service: self.session_service,
                memory_service: self.memory_service,
                plugin_manager: self.plugin_manager,
                aggregator: tokio::sync::RwLock::new(super::transcript::TranscriptAggregator::new()),
                identity,
                config: self.integration_config,
                adk_tools: adk_tools_by_name,
                request_context: self.request_context,
                pending_events: std::sync::Mutex::new(std::collections::VecDeque::new()),
                connected_calls: std::sync::Mutex::new(std::collections::HashSet::new()),
                concurrent_dispatch: self.concurrent_tool_dispatch,
                #[cfg(feature = "guardrails")]
                output_guardrails: self.output_guardrails,
            }),
        })
    }
}
