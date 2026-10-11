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
use adk_core::audit_log::{AuditFailureMode, AuditRecorder, AuditTrail};
use adk_core::{
    ActionLedger, Agent, ApprovalStore, CacheCapable, ContextCacheConfig, GovernanceControl,
    Memory, Result, RunBudget, RunConfig, SpendLedger, ToolPolicy,
};
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
    tool_policy: Option<Arc<dyn ToolPolicy>>,
    governance: Option<GovernanceControl>,
    action_ledger: Option<Arc<dyn ActionLedger>>,
    approval_store: Option<Arc<dyn ApprovalStore>>,
    spend_ledger: Option<Arc<dyn SpendLedger>>,
    durable_runs: Option<Arc<dyn adk_core::DurableRunStore>>,
    audit_recorder: Option<Arc<dyn AuditRecorder>>,
    audit_failure_mode: AuditFailureMode,
    identity_source: Option<Arc<dyn adk_core::IdentitySource>>,
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
            tool_policy: None,
            governance: None,
            action_ledger: None,
            approval_store: None,
            spend_ledger: None,
            durable_runs: None,
            audit_recorder: None,
            audit_failure_mode: AuditFailureMode::Block,
            identity_source: None,
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
            tool_policy: self.tool_policy,
            governance: self.governance,
            action_ledger: self.action_ledger,
            approval_store: self.approval_store,
            spend_ledger: self.spend_ledger,
            durable_runs: self.durable_runs,
            audit_recorder: self.audit_recorder,
            audit_failure_mode: self.audit_failure_mode,
            identity_source: self.identity_source,
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
            tool_policy: self.tool_policy,
            governance: self.governance,
            action_ledger: self.action_ledger,
            approval_store: self.approval_store,
            spend_ledger: self.spend_ledger,
            durable_runs: self.durable_runs,
            audit_recorder: self.audit_recorder,
            audit_failure_mode: self.audit_failure_mode,
            identity_source: self.identity_source,
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
            tool_policy: self.tool_policy,
            governance: self.governance,
            action_ledger: self.action_ledger,
            approval_store: self.approval_store,
            spend_ledger: self.spend_ledger,
            durable_runs: self.durable_runs,
            audit_recorder: self.audit_recorder,
            audit_failure_mode: self.audit_failure_mode,
            identity_source: self.identity_source,
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

    /// Set the policy evaluated for every tool call (optional).
    ///
    /// The runner installs it on each run's [`RunConfig::tool_policy`] when the run
    /// does not carry one, so it governs transfer targets and agents behind an agent
    /// tool as well. A [`DeclarativePolicy`](adk_core::DeclarativePolicy) denies every
    /// tool no rule permits.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::DeclarativePolicy;
    ///
    /// let runner = Runner::builder()
    ///     .app_name("ops")
    ///     .agent(agent)
    ///     .session_service(sessions)
    ///     .tool_policy(Arc::new(DeclarativePolicy::builder().allow_read_only().build()))
    ///     .build()?;
    /// ```
    pub fn tool_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.tool_policy = Some(policy);
        self
    }

    /// Share a kill switch with this runner (optional).
    ///
    /// Without one the runner creates its own, reachable through
    /// [`Runner::governance`]. Freezing it fails new runs at their start and stops
    /// running ones before their next model or tool call.
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Set the ledger that records non-idempotent tool calls (optional).
    ///
    /// The runner installs it on each run's [`RunConfig::action_ledger`] when the run
    /// does not carry one. Every backend implements the same
    /// [`ActionLedger`] trait, so switching from SQLite to PostgreSQL, Redis, or
    /// MongoDB changes only the value passed here.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_session::PostgresActionLedger;
    ///
    /// let ledger = PostgresActionLedger::from_pool(pool.clone());
    /// ledger.migrate().await?;
    /// let runner = Runner::builder()
    ///     .app_name("payments")
    ///     .agent(agent)
    ///     .session_service(sessions)
    ///     .action_ledger(Arc::new(ledger))
    ///     .build()?;
    /// ```
    pub fn action_ledger(mut self, ledger: Arc<dyn ActionLedger>) -> Self {
        self.action_ledger = Some(ledger);
        self
    }

    /// Set where calls held for approval and their decisions are kept (optional).
    ///
    /// The runner installs it on each run's [`RunConfig::approval_store`] when the run
    /// does not carry one.
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Set the ledger that model calls and payment tools reserve budget against
    /// (optional).
    ///
    /// The runner installs it on each run's [`RunConfig::spend_ledger`] when the run
    /// does not carry one.
    pub fn spend_ledger(mut self, ledger: Arc<dyn SpendLedger>) -> Self {
        self.spend_ledger = Some(ledger);
        self
    }

    /// Set the durable run queue shared by every replica (optional).
    ///
    /// [`Runner::interrupt`](crate::Runner::interrupt) then also requests cancellation of
    /// the session's unfinished durable runs, wherever their worker runs.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = Runner::builder()
    ///     .app_name("ops")
    ///     .agent(agent)
    ///     .session_service(sessions)
    ///     .durable_run_store(Arc::new(adk_core::InMemoryRunStore::new()))
    ///     .build()?;
    /// ```
    pub fn durable_run_store(mut self, store: Arc<dyn adk_core::DurableRunStore>) -> Self {
        self.durable_runs = Some(store);
        self
    }

    /// Record the runner's governed actions in `recorder` (optional).
    ///
    /// Every run records each tool decision (allowed, denied, held for approval), each
    /// approval decision, each payment made through the payment tools, and each call
    /// answered with an unknown outcome; the runner's kill switch records freezes and
    /// unfreezes. Records name the acting agent and the subject it acts for, and carry
    /// argument digests, never arguments. An action whose record cannot be written does
    /// not happen; see [`audit_failure_mode`](Self::audit_failure_mode).
    ///
    /// The runner installs the trail on each run's [`RunConfig::audit`] when the run does
    /// not carry one.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_auth::signed_audit::{AuditLog, Ed25519Signer};
    ///
    /// let log = AuditLog::builder(store, "replica-1", signer).open().await?;
    /// let runner = Runner::builder()
    ///     .app_name("ops")
    ///     .agent(agent)
    ///     .session_service(sessions)
    ///     .audit_log(Arc::new(log))
    ///     .build()?;
    /// ```
    pub fn audit_log(mut self, recorder: Arc<dyn AuditRecorder>) -> Self {
        self.audit_recorder = Some(recorder);
        self
    }

    /// Set what happens when the [`audit_log`](Self::audit_log) cannot record an action
    /// (optional). The default, [`AuditFailureMode::Block`], refuses the action.
    pub fn audit_failure_mode(mut self, mode: AuditFailureMode) -> Self {
        self.audit_failure_mode = mode;
        self
    }

    /// Sets where each run's agent principal comes from.
    ///
    /// The runner asks the source for the current principal at the start of every run
    /// and installs it on the run's [`RunConfig::acting_context`] when the run carries
    /// none. Without a source, each agent acts as
    /// `urn:adk:agent:{app_name}/{agent_name}`. A run whose source fails ends with the
    /// source's error before any agent runs.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_core::{AgentId, AgentPrincipal, StaticIdentitySource};
    ///
    /// let source = StaticIdentitySource::new(
    ///     AgentPrincipal::new(AgentId::new("spiffe://acme.example/agent/billing")?)
    ///         .with_instance(std::env::var("HOSTNAME")?),
    /// );
    /// let runner = Runner::builder()
    ///     .app_name("billing")
    ///     .agent(agent)
    ///     .session_service(sessions)
    ///     .identity_source(Arc::new(source))
    ///     .build()?;
    /// ```
    pub fn identity_source(mut self, source: Arc<dyn adk_core::IdentitySource>) -> Self {
        self.identity_source = Some(source);
        self
    }

    /// Sets a fixed principal every run's agents act as.
    ///
    /// Shorthand for [`identity_source`](Self::identity_source) with a
    /// [`StaticIdentitySource`](adk_core::StaticIdentitySource).
    pub fn agent_principal(self, principal: adk_core::AgentPrincipal) -> Self {
        self.identity_source(Arc::new(adk_core::StaticIdentitySource::new(principal)))
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
            tool_policy: self.tool_policy,
            governance: self.governance,
            action_ledger: self.action_ledger,
            approval_store: self.approval_store,
            spend_ledger: self.spend_ledger,
            durable_runs: self.durable_runs,
            audit: self.audit_recorder.map(|recorder| {
                AuditTrail::new(recorder).with_failure_mode(self.audit_failure_mode)
            }),
            identity_source: self.identity_source,
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
            tool_policy: self.tool_policy,
            governance: self.governance,
            action_ledger: self.action_ledger,
            approval_store: self.approval_store,
            spend_ledger: self.spend_ledger,
            durable_runs: self.durable_runs,
            audit: self.audit_recorder.map(|recorder| {
                AuditTrail::new(recorder).with_failure_mode(self.audit_failure_mode)
            }),
            identity_source: self.identity_source,
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
