use crate::InvocationContext;
use crate::cache::CacheManager;
#[cfg(feature = "artifacts")]
use adk_artifact::ArtifactService;
use adk_core::{
    Agent, AppName, CacheCapable, Content, ContextCacheConfig, Event, EventStream, Memory,
    ReadonlyContext, Result, RunConfig, SessionId, UserId,
};
#[cfg(feature = "plugins")]
use adk_plugin::PluginManager;
use adk_session::SessionService;
#[cfg(feature = "skills")]
use adk_skill::{SkillInjector, SkillInjectorConfig};
use async_stream::stream;
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

/// A run currently in flight, tracked so it can be interrupted.
///
/// Keyed by run ID rather than session ID: a session ID is only unique within an
/// app and user, and one identity may have several runs in flight at once.
#[derive(Debug, Clone)]
struct ActiveRun {
    identity: adk_core::AdkIdentity,
    token: CancellationToken,
}

/// Registry of in-flight runs, keyed by run ID.
type ActiveRuns = Arc<std::sync::Mutex<std::collections::HashMap<u64, ActiveRun>>>;

fn preserve_streamed_content(accumulated: &mut HashMap<String, Content>, event: &mut Event) {
    if event.llm_response.partial {
        if let Some(chunk) = &event.llm_response.content {
            accumulated
                .entry(event.id.clone())
                .and_modify(|content| content.parts.extend(chunk.parts.clone()))
                .or_insert_with(|| chunk.clone());
        }
    } else if event.llm_response.content.is_none() {
        event.llm_response.content = accumulated.remove(&event.id);
    } else {
        accumulated.remove(&event.id);
    }
}

/// Deregisters a run when its event stream is dropped.
///
/// Removal is by run ID, so a finishing run can never deregister a different run
/// that happens to share the same session ID.
struct ActiveRunCleanup {
    active_runs: ActiveRuns,
    run_id: u64,
}

impl Drop for ActiveRunCleanup {
    fn drop(&mut self) {
        let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.remove(&self.run_id);
    }
}
use tracing::Instrument;

/// Configuration for constructing a [`Runner`].
///
/// Use [`Runner::builder()`] for a compile-time-safe way to construct this.
pub struct RunnerConfig {
    /// Application name used for session scoping.
    pub app_name: String,
    /// The root agent to execute.
    pub agent: Arc<dyn Agent>,
    /// Session persistence backend.
    pub session_service: Arc<dyn SessionService>,
    #[cfg(feature = "artifacts")]
    /// Optional artifact storage service.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Optional memory/RAG service.
    pub memory_service: Option<Arc<dyn Memory>>,
    #[cfg(feature = "plugins")]
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<PluginManager>>,
    /// Optional run configuration (streaming mode, etc.)
    /// If not provided, uses default (SSE streaming)
    #[allow(dead_code)]
    pub run_config: Option<RunConfig>,
    /// Optional context compaction configuration.
    /// When set, the runner will periodically summarize older events
    /// to reduce context size sent to the LLM.
    pub compaction_config: Option<adk_core::EventsCompactionConfig>,
    /// Optional context cache configuration for automatic prompt caching lifecycle.
    /// When set alongside `cache_capable`, the runner will automatically create and
    /// manage cached content resources for supported providers.
    ///
    /// When `cache_capable` is set but this field is `None`, the runner
    /// automatically uses [`ContextCacheConfig::default()`] (4096 min tokens,
    /// 600s TTL, refresh every 3 invocations).
    pub context_cache_config: Option<ContextCacheConfig>,
    /// Optional cache-capable model reference for automatic cache management.
    /// Set this to the same model used by the agent if it supports caching.
    pub cache_capable: Option<Arc<dyn CacheCapable>>,
    /// Optional request context from the server's auth middleware bridge.
    /// When set, the runner passes it to `InvocationContext` so that
    /// `user_scopes()` and `user_id()` reflect the authenticated identity.
    pub request_context: Option<adk_core::RequestContext>,
    /// Optional cooperative cancellation token for externally managed runs.
    pub cancellation_token: Option<CancellationToken>,
    /// Optional intra-invocation compaction configuration.
    /// When set, the runner estimates token count before each agent run
    /// and triggers mid-invocation summarization when the threshold is exceeded.
    pub intra_compaction_config: Option<adk_core::IntraCompactionConfig>,
    /// Optional summarizer for intra-invocation compaction.
    /// Required when `intra_compaction_config` is set.
    pub intra_compaction_summarizer: Option<Arc<dyn adk_core::BaseEventsSummarizer>>,
    /// Optional context compaction configuration for token-budget overflow handling.
    ///
    /// When set, the runner applies the configured [`CompactionStrategy`](crate::compaction::CompactionStrategy)
    /// to shrink the event history when the context exceeds the token budget,
    /// retrying the model request up to `max_retries` times.
    ///
    /// This field is only available when the `context-compaction` feature is enabled.
    #[cfg(feature = "context-compaction")]
    pub context_compaction: Option<crate::compaction::CompactionConfig>,
}

/// Agent execution runtime.
///
/// Orchestrates session retrieval, agent dispatch, event streaming, context
/// caching, and compaction. Construct via [`Runner::builder()`] or
/// [`Runner::new()`].
pub struct Runner {
    app_name: String,
    root_agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    #[cfg(feature = "artifacts")]
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn Memory>>,
    #[cfg(feature = "plugins")]
    plugin_manager: Option<Arc<PluginManager>>,
    #[cfg(feature = "skills")]
    skill_injector: Option<Arc<SkillInjector>>,
    run_config: RunConfig,
    compaction_config: Option<adk_core::EventsCompactionConfig>,
    context_cache_config: Option<ContextCacheConfig>,
    cache_capable: Option<Arc<dyn CacheCapable>>,
    cache_manager: Option<Arc<tokio::sync::Mutex<CacheManager>>>,
    request_context: Option<adk_core::RequestContext>,
    cancellation_token: Option<CancellationToken>,
    intra_compactor: Option<Arc<crate::intra_compaction::IntraInvocationCompactor>>,
    /// Optional context compaction configuration for token-budget overflow handling.
    #[cfg(feature = "context-compaction")]
    context_compaction: Option<Arc<crate::compaction::CompactionConfig>>,
    /// Per-session cancellation tokens for the interrupt API.
    /// Each `run()` call registers a token here; `interrupt()` cancels it.
    active_runs: ActiveRuns,
    next_run_id: Arc<std::sync::atomic::AtomicU64>,
    /// Serializes externally triggered invocations per session. The weak values prevent the
    /// per-trigger session policy from retaining one lock forever for every completed event.
    pub(crate) external_session_locks: Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    >,
}

impl Runner {
    /// Create a typestate builder for constructing a `Runner`.
    ///
    /// The builder enforces at compile time that the three required fields
    /// (`app_name`, `agent`, `session_service`) are set before `build()` is
    /// callable.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let runner = Runner::builder()
    ///     .app_name("my-app")
    ///     .agent(agent)
    ///     .session_service(session_service)
    ///     .build()?;
    /// ```
    pub fn builder() -> crate::builder::RunnerConfigBuilder<
        crate::builder::NoAppName,
        crate::builder::NoAgent,
        crate::builder::NoSessionService,
    > {
        crate::builder::RunnerConfigBuilder::new()
    }

    /// Create a new runner from a [`RunnerConfig`].
    ///
    /// Prefer [`Runner::builder()`] for a compile-time-safe construction API.
    pub fn new(config: RunnerConfig) -> Result<Self> {
        let run_config = config.run_config.unwrap_or_default();

        // When a cache-capable model is provided but no explicit cache config,
        // use the default ContextCacheConfig to enable caching automatically.
        let effective_cache_config = config
            .context_cache_config
            .or_else(|| config.cache_capable.as_ref().map(|_| ContextCacheConfig::default()));

        let cache_manager = effective_cache_config
            .as_ref()
            .map(|c| Arc::new(tokio::sync::Mutex::new(CacheManager::new(c.clone()))));

        let intra_compactor = config.intra_compaction_config.as_ref().and_then(|ic_config| {
            config.intra_compaction_summarizer.as_ref().map(|summarizer| {
                Arc::new(crate::intra_compaction::IntraInvocationCompactor::new(
                    ic_config.clone(),
                    summarizer.clone(),
                ))
            })
        });

        Ok(Self {
            app_name: config.app_name,
            root_agent: config.agent,
            session_service: config.session_service,
            #[cfg(feature = "artifacts")]
            artifact_service: config.artifact_service,
            memory_service: config.memory_service,
            #[cfg(feature = "plugins")]
            plugin_manager: config.plugin_manager,
            #[cfg(feature = "skills")]
            skill_injector: None,
            run_config,
            compaction_config: config.compaction_config,
            context_cache_config: effective_cache_config,
            cache_capable: config.cache_capable,
            cache_manager,
            request_context: config.request_context,
            cancellation_token: config.cancellation_token,
            intra_compactor,
            #[cfg(feature = "context-compaction")]
            context_compaction: config.context_compaction.map(Arc::new),
            active_runs: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            next_run_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            external_session_locks: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        })
    }

    /// The executable root owned by this runner.
    pub(crate) fn root_agent(&self) -> Arc<dyn Agent> {
        Arc::clone(&self.root_agent)
    }

    /// Enable skill injection using a pre-built injector.
    ///
    /// Skill injection runs before plugin `on_user_message` callbacks.
    #[cfg(feature = "skills")]
    pub fn with_skill_injector(mut self, injector: SkillInjector) -> Self {
        self.skill_injector = Some(Arc::new(injector));
        self
    }

    /// Enable skill injection by auto-loading `.skills/` from the given root path.
    #[cfg(feature = "skills")]
    #[deprecated(note = "Use with_auto_skills_mut instead")]
    pub fn with_auto_skills(
        mut self,
        root: impl AsRef<std::path::Path>,
        config: SkillInjectorConfig,
    ) -> adk_skill::SkillResult<Self> {
        self.with_auto_skills_mut(root, config)?;
        Ok(self)
    }

    /// Enable skill injection by auto-loading `.skills/` from the given root path.
    ///
    /// Unlike [`with_auto_skills`](Self::with_auto_skills), this method borrows
    /// the Runner mutably instead of consuming it. On error, the Runner remains
    /// valid with no skill injector configured.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills_mut(
        &mut self,
        root: impl AsRef<std::path::Path>,
        config: SkillInjectorConfig,
    ) -> adk_skill::SkillResult<()> {
        let injector = SkillInjector::from_root(root, config)?;
        self.skill_injector = Some(Arc::new(injector));
        Ok(())
    }

    /// Execute the root agent for the given user and session, returning an event stream.
    ///
    /// Retrieves the existing session, resolves the target agent, runs plugins and skills, and
    /// streams events as the agent executes.
    pub async fn run(
        &self,
        user_id: UserId,
        session_id: SessionId,
        user_content: Content,
    ) -> Result<EventStream> {
        self.run_with_config(user_id, session_id, user_content, None).await
    }

    /// Returns the runner's application name.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }

    /// Returns the runner's session service.
    pub fn session_service(&self) -> &Arc<dyn adk_session::SessionService> {
        &self.session_service
    }

    /// Returns the runner's configured [`RunConfig`].
    ///
    /// Useful as a base to clone and adjust for [`Self::run_with_config`].
    pub fn run_config(&self) -> &RunConfig {
        &self.run_config
    }

    /// Runs the agent with a per-invocation [`RunConfig`] override.
    ///
    /// Passing `None` uses the runner's configured `RunConfig`. Supply one to vary a single
    /// invocation — injecting `runtime_toolsets` for tools that only exist for the duration of
    /// that run, for example, as `SandboxRunner` does with tools bound to a live sandbox session.
    ///
    /// # Errors
    ///
    /// Returns an error if invocation setup fails before the stream is created. Session lookup
    /// and agent execution failures are yielded by the returned stream.
    pub async fn run_with_config(
        &self,
        user_id: UserId,
        session_id: SessionId,
        user_content: Content,
        run_config: Option<RunConfig>,
    ) -> Result<EventStream> {
        let app_name = self.app_name.clone();
        let typed_app_name = AppName::try_from(app_name.clone())?;
        let session_service = self.session_service.clone();
        let root_agent = self.root_agent.clone();
        #[cfg(feature = "artifacts")]
        let artifact_service = self.artifact_service.clone();
        let memory_service = self.memory_service.clone();
        #[cfg(feature = "plugins")]
        let plugin_manager = self.plugin_manager.clone();
        #[cfg(feature = "skills")]
        let skill_injector = self.skill_injector.clone();
        let mut run_config = run_config.unwrap_or_else(|| self.run_config.clone());
        let compaction_config = self.compaction_config.clone();
        let context_cache_config = self.context_cache_config.clone();
        let cache_capable = self.cache_capable.clone();
        let cache_manager_ref = self.cache_manager.clone();
        let request_context = self.request_context.clone();
        let cancellation_token = self.cancellation_token.clone();
        let intra_compactor = self.intra_compactor.clone();
        #[cfg(feature = "context-compaction")]
        let context_compaction = self.context_compaction.clone();

        // Built once and used for every persistence write, so a backend with a
        // composite natural key can bind each event to its tenant.
        let identity =
            adk_core::AdkIdentity::new(typed_app_name.clone(), user_id.clone(), session_id.clone());

        // Register this run for the interrupt API. The registration is keyed by a
        // unique run ID rather than the raw session ID: two identities can share a
        // session ID, and one identity can have two runs in flight, and both cases
        // previously overwrote each other's token.
        let session_token = CancellationToken::new();
        let run_id = self.next_run_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
            runs.insert(
                run_id,
                ActiveRun { identity: identity.clone(), token: session_token.clone() },
            );
        }
        let active_runs = self.active_runs.clone();
        // Effective token: cancelled if either the global token or the session token fires
        let effective_token = if let Some(ref global) = cancellation_token {
            let combined = CancellationToken::new();
            let combined_clone = combined.clone();
            let global_clone = global.clone();
            let session_clone = session_token.clone();
            // Watch both tokens — cancel the combined token when either fires
            let combined_for_global = combined_clone.clone();
            tokio::spawn(async move {
                global_clone.cancelled().await;
                combined_for_global.cancel();
            });
            let combined_for_session = combined_clone;
            tokio::spawn(async move {
                session_clone.cancelled().await;
                combined_for_session.cancel();
            });
            Some(combined)
        } else {
            Some(session_token.clone())
        };

        // Built here rather than inside the generator: registration happens as soon
        // as `run` is called, so deregistration must also survive a stream that is
        // dropped before it is ever polled. Moving the guard into the generator
        // keeps it alive exactly as long as the stream.
        let cleanup = ActiveRunCleanup { active_runs: active_runs.clone(), run_id };

        let s = stream! {
            let _cleanup = cleanup;

            // Use the effective token (combines global + per-session)
            let cancellation_token = effective_token;
            // Resolve the existing session.
            let session = match session_service
                .get(adk_session::GetRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.to_string(),
                    session_id: session_id.to_string(),
                    num_recent_events: run_config.history_max_events,
                    after: None,
                })
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Find which agent should handle this request
            let agent_to_run = Self::find_agent_to_run(&root_agent, session.as_ref());

            // Let validated composite roots apply policy for the concrete agent
            // selected for this turn. Ordinary agents keep the default no-op.
            root_agent.configure_run(agent_to_run.name(), &mut run_config);
            if let Some(targets) = root_agent.transfer_targets_for(agent_to_run.name()) {
                run_config.transfer_targets = targets;
                run_config.parent_agent = None;
            }

            // Clone services for potential reuse in transfer
            #[cfg(feature = "artifacts")]
            let artifact_service_clone = artifact_service.clone();
            let memory_service_clone = memory_service.clone();

            // Create invocation context with MutableSession
            let invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
            #[cfg(any(feature = "skills", feature = "plugins"))]
            let mut effective_user_content = user_content.clone();
            #[cfg(not(any(feature = "skills", feature = "plugins")))]
            let effective_user_content = user_content.clone();
            #[cfg(feature = "skills")]
            let mut selected_skill_name = String::new();
            #[cfg(not(feature = "skills"))]
            let selected_skill_name = String::new();
            #[cfg(feature = "skills")]
            let mut selected_skill_id = String::new();
            #[cfg(not(feature = "skills"))]
            let selected_skill_id = String::new();

            #[cfg(feature = "skills")]
            if let Some(injector) = skill_injector.as_ref()
                && let Some(matched) = adk_skill::apply_skill_injection(
                    &mut effective_user_content,
                    injector.index(),
                    injector.policy(),
                    injector.max_injected_chars(),
                ) {
                    selected_skill_name = matched.skill.name;
                    selected_skill_id = matched.skill.id;
                }

            let mut invocation_ctx = match InvocationContext::new_typed(
                invocation_id.clone(),
                agent_to_run.clone(),
                user_id.clone(),
                typed_app_name.clone(),
                session_id.clone(),
                effective_user_content.clone(),
                Arc::from(session),
            ) {
                Ok(ctx) => ctx,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Add optional services
            #[cfg(feature = "artifacts")]
            if let Some(service) = artifact_service {
                // Wrap service with ScopedArtifacts to bind session context
                let scoped = adk_artifact::ScopedArtifacts::new(
                    service,
                    app_name.clone(),
                    user_id.to_string(),
                    session_id.to_string(),
                );
                invocation_ctx = invocation_ctx.with_artifacts(Arc::new(scoped));
            }
            if let Some(memory) = memory_service {
                invocation_ctx = invocation_ctx.with_memory(memory);
            }

            // Apply run config (streaming mode, etc.)
            invocation_ctx = invocation_ctx.with_run_config(run_config.clone());

            // Apply request context from auth middleware bridge if present
            if let Some(rc) = request_context.clone() {
                invocation_ctx = invocation_ctx.with_request_context(rc);
            }

            // Expose cooperative cancellation to the agent/tools.
            if let Some(token) = cancellation_token.as_ref() {
                invocation_ctx = invocation_ctx.with_cancellation_token(token.clone());
            }

            let mut ctx = Arc::new(invocation_ctx);

            #[cfg(feature = "plugins")]
            if let Some(manager) = plugin_manager.as_ref() {
                match manager
                    .run_before_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>)
                    .await
                {
                    Ok(Some(content)) => {
                        let mut early_event = adk_core::Event::new(ctx.invocation_id());
                        early_event.author = agent_to_run.name().to_string();
                        early_event.llm_response.content = Some(content);

                        ctx.mutable_session().append_event(early_event.clone());
                        if let Err(e) = session_service
                            .append_event_for_identity(adk_session::AppendEventRequest {
                                identity: identity.clone(),
                                event: early_event.clone(),
                            })
                            .await {
                            yield Err(e);
                            return;
                        }

                        yield Ok(early_event);
                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        yield Err(e);
                        return;
                    }
                }

                match manager
                    .run_on_user_message(
                        ctx.clone() as Arc<dyn adk_core::InvocationContext>,
                        effective_user_content.clone(),
                    )
                    .await
                {
                    Ok(Some(modified)) => {
                        effective_user_content = modified;

                        let mut refreshed_ctx = match InvocationContext::with_mutable_session(
                            ctx.invocation_id().to_string(),
                            agent_to_run.clone(),
                            ctx.user_id().to_string(),
                            ctx.app_name().to_string(),
                            ctx.session_id().to_string(),
                            effective_user_content.clone(),
                            ctx.mutable_session().clone(),
                        ) {
                            Ok(ctx) => ctx,
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        };
                        refreshed_ctx = refreshed_ctx.with_orchestration_root_invocation_id(
                            adk_core::InvocationContext::orchestration_root_invocation_id(
                                ctx.as_ref(),
                            )
                            .to_string(),
                        );

                        #[cfg(feature = "artifacts")]
                        if let Some(service) = artifact_service_clone.clone() {
                            let scoped = adk_artifact::ScopedArtifacts::new(
                                service,
                                ctx.app_name().to_string(),
                                ctx.user_id().to_string(),
                                ctx.session_id().to_string(),
                            );
                            refreshed_ctx = refreshed_ctx.with_artifacts(Arc::new(scoped));
                        }
                        if let Some(memory) = memory_service_clone.clone() {
                            refreshed_ctx = refreshed_ctx.with_memory(memory);
                        }
                        refreshed_ctx = refreshed_ctx.with_run_config(run_config.clone());
                        if let Some(rc) = request_context.clone() {
                            refreshed_ctx = refreshed_ctx.with_request_context(rc);
                        }
                        if let Some(token) = cancellation_token.as_ref() {
                            refreshed_ctx = refreshed_ctx.with_cancellation_token(token.clone());
                        }
                        ctx = Arc::new(refreshed_ctx);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if let Some(manager) = plugin_manager.as_ref() {
                            manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        }
                        yield Err(e);
                        return;
                    }
                }
            }

            // Append user message to session service (persistent storage)
            let mut user_event = adk_core::Event::new(ctx.invocation_id());
            user_event.author = "user".to_string();
            user_event.llm_response.content = Some(effective_user_content.clone());

            // Also add to mutable session for immediate visibility
            // Note: adk_session::Event is a re-export of adk_core::Event, so we can use it directly
            ctx.mutable_session().append_event(user_event.clone());

            if let Err(e) = session_service
                .append_event_for_identity(adk_session::AppendEventRequest {
                    identity: identity.clone(),
                    event: user_event,
                })
                .await {
                #[cfg(feature = "plugins")]
                if let Some(manager) = plugin_manager.as_ref() {
                    manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                }
                yield Err(e);
                return;
            }

            // ===== CONTEXT CACHE LIFECYCLE =====
            // If context caching is configured and a cache-capable model is available,
            // create or refresh the cached content before agent execution.
            // Cache failures are non-fatal — log a warning and proceed without cache.
            if let (Some(cm_mutex), Some(cache_model)) = (&cache_manager_ref, &cache_capable) {
                let should_refresh_cache = {
                    let cm = cm_mutex.lock().await;
                    cm.is_enabled() && (cm.active_cache_name().is_none() || cm.needs_refresh())
                };

                if should_refresh_cache {
                    // Gather system instruction from the agent's description
                    // (the full instruction is resolved inside the agent, but the
                    // description provides a reasonable proxy for cache keying).
                    let system_instruction = agent_to_run.description().to_string();
                    let tools = std::collections::HashMap::new();
                    let ttl = context_cache_config.as_ref().map_or(600, |c| c.ttl_seconds);

                    match cache_model.create_cache(&system_instruction, &tools, ttl).await {
                        Ok(name) => {
                            let old_cache = {
                                let mut cm = cm_mutex.lock().await;
                                let old = cm.clear_active_cache();
                                cm.set_active_cache(name);
                                old
                            };

                            if let Some(old) = old_cache
                                && let Err(e) = cache_model.delete_cache(&old).await {
                                    tracing::warn!(
                                        old_cache = %old,
                                        error = %e,
                                        "failed to delete old cache, proceeding with new cache"
                                    );
                                }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "cache creation failed, proceeding without cache"
                            );
                        }
                    }
                }

                // Attach cache name to run config so agents can use it.
                let cache_name = {
                    let mut cm = cm_mutex.lock().await;
                    if cm.is_enabled() {
                        cm.record_invocation().map(str::to_string)
                    } else {
                        None
                    }
                };

                if let Some(cache_name) = cache_name {
                    run_config.cached_content = Some(cache_name);
                    // Rebuild the invocation context with the updated run config.
                    let mut refreshed_ctx = match InvocationContext::with_mutable_session(
                        ctx.invocation_id().to_string(),
                        agent_to_run.clone(),
                        ctx.user_id().to_string(),
                        ctx.app_name().to_string(),
                        ctx.session_id().to_string(),
                        effective_user_content.clone(),
                        ctx.mutable_session().clone(),
                    ) {
                        Ok(ctx) => ctx,
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    };
                    refreshed_ctx = refreshed_ctx.with_orchestration_root_invocation_id(
                        adk_core::InvocationContext::orchestration_root_invocation_id(ctx.as_ref())
                            .to_string(),
                    );
                    #[cfg(feature = "artifacts")]
                    if let Some(service) = artifact_service_clone.clone() {
                        let scoped = adk_artifact::ScopedArtifacts::new(
                            service,
                            ctx.app_name().to_string(),
                            ctx.user_id().to_string(),
                            ctx.session_id().to_string(),
                        );
                        refreshed_ctx = refreshed_ctx.with_artifacts(Arc::new(scoped));
                    }
                    if let Some(memory) = memory_service_clone.clone() {
                        refreshed_ctx = refreshed_ctx.with_memory(memory);
                    }
                    refreshed_ctx = refreshed_ctx.with_run_config(run_config.clone());
                    if let Some(rc) = request_context.clone() {
                        refreshed_ctx = refreshed_ctx.with_request_context(rc);
                    }
                    if let Some(token) = cancellation_token.as_ref() {
                        refreshed_ctx = refreshed_ctx.with_cancellation_token(token.clone());
                    }
                    ctx = Arc::new(refreshed_ctx);
                }
            }

            // ===== INTRA-INVOCATION COMPACTION =====
            // If intra-compaction is configured, check if the session events
            // exceed the token threshold and compact them before the agent runs.
            if let Some(ref compactor) = intra_compactor {
                compactor.reset_cycle();
                let session_events = ctx.mutable_session().as_ref().events_snapshot();
                match compactor.maybe_compact(&session_events).await {
                    Ok(Some(compacted_events)) => {
                        ctx.mutable_session().replace_events(compacted_events);
                        tracing::info!("intra-invocation compaction applied before agent execution");
                    }
                    Ok(None) => {} // No compaction needed
                    Err(e) => {
                        tracing::warn!(error = %e, "intra-invocation compaction check failed");
                    }
                }
            }

            // ===== CONTEXT COMPACTION (TOKEN BUDGET) =====
            // If context-compaction is configured, proactively check the estimated
            // token count before calling the agent. If it exceeds the budget,
            // apply compaction to bring it under the limit.
            #[cfg(feature = "context-compaction")]
            if let Some(ref cc_config) = context_compaction {
                let session_events = ctx.mutable_session().events_snapshot();
                let estimated = crate::compaction::estimate_event_tokens(&session_events);
                if estimated > cc_config.context_budget {
                    tracing::info!(
                        estimated_tokens = estimated,
                        budget = cc_config.context_budget,
                        "context exceeds budget, applying proactive compaction"
                    );
                    match crate::compaction::apply_compaction_with_retry(cc_config, session_events).await {
                        Ok(compacted) => {
                            ctx.mutable_session().replace_events(compacted);
                            tracing::info!("proactive context compaction succeeded");
                        }
                        Err(e) => {
                            // Proactive compaction failed — proceed anyway and let the
                            // model reject the request if it's truly too large.
                            tracing::warn!(error = %e, "proactive context compaction failed, proceeding with full context");
                        }
                    }
                }
            }

            // Run the agent with instrumentation (ADK-Go style attributes)
            let agent_span = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.invocation_id" = ctx.invocation_id(),
                "gcp.vertex.agent.session_id" = ctx.session_id(),
                "gcp.vertex.agent.event_id" = ctx.invocation_id(), // Use invocation_id as event_id for agent spans
                "gen_ai.conversation.id" = ctx.session_id(),
                "adk.app_name" = ctx.app_name(),
                "adk.user_id" = ctx.user_id(),
                "agent.name" = %agent_to_run.name(),
                "adk.skills.selected_name" = %selected_skill_name,
                "adk.skills.selected_id" = %selected_skill_id
            );

            let mut agent_stream = match agent_to_run.run(ctx.clone()).instrument(agent_span.clone()).await {
                Ok(s) => s,
                #[cfg(feature = "context-compaction")]
                Err(e) if context_compaction.is_some() && crate::compaction::is_token_limit_error(&e) => {
                    // Token limit error on agent.run() — apply compaction and retry
                    let cc_config = context_compaction.as_ref().unwrap();
                    tracing::warn!(
                        error = %e,
                        "agent execution failed with token limit error, attempting compaction"
                    );
                    let session_events = ctx.mutable_session().events_snapshot();
                    match crate::compaction::apply_compaction_with_retry(cc_config, session_events).await {
                        Ok(compacted) => {
                            ctx.mutable_session().replace_events(compacted);
                            tracing::info!("context compaction succeeded after token limit error, retrying agent");
                            // Retry the agent call with compacted context
                            match agent_to_run.run(ctx.clone()).instrument(agent_span).await {
                                Ok(s) => s,
                                Err(retry_err) => {
                                    #[cfg(feature = "plugins")]
                                    if let Some(manager) = plugin_manager.as_ref() {
                                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                    }
                                    yield Err(retry_err);
                                    return;
                                }
                            }
                        }
                        Err(compaction_err) => {
                            #[cfg(feature = "plugins")]
                            if let Some(manager) = plugin_manager.as_ref() {
                                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                            }
                            yield Err(compaction_err);
                            return;
                        }
                    }
                }
                Err(e) => {
                    #[cfg(feature = "plugins")]
                    if let Some(manager) = plugin_manager.as_ref() {
                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                    }
                    yield Err(e);
                    return;
                }
            };

            // Stream events and check for transfers
            use futures::StreamExt;
            let mut transfer_target: Option<(String, String)> = None;
            let mut streamed_content = HashMap::new();

            while let Some(result) = {
                // Race the next event against cancellation so an in-flight
                // await (LLM streaming, tool I/O) is interrupted promptly
                // rather than only at poll boundaries. Dropping the stream on
                // cancellation releases the underlying provider connection.
                match cancellation_token.as_ref() {
                    Some(token) => {
                        tokio::select! {
                            biased;
                            _ = token.cancelled() => {
                                tracing::info!("cancellation fired during agent stream await");
                                #[cfg(feature = "plugins")]
                                if let Some(manager) = plugin_manager.as_ref() {
                                    manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                }
                                return;
                            }
                            result = agent_stream.next() => result,
                        }
                    }
                    None => agent_stream.next().await,
                }
            } {
                match result {
                    Ok(mut event) => {
                        #[cfg(feature = "plugins")]
                        if let Some(manager) = plugin_manager.as_ref() {
                            match manager
                                .run_on_event(
                                    ctx.clone() as Arc<dyn adk_core::InvocationContext>,
                                    event.clone(),
                                )
                                .await
                            {
                                Ok(Some(modified)) => {
                                    event = modified;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                    yield Err(e);
                                    return;
                                }
                            }
                        }

                        preserve_streamed_content(&mut streamed_content, &mut event);

                        // Check for transfer action
                        if let Some(target) = &event.actions.transfer_to_agent {
                            let source = if event.author.is_empty() {
                                agent_to_run.name()
                            } else {
                                &event.author
                            };
                            if let Some(allowed) = root_agent.transfer_targets_for(source)
                                && !allowed.contains(target)
                            {
                                if root_agent.strict_transfer_policy() {
                                    yield Err(adk_core::AdkError::new(
                                        adk_core::ErrorComponent::Agent,
                                        adk_core::ErrorCategory::Forbidden,
                                        "agent.transfer.target_forbidden",
                                        format!(
                                            "agent '{source}' cannot hand off to '{target}'; allowed targets: {}",
                                            allowed.join(", ")
                                        ),
                                    ));
                                    return;
                                }
                                tracing::warn!(source, target, "handoff target rejected by root policy");
                            } else {
                                transfer_target = Some((source.to_string(), target.clone()));
                            }
                        }

                        // CRITICAL: Apply state_delta to the mutable session immediately.
                        // This is the key fix for state propagation between sequential agents.
                        // When an agent sets output_key, it emits an event with state_delta.
                        // We must apply this to the mutable session so downstream agents
                        // can read the value via ctx.session().state().get().
                        if !event.actions.state_delta.is_empty() {
                            ctx.mutable_session().apply_state_delta(&event.actions.state_delta);
                        }

                        // Also add the event to the mutable session's event list
                        ctx.mutable_session().append_event(event.clone());

                        // Append event to session service (persistent storage)
                        // Skip partial streaming chunks — only persist the final
                        // event. Streaming chunks share the same event ID, so
                        // persisting each one would violate the primary key
                        // constraint. The final chunk (partial=false) carries the
                        // complete accumulated content.
                        if !event.llm_response.partial
                            && let Err(e) = session_service
                                .append_event_for_identity(adk_session::AppendEventRequest {
                                    identity: identity.clone(),
                                    event: event.clone(),
                                })
                                .await {
                                #[cfg(feature = "plugins")]
                                if let Some(manager) = plugin_manager.as_ref() {
                                    manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                }
                                yield Err(e);
                                return;
                            }
                        yield Ok(event);
                    }
                    Err(e) => {
                        #[cfg(feature = "plugins")]
                        if let Some(manager) = plugin_manager.as_ref() {
                            manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        }
                        yield Err(e);
                        return;
                    }
                }
            }

            // ===== TRANSFER LOOP =====
            // Support multi-hop transfers with a max-depth guard.
            // When an agent emits transfer_to_agent, the runner resolves the
            // target from the root agent tree, computes transfer_targets
            // (parent + peers) for the new agent, and runs it. This repeats
            // until no further transfer is requested or the depth limit is hit.
            const DEFAULT_MAX_TRANSFER_DEPTH: u32 = 10;
            let max_depth = run_config.max_transfer_depth.unwrap_or(DEFAULT_MAX_TRANSFER_DEPTH);
            let mut transfer_depth: u32 = 0;
            let mut current_transfer_target = transfer_target;

            while let Some((transfer_source, target_name)) = current_transfer_target.take() {
                transfer_depth += 1;
                if transfer_depth > max_depth {
                    tracing::warn!(
                        depth = transfer_depth,
                        target = %target_name,
                        "max transfer depth exceeded, stopping transfer chain"
                    );
                    if root_agent.strict_transfer_policy() {
                        yield Err(adk_core::AdkError::new(
                            adk_core::ErrorComponent::Agent,
                            adk_core::ErrorCategory::InvalidInput,
                            "agent.transfer.depth_exceeded",
                            format!(
                                "maximum handoff depth {max_depth} exceeded while transferring to '{target_name}'"
                            ),
                        ));
                        return;
                    }
                    break;
                }

                let governance = adk_core::AgentTransferRequest {
                    invocation_id: invocation_id.clone(),
                    from: transfer_source.clone(),
                    to: target_name.clone(),
                    depth: transfer_depth,
                };
                match root_agent.govern_transfer(&governance).await {
                    Ok(adk_core::AgentTransferDecision::Allow) => {}
                    Ok(adk_core::AgentTransferDecision::Deny { reason }) => {
                        yield Err(adk_core::AdkError::new(
                            adk_core::ErrorComponent::Agent,
                            adk_core::ErrorCategory::Forbidden,
                            "agent.transfer.denied",
                            format!(
                                "handoff from '{transfer_source}' to '{target_name}' was denied: {reason}"
                            ),
                        ));
                        return;
                    }
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }

                let target_agent = match Self::find_agent(&root_agent, &target_name) {
                    Some(a) => a,
                    None => {
                        tracing::warn!(target = %target_name, "transfer target not found in agent tree");
                        if root_agent.strict_transfer_policy() {
                            yield Err(adk_core::AdkError::new(
                                adk_core::ErrorComponent::Agent,
                                adk_core::ErrorCategory::NotFound,
                                "agent.transfer.target_not_found",
                                format!(
                                    "handoff target '{target_name}' was not found in the agent tree"
                                ),
                            ));
                            return;
                        }
                        break;
                    }
                };

                // Compute transfer_targets for the target agent:
                // - parent: the agent that transferred to it (or root if applicable)
                // - peers: siblings in the agent tree
                // - children: handled by the agent itself via sub_agents()
                let mut transfer_run_config = run_config.clone();
                if let Some(targets) = root_agent.transfer_targets_for(&target_name) {
                    transfer_run_config.transfer_targets = targets;
                    transfer_run_config.parent_agent = None;
                } else {
                    let (parent_name, peer_names) =
                        Self::compute_transfer_context(&root_agent, &target_name);
                    let mut targets = Vec::new();
                    if let Some(ref parent) = parent_name {
                        targets.push(parent.clone());
                    }
                    targets.extend(peer_names);
                    transfer_run_config.transfer_targets = targets;
                    transfer_run_config.parent_agent = parent_name;
                }
                root_agent.configure_run(&target_name, &mut transfer_run_config);

                // For transfers, we reuse the same mutable session to preserve state
                let transfer_invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
                let mut transfer_ctx = match InvocationContext::with_mutable_session(
                    transfer_invocation_id.clone(),
                    target_agent.clone(),
                    ctx.user_id().to_string(),
                    ctx.app_name().to_string(),
                    ctx.session_id().to_string(),
                    effective_user_content.clone(),
                    ctx.mutable_session().clone(),
                ) {
                    Ok(ctx) => ctx,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                transfer_ctx = transfer_ctx.with_orchestration_root_invocation_id(
                    adk_core::InvocationContext::orchestration_root_invocation_id(ctx.as_ref())
                        .to_string(),
                );

                #[cfg(feature = "artifacts")]
                if let Some(ref service) = artifact_service_clone {
                    let scoped = adk_artifact::ScopedArtifacts::new(
                        service.clone(),
                        ctx.app_name().to_string(),
                        ctx.user_id().to_string(),
                        ctx.session_id().to_string(),
                    );
                    transfer_ctx = transfer_ctx.with_artifacts(Arc::new(scoped));
                }
                if let Some(ref memory) = memory_service_clone {
                    transfer_ctx = transfer_ctx.with_memory(memory.clone());
                }
                transfer_ctx = transfer_ctx.with_run_config(transfer_run_config);
                if let Some(rc) = request_context.clone() {
                    transfer_ctx = transfer_ctx.with_request_context(rc);
                }
                if let Some(token) = cancellation_token.as_ref() {
                    transfer_ctx = transfer_ctx.with_cancellation_token(token.clone());
                }
                if let Some(shared_state) = adk_core::CallbackContext::shared_state(ctx.as_ref()) {
                    transfer_ctx = transfer_ctx.with_shared_state(shared_state);
                }

                let transfer_ctx = Arc::new(transfer_ctx);

                // Run the transferred agent
                let mut transfer_stream = match target_agent.run(transfer_ctx.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        #[cfg(feature = "plugins")]
                        if let Some(manager) = plugin_manager.as_ref() {
                            manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        }
                        yield Err(e);
                        return;
                    }
                };

                // Stream events from the transferred agent, capturing any further transfer
                while let Some(result) = {
                    // Race the next event against cancellation for prompt
                    // mid-await interruption of the transferred agent.
                    match cancellation_token.as_ref() {
                        Some(token) => {
                            tokio::select! {
                                biased;
                                _ = token.cancelled() => {
                                    tracing::info!("cancellation fired during transferred agent stream await");
                                    #[cfg(feature = "plugins")]
                                    if let Some(manager) = plugin_manager.as_ref() {
                                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                    }
                                    return;
                                }
                                result = transfer_stream.next() => result,
                            }
                        }
                        None => transfer_stream.next().await,
                    }
                } {
                    match result {
                        Ok(mut event) => {
                            #[cfg(feature = "plugins")]
                            if let Some(manager) = plugin_manager.as_ref() {
                                match manager
                                    .run_on_event(
                                        transfer_ctx.clone() as Arc<dyn adk_core::InvocationContext>,
                                        event.clone(),
                                    )
                                    .await
                                {
                                    Ok(Some(modified)) => {
                                        event = modified;
                                    }
                                    Ok(None) => {}
                                    Err(e) => {
                                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                        yield Err(e);
                                        return;
                                    }
                                }
                            }

                            preserve_streamed_content(&mut streamed_content, &mut event);

                            // Capture further transfer requests
                            if let Some(target) = &event.actions.transfer_to_agent {
                                let source = if event.author.is_empty() {
                                    target_agent.name()
                                } else {
                                    &event.author
                                };
                                if let Some(allowed) = root_agent.transfer_targets_for(source)
                                    && !allowed.contains(target)
                                {
                                    if root_agent.strict_transfer_policy() {
                                        yield Err(adk_core::AdkError::new(
                                            adk_core::ErrorComponent::Agent,
                                            adk_core::ErrorCategory::Forbidden,
                                            "agent.transfer.target_forbidden",
                                            format!(
                                                "agent '{source}' cannot hand off to '{target}'; allowed targets: {}",
                                                allowed.join(", ")
                                            ),
                                        ));
                                        return;
                                    }
                                    tracing::warn!(source, target, "handoff target rejected by root policy");
                                } else {
                                    current_transfer_target =
                                        Some((source.to_string(), target.clone()));
                                }
                            }

                            // Apply state delta for transferred agent too
                            if !event.actions.state_delta.is_empty() {
                                transfer_ctx.mutable_session().apply_state_delta(&event.actions.state_delta);
                            }

                            // Add to mutable session
                            transfer_ctx.mutable_session().append_event(event.clone());

                            if !event.llm_response.partial
                                && let Err(e) = session_service
                                    .append_event_for_identity(adk_session::AppendEventRequest {
                                        identity: identity.clone(),
                                        event: event.clone(),
                                    })
                                    .await {
                                    #[cfg(feature = "plugins")]
                                    if let Some(manager) = plugin_manager.as_ref() {
                                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                    }
                                    yield Err(e);
                                    return;
                                }
                            yield Ok(event);
                        }
                        Err(e) => {
                            #[cfg(feature = "plugins")]
                            if let Some(manager) = plugin_manager.as_ref() {
                                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                            }
                            yield Err(e);
                            return;
                        }
                    }
                }
            }

            // ===== CONTEXT COMPACTION =====
            // After all events have been processed, check if compaction should trigger.
            // This runs in the background after the invocation completes.
            if let Some(ref compaction_cfg) = compaction_config {
                let event_count = ctx.mutable_session().as_ref().events_len();

                if event_count > 0 {
                    let all_events = ctx.mutable_session().as_ref().events_snapshot();
                    let invocation_count = all_events.iter().filter(|e| e.author == "user").count()
                        as u32;

                    if invocation_count > 0
                        && invocation_count.is_multiple_of(compaction_cfg.compaction_interval)
                    {
                        // Determine the window of events to compact
                        // We compact all events except the most recent overlap_size invocations
                        let overlap = compaction_cfg.overlap_size as usize;

                        // Find the boundary: keep the last `overlap` user messages and everything after
                        let user_msg_indices: Vec<usize> = all_events.iter()
                            .enumerate()
                            .filter(|(_, e)| e.author == "user")
                            .map(|(i, _)| i)
                            .collect();

                        // Keep the last `overlap` user messages intact.
                        // When overlap is 0, compact everything.
                        let compact_up_to = if overlap == 0 {
                            all_events.len()
                        } else if user_msg_indices.len() > overlap {
                            // Compact up to (but not including) the overlap-th-from-last user message
                            user_msg_indices[user_msg_indices.len() - overlap]
                        } else {
                            // Not enough user messages to satisfy overlap — skip compaction
                            0
                        };

                        if compact_up_to > 0 {
                            let events_to_compact = &all_events[..compact_up_to];

                            match compaction_cfg.summarizer.summarize_events(events_to_compact).await {
                                Ok(Some(compaction_event)) => {
                                    // Persist the compaction event
                                    if let Err(e) = session_service
                                        .append_event_for_identity(adk_session::AppendEventRequest {
                                            identity: identity.clone(),
                                            event: compaction_event.clone(),
                                        })
                                        .await {
                                        tracing::warn!(error = %e, "Failed to persist compaction event");
                                    } else {
                                        tracing::info!(
                                            compacted_events = compact_up_to,
                                            "Context compaction completed"
                                        );
                                    }
                                }
                                Ok(None) => {
                                    tracing::debug!("Compaction summarizer returned no result");
                                }
                                Err(e) => {
                                    // Compaction failure is non-fatal — log and continue
                                    tracing::warn!(error = %e, "Context compaction failed");
                                }
                            }
                        }
                    }
                }
            }

            #[cfg(feature = "plugins")]
            if let Some(manager) = plugin_manager.as_ref() {
                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
            }
        };

        Ok(Box::pin(s))
    }

    /// Convenience method that accepts string arguments.
    ///
    /// Converts `user_id` and `session_id` to their typed equivalents
    /// and delegates to [`run()`](Self::run).
    ///
    /// # Errors
    ///
    /// Returns an error if either string fails identity validation
    /// (empty, contains null bytes, or exceeds length limit).
    pub async fn run_str(
        &self,
        user_id: &str,
        session_id: &str,
        user_content: Content,
    ) -> Result<EventStream> {
        let user_id = UserId::try_from(user_id)?;
        let session_id = SessionId::try_from(session_id)?;
        self.run(user_id, session_id, user_content).await
    }

    /// Interrupt a running agent for the given session.
    ///
    /// Cancels the agent's current execution within the event loop. Events
    /// already produced and appended to the session are preserved — only
    /// future events are stopped. The caller can then issue a new `run()`
    /// call with a different instruction to redirect the agent.
    ///
    /// Returns `true` if a running session was found and interrupted,
    /// `false` if no active run exists for that session ID.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Start a run in the background
    /// let mut stream = runner.run(user_id, session_id, content).await?;
    /// tokio::spawn(async move { while stream.next().await.is_some() {} });
    ///
    /// // Later, interrupt it
    /// let was_running = runner.interrupt("session-1");
    /// assert!(was_running);
    ///
    /// // Redirect with a new instruction
    /// let mut stream = runner.run(user_id, session_id, new_content).await?;
    /// ```
    pub fn interrupt(&self, session_id: &str) -> bool {
        let runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        let matching: Vec<&ActiveRun> =
            runs.values().filter(|run| run.identity.session_id.as_ref() == session_id).collect();
        if matching.is_empty() {
            tracing::debug!(session.id = session_id, "no active run to interrupt");
            return false;
        }
        tracing::info!(
            session.id = session_id,
            run.count = matching.len(),
            "interrupting running agent"
        );
        for run in matching {
            run.token.cancel();
        }
        true
    }

    /// Interrupts runs for one exact identity.
    ///
    /// A session ID is only unique within an app and user, so this is the precise
    /// form of [`Runner::interrupt`] for a `Runner` shared across tenants.
    /// Returns `true` when at least one run was cancelled.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let cancelled = runner.interrupt_identity("my-app", "user-1", "session-1");
    /// ```
    pub fn interrupt_identity(&self, app_name: &str, user_id: &str, session_id: &str) -> bool {
        let runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        let mut cancelled = false;
        for run in runs.values() {
            if run.identity.app_name.as_ref() == app_name
                && run.identity.user_id.as_ref() == user_id
                && run.identity.session_id.as_ref() == session_id
            {
                run.token.cancel();
                cancelled = true;
            }
        }
        if !cancelled {
            tracing::debug!(
                app.name = app_name,
                user.id = user_id,
                session.id = session_id,
                "no active run to interrupt"
            );
        }
        cancelled
    }

    /// Returns the identity of every run currently in flight.
    ///
    /// One identity appears once per in-flight run, so a repeated entry means that
    /// identity has concurrent runs.
    pub fn active_runs(&self) -> Vec<adk_core::AdkIdentity> {
        let runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.values().map(|run| run.identity.clone()).collect()
    }

    /// Returns the session IDs of all currently running agent executions.
    ///
    /// Session IDs are deduplicated. Use [`Runner::active_runs`] when the app and
    /// user dimensions matter.
    pub fn active_session_ids(&self) -> Vec<String> {
        let runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> =
            runs.values().map(|run| run.identity.session_id.as_ref().to_string()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Returns a reference to the context compaction configuration, if set.
    ///
    /// This is used by the runner's generate_content loop to detect token limit
    /// errors and apply compaction strategies before retrying.
    #[cfg(feature = "context-compaction")]
    pub fn context_compaction(&self) -> Option<&crate::compaction::CompactionConfig> {
        self.context_compaction.as_deref()
    }

    /// Find which agent should handle the request based on session history
    pub fn find_agent_to_run(
        root_agent: &Arc<dyn Agent>,
        session: &dyn adk_session::Session,
    ) -> Arc<dyn Agent> {
        // Look at recent events to find last agent that responded
        let events = session.events();
        for i in (0..events.len()).rev() {
            if let Some(event) = events.at(i) {
                // Check for explicit transfer
                if let Some(target_name) = &event.actions.transfer_to_agent
                    && let Some(agent) = Self::find_agent(root_agent, target_name)
                {
                    return agent;
                }

                if event.author == "user" {
                    continue;
                }

                // Try to find this agent in the tree
                if let Some(agent) = Self::find_agent(root_agent, &event.author) {
                    // Check if agent allows transfer up the tree
                    if Self::is_transferable(root_agent, &agent) {
                        return agent;
                    }
                }
            }
        }

        // Default to root agent
        root_agent.clone()
    }

    /// Check if an agent found in session history can be resumed directly for
    /// the next user message.
    ///
    /// An agent is a valid direct-resumption target only if it *and* every
    /// ancestor up to the root permit agent transfer
    /// ([`Agent::supports_agent_transfer`]). A deterministic workflow agent
    /// (sequential, parallel, loop, conditional) anywhere on that path returns
    /// `false`, which forces resumption to restart from the workflow root
    /// rather than a single sub-agent that happened to respond last. This
    /// mirrors Google ADK's `_is_transferable_across_agent_tree`.
    ///
    /// LLM-driven transfer-policy enforcement
    /// (`disallow_transfer_to_parent` / `disallow_transfer_to_peers`) is still
    /// handled inside `LlmAgent::run()` when it builds the `transfer_to_agent`
    /// tool's valid-target list; this check only governs cross-turn resumption.
    fn is_transferable(root_agent: &Arc<dyn Agent>, agent: &Arc<dyn Agent>) -> bool {
        // Walk the tree from the root down to the target. Every agent on that
        // path (root through target, inclusive) must support transfer for the
        // target to be a valid direct-resumption point. `Some(true)` = found
        // and fully transferable, `Some(false)` = found but a workflow agent
        // sits on the path, `None` = target not present in this subtree.
        fn path_supports_transfer(current: &Arc<dyn Agent>, target: &str) -> Option<bool> {
            if current.name() == target {
                return Some(current.supports_agent_transfer());
            }
            for sub in current.sub_agents() {
                if let Some(sub_ok) = path_supports_transfer(sub, target) {
                    return Some(current.supports_agent_transfer() && sub_ok);
                }
            }
            None
        }

        path_supports_transfer(root_agent, agent.name()).unwrap_or(true)
    }

    /// Recursively search agent tree for agent with given name
    pub fn find_agent(current: &Arc<dyn Agent>, target_name: &str) -> Option<Arc<dyn Agent>> {
        if current.name() == target_name {
            return Some(current.clone());
        }

        for sub_agent in current.sub_agents() {
            if let Some(found) = Self::find_agent(sub_agent, target_name) {
                return Some(found);
            }
        }

        None
    }

    /// Compute the parent name and peer names for a given agent in the tree.
    /// Returns `(parent_name, peer_names)`.
    ///
    /// Walks the agent tree to find the parent of `target_name`, then collects
    /// the parent's name and the sibling agent names (excluding the target itself).
    pub fn compute_transfer_context(
        root: &Arc<dyn Agent>,
        target_name: &str,
    ) -> (Option<String>, Vec<String>) {
        // If the target is the root itself, there's no parent or peers
        if root.name() == target_name {
            return (None, Vec::new());
        }

        // BFS/DFS to find the parent of target_name
        fn find_parent(current: &Arc<dyn Agent>, target: &str) -> Option<Arc<dyn Agent>> {
            for sub in current.sub_agents() {
                if sub.name() == target {
                    return Some(current.clone());
                }
                if let Some(found) = find_parent(sub, target) {
                    return Some(found);
                }
            }
            None
        }

        match find_parent(root, target_name) {
            Some(parent) => {
                let parent_name = parent.name().to_string();
                let peers: Vec<String> = parent
                    .sub_agents()
                    .iter()
                    .filter(|a| a.name() != target_name)
                    .map(|a| a.name().to_string())
                    .collect();
                (Some(parent_name), peers)
            }
            None => (None, Vec::new()),
        }
    }
}

#[cfg(test)]
mod streamed_content_tests {
    use super::preserve_streamed_content;
    use adk_core::{Content, Event, Part};
    use std::collections::HashMap;

    fn text(event: &Event) -> String {
        event
            .content()
            .map(|content| content.parts.iter().filter_map(Part::text).collect())
            .unwrap_or_default()
    }

    #[test]
    fn final_empty_event_preserves_streamed_text_for_persistence() {
        let mut accumulated = HashMap::new();
        let mut first = Event::with_id("response-1", "inv-1");
        first.llm_response.partial = true;
        first.llm_response.content = Some(Content::new("model").with_text("Verify "));
        preserve_streamed_content(&mut accumulated, &mut first);

        let mut second = Event::with_id("response-1", "inv-1");
        second.llm_response.partial = true;
        second.llm_response.content = Some(Content::new("model").with_text("the invoice."));
        preserve_streamed_content(&mut accumulated, &mut second);

        let mut final_event = Event::with_id("response-1", "inv-1");
        final_event.llm_response.turn_complete = true;
        preserve_streamed_content(&mut accumulated, &mut final_event);

        assert_eq!(text(&final_event), "Verify the invoice.");
        assert!(accumulated.is_empty());
    }
}
