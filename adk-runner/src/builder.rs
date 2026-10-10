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

#[cfg(feature = "artifacts")]
use adk_artifact::ArtifactService;
use adk_core::{Agent, CacheCapable, ContextCacheConfig, Memory, Result, RunBudget, RunConfig};
#[cfg(feature = "plugins")]
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
    #[cfg(feature = "artifacts")]
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn Memory>>,
    #[cfg(feature = "plugins")]
    plugin_manager: Option<Arc<PluginManager>>,
    run_config: Option<RunConfig>,
    compaction_config: Option<adk_core::EventsCompactionConfig>,
    context_cache_config: Option<ContextCacheConfig>,
    cache_capable: Option<Arc<dyn CacheCapable>>,
    request_context: Option<adk_core::RequestContext>,
    cancellation_token: Option<CancellationToken>,
    intra_compaction_config: Option<adk_core::IntraCompactionConfig>,
    intra_compaction_summarizer: Option<Arc<dyn adk_core::BaseEventsSummarizer>>,
    #[cfg(feature = "context-compaction")]
    context_compaction: Option<crate::compaction::CompactionConfig>,
    budget: Option<RunBudget>,
    _marker: PhantomData<(A, G, S)>,
}

impl RunnerConfigBuilder<NoAppName, NoAgent, NoSessionService> {
    /// Create a new builder with all fields unset and defaults applied.
    pub fn new() -> Self {
        Self {
            app_name: None,
            agent: None,
            session_service: None,
            #[cfg(feature = "artifacts")]
            artifact_service: None,
            memory_service: None,
            #[cfg(feature = "plugins")]
            plugin_manager: None,
            run_config: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            request_context: None,
            cancellation_token: None,
            intra_compaction_config: None,
            intra_compaction_summarizer: None,
            #[cfg(feature = "context-compaction")]
            context_compaction: None,
            budget: None,
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
            #[cfg(feature = "artifacts")]
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            intra_compaction_config: self.intra_compaction_config,
            intra_compaction_summarizer: self.intra_compaction_summarizer,
            #[cfg(feature = "context-compaction")]
            context_compaction: self.context_compaction,
            budget: self.budget,
            _marker: PhantomData,
        }
    }

    /// Set the root agent (required).
    pub fn agent(self, agent: Arc<dyn Agent>) -> RunnerConfigBuilder<A, HasAgent, S> {
        RunnerConfigBuilder {
            app_name: self.app_name,
            agent: Some(agent),
            session_service: self.session_service,
            #[cfg(feature = "artifacts")]
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            intra_compaction_config: self.intra_compaction_config,
            intra_compaction_summarizer: self.intra_compaction_summarizer,
            #[cfg(feature = "context-compaction")]
            context_compaction: self.context_compaction,
            budget: self.budget,
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
            #[cfg(feature = "artifacts")]
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: self.plugin_manager,
            run_config: self.run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            intra_compaction_config: self.intra_compaction_config,
            intra_compaction_summarizer: self.intra_compaction_summarizer,
            #[cfg(feature = "context-compaction")]
            context_compaction: self.context_compaction,
            budget: self.budget,
            _marker: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// Optional-field setters (no type-state change)
// ---------------------------------------------------------------------------

impl<A, G, S> RunnerConfigBuilder<A, G, S> {
    /// Set the artifact service (optional).
    #[cfg(feature = "artifacts")]
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
    #[cfg(feature = "plugins")]
    pub fn plugin_manager(mut self, manager: Arc<PluginManager>) -> Self {
        self.plugin_manager = Some(manager);
        self
    }

    /// Set the run configuration (optional).
    pub fn run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }

    /// Set the resource limits for every run (optional).
    ///
    /// Each run gets fresh counters for these limits, checked before every model
    /// call and tool dispatch. A run that reaches a limit ends with an event
    /// explaining why and a `ResourceExhausted` error. Overrides any budget in
    /// [`run_config`](Self::run_config). See [`adk_core::budget`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::RunBudget;
    ///
    /// let runner = Runner::builder()
    ///     .app_name("my-app")
    ///     .agent(agent)
    ///     .session_service(session_service)
    ///     .budget(RunBudget::new().max_cost_usd(50.0).max_model_calls(200))
    ///     .build()?;
    /// ```
    pub fn budget(mut self, budget: RunBudget) -> Self {
        self.budget = Some(budget);
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
    ///
    /// Each run derives a child of this token, so cancelling it stops every in-flight
    /// run, while [`Runner::interrupt`] cancels only the targeted run.
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Set the intra-invocation compaction configuration (optional).
    pub fn intra_compaction_config(mut self, config: adk_core::IntraCompactionConfig) -> Self {
        self.intra_compaction_config = Some(config);
        self
    }

    /// Set the summarizer for intra-invocation compaction (optional).
    pub fn intra_compaction_summarizer(
        mut self,
        summarizer: Arc<dyn adk_core::BaseEventsSummarizer>,
    ) -> Self {
        self.intra_compaction_summarizer = Some(summarizer);
        self
    }

    /// Set the context compaction configuration for token-budget overflow handling (optional).
    ///
    /// When configured, the runner applies the given [`CompactionStrategy`](crate::compaction::CompactionStrategy)
    /// to shrink the event history when the context exceeds the token budget.
    #[cfg(feature = "context-compaction")]
    pub fn context_compaction(mut self, config: crate::compaction::CompactionConfig) -> Self {
        self.context_compaction = Some(config);
        self
    }
}

// ---------------------------------------------------------------------------
// build() — only available when all required fields are set
// ---------------------------------------------------------------------------

impl RunnerConfigBuilder<HasAppName, HasAgent, HasSessionService> {
    /// Consume the builder and produce a [`RunnerConfig`] without creating a [`Runner`].
    ///
    /// Use this when you need the raw config (e.g. to wrap in `Arc` for A2A handlers).
    pub fn build_config(self) -> RunnerConfig {
        let run_config = merge_budget(self.run_config, self.budget);
        RunnerConfig {
            app_name: self.app_name.expect("typestate guarantees app_name is set"),
            agent: self.agent.expect("typestate guarantees agent is set"),
            session_service: self
                .session_service
                .expect("typestate guarantees session_service is set"),
            #[cfg(feature = "artifacts")]
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: self.plugin_manager,
            run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            intra_compaction_config: self.intra_compaction_config,
            intra_compaction_summarizer: self.intra_compaction_summarizer,
            #[cfg(feature = "context-compaction")]
            context_compaction: self.context_compaction,
        }
    }

    /// Consume the builder and create a [`Runner`].
    ///
    /// Delegates to [`Runner::new()`] internally.
    ///
    /// # Errors
    ///
    /// Returns an error if `Runner::new()` fails (e.g. invalid `app_name`).
    pub fn build(self) -> Result<Runner> {
        let run_config = merge_budget(self.run_config, self.budget);
        let config = RunnerConfig {
            // SAFETY: typestate guarantees these are `Some`.
            app_name: self.app_name.expect("typestate guarantees app_name is set"),
            agent: self.agent.expect("typestate guarantees agent is set"),
            session_service: self
                .session_service
                .expect("typestate guarantees session_service is set"),
            #[cfg(feature = "artifacts")]
            artifact_service: self.artifact_service,
            memory_service: self.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: self.plugin_manager,
            run_config,
            compaction_config: self.compaction_config,
            context_cache_config: self.context_cache_config,
            cache_capable: self.cache_capable,
            request_context: self.request_context,
            cancellation_token: self.cancellation_token,
            intra_compaction_config: self.intra_compaction_config,
            intra_compaction_summarizer: self.intra_compaction_summarizer,
            #[cfg(feature = "context-compaction")]
            context_compaction: self.context_compaction,
        };
        Runner::new(config)
    }
}

/// Applies a builder-level budget on top of the configured run config.
fn merge_budget(run_config: Option<RunConfig>, budget: Option<RunBudget>) -> Option<RunConfig> {
    match budget {
        Some(budget) => {
            let mut config = run_config.unwrap_or_default();
            config.budget = Some(budget);
            Some(config)
        }
        None => run_config,
    }
}
