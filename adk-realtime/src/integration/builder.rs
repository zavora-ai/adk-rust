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
use crate::model::BoxedModel;
use crate::runner::{EventHandler, RealtimeRunner, RunnerConfig, ToolHandler};

use super::context::DefaultToolContextFactory;
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
    /// Optional event handler for raw realtime events.
    pub(crate) event_handler: Option<Arc<dyn EventHandler>>,
    /// Session identity triple (required for build).
    pub(crate) identity: Option<SessionIdentity>,
    /// Integration-layer configuration (transcript persistence, memory injection, etc.).
    pub(crate) integration_config: IntegrationConfig,
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
            event_handler: None,
            identity: None,
            integration_config: IntegrationConfig::default(),
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
        self.native_tools.insert(name, (definition, Arc::new(handler)));
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

        // Create context factory for ADK tools
        let context_factory = Arc::new(DefaultToolContextFactory {
            identity: identity.clone(),
            memory_service: self.memory_service.clone(),
        });

        // Bridge ADK tools via ToolBridgeAdapter
        let mut all_tools = self.native_tools;
        for tool in &self.adk_tools {
            let def = ToolBridgeAdapter::definition(tool.as_ref());
            let adapter = Arc::new(ToolBridgeAdapter::new(tool.clone(), context_factory.clone()));
            all_tools.insert(def.name.clone(), (def, adapter as Arc<dyn ToolHandler>));
        }

        // Build underlying RealtimeRunner
        let mut runner_builder = RealtimeRunner::builder()
            .model(model)
            .config(self.config)
            .runner_config(self.runner_config);

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
            runner,
            session_service: self.session_service,
            memory_service: self.memory_service,
            plugin_manager: self.plugin_manager,
            aggregator: tokio::sync::RwLock::new(super::transcript::TranscriptAggregator::new()),
            identity,
            config: self.integration_config,
            adk_tools: adk_tools_by_name,
        })
    }
}
