#[cfg(feature = "skills")]
use crate::skill_shim::load_skill_index;
use crate::skill_shim::{SelectionPolicy, SkillIndex};
use adk_core::{
    AfterAgentCallback, Agent, BeforeAgentCallback, CallbackContext, Content, Event, EventStream,
    InvocationContext, Part, ReadonlyContext, Result, Session, State, WorkflowPosition,
};
use async_stream::stream;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Default maximum iterations for LoopAgent when none is specified.
/// Prevents infinite loops from consuming unbounded resources.
pub const DEFAULT_LOOP_MAX_ITERATIONS: u32 = 1000;

/// Loop agent executes sub-agents repeatedly for N iterations or until escalation
pub struct LoopAgent {
    name: String,
    description: String,
    sub_agents: Vec<Arc<dyn Agent>>,
    max_iterations: u32,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
}

impl LoopAgent {
    /// Create a new loop agent with the given name and sub-agents.
    pub fn new(name: impl Into<String>, sub_agents: Vec<Arc<dyn Agent>>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            sub_agents,
            max_iterations: DEFAULT_LOOP_MAX_ITERATIONS,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: 2000,
            before_callbacks: Arc::new(Vec::new()),
            after_callbacks: Arc::new(Vec::new()),
        }
    }

    /// Set the agent description.
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Set the maximum number of loop iterations.
    ///
    /// Each iteration runs every sub-agent once, in order; an escalation ends the loop early.
    /// `0` runs no iterations: the before- and after-agent callbacks still run and the agent
    /// completes immediately without invoking any sub-agent.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_agent::LoopAgent;
    ///
    /// let refine = LoopAgent::new("refine", vec![writer, critic]).with_max_iterations(3);
    /// ```
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set a preloaded skills index for this agent.
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills(self) -> Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Auto-load skills from `.skills/` under a custom root directory.
    #[cfg(feature = "skills")]
    pub fn with_skills_from_root(mut self, root: impl AsRef<std::path::Path>) -> Result<Self> {
        let index = load_skill_index(root).map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    /// Customize skill selection behavior.
    #[cfg(feature = "skills")]
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    /// Limit injected skill content length.
    #[cfg(feature = "skills")]
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
        self
    }

    /// Add a before-agent callback.
    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.before_callbacks) {
            callbacks.push(callback);
        }
        self
    }

    /// Add an after-agent callback.
    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        if let Some(callbacks) = Arc::get_mut(&mut self.after_callbacks) {
            callbacks.push(callback);
        }
        self
    }
}

/// Tracks the history a loop's sub-agents produce, for a parent session that does not.
///
/// Under the runner the parent session records every event as it is yielded
/// ([`Session::records_invocation_events`]), and history is read from it. Otherwise this
/// keeps the history the loop started with and adds each sub-agent event to it, with its
/// author, so each sub-agent sees other agents' turns as context.
struct HistoryTrackingSession {
    parent_ctx: Arc<dyn InvocationContext>,
    history: RwLock<TrackedHistory>,
    /// Event id of the streamed response that produced the last history entry.
    streamed_id: RwLock<Option<String>>,
    state: StateTrackingState,
}

/// History entries and, for each, the author and branch of the event that produced it;
/// `None` for the history the loop started with.
#[derive(Default)]
struct TrackedHistory {
    contents: Vec<Content>,
    origins: Vec<Option<(String, String)>>,
}

impl TrackedHistory {
    fn push(&mut self, content: Content, origin: Option<(String, String)>) {
        self.contents.push(content);
        self.origins.push(origin);
    }

    /// Whether the last entry came from `origin`'s event author.
    fn last_from(&self, origin: &(String, String)) -> bool {
        matches!(self.origins.last(), Some(Some(last)) if last == origin)
    }
}

struct StateTrackingState {
    values: RwLock<HashMap<String, serde_json::Value>>,
}

impl StateTrackingState {
    fn new(parent_ctx: &Arc<dyn InvocationContext>) -> Self {
        Self { values: RwLock::new(parent_ctx.session().state().all()) }
    }

    fn apply_delta(&self, delta: &HashMap<String, serde_json::Value>) {
        if delta.is_empty() {
            return;
        }

        let mut values = self.values.write().unwrap_or_else(|e| e.into_inner());
        for (key, value) in delta {
            values.insert(key.clone(), value.clone());
        }
    }
}

impl State for StateTrackingState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.values.read().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        self.values.write().unwrap_or_else(|e| e.into_inner()).insert(key, value);
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.values.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl HistoryTrackingSession {
    fn new(parent_ctx: Arc<dyn InvocationContext>) -> Self {
        let mut history = TrackedHistory::default();
        for content in parent_ctx.session().conversation_history() {
            history.push(content, None);
        }
        Self {
            history: RwLock::new(history),
            streamed_id: RwLock::new(None),
            state: StateTrackingState::new(&parent_ctx),
            parent_ctx,
        }
    }

    /// Whether history is read from the parent session, which records every event itself.
    fn delegates(&self) -> bool {
        self.parent_ctx.session().records_invocation_events()
    }

    fn apply_event(&self, event: &Event) {
        if let Some(content) = &event.llm_response.content {
            let origin = (event.author.clone(), event.branch.clone());
            // Consolidate streaming chunks: chunks of one response merge into the
            // entry its first chunk started instead of creating new entries.
            // This prevents N streaming chunks from becoming N separate Content
            // entries that bloat context for subsequent agents.
            let mut history = self.history.write().unwrap_or_else(|e| e.into_inner());
            let mut streamed_id = self.streamed_id.write().unwrap_or_else(|e| e.into_inner());
            // A resumed call's response takes its pending response's place, so a later
            // sub-agent sees one response per call.
            let unplaced;
            let content = if !event.llm_response.partial
                && content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
            {
                unplaced =
                    adk_core::supersede_pending_responses(&mut history.contents, content.clone());
                match &unplaced {
                    Some(rest) => rest,
                    None => {
                        *streamed_id = None;
                        drop(history);
                        drop(streamed_id);
                        self.state.apply_delta(&event.actions.state_delta);
                        return;
                    }
                }
            } else {
                content
            };
            let continues_stream = streamed_id.as_deref() == Some(event.id.as_str());
            let same_author = history.last_from(&origin);

            if event.llm_response.partial {
                // Partial chunk — merge into the entry this response started, so a
                // later complete snapshot replaces only this response.
                if continues_stream
                    && same_author
                    && let Some(last) = history.contents.last_mut()
                    && last.role == content.role
                {
                    merge_parts(last, &content.parts);
                    return;
                }
                // No matching last entry — start a new one
                history.push(content.clone(), Some(origin));
                *streamed_id = Some(event.id.clone());
            } else {
                let content_complete = event
                    .llm_response
                    .provider_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("content_complete"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
                let replaced_stream = continues_stream && content_complete && same_author;
                *streamed_id = None;
                if replaced_stream && let Some(last) = history.contents.last_mut() {
                    // A complete snapshot already contains every streamed delta.
                    *last = content.clone();
                } else if continues_stream
                    && same_author
                    && let Some(last) = history.contents.last_mut()
                    && last.role == content.role
                {
                    // Without a complete snapshot the accumulated text is already in the
                    // last entry from the partial merges above, so the final chunk (which
                    // may carry the last fragment or be empty) is merged into it.
                    merge_parts(last, &content.parts);
                } else if !content.parts.is_empty() {
                    history.push(content.clone(), Some(origin));
                }
            }
        }
        self.state.apply_delta(&event.actions.state_delta);
    }
}

/// Appends `parts` to `last`, joining text onto its last text part.
fn merge_parts(last: &mut Content, parts: &[Part]) {
    for part in parts {
        if let Part::Text { text } = part
            && let Some(Part::Text { text: existing }) = last.parts.last_mut()
        {
            existing.push_str(text);
        } else {
            last.parts.push(part.clone());
        }
    }
}

impl Session for HistoryTrackingSession {
    fn id(&self) -> &str {
        self.parent_ctx.session().id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.session().app_name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.session().user_id()
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<Content> {
        if self.delegates() {
            return self.parent_ctx.session().conversation_history();
        }
        self.history.read().unwrap_or_else(|e| e.into_inner()).contents.clone()
    }

    fn conversation_history_for_agent(&self, agent_name: &str) -> Vec<Content> {
        if self.delegates() {
            return self.parent_ctx.session().conversation_history_for_agent(agent_name);
        }
        self.conversation_history()
    }

    fn conversation_history_scoped(&self, agent_name: Option<&str>, branch: &str) -> Vec<Content> {
        if self.delegates() {
            return self.parent_ctx.session().conversation_history_scoped(agent_name, branch);
        }
        self.conversation_history()
    }

    fn conversation_history_with_context(&self, agent_name: &str, branch: &str) -> Vec<Content> {
        if self.delegates() {
            return self.parent_ctx.session().conversation_history_with_context(agent_name, branch);
        }
        let history = self.history.read().unwrap_or_else(|e| e.into_inner());
        let call_ids: std::collections::HashSet<String> = history
            .contents
            .iter()
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionCall { id: Some(id), .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        history
            .contents
            .iter()
            .zip(&history.origins)
            .filter_map(|(content, origin)| match origin {
                Some((_, event_branch))
                    if adk_core::history::is_tool_branch_event(branch, event_branch, &call_ids) =>
                {
                    None
                }
                Some((author, _)) if author != "user" && author != agent_name => {
                    adk_core::history::other_agent_context(author, content)
                }
                Some(_) | None => Some(content.clone()),
            })
            .collect()
    }

    fn records_invocation_events(&self) -> bool {
        self.delegates()
    }

    fn held_tool_calls(&self, agent_name: &str, branch: &str) -> Vec<adk_core::HeldToolCall> {
        self.parent_ctx.session().held_tool_calls(agent_name, branch)
    }

    fn workflow_resume_positions(
        &self,
        workflow: &str,
        branch: &str,
    ) -> Vec<adk_core::WorkflowPosition> {
        self.parent_ctx.session().workflow_resume_positions(workflow, branch)
    }

    fn append_to_history(&self, content: Content) {
        if self.delegates() {
            self.parent_ctx.session().append_to_history(content);
            return;
        }
        self.history.write().unwrap_or_else(|e| e.into_inner()).push(content, None);
    }
}

/// Builds a [`HistoryTrackingContext`] for the wrapper conformance tests.
#[cfg(test)]
pub(crate) fn history_tracking_context_for_test(
    parent: Arc<dyn InvocationContext>,
) -> Arc<dyn InvocationContext> {
    Arc::new(HistoryTrackingContext::new(parent))
}

struct HistoryTrackingContext {
    parent_ctx: Arc<dyn InvocationContext>,
    session: HistoryTrackingSession,
}

impl HistoryTrackingContext {
    fn new(parent_ctx: Arc<dyn InvocationContext>) -> Self {
        let session = HistoryTrackingSession::new(parent_ctx.clone());
        Self { parent_ctx, session }
    }

    fn apply_event(&self, event: &Event) {
        self.session.apply_event(event);
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for HistoryTrackingContext {
    fn invocation_id(&self) -> &str {
        self.parent_ctx.invocation_id()
    }

    fn agent_name(&self) -> &str {
        self.parent_ctx.agent_name()
    }

    fn user_id(&self) -> &str {
        self.parent_ctx.user_id()
    }

    fn app_name(&self) -> &str {
        self.parent_ctx.app_name()
    }

    fn session_id(&self) -> &str {
        self.parent_ctx.session_id()
    }

    fn branch(&self) -> &str {
        self.parent_ctx.branch()
    }

    fn user_content(&self) -> &Content {
        self.parent_ctx.user_content()
    }

    fn acting_context(&self) -> adk_core::ActingContext {
        self.parent_ctx.acting_context()
    }

    fn is_authenticated(&self) -> bool {
        self.parent_ctx.is_authenticated()
    }
}

#[async_trait]
impl CallbackContext for HistoryTrackingContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent_ctx.artifacts()
    }

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.parent_ctx.shared_state()
    }
}

#[async_trait]
impl InvocationContext for HistoryTrackingContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.parent_ctx.agent()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        self.parent_ctx.memory()
    }

    fn session(&self) -> &dyn Session {
        &self.session
    }

    fn run_config(&self) -> &adk_core::RunConfig {
        self.parent_ctx.run_config()
    }

    fn end_invocation(&self) {
        self.parent_ctx.end_invocation();
    }

    fn ended(&self) -> bool {
        self.parent_ctx.ended()
    }

    fn is_cancelled(&self) -> bool {
        self.parent_ctx.is_cancelled()
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent_ctx.user_scopes()
    }

    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.parent_ctx.request_metadata()
    }

    fn authoritative_transfer_targets(&self) -> bool {
        self.parent_ctx.authoritative_transfer_targets()
    }
    fn delegation_depth(&self) -> u32 {
        self.parent_ctx.delegation_depth()
    }
    fn max_delegation_depth(&self) -> Option<u32> {
        self.parent_ctx.max_delegation_depth()
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.parent_ctx.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.parent_ctx.orchestration_edge_id()
    }

    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.parent_ctx.requires_tool_confirmation(tool_name)
    }

    async fn get_secret(&self, name: &str) -> adk_core::Result<Option<String>> {
        self.parent_ctx.get_secret(name).await
    }

    async fn get_secret_for(
        &self,
        request: &adk_core::SecretRequest,
    ) -> adk_core::Result<Option<String>> {
        self.parent_ctx.get_secret_for(request).await
    }
}

/// The earliest recorded `(iteration, sub-agent index)` a resuming run starts at, if any
/// names a sub-agent of this loop within its iterations.
fn resume_point(
    positions: &[WorkflowPosition],
    sub_agents: &[Arc<dyn Agent>],
    max_iterations: u32,
) -> Option<(u32, usize)> {
    positions
        .iter()
        .filter(|position| position.iteration < max_iterations)
        .filter_map(|position| {
            let index = sub_agents.iter().position(|agent| agent.name() == position.sub_agent)?;
            Some((position.iteration, index))
        })
        .min()
}

#[async_trait]
impl Agent for LoopAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    fn supports_agent_transfer(&self) -> bool {
        // Deterministic workflow agent: on cross-turn resumption the runner
        // must restart from this root so every sub-agent runs again, rather
        // than resuming a single sub-agent that responded last.
        false
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let sub_agents = self.sub_agents.clone();
        let max_iterations = self.max_iterations;
        let before_callbacks = self.before_callbacks.clone();
        let after_callbacks = self.after_callbacks.clone();
        let agent_name = self.name.clone();
        let run_ctx = super::skill_context::with_skill_injected_context(
            ctx,
            self.skills_index.as_ref(),
            &self.skill_policy,
            self.max_skill_chars,
        );
        let run_ctx = Arc::new(HistoryTrackingContext::new(run_ctx));
        let resume_at = resume_point(
            &run_ctx.session().workflow_resume_positions(&agent_name, run_ctx.branch()),
            &sub_agents,
            max_iterations,
        );

        let s = stream! {
            use futures::StreamExt;

            // ===== BEFORE AGENT CALLBACKS =====
            for callback in before_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut early_event = Event::new(run_ctx.invocation_id());
                        early_event.author = agent_name.clone();
                        early_event.llm_response.content = Some(content);
                        yield Ok(early_event);

                        for after_cb in after_callbacks.as_ref() {
                            match after_cb(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                                Ok(Some(after_content)) => {
                                    let mut after_event = Event::new(run_ctx.invocation_id());
                                    after_event.author = agent_name.clone();
                                    after_event.llm_response.content = Some(after_content);
                                    yield Ok(after_event);
                                    return;
                                }
                                Ok(None) => continue,
                                Err(e) => { yield Err(e); return; }
                            }
                        }
                        return;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }

            // A range cannot underflow, so `max_iterations == 0` runs the body zero times
            // instead of wrapping to `u32::MAX`. A run that resumes a hold starts at the
            // iteration and sub-agent it stopped at, so sub-agents that already finished in
            // that iteration do not run again.
            let (first_iteration, mut first_index) = resume_at.unwrap_or((0, 0));
            for iteration in first_iteration..max_iterations {
                let mut should_exit = false;

                for (index, agent) in sub_agents.iter().enumerate().skip(first_index) {
                    let mut stream = agent.run(run_ctx.clone() as Arc<dyn InvocationContext>).await?;
                    let mut held = false;

                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(mut event) => {
                                if event.actions.tool_confirmation.is_some() {
                                    held = true;
                                    event.actions.workflow_positions.insert(
                                        agent_name.clone(),
                                        WorkflowPosition::new(iteration, agent.name()),
                                    );
                                }
                                run_ctx.apply_event(&event);
                                if event.actions.escalate {
                                    should_exit = true;
                                }
                                yield Ok(event);
                            }
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }

                    // A held call pauses the workflow: no later sub-agent runs, and the
                    // after-agent callbacks wait for the run that finishes it.
                    if held {
                        tracing::info!(
                            agent.name = %agent_name,
                            workflow.iteration = iteration,
                            workflow.sub_agent = %agent.name(),
                            workflow.index = index,
                            "workflow paused on a held tool call"
                        );
                        return;
                    }

                    if should_exit {
                        break;
                    }
                }
                first_index = 0;

                if should_exit {
                    break;
                }
            }

            // ===== AFTER AGENT CALLBACKS =====
            for callback in after_callbacks.as_ref() {
                match callback(run_ctx.clone() as Arc<dyn CallbackContext>).await {
                    Ok(Some(content)) => {
                        let mut after_event = Event::new(run_ctx.invocation_id());
                        after_event.author = agent_name.clone();
                        after_event.llm_response.content = Some(content);
                        yield Ok(after_event);
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => { yield Err(e); return; }
                }
            }
        };

        Ok(Box::pin(s))
    }
}
