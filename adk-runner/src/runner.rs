use crate::InvocationContext;
use crate::cache::CacheManager;
use adk_artifact::ArtifactService;
use adk_core::{
    Agent, CacheCapable, Content, ContextCacheConfig, EventStream, Memory, Result, RunConfig,
};
use adk_plugin::PluginManager;
use adk_session::SessionService;
use adk_skill::{SkillInjector, SkillInjectorConfig};
use async_stream::stream;
use std::sync::Arc;
use tracing::Instrument;

pub struct RunnerConfig {
    pub app_name: String,
    pub agent: Arc<dyn Agent>,
    pub session_service: Arc<dyn SessionService>,
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    pub memory_service: Option<Arc<dyn Memory>>,
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
    pub context_cache_config: Option<ContextCacheConfig>,
    /// Optional cache-capable model reference for automatic cache management.
    /// Set this to the same model used by the agent if it supports caching.
    pub cache_capable: Option<Arc<dyn CacheCapable>>,
}

pub struct Runner {
    app_name: String,
    root_agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn Memory>>,
    plugin_manager: Option<Arc<PluginManager>>,
    skill_injector: Option<Arc<SkillInjector>>,
    run_config: RunConfig,
    compaction_config: Option<adk_core::EventsCompactionConfig>,
    context_cache_config: Option<ContextCacheConfig>,
    cache_capable: Option<Arc<dyn CacheCapable>>,
    cache_manager: Option<Arc<tokio::sync::Mutex<CacheManager>>>,
}

impl Runner {
    pub fn new(config: RunnerConfig) -> Result<Self> {
        let cache_manager = config
            .context_cache_config
            .as_ref()
            .map(|c| Arc::new(tokio::sync::Mutex::new(CacheManager::new(c.clone()))));
        Ok(Self {
            app_name: config.app_name,
            root_agent: config.agent,
            session_service: config.session_service,
            artifact_service: config.artifact_service,
            memory_service: config.memory_service,
            plugin_manager: config.plugin_manager,
            skill_injector: None,
            run_config: config.run_config.unwrap_or_default(),
            compaction_config: config.compaction_config,
            context_cache_config: config.context_cache_config,
            cache_capable: config.cache_capable,
            cache_manager,
        })
    }

    /// Enable skill injection using a pre-built injector.
    ///
    /// Skill injection runs before plugin `on_user_message` callbacks.
    pub fn with_skill_injector(mut self, injector: SkillInjector) -> Self {
        self.skill_injector = Some(Arc::new(injector));
        self
    }

    /// Enable skill injection by auto-loading `.skills/` from the given root path.
    pub fn with_auto_skills(
        mut self,
        root: impl AsRef<std::path::Path>,
        config: SkillInjectorConfig,
    ) -> adk_skill::SkillResult<Self> {
        let injector = SkillInjector::from_root(root, config)?;
        self.skill_injector = Some(Arc::new(injector));
        Ok(self)
    }

    pub async fn run(
        &self,
        user_id: String,
        session_id: String,
        user_content: Content,
    ) -> Result<EventStream> {
        let app_name = self.app_name.clone();
        let session_service = self.session_service.clone();
        let root_agent = self.root_agent.clone();
        let artifact_service = self.artifact_service.clone();
        let memory_service = self.memory_service.clone();
        let plugin_manager = self.plugin_manager.clone();
        let skill_injector = self.skill_injector.clone();
        let mut run_config = self.run_config.clone();
        let compaction_config = self.compaction_config.clone();
        let context_cache_config = self.context_cache_config.clone();
        let cache_capable = self.cache_capable.clone();
        let cache_manager_ref = self.cache_manager.clone();

        let s = stream! {
            // Get or create session
            let session = match session_service
                .get(adk_session::GetRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    num_recent_events: None,
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

            // Clone services for potential reuse in transfer
            let artifact_service_clone = artifact_service.clone();
            let memory_service_clone = memory_service.clone();

            // Create invocation context with MutableSession
            let invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
            let mut effective_user_content = user_content.clone();
            let mut selected_skill_name = String::new();
            let mut selected_skill_id = String::new();

            if let Some(injector) = skill_injector.as_ref() {
                if let Some(matched) = adk_skill::apply_skill_injection(
                    &mut effective_user_content,
                    injector.index(),
                    injector.policy(),
                    injector.max_injected_chars(),
                ) {
                    selected_skill_name = matched.skill.name;
                    selected_skill_id = matched.skill.id;
                }
            }

            let mut invocation_ctx = InvocationContext::new(
                invocation_id.clone(),
                agent_to_run.clone(),
                user_id.clone(),
                app_name.clone(),
                session_id.clone(),
                effective_user_content.clone(),
                Arc::from(session),
            );

            // Add optional services
            if let Some(service) = artifact_service {
                // Wrap service with ScopedArtifacts to bind session context
                let scoped = adk_artifact::ScopedArtifacts::new(
                    service,
                    app_name.clone(),
                    user_id.clone(),
                    session_id.clone(),
                );
                invocation_ctx = invocation_ctx.with_artifacts(Arc::new(scoped));
            }
            if let Some(memory) = memory_service {
                invocation_ctx = invocation_ctx.with_memory(memory);
            }

            // Apply run config (streaming mode, etc.)
            invocation_ctx = invocation_ctx.with_run_config(run_config.clone());

            let mut ctx = Arc::new(invocation_ctx);

            if let Some(manager) = plugin_manager.as_ref() {
                match manager
                    .run_before_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>)
                    .await
                {
                    Ok(Some(content)) => {
                        let mut early_event = adk_core::Event::new(&invocation_id);
                        early_event.author = agent_to_run.name().to_string();
                        early_event.llm_response.content = Some(content);

                        ctx.mutable_session().append_event(early_event.clone());
                        if let Err(e) = session_service.append_event(&session_id, early_event.clone()).await {
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

                        let mut refreshed_ctx = InvocationContext::with_mutable_session(
                            invocation_id.clone(),
                            agent_to_run.clone(),
                            user_id.clone(),
                            app_name.clone(),
                            session_id.clone(),
                            effective_user_content.clone(),
                            ctx.mutable_session().clone(),
                        );

                        if let Some(service) = artifact_service_clone.clone() {
                            let scoped = adk_artifact::ScopedArtifacts::new(
                                service,
                                app_name.clone(),
                                user_id.clone(),
                                session_id.clone(),
                            );
                            refreshed_ctx = refreshed_ctx.with_artifacts(Arc::new(scoped));
                        }
                        if let Some(memory) = memory_service_clone.clone() {
                            refreshed_ctx = refreshed_ctx.with_memory(memory);
                        }
                        refreshed_ctx = refreshed_ctx.with_run_config(run_config.clone());
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
            let mut user_event = adk_core::Event::new(&invocation_id);
            user_event.author = "user".to_string();
            user_event.llm_response.content = Some(effective_user_content.clone());

            // Also add to mutable session for immediate visibility
            // Note: adk_session::Event is a re-export of adk_core::Event, so we can use it directly
            ctx.mutable_session().append_event(user_event.clone());

            if let Err(e) = session_service.append_event(&session_id, user_event).await {
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
                let mut cm = cm_mutex.lock().await;
                if cm.is_enabled() {
                    if cm.active_cache_name().is_none() || cm.needs_refresh() {
                        // Gather system instruction from the agent's description
                        // (the full instruction is resolved inside the agent, but the
                        // description provides a reasonable proxy for cache keying)
                        let system_instruction = agent_to_run.description().to_string();
                        let tools = std::collections::HashMap::new();
                        let ttl = context_cache_config.as_ref().map_or(600, |c| c.ttl_seconds);

                        match cache_model.create_cache(&system_instruction, &tools, ttl).await {
                            Ok(name) => {
                                // Delete old cache if refreshing
                                if let Some(old) = cm.clear_active_cache() {
                                    if let Err(e) = cache_model.delete_cache(&old).await {
                                        tracing::warn!(
                                            old_cache = %old,
                                            error = %e,
                                            "failed to delete old cache, proceeding with new cache"
                                        );
                                    }
                                }
                                cm.set_active_cache(name);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "cache creation failed, proceeding without cache"
                                );
                            }
                        }
                    }

                    // Attach cache name to run config so agents can use it
                    if let Some(cache_name) = cm.record_invocation() {
                        run_config.cached_content = Some(cache_name.to_string());
                        // Rebuild the invocation context with the updated run config
                        let mut refreshed_ctx = InvocationContext::with_mutable_session(
                            invocation_id.clone(),
                            agent_to_run.clone(),
                            user_id.clone(),
                            app_name.clone(),
                            session_id.clone(),
                            effective_user_content.clone(),
                            ctx.mutable_session().clone(),
                        );
                        if let Some(service) = artifact_service_clone.clone() {
                            let scoped = adk_artifact::ScopedArtifacts::new(
                                service,
                                app_name.clone(),
                                user_id.clone(),
                                session_id.clone(),
                            );
                            refreshed_ctx = refreshed_ctx.with_artifacts(Arc::new(scoped));
                        }
                        if let Some(memory) = memory_service_clone.clone() {
                            refreshed_ctx = refreshed_ctx.with_memory(memory);
                        }
                        refreshed_ctx = refreshed_ctx.with_run_config(run_config.clone());
                        ctx = Arc::new(refreshed_ctx);
                    }
                }
            }

            // Run the agent with instrumentation (ADK-Go style attributes)
            let agent_span = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.invocation_id" = %invocation_id,
                "gcp.vertex.agent.session_id" = %session_id,
                "gcp.vertex.agent.event_id" = %invocation_id, // Use invocation_id as event_id for agent spans
                "gen_ai.conversation.id" = %session_id,
                "agent.name" = %agent_to_run.name(),
                "adk.skills.selected_name" = %selected_skill_name,
                "adk.skills.selected_id" = %selected_skill_id
            );

            let mut agent_stream = match agent_to_run.run(ctx.clone()).instrument(agent_span).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(manager) = plugin_manager.as_ref() {
                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                    }
                    yield Err(e);
                    return;
                }
            };

            // Stream events and check for transfers
            use futures::StreamExt;
            let mut transfer_target: Option<String> = None;

            while let Some(result) = agent_stream.next().await {
                match result {
                    Ok(event) => {
                        let mut event = event;

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

                        // Check for transfer action
                        if let Some(target) = &event.actions.transfer_to_agent {
                            transfer_target = Some(target.clone());
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
                        if let Err(e) = session_service.append_event(&session_id, event.clone()).await {
                            if let Some(manager) = plugin_manager.as_ref() {
                                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                            }
                            yield Err(e);
                            return;
                        }
                        yield Ok(event);
                    }
                    Err(e) => {
                        if let Some(manager) = plugin_manager.as_ref() {
                            manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                        }
                        yield Err(e);
                        return;
                    }
                }
            }

            // If a transfer was requested, automatically invoke the target agent
            if let Some(target_name) = transfer_target {
                if let Some(target_agent) = Self::find_agent(&root_agent, &target_name) {
                    // For transfers, we reuse the same mutable session to preserve state
                    let transfer_invocation_id = format!("inv-{}", uuid::Uuid::new_v4());
                    let mut transfer_ctx = InvocationContext::with_mutable_session(
                        transfer_invocation_id.clone(),
                        target_agent.clone(),
                        user_id.clone(),
                        app_name.clone(),
                        session_id.clone(),
                        effective_user_content.clone(),
                        ctx.mutable_session().clone(),
                    );

                    if let Some(service) = artifact_service_clone {
                        let scoped = adk_artifact::ScopedArtifacts::new(
                            service,
                            app_name.clone(),
                            user_id.clone(),
                            session_id.clone(),
                        );
                        transfer_ctx = transfer_ctx.with_artifacts(Arc::new(scoped));
                    }
                    if let Some(memory) = memory_service_clone {
                        transfer_ctx = transfer_ctx.with_memory(memory);
                    }

                    let transfer_ctx = Arc::new(transfer_ctx);

                    // Run the transferred agent
                    let mut transfer_stream = match target_agent.run(transfer_ctx.clone()).await {
                        Ok(s) => s,
                        Err(e) => {
                            if let Some(manager) = plugin_manager.as_ref() {
                                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                            }
                            yield Err(e);
                            return;
                        }
                    };

                    // Stream events from the transferred agent
                    while let Some(result) = transfer_stream.next().await {
                        match result {
                            Ok(event) => {
                                let mut event = event;
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

                                // Apply state delta for transferred agent too
                                if !event.actions.state_delta.is_empty() {
                                    transfer_ctx.mutable_session().apply_state_delta(&event.actions.state_delta);
                                }

                                // Add to mutable session
                                transfer_ctx.mutable_session().append_event(event.clone());

                                if let Err(e) = session_service.append_event(&session_id, event.clone()).await {
                                    if let Some(manager) = plugin_manager.as_ref() {
                                        manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                    }
                                    yield Err(e);
                                    return;
                                }
                                yield Ok(event);
                            }
                            Err(e) => {
                                if let Some(manager) = plugin_manager.as_ref() {
                                    manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
                                }
                                yield Err(e);
                                return;
                            }
                        }
                    }
                }
            }

            // ===== CONTEXT COMPACTION =====
            // After all events have been processed, check if compaction should trigger.
            // This runs in the background after the invocation completes.
            if let Some(ref compaction_cfg) = compaction_config {
                // Count invocations by counting user events in the session
                let all_events = ctx.mutable_session().as_ref().events_snapshot();
                let invocation_count = all_events.iter()
                    .filter(|e| e.author == "user")
                    .count() as u32;

                if invocation_count > 0 && invocation_count % compaction_cfg.compaction_interval == 0 {
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
                                if let Err(e) = session_service.append_event(
                                    &session_id,
                                    compaction_event.clone(),
                                ).await {
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

            if let Some(manager) = plugin_manager.as_ref() {
                manager.run_after_run(ctx.clone() as Arc<dyn adk_core::InvocationContext>).await;
            }
        };

        Ok(Box::pin(s))
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
                if let Some(target_name) = &event.actions.transfer_to_agent {
                    if let Some(agent) = Self::find_agent(root_agent, target_name) {
                        return agent;
                    }
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

    /// Check if agent and its parent chain allow transfer up the tree
    fn is_transferable(root_agent: &Arc<dyn Agent>, agent: &Arc<dyn Agent>) -> bool {
        // For now, always allow transfer
        // TODO: Check DisallowTransferToParent flag when LlmAgent supports it
        let _ = (root_agent, agent);
        true
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
}
