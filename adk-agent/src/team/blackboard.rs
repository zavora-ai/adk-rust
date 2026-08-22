use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use adk_core::{
    Agent, Artifacts, CallbackContext, Content, EventStream, InvocationContext, Memory,
    ReadonlyContext, Result, RunConfig, Session, State,
};
use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    EventDisposition, RelationshipKind, ResolvedTeamMember, TeamBudget, TeamEdgeStart, TeamError,
    TeamRuntimeRegistry, TeamTerminationPolicy,
};

/// Portable, governed shared-transcript team definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlackboardSpec {
    /// Executable root name.
    pub name: String,
    /// Human-readable purpose.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Exact participant names. Order is meaningful for round-robin scheduling.
    pub members: Vec<String>,
    /// Speaker selection strategy.
    pub schedule: BlackboardSchedule,
    /// Exact permitted speaker transitions for model-selected scheduling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<BlackboardTransition>,
    /// Bounded execution and transcript policy.
    #[serde(default)]
    pub policy: BlackboardPolicy,
}

/// Speaker scheduling for a blackboard team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "strategy")]
pub enum BlackboardSchedule {
    /// Every member speaks once per round in declared order.
    RoundRobin,
    /// A designated member selects one permitted speaker per round by emitting
    /// a normal agent transfer event. The transfer is consumed internally.
    Selector {
        /// Selector member name.
        selector: String,
    },
}

/// One exact permitted speaker transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlackboardTransition {
    /// Current speaker or selector.
    pub from: String,
    /// Next speaker.
    pub to: String,
}

/// Bounded blackboard execution policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlackboardPolicy {
    /// Maximum complete scheduling rounds.
    pub max_rounds: u32,
    /// Maximum messages visible to each next speaker.
    pub history: BlackboardHistoryPolicy,
    /// Aggregate resource budget.
    #[serde(default)]
    pub budget: TeamBudget,
    /// Clean termination conditions.
    #[serde(default)]
    pub termination: TeamTerminationPolicy,
}

impl Default for BlackboardPolicy {
    fn default() -> Self {
        Self {
            max_rounds: 4,
            history: BlackboardHistoryPolicy::Last { max_messages: 32 },
            budget: TeamBudget { max_events: Some(128), ..TeamBudget::default() },
            termination: TeamTerminationPolicy::default(),
        }
    }
}

/// Transcript projection visible to each blackboard speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum BlackboardHistoryPolicy {
    /// Broadcast the full transcript.
    Full,
    /// Broadcast only the most recent messages.
    Last {
        /// Maximum visible messages.
        max_messages: usize,
    },
}

impl BlackboardSpec {
    /// Validates members, bounds, schedule, and exact transitions.
    pub fn validate(&self) -> std::result::Result<(), TeamError> {
        if self.name.trim().is_empty() {
            return Err(TeamError::EmptyName { field: "blackboard.name" });
        }
        if self.members.is_empty() {
            return Err(TeamError::InvalidPolicy("blackboard.members"));
        }
        if self.policy.max_rounds == 0 {
            return Err(TeamError::InvalidPolicy("blackboard.maxRounds"));
        }
        if matches!(self.policy.history, BlackboardHistoryPolicy::Last { max_messages: 0 }) {
            return Err(TeamError::InvalidPolicy("blackboard.history.maxMessages"));
        }
        let mut names = BTreeSet::new();
        for member in &self.members {
            if member.trim().is_empty() {
                return Err(TeamError::EmptyName { field: "blackboard.member" });
            }
            if !names.insert(member.as_str()) {
                return Err(TeamError::DuplicateMember(member.clone()));
            }
        }
        if names.contains(self.name.as_str()) {
            return Err(TeamError::TeamNameCollision(self.name.clone()));
        }
        if let BlackboardSchedule::Selector { selector } = &self.schedule {
            if !names.contains(selector.as_str()) {
                return Err(TeamError::UnknownCoordinator(selector.clone()));
            }
            if !self.transitions.iter().any(|transition| transition.from == *selector) {
                return Err(TeamError::InvalidPolicy("blackboard.selectorTransitions"));
            }
        }
        let mut transitions = BTreeSet::new();
        for transition in &self.transitions {
            if !names.contains(transition.from.as_str()) {
                return Err(TeamError::UnknownRelationshipMember {
                    endpoint: "source",
                    name: transition.from.clone(),
                });
            }
            if !names.contains(transition.to.as_str()) {
                return Err(TeamError::UnknownRelationshipMember {
                    endpoint: "target",
                    name: transition.to.clone(),
                });
            }
            if transition.from == transition.to {
                return Err(TeamError::SelfRelationship(transition.from.clone()));
            }
            if !transitions.insert((transition.from.as_str(), transition.to.as_str())) {
                return Err(TeamError::InvalidPolicy("blackboard.duplicateTransition"));
            }
        }
        Ok(())
    }

    /// Binds exact member names and returns an executable blackboard root.
    pub fn compile(
        &self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
    ) -> std::result::Result<CompiledBlackboardTeam, TeamError> {
        self.validate()?;
        let declared: BTreeSet<&str> = self.members.iter().map(String::as_str).collect();
        let mut registry = HashMap::new();
        for agent in agents {
            let name = agent.name().to_string();
            if !declared.contains(name.as_str()) {
                return Err(TeamError::UnexpectedAgentBinding(name));
            }
            if registry.insert(name.clone(), agent).is_some() {
                return Err(TeamError::DuplicateAgentBinding(name));
            }
        }
        let members: Vec<Arc<dyn Agent>> = self
            .members
            .iter()
            .map(|name| {
                registry.get(name).cloned().ok_or_else(|| TeamError::MissingAgent(name.clone()))
            })
            .collect::<std::result::Result<_, _>>()?;
        let roster = self
            .members
            .iter()
            .map(|name| ResolvedTeamMember {
                member: name.clone(),
                binding: name.clone(),
                capabilities: Vec::new(),
                version: None,
                digest: None,
                trust_labels: Vec::new(),
            })
            .collect();
        Ok(CompiledBlackboardTeam {
            spec: self.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            members,
            runtime: Arc::new(TeamRuntimeRegistry::new(
                self.name.clone(),
                roster,
                self.policy.budget.clone(),
                self.policy.termination.clone(),
                Vec::new(),
            )),
        })
    }
}

/// Executable blackboard/group-chat root.
pub struct CompiledBlackboardTeam {
    spec: BlackboardSpec,
    name: String,
    description: String,
    members: Vec<Arc<dyn Agent>>,
    runtime: Arc<TeamRuntimeRegistry>,
}

impl CompiledBlackboardTeam {
    /// Returns the portable blackboard definition.
    pub fn spec(&self) -> &BlackboardSpec {
        &self.spec
    }

    /// Returns the latest serializable execution receipt.
    pub fn execution_snapshot(&self, invocation_id: &str) -> Option<super::TeamExecutionSnapshot> {
        self.runtime.snapshot(invocation_id)
    }

    /// Restores a persisted execution receipt for a matching team and roster.
    pub fn restore_execution_snapshot(
        &self,
        snapshot: super::TeamExecutionSnapshot,
    ) -> std::result::Result<(), TeamError> {
        self.runtime.restore(snapshot)
    }
}

impl std::fmt::Debug for CompiledBlackboardTeam {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompiledBlackboardTeam")
            .field("name", &self.name)
            .field("members", &self.spec.members)
            .finish()
    }
}

#[async_trait]
impl Agent for CompiledBlackboardTeam {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.members
    }

    fn supports_agent_transfer(&self) -> bool {
        false
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let spec = self.spec.clone();
        let members = self.members.clone();
        let runtime = self.runtime.clone();
        let root_invocation_id = ctx.orchestration_root_invocation_id().to_string();
        runtime.check_budget(&root_invocation_id)?;
        let stream = async_stream::stream! {
            for _round in 0..spec.policy.max_rounds {
                let scheduled: Vec<Arc<dyn Agent>> = match &spec.schedule {
                    BlackboardSchedule::RoundRobin => members.clone(),
                    BlackboardSchedule::Selector { selector } => {
                        let Some(selector_agent) = members.iter().find(|member| member.name() == selector).cloned() else {
                            yield Err(adk_core::AdkError::agent(format!("blackboard selector '{selector}' is unavailable")));
                            return;
                        };
                        vec![selector_agent]
                    }
                };
                let mut selected = None;
                for speaker in scheduled {
                    let projected = Arc::new(BlackboardContext::new(
                        ctx.clone(),
                        speaker.clone(),
                        spec.policy.history,
                    ));
                    let mut events = match speaker.run(projected).await {
                        Ok(events) => events,
                        Err(error) => {
                            runtime.fail(&root_invocation_id, error.to_string());
                            yield Err(error);
                            return;
                        }
                    };
                    while let Some(result) = events.next().await {
                        let mut event = match result {
                            Ok(event) => event,
                            Err(error) => {
                                runtime.fail(&root_invocation_id, error.to_string());
                                yield Err(error);
                                return;
                            }
                        };
                        if event.author.is_empty() {
                            event.author = speaker.name().to_string();
                        }
                        if let Some(target) = event.actions.transfer_to_agent.take() {
                            let allowed = spec.transitions.iter().any(|transition| {
                                transition.from == speaker.name() && transition.to == target
                            });
                            if !allowed {
                                let error = adk_core::AdkError::agent(format!(
                                    "blackboard speaker '{}' cannot select '{}'; transition is not declared",
                                    speaker.name(), target
                                ));
                                runtime.fail(&root_invocation_id, error.to_string());
                                yield Err(error);
                                return;
                            }
                            let edge_id = match runtime.start_edge(
                                &root_invocation_id,
                                TeamEdgeStart {
                                    execution_id: None,
                                    parent_id: ctx.orchestration_edge_id().map(str::to_string),
                                    from: speaker.name(),
                                    to: &target,
                                    kind: RelationshipKind::Handoff,
                                    attempt: 1,
                                },
                            ) {
                                Ok(edge_id) => edge_id,
                                Err(error) => {
                                    yield Err(error);
                                    return;
                                }
                            };
                            runtime.finish_edge(&root_invocation_id, &edge_id, None);
                            selected = Some(target);
                        }
                        match runtime.record_event(
                            &root_invocation_id,
                            ctx.orchestration_edge_id(),
                            &mut event,
                        ) {
                            Ok(EventDisposition::Continue) => yield Ok(event),
                            Ok(EventDisposition::Terminate) => {
                                ctx.end_invocation();
                                yield Ok(event);
                                return;
                            }
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    }
                }

                if let BlackboardSchedule::Selector { selector } = &spec.schedule {
                    let Some(target) = selected.take() else {
                        let error = adk_core::AdkError::agent(format!(
                            "blackboard selector '{selector}' did not select a permitted speaker"
                        ));
                        runtime.fail(&root_invocation_id, error.to_string());
                        yield Err(error);
                        return;
                    };
                    let Some(speaker) = members.iter().find(|member| member.name() == target).cloned() else {
                        yield Err(adk_core::AdkError::agent(format!("selected blackboard speaker '{target}' is unavailable")));
                        return;
                    };
                    let projected = Arc::new(BlackboardContext::new(
                        ctx.clone(),
                        speaker.clone(),
                        spec.policy.history,
                    ));
                    let mut events = match speaker.run(projected).await {
                        Ok(events) => events,
                        Err(error) => {
                            runtime.fail(&root_invocation_id, error.to_string());
                            yield Err(error);
                            return;
                        }
                    };
                    while let Some(result) = events.next().await {
                        let mut event = match result {
                            Ok(event) => event,
                            Err(error) => {
                                runtime.fail(&root_invocation_id, error.to_string());
                                yield Err(error);
                                return;
                            }
                        };
                        if event.author.is_empty() {
                            event.author = speaker.name().to_string();
                        }
                        if event.actions.transfer_to_agent.is_some() {
                            let error = adk_core::AdkError::agent(format!(
                                "selected blackboard speaker '{}' cannot transfer during a selector-managed turn",
                                speaker.name()
                            ));
                            runtime.fail(&root_invocation_id, error.to_string());
                            yield Err(error);
                            return;
                        }
                        match runtime.record_event(&root_invocation_id, None, &mut event) {
                            Ok(EventDisposition::Continue) => yield Ok(event),
                            Ok(EventDisposition::Terminate) => {
                                ctx.end_invocation();
                                yield Ok(event);
                                return;
                            }
                            Err(error) => {
                                yield Err(error);
                                return;
                            }
                        }
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

struct BlackboardContext {
    inner: Arc<dyn InvocationContext>,
    agent: Arc<dyn Agent>,
    session: ProjectedSession,
}

impl BlackboardContext {
    fn new(
        inner: Arc<dyn InvocationContext>,
        agent: Arc<dyn Agent>,
        history_policy: BlackboardHistoryPolicy,
    ) -> Self {
        let mut history = inner.session().conversation_history();
        if let BlackboardHistoryPolicy::Last { max_messages } = history_policy
            && history.len() > max_messages
        {
            history.drain(..history.len() - max_messages);
        }
        Self {
            session: ProjectedSession {
                id: inner.session().id().to_string(),
                app_name: inner.session().app_name().to_string(),
                user_id: inner.session().user_id().to_string(),
                state: SnapshotState::new(inner.session().state().all()),
                history,
            },
            inner,
            agent,
        }
    }
}

#[async_trait]
impl ReadonlyContext for BlackboardContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }
    fn agent_name(&self) -> &str {
        self.agent.name()
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn branch(&self) -> &str {
        self.inner.branch()
    }
    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for BlackboardContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }
    fn tool_outcome(&self) -> Option<adk_core::ToolOutcome> {
        self.inner.tool_outcome()
    }
    fn tool_name(&self) -> Option<&str> {
        self.inner.tool_name()
    }
    fn tool_input(&self) -> Option<&serde_json::Value> {
        self.inner.tool_input()
    }
    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.inner.shared_state()
    }
}

#[async_trait]
impl InvocationContext for BlackboardContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.inner.memory()
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        self.inner.run_config()
    }
    fn end_invocation(&self) {
        self.inner.end_invocation();
    }
    fn ended(&self) -> bool {
        self.inner.ended()
    }
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }
    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        self.inner.request_metadata()
    }
    fn authoritative_transfer_targets(&self) -> bool {
        true
    }
    fn delegation_depth(&self) -> u32 {
        self.inner.delegation_depth()
    }
    fn max_delegation_depth(&self) -> Option<u32> {
        self.inner.max_delegation_depth()
    }
    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }
    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }
    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.inner.requires_tool_confirmation(tool_name)
    }
    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }
    async fn get_secret_for(&self, request: &adk_core::SecretRequest) -> Result<Option<String>> {
        self.inner.get_secret_for(request).await
    }
}

struct ProjectedSession {
    id: String,
    app_name: String,
    user_id: String,
    state: SnapshotState,
    history: Vec<Content>,
}

impl Session for ProjectedSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<Content> {
        self.history.clone()
    }
}

struct SnapshotState(RwLock<HashMap<String, serde_json::Value>>);

impl SnapshotState {
    fn new(values: HashMap<String, serde_json::Value>) -> Self {
        Self(RwLock::new(values))
    }
}

impl State for SnapshotState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.read().unwrap_or_else(|error| error.into_inner()).get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        if adk_core::validate_state_key(&key).is_ok() {
            self.0.write().unwrap_or_else(|error| error.into_inner()).insert(key, value);
        }
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.read().unwrap_or_else(|error| error.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Event;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EmptyState;

    impl State for EmptyState {
        fn get(&self, _key: &str) -> Option<serde_json::Value> {
            None
        }

        fn set(&mut self, _key: String, _value: serde_json::Value) {}

        fn all(&self) -> HashMap<String, serde_json::Value> {
            HashMap::new()
        }
    }

    struct TestSession;

    impl Session for TestSession {
        fn id(&self) -> &str {
            "blackboard-session"
        }

        fn app_name(&self) -> &str {
            "blackboard-app"
        }

        fn user_id(&self) -> &str {
            "blackboard-user"
        }

        fn state(&self) -> &dyn State {
            &EmptyState
        }

        fn conversation_history(&self) -> Vec<Content> {
            (0..4).map(|index| Content::new("user").with_text(format!("message-{index}"))).collect()
        }
    }

    struct TestContext {
        content: Content,
        config: RunConfig,
        session: TestSession,
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "blackboard-invocation"
        }

        fn agent_name(&self) -> &str {
            "blackboard"
        }

        fn user_id(&self) -> &str {
            "blackboard-user"
        }

        fn app_name(&self) -> &str {
            "blackboard-app"
        }

        fn session_id(&self) -> &str {
            "blackboard-session"
        }

        fn branch(&self) -> &str {
            ""
        }

        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl InvocationContext for TestContext {
        fn agent(&self) -> Arc<dyn Agent> {
            panic!("not used")
        }

        fn memory(&self) -> Option<Arc<dyn Memory>> {
            None
        }

        fn session(&self) -> &dyn Session {
            &self.session
        }

        fn run_config(&self) -> &RunConfig {
            &self.config
        }

        fn end_invocation(&self) {}

        fn ended(&self) -> bool {
            false
        }
    }

    struct Speaker {
        name: String,
        transfer: Option<String>,
        expected_history: usize,
        runs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Agent for Speaker {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "deterministic blackboard speaker"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            assert_eq!(ctx.session().conversation_history().len(), self.expected_history);
            self.runs.fetch_add(1, Ordering::SeqCst);
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name.clone();
            event.actions.transfer_to_agent.clone_from(&self.transfer);
            event.llm_response.content = Some(Content::new("model").with_text(&self.name));
            Ok(Box::pin(futures::stream::once(async { Ok(event) })))
        }
    }

    fn context() -> Arc<TestContext> {
        Arc::new(TestContext {
            content: Content::new("user").with_text("discuss"),
            config: RunConfig::default(),
            session: TestSession,
        })
    }

    #[tokio::test]
    async fn round_robin_bounds_rounds_and_projects_history() {
        let first_runs = Arc::new(AtomicUsize::new(0));
        let second_runs = Arc::new(AtomicUsize::new(0));
        let spec = BlackboardSpec {
            name: "blackboard".to_string(),
            description: String::new(),
            members: vec!["first".to_string(), "second".to_string()],
            schedule: BlackboardSchedule::RoundRobin,
            transitions: Vec::new(),
            policy: BlackboardPolicy {
                max_rounds: 2,
                history: BlackboardHistoryPolicy::Last { max_messages: 2 },
                ..BlackboardPolicy::default()
            },
        };
        let team = spec
            .compile([
                Arc::new(Speaker {
                    name: "first".to_string(),
                    transfer: None,
                    expected_history: 2,
                    runs: first_runs.clone(),
                }) as Arc<dyn Agent>,
                Arc::new(Speaker {
                    name: "second".to_string(),
                    transfer: None,
                    expected_history: 2,
                    runs: second_runs.clone(),
                }),
            ])
            .unwrap();
        let events = team.run(context()).await.unwrap().collect::<Vec<_>>().await;
        assert_eq!(events.len(), 4);
        assert_eq!(first_runs.load(Ordering::SeqCst), 2);
        assert_eq!(second_runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn selector_enforces_exact_transition_allowlist() {
        let spec = BlackboardSpec {
            name: "blackboard".to_string(),
            description: String::new(),
            members: vec!["selector".to_string(), "speaker".to_string()],
            schedule: BlackboardSchedule::Selector { selector: "selector".to_string() },
            transitions: vec![BlackboardTransition {
                from: "selector".to_string(),
                to: "speaker".to_string(),
            }],
            policy: BlackboardPolicy { max_rounds: 1, ..BlackboardPolicy::default() },
        };
        let team = spec
            .compile([
                Arc::new(Speaker {
                    name: "selector".to_string(),
                    transfer: Some("undeclared".to_string()),
                    expected_history: 4,
                    runs: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn Agent>,
                Arc::new(Speaker {
                    name: "speaker".to_string(),
                    transfer: None,
                    expected_history: 4,
                    runs: Arc::new(AtomicUsize::new(0)),
                }),
            ])
            .unwrap();
        let events = team.run(context()).await.unwrap().collect::<Vec<_>>().await;
        assert!(events[0].as_ref().unwrap_err().to_string().contains("not declared"));
    }
}
