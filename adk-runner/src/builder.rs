//! Typestate builder for [`RunnerConfig`] / [`Runner`].
//!
//! The builder enforces at compile time that the three required fields
//! (`app_name`, `agent`, `session_service`) are set before `build()` is
//! callable.
//!
//! # Example
//!
//! ```rust,ignore
//! let runner = Runner::builder()
//!     .app_name("my-app")
//!     .agent(agent)
//!     .session_service(session_service)
//!     .memory_service(memory)
//!     .build()?;
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use adk_artifact::ArtifactService;
use adk_core::{Agent, CacheCapable, ContextCacheConfig, Memory, Result, RunConfig};
use adk_plugin::PluginManager;
use adk_session::SessionService;
use tokio_util::sync::CancellationToken;

use crate::runner::{Runner, RunnerConfig};

// ---------------------------------------------------------------------------
// Typestate marker types
// ---------------------------------------------------------------------------

/// Marker: `app_name` has not been set.
pub struct NoAppName;
/// Marker: `app_name` has been set.
pub struct HasAppName;
/// Marker: `agent` has not been set.
pub struct NoAgent;
/// Marker: `agent` has been set.
pub struct HasAgent;
/// Marker: `session_service` has not been set.
pub struct NoSessionService;
/// Marker: `session_service` has been set.
pub struct HasSessionService;

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// A typestate builder for constructing a [`Runner`].
///
/// The three type parameters track whether the required fields have been
/// provided. `build()` is only available when all three are `Has*`.
pub struct RunnerConfigBuilder<A, G, S> {
    app_name: Option<String>,
    agent: Option<Arc<dyn Agent>>,
    session_service: Option<Arc<dyn SessionService>>,
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn Memory>>,
    plugin_manager: Option<Arc<PluginManager>>,
    run_config: Option<RunConfig>,
    compaction_config: Option<adk_core::EventsCompactionConfig>,
    context_cache_config: Option<ContextCacheConfig>,
    cache_capable: Option<Arc<dyn CacheCapable>>,
    request_context: Option<adk_core::RequestContext>,
    cancellation_token: Option<CancellationToken>,
    _marker: PhantomData<(A, G, S)>,
}

impl RunnerConfigBuilder<NoAppName, NoAgent, NoSessionService> {
    /// Create a new builder with all fields unset and defaults applied.
    pub fn new() -> Self {
        Self {
            app_name: None,
            agent: None,
            session_service: None,
            artifact_service: None,
            memory_service: None,
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: None,
            _marker: PhantomData,
        }
    }
}

impl Default for RunnerConfigBuilder<NoAppName, NoAgent, NoSessionService> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Required-field setters (transition type state)
// ---------------------------------------------------------------------------

impl<A, G, S> RunnerConfigBuilder<A, G, S> {
    /// Set the application name (required).
    pub fn app_name(self, name: impl Into<String>) -> RunnerConfigBuilder<HasAppName, G, S> {
        RunnerConfigBuilder {
            app_name: Some(name.into()),
            agent: self.agent,
            session_service: self.session_service,
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            _marker: PhantomData,
        }
    }

    /// Set the root agent (required).
    pub fn agent(self, agent: Arc<dyn Agent>) -> RunnerConfigBuilder<A, HasAgent, S> {
        RunnerConfigBuilder {
            app_name: self.app_name,
            agent: Some(agent),
            session_service: self.session_service,
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            _marker: PhantomData,
        }
    }

    /// Set the session service (required).
    pub fn session_service(
        self,
        service: Arc<dyn SessionService>,
    ) -> RunnerConfigBuilder<A, G, HasSessionService> {
        RunnerConfigBuilder {
            app_name: self.app_name,
            agent: self.agent,
            session_service: Some(service),
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            _marker: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Optional-field setters (no type-state change)
// ---------------------------------------------------------------------------

impl<A, G, S> RunnerConfigBuilder<A, G, S> {
    /// Set the artifact service (optional).
    pub fn artifact_service(mut self, service: Arc<dyn ArtifactService>) -> Self {
        self.artifact_service = Some(service);
        self
    }

    /// Set the memory service (optional).
    pub fn memory_service(mut self, service: Arc<dyn Memory>) -> Self {
        self.memory_service = Some(service);
        self
    }

    /// Set the plugin manager (optional).
    pub fn plugin_manager(mut self, manager: Arc<PluginManager>) -> Self {
        self.plugin_manager = Some(manager);
        self
    }

    /// Set the run configuration (optional).
    pub fn run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }

    /// Set the events compaction configuration (optional).
    pub fn compaction_config(mut self, config: adk_core::EventsCompactionConfig) -> Self {
        self.compaction_config = Some(config);
        self
    }

    /// Set the context cache configuration (optional).
    pub fn context_cache_config(mut self, config: ContextCacheConfig) -> Self {
        self.context_cache_config = Some(config);
        self
    }

    /// Set the cache-capable model reference (optional).
    pub fn cache_capable(mut self, model: Arc<dyn CacheCapable>) -> Self {
        self.cache_capable = Some(model);
        self
    }

    /// Set the request context from auth middleware (optional).
    pub fn request_context(mut self, ctx: adk_core::RequestContext) -> Self {
        self.request_context = Some(ctx);
        self
    }

    /// Set a cooperative cancellation token (optional).
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }
}

// ---------------------------------------------------------------------------
// build() — only available when all required fields are set
// ---------------------------------------------------------------------------

impl RunnerConfigBuilder<HasAppName, HasAgent, HasSessionService> {
    /// Consume the builder and create a [`Runner`].
    ///
    /// Delegates to [`Runner::new()`] internally.
    ///
    /// # Errors
    ///
    /// Returns an error if `Runner::new()` fails (e.g. invalid `app_name`).
    pub fn build(self) -> Result<Runner> {
        let config = RunnerConfig {
            // SAFETY: typestate guarantees these are `Some`.
            app_name: self.app_name.expect("typestate guarantees app_name is set"),
            agent: self.agent.expect("typestate guarantees agent is set"),
            session_service: self
                .session_service
                .expect("typestate guarantees session_service is set"),
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
        };
        Runner::new(config)
    }
}
