//! Team members lowered to one `TeamSpec` (spec 3.1 §7.10, feature `team`).
//!
//! The harness coordinator and its members compile to a team with one `Delegate` edge per
//! member, which gives every delegation a timeout, a circuit breaker, a team budget,
//! termination and runtime discovery, and lets the harness itself be a member of a larger
//! team (spec 3.1 §6 D-5).
//!
//! | Member kind | Agent | Its tool calls |
//! |---|---|---|
//! | [`MemberKind::Local`] | an `LlmAgent` the harness builds over [`HarnessToolset::member`] | the shared [`HarnessPolicy`](crate::HarnessPolicy) and the parent's mode |
//! | `MemberKind::Acp` (`team-acp`) | an ACP-compatible coding agent over `adk-acp` | each permission request through the governed path |
//! | `MemberKind::Remote` (`team-a2a`) | `RemoteA2aAgent` | governed by the remote agent |
//! | [`MemberKind::Agent`] | the agent supplied | the shared policy; its tools are `External` |

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_agent::{
    CircuitBreakerPolicy, LlmAgentBuilder, RelationshipApprovalPolicy, RelationshipFailureStrategy,
    RelationshipKind, RelationshipPolicy, TeamAgentDescriptor, TeamAgentHealth, TeamAgentRegistry,
    TeamContextPolicy, TeamHistoryPolicy, TeamMemberSpec, TeamPolicy, TeamRelationship,
    TeamResumePolicy, TeamSpec, TeamStateMergePolicy,
};
use adk_core::audit_log::AuditActor;
use adk_core::{
    AdkError, Agent, AgentCapabilities, AgentTopology, Content, EventStream, InstructionProvider,
    InvocationContext, Llm, ReadonlyContext, Result, RunConfig, Tool,
};
use adk_devtools::Workspace;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tracing::Instrument;

use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};
use crate::policy::{ToolClassRegistry, ToolKind};
use crate::run_context::ScopedContext;
use crate::toolset::HarnessToolset;

/// A member's delegation timeout unless its spec sets another (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MEMBER_TIMEOUT.as_secs(), 15 * 60);
/// ```
pub const MEMBER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// A member's most model turns unless its spec sets another (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MEMBER_MAX_ITERATIONS, 50);
/// ```
pub const MEMBER_MAX_ITERATIONS: u32 = 50;

/// The circuit breaker on every member edge (spec 3.1 §7.10): three consecutive failures
/// open the edge for a minute.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MEMBER_CIRCUIT_BREAKER.failure_threshold, 3);
/// ```
pub const MEMBER_CIRCUIT_BREAKER: CircuitBreakerPolicy =
    CircuitBreakerPolicy { failure_threshold: 3, reset_after_ms: 60_000 };

/// Names of the read tools a [`MemberTools::ReadOnly`] member gets.
const READ_ONLY_MEMBER_TOOLS: &[&str] =
    &["read_file", "glob", "grep", "write_todos", "update_todo"];

/// One member of the harness team (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// let explore = adk_harness::MemberSpec::explore();
/// assert_eq!(explore.name, "explore");
/// assert_eq!(explore.max_iterations, 50);
/// ```
#[derive(Clone)]
pub struct MemberSpec {
    /// Member name.
    pub name: String,
    /// When to use it (model-facing).
    pub description: String,
    /// What the member is.
    pub kind: MemberKind,
    /// The member's tools.
    pub tools: MemberTools,
    /// Whether the member sees skills.
    pub skills: bool,
    /// Most model turns.
    pub max_iterations: u32,
    /// Delegation timeout; never unlimited.
    pub timeout: Duration,
    /// Structured output schema.
    pub output_schema: Option<Value>,
}

impl fmt::Debug for MemberSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemberSpec")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("kind", &self.kind_label())
            .field("skills", &self.skills)
            .field("max_iterations", &self.max_iterations)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// What a member is (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// let kind = adk_harness::MemberKind::Local { instruction: "Find the relevant files.".into(), model: None };
/// # let _ = kind;
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub enum MemberKind {
    /// An `LlmAgent` the harness builds.
    Local {
        /// The member's instruction.
        instruction: String,
        /// Its model; `None` uses the member model or the harness model.
        model: Option<Arc<dyn Llm>>,
    },
    /// An ACP-compatible coding agent.
    #[cfg(feature = "team-acp")]
    Acp(adk_acp::AcpAgentConfig),
    /// A remote agent over A2A.
    #[cfg(feature = "team-a2a")]
    Remote(adk_server::a2a::RemoteA2aConfig),
    /// Any agent the caller supplies.
    Agent(Arc<dyn Agent>),
}

/// The tools a member gets (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// assert!(matches!(adk_harness::MemberTools::default(), adk_harness::MemberTools::ReadOnly));
/// ```
#[derive(Clone, Default)]
pub enum MemberTools {
    /// Read tools only; the default.
    #[default]
    ReadOnly,
    /// Every harness tool a member may have.
    Full,
    /// Exactly these tools.
    Custom(Vec<Arc<dyn Tool>>),
}

impl MemberSpec {
    fn local(name: &str, description: &str, instruction: &str, tools: MemberTools) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            kind: MemberKind::Local { instruction: instruction.to_string(), model: None },
            tools,
            skills: false,
            max_iterations: MEMBER_MAX_ITERATIONS,
            timeout: MEMBER_TIMEOUT,
            output_schema: None,
        }
    }

    /// A read-only explorer.
    pub fn explore() -> Self {
        Self::local(
            "explore",
            "Explores the codebase read-only and reports what it found, with file paths.",
            "Investigate the question with read-only tools. Report findings with file paths and line numbers.",
            MemberTools::ReadOnly,
        )
    }

    /// A general-purpose member with the full tool set.
    pub fn general() -> Self {
        Self::local(
            "general",
            "Handles a self-contained subtask with the full tool set and reports the result.",
            "Complete the subtask you are given and report what you changed and verified.",
            MemberTools::Full,
        )
    }

    /// A reviewer that is an ACP-compatible coding agent started with `cmd` (spec 3.1
    /// §7.10, feature `team-acp`). It runs in the harness workspace and every permission
    /// it asks for is decided by the harness policy.
    ///
    /// # Example
    ///
    /// ```rust
    /// let reviewer = adk_harness::MemberSpec::reviewer_acp("my-coding-agent --acp");
    /// assert_eq!(reviewer.name, "reviewer");
    /// ```
    #[cfg(feature = "team-acp")]
    pub fn reviewer_acp(cmd: &str) -> Self {
        Self {
            name: "reviewer".to_string(),
            description: "Reviews a change with an independent coding agent and reports problems, \
                          with file paths, or confirms it is ready."
                .to_string(),
            kind: MemberKind::Acp(adk_acp::AcpAgentConfig::new(cmd)),
            tools: MemberTools::ReadOnly,
            skills: false,
            max_iterations: MEMBER_MAX_ITERATIONS,
            timeout: MEMBER_TIMEOUT,
            output_schema: None,
        }
    }

    /// The member's kind as audit records name it: `local`, `acp`, `a2a` or `agent`.
    pub fn kind_label(&self) -> &'static str {
        match &self.kind {
            MemberKind::Local { .. } => "local",
            #[cfg(feature = "team-acp")]
            MemberKind::Acp(_) => "acp",
            #[cfg(feature = "team-a2a")]
            MemberKind::Remote(_) => "a2a",
            MemberKind::Agent(_) => "agent",
        }
    }

    /// The contract of the coordinator's `Delegate` edge to this member (spec 3.1 §7.10).
    ///
    /// | Field | Value |
    /// |---|---|
    /// | `context` | `Isolated` |
    /// | `history` | `None` |
    /// | `state_keys`, `state_write_keys`, `artifact_prefixes` | empty |
    /// | `timeout_ms` | the member's `timeout` |
    /// | `approval` | `Never`: the member's own calls are governed by the shared policy |
    /// | `failure` | read-only local members retry once after a second; any other member is attempted once, since a retry would repeat its effects (§9.2 I37) |
    /// | `circuit_breaker` | [`MEMBER_CIRCUIT_BREAKER`] |
    /// | `output_schema` | the member's `output_schema` |
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_agent::{RelationshipApprovalPolicy, TeamContextPolicy};
    ///
    /// let policy = adk_harness::MemberSpec::explore().relationship_policy();
    /// assert_eq!(policy.context, Some(TeamContextPolicy::Isolated));
    /// assert_eq!(policy.timeout_ms, Some(15 * 60 * 1000));
    /// assert_eq!(policy.approval, RelationshipApprovalPolicy::Never);
    /// ```
    pub fn relationship_policy(&self) -> RelationshipPolicy {
        let read_only_local = matches!(self.kind, MemberKind::Local { .. })
            && matches!(self.tools, MemberTools::ReadOnly);
        RelationshipPolicy {
            input_schema: None,
            output_schema: self.output_schema.clone(),
            context: Some(TeamContextPolicy::Isolated),
            history: TeamHistoryPolicy::None,
            state_keys: Vec::new(),
            state_write_keys: Vec::new(),
            state_merge: TeamStateMergePolicy::RejectConflicts,
            artifact_prefixes: Vec::new(),
            resume: TeamResumePolicy::FailClosed,
            timeout_ms: Some(u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX)),
            approval: RelationshipApprovalPolicy::Never,
            failure: if read_only_local {
                RelationshipFailureStrategy::Retry { max_attempts: 2, backoff_ms: 1_000 }
            } else {
                RelationshipFailureStrategy::Retry { max_attempts: 1, backoff_ms: 0 }
            },
            circuit_breaker: Some(MEMBER_CIRCUIT_BREAKER),
        }
    }
}

/// The team policy of a harness team (spec 3.1 §7.10): `base` (or the default) with an
/// isolated context and a delegation depth of 1, which the harness always enforces
/// (spec 3.1 §9.2 I35). The budget, termination, failure policy and concurrency come from
/// `base`; the default allows four concurrent delegations.
///
/// # Example
///
/// ```rust
/// use adk_agent::{TeamContextPolicy, TeamPolicy};
///
/// let policy = adk_harness::harness_team_policy(Some(TeamPolicy { max_delegation_depth: 3, ..TeamPolicy::default() }));
/// assert_eq!(policy.context, TeamContextPolicy::Isolated);
/// assert_eq!(policy.max_delegation_depth, 1);
/// assert_eq!(policy.max_concurrent_delegations, 4);
/// ```
pub fn harness_team_policy(base: Option<TeamPolicy>) -> TeamPolicy {
    let mut policy = base.unwrap_or_default();
    policy.context = TeamContextPolicy::Isolated;
    policy.max_delegation_depth = 1;
    policy
}

/// The compiled team of a harness (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust,ignore
/// let team = harness.runtime().team().expect("the harness has members");
/// assert_eq!(team.spec().coordinator, "harness");
/// ```
pub struct HarnessTeam {
    spec: TeamSpec,
    root: Arc<dyn Agent>,
    members: BTreeMap<String, Arc<dyn Agent>>,
    own: BTreeMap<String, Arc<dyn Agent>>,
}

impl fmt::Debug for HarnessTeam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessTeam")
            .field("name", &self.spec.name)
            .field("members", &self.members.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl HarnessTeam {
    /// The `TeamSpec` the members lowered to.
    pub fn spec(&self) -> &TeamSpec {
        &self.spec
    }

    /// The members' names, in name order.
    pub fn member_names(&self) -> Vec<String> {
        self.members.keys().cloned().collect()
    }

    /// The compiled team root.
    pub(crate) fn root(&self) -> &Arc<dyn Agent> {
        &self.root
    }

    /// A review round over `reviewers` (spec 3.1 §7.10 "group work"): a round-robin
    /// `BlackboardSpec` in which each reviewer reads what the others said, for at most
    /// `max_rounds` rounds. Run it as an isolated delegation, as a plan's `Review` gate does.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a reviewer is not a member the harness built, or
    /// the round is invalid (no reviewers, zero rounds).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let round = harness.runtime().team().unwrap().review_round("review", &["reviewer", "explore"], 2)?;
    /// ```
    pub fn review_round(
        &self,
        name: &str,
        reviewers: &[&str],
        max_rounds: u32,
    ) -> Result<Arc<dyn Agent>> {
        self.review_round_with(
            name,
            reviewers,
            adk_agent::BlackboardPolicy { max_rounds, ..Default::default() },
        )
    }

    /// A review round over `reviewers` under `policy` (spec 3.1 §7.10 "group work"): a
    /// round-robin `BlackboardSpec` whose rounds, history window, `TeamBudget` and
    /// termination are the policy's. Each reviewer sees what the others said, called and got
    /// back as other-agent context. Run it as an isolated delegation, or as a runner root
    /// under the harness layer with [`HarnessAgent::governed_root`](crate::HarnessAgent::governed_root).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a reviewer is not a member the harness built, or
    /// the round is invalid (no reviewers, zero rounds, an empty history window).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_agent::{BlackboardHistoryPolicy, BlackboardPolicy};
    ///
    /// let team = harness.runtime().team().expect("the harness has members");
    /// let round = team.review_round_with(
    ///     "review",
    ///     &["review_correctness", "review_style"],
    ///     BlackboardPolicy { max_rounds: 2, history: BlackboardHistoryPolicy::Full, ..Default::default() },
    /// )?;
    /// ```
    pub fn review_round_with(
        &self,
        name: &str,
        reviewers: &[&str],
        policy: adk_agent::BlackboardPolicy,
    ) -> Result<Arc<dyn Agent>> {
        let agents = reviewers
            .iter()
            .map(|reviewer| {
                self.own.get(*reviewer).cloned().ok_or_else(|| {
                    AdkError::config(format!("'{reviewer}' is not a member the harness built"))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let spec = adk_agent::BlackboardSpec {
            name: name.to_string(),
            description: format!("A review round of {}.", reviewers.join(", ")),
            members: reviewers.iter().map(|reviewer| (*reviewer).to_string()).collect(),
            schedule: adk_agent::BlackboardSchedule::RoundRobin,
            transitions: Vec::new(),
            policy,
        };
        Ok(Arc::new(spec.compile(agents).map_err(AdkError::from)?))
    }

    /// The agent a plan's `Review` gate delegates to (spec 3.1 §7.11): the member named
    /// `reviewer*` when there is one, a review round when there are several, else
    /// `explore`.
    #[cfg(feature = "graph")]
    pub(crate) fn reviewer(&self) -> Option<Arc<dyn Agent>> {
        let reviewers: Vec<&str> = self
            .own
            .keys()
            .map(String::as_str)
            .filter(|name| name.starts_with("reviewer"))
            .collect();
        match reviewers.as_slice() {
            [] => self.own.get("explore").cloned(),
            [one] => self.own.get(*one).cloned(),
            several => self.review_round("review_round", several, 1).ok(),
        }
    }
}

/// The members a harness lowers, resolved before its policy is built: their names
/// register in the tool-class registry (spec 3.1 §7.4, §9.2 I12).
pub(crate) struct TeamPlan {
    members: Vec<MemberSpec>,
    discovered: Vec<TeamAgentDescriptor>,
    registry: Option<Arc<dyn TeamAgentRegistry>>,
    policy: TeamPolicy,
}

/// What the lowering needs from the harness build.
pub(crate) struct TeamInputs<'a> {
    pub(crate) harness: &'a str,
    pub(crate) coordinator: Arc<dyn Agent>,
    pub(crate) model: Arc<dyn Llm>,
    pub(crate) tools: &'a [Arc<dyn Tool>],
    pub(crate) mode_store: Arc<dyn ModeStore>,
    pub(crate) default_mode: HarnessMode,
    pub(crate) workspace: &'a Workspace,
    pub(crate) tool_timeout: Duration,
    #[cfg(feature = "skills")]
    pub(crate) skills: &'a crate::skills::SkillsConfig,
    /// The coordinator toolset's request observers (identity gate, loop guard).
    pub(crate) observers: &'a [Arc<dyn crate::toolset::RequestObserver>],
    /// The harness's effort policy; members run at `members` (spec 3.1 §7.23).
    pub(crate) effort: crate::posture::EffortPolicy,
}

/// Runs `future` to completion from a synchronous caller, inside a runtime or not.
///
/// `HarnessAgentBuilder::build` is synchronous and a member registry is asynchronous, so the
/// registry is read on a scoped thread with its own runtime; the caller's runtime, if any,
/// is never blocked on itself.
fn block_on<T: Send>(future: impl std::future::Future<Output = T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime =
                    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(
                        |error| AdkError::config(format!("cannot start a runtime: {error}")),
                    )?;
                Ok(runtime.block_on(future))
            })
            .join()
            .map_err(|_| AdkError::config("the member registry panicked"))?
    })
}

impl TeamPlan {
    /// Resolves the member list: the defaults (`explore`, `general`) unless left out or
    /// replaced by a member of the same name, the members given, and the candidates of
    /// `registry` that no other member names.
    ///
    /// Returns `None` when the harness has no members.
    pub(crate) fn prepare(
        harness: &str,
        members: Vec<MemberSpec>,
        defaults: bool,
        registry: Option<Arc<dyn TeamAgentRegistry>>,
        policy: Option<TeamPolicy>,
        reserved: &HashSet<String>,
    ) -> Result<Option<Self>> {
        let mut all: Vec<MemberSpec> = Vec::new();
        if defaults {
            for default in [MemberSpec::explore(), MemberSpec::general()] {
                if !members.iter().any(|member| member.name == default.name) {
                    all.push(default);
                }
            }
        }
        all.extend(members);
        let mut names = HashSet::new();
        for member in &all {
            let name = member.name.trim();
            if name.is_empty() || name != member.name {
                return Err(AdkError::config(format!(
                    "member name '{}' must be non-empty with no surrounding spaces",
                    member.name
                )));
            }
            if name == harness || reserved.contains(name) {
                return Err(AdkError::config(format!(
                    "member '{name}' collides with the harness or one of its tools; members are \
                     called as tools named after them, so choose another name"
                )));
            }
            if !names.insert(name.to_string()) {
                return Err(AdkError::config(format!("member '{name}' is declared twice")));
            }
            if member.timeout.is_zero() {
                return Err(AdkError::config(format!(
                    "member '{name}' has a zero timeout; every member edge has a timeout (spec 3.1 §9.2 I35)"
                )));
            }
            if let MemberKind::Agent(agent) = &member.kind
                && agent.name() == harness
            {
                return Err(AdkError::config(format!(
                    "member '{name}' wraps an agent named like the harness"
                )));
            }
        }
        let discovered = match &registry {
            Some(registry) => {
                let registry = registry.clone();
                let candidates = block_on(async move { registry.candidates().await })??;
                let mut seen = HashSet::new();
                candidates
                    .into_iter()
                    .filter(|candidate| candidate.health != TeamAgentHealth::Unavailable)
                    .filter(|candidate| {
                        candidate.name != harness
                            && !names.contains(&candidate.name)
                            && !reserved.contains(&candidate.name)
                            && seen.insert(candidate.name.clone())
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        if all.is_empty() && discovered.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self { members: all, discovered, registry, policy: harness_team_policy(policy) }))
    }

    /// Every member's name.
    pub(crate) fn names(&self) -> Vec<String> {
        self.members
            .iter()
            .map(|member| member.name.clone())
            .chain(self.discovered.iter().map(|candidate| candidate.name.clone()))
            .collect()
    }

    /// Registers the members' classes (spec 3.1 §7.4): every delegation is `Delegate`; a
    /// local or ACP member is one of the harness's own agents, so its calls classify as
    /// harness classes; an ACP member's read and edit permission requests are `Read` and
    /// `Edit`.
    pub(crate) fn register_classes(&self, registry: &mut ToolClassRegistry) {
        let mut updated = std::mem::take(registry);
        for name in self.names() {
            updated.register(name, ToolKind::Delegate);
        }
        for member in &self.members {
            match &member.kind {
                MemberKind::Local { .. } => updated = updated.with_agent(member.name.clone()),
                #[cfg(feature = "team-acp")]
                MemberKind::Acp(_) => {
                    updated = updated.with_agent(member.name.clone());
                    updated.register(acp::ACP_READ, ToolKind::Read);
                    updated.register(acp::ACP_EDIT, ToolKind::Edit);
                }
                #[cfg(feature = "team-a2a")]
                MemberKind::Remote(_) => {}
                MemberKind::Agent(_) => {}
            }
        }
        *registry = updated;
    }

    /// Lowers the members to one `TeamSpec` with the coordinator and compiles it
    /// (spec 3.1 §7.10).
    pub(crate) fn compile(self, inputs: TeamInputs<'_>) -> Result<HarnessTeam> {
        let mut agents: BTreeMap<String, Arc<dyn Agent>> = BTreeMap::new();
        let mut team_members = vec![
            TeamMemberSpec::new(inputs.harness)
                .with_description(inputs.coordinator.description().to_string()),
        ];
        let mut relationships = Vec::new();
        for member in &self.members {
            let agent = build_member(member, &inputs)?;
            team_members.push(
                TeamMemberSpec::new(member.name.clone())
                    .with_description(member.description.clone()),
            );
            relationships.push(
                TeamRelationship::new(
                    inputs.harness,
                    member.name.clone(),
                    RelationshipKind::Delegate,
                )
                .with_policy(member.relationship_policy()),
            );
            agents.insert(member.name.clone(), agent);
        }
        for candidate in &self.discovered {
            let spec = MemberSpec {
                name: candidate.name.clone(),
                description: candidate.description.clone(),
                kind: MemberKind::Agent(Arc::new(PendingRegistryAgent)),
                tools: MemberTools::ReadOnly,
                skills: false,
                max_iterations: MEMBER_MAX_ITERATIONS,
                timeout: MEMBER_TIMEOUT,
                output_schema: None,
            };
            let mut member = TeamMemberSpec::new(candidate.name.clone());
            if !candidate.description.is_empty() {
                member = member.with_description(candidate.description.clone());
            }
            team_members.push(member);
            relationships.push(
                TeamRelationship::new(
                    inputs.harness,
                    candidate.name.clone(),
                    RelationshipKind::Delegate,
                )
                .with_policy(spec.relationship_policy()),
            );
        }
        let spec = TeamSpec {
            name: format!("{}_team", inputs.harness),
            description: format!("The {} harness and its members.", inputs.harness),
            coordinator: inputs.harness.to_string(),
            members: team_members,
            relationships,
            policy: self.policy,
        };
        let mut own: HashMap<String, Arc<dyn Agent>> =
            agents.iter().map(|(name, agent)| (name.clone(), agent.clone())).collect();
        own.insert(
            inputs.harness.to_string(),
            Arc::new(Coordinator { inner: inputs.coordinator.clone() }),
        );
        let registry: Arc<dyn TeamAgentRegistry> = Arc::new(HarnessMemberRegistry {
            own,
            discovered: self.discovered.iter().map(|candidate| candidate.name.clone()).collect(),
            external: self.registry.clone(),
        });
        let compile_spec = spec.clone();
        let compiled = block_on(async move { compile_spec.compile_with_registry(registry).await })?
            .map_err(AdkError::from)?;
        // Members run as the team runs them, under its budget and lifecycle.
        let members = compiled
            .sub_agents()
            .iter()
            .filter(|agent| agent.name() != inputs.harness)
            .map(|agent| (agent.name().to_string(), agent.clone()))
            .collect();
        Ok(HarnessTeam { spec, root: Arc::new(compiled), members, own: agents })
    }
}

/// Builds the agent of one member, wrapped so its delegation is audited and its state
/// writes stay its own.
fn build_member(member: &MemberSpec, inputs: &TeamInputs<'_>) -> Result<Arc<dyn Agent>> {
    let inner: Arc<dyn Agent> = match &member.kind {
        MemberKind::Local { instruction, model } => {
            Arc::new(build_local(member, instruction, model.clone(), inputs)?)
        }
        #[cfg(feature = "team-acp")]
        MemberKind::Acp(config) => {
            let mut config = config.clone();
            // An ACP member works in the harness workspace, never wherever the process started.
            config.working_dir = inputs.workspace.root().to_path_buf();
            Arc::new(acp::AcpMember::new(&member.name, &member.description, config))
        }
        #[cfg(feature = "team-a2a")]
        MemberKind::Remote(config) => {
            let mut config = config.clone();
            config.name = member.name.clone();
            config.description = member.description.clone();
            Arc::new(adk_server::a2a::RemoteA2aAgent::new(config))
        }
        MemberKind::Agent(agent) => agent.clone(),
    };
    Ok(Arc::new(MemberAgent::new(member, inner)))
}

fn build_local(
    member: &MemberSpec,
    instruction: &str,
    model: Option<Arc<dyn Llm>>,
    inputs: &TeamInputs<'_>,
) -> Result<adk_agent::LlmAgent> {
    let tools: Vec<Arc<dyn Tool>> = match &member.tools {
        MemberTools::ReadOnly => inputs
            .tools
            .iter()
            .filter(|tool| READ_ONLY_MEMBER_TOOLS.contains(&tool.name()))
            .cloned()
            .collect(),
        MemberTools::Full => inputs.tools.to_vec(),
        MemberTools::Custom(tools) => tools.clone(),
    };
    let text = format!(
        "{instruction}\n\nYou are the `{name}` member of a harness team, working in the workspace at \
         {root}. You receive one request without the conversation that led to it; work on it with \
         your own tools and report what you found or changed, with file paths. You cannot \
         delegate or change the harness mode: the user's mode and approvals govern your tool \
         calls.",
        name = member.name,
        root = inputs.workspace.root().display(),
    );
    let provider: InstructionProvider = Box::new(move |_ctx: Arc<dyn ReadonlyContext>| {
        let text = text.clone();
        Box::pin(async move { Ok(text) })
    });
    let mut builder = LlmAgentBuilder::new(member.name.clone())
        .model(model.unwrap_or_else(|| inputs.model.clone()))
        .description(member.description.clone())
        .instruction_provider(provider)
        .toolset(Arc::new(
            HarnessToolset::member(tools, inputs.mode_store.clone(), inputs.default_mode)
                .with_observers(inputs.observers)
                .with_effort(inputs.effort),
        ))
        .max_iterations(member.max_iterations)
        .tool_timeout(inputs.tool_timeout);
    #[cfg(feature = "skills")]
    if member.skills
        && let crate::skills::SkillsWiring::Toolset(skills) =
            crate::skills::wire(inputs.workspace.root(), inputs.skills)?
    {
        builder = builder.toolset(skills);
    }
    if let Some(schema) = &member.output_schema {
        builder = builder.output_schema(schema.clone());
    }
    builder.build()
}

/// The coordinator as its team member: the harness `LlmAgent` under a context that keeps
/// the session state visible to its toolsets, which the team's own context does not
/// forward.
pub(crate) struct Coordinator {
    pub(crate) inner: Arc<dyn Agent>,
}

#[async_trait]
impl Agent for Coordinator {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        self.inner.sub_agents()
    }

    fn capabilities(&self) -> AgentCapabilities {
        self.inner.capabilities()
    }

    fn supports_agent_transfer(&self) -> bool {
        self.inner.supports_agent_transfer()
    }

    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        self.inner.configure_run(agent_name, config);
    }

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        self.inner.screen_user_input(content).await
    }

    async fn close(&self) {
        self.inner.close().await;
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.inner.run(Arc::new(ScopedContext::new(ctx))).await
    }
}

/// Stands in for a registry-discovered member until the registry resolves it.
struct PendingRegistryAgent;

#[async_trait]
impl Agent for PendingRegistryAgent {
    fn name(&self) -> &str {
        "pending"
    }

    fn description(&self) -> &str {
        ""
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Err(AdkError::agent("a discovered member runs only through its registry binding"))
    }
}

/// The registry a harness team compiles against: the harness's own agents first, then the
/// caller's registry for discovered members, each wrapped like any member.
struct HarnessMemberRegistry {
    own: HashMap<String, Arc<dyn Agent>>,
    discovered: HashSet<String>,
    external: Option<Arc<dyn TeamAgentRegistry>>,
}

const OWN_BINDING_PREFIX: &str = "adk-harness:";

#[async_trait]
impl TeamAgentRegistry for HarnessMemberRegistry {
    async fn candidates(&self) -> Result<Vec<TeamAgentDescriptor>> {
        let mut candidates: Vec<TeamAgentDescriptor> = self
            .own
            .iter()
            .map(|(name, agent)| TeamAgentDescriptor {
                binding: format!("{OWN_BINDING_PREFIX}{name}"),
                name: name.clone(),
                description: agent.description().to_string(),
                capabilities: Vec::new(),
                priority: i32::MAX,
                version: None,
                digest: None,
                trust_labels: Vec::new(),
                health: TeamAgentHealth::Healthy,
                expires_at_ms: None,
            })
            .collect();
        if let Some(external) = &self.external {
            candidates.extend(
                external
                    .candidates()
                    .await?
                    .into_iter()
                    .filter(|candidate| self.discovered.contains(&candidate.name)),
            );
        }
        Ok(candidates)
    }

    async fn resolve(&self, binding: &str) -> Result<Arc<dyn Agent>> {
        if let Some(name) = binding.strip_prefix(OWN_BINDING_PREFIX)
            && let Some(agent) = self.own.get(name)
        {
            return Ok(agent.clone());
        }
        let external = self
            .external
            .as_ref()
            .ok_or_else(|| AdkError::agent(format!("unknown member binding '{binding}'")))?;
        let candidate = external
            .candidates()
            .await?
            .into_iter()
            .find(|candidate| candidate.binding == binding)
            .ok_or_else(|| AdkError::agent(format!("unknown member binding '{binding}'")))?;
        let agent = external.resolve(binding).await?;
        let spec = MemberSpec {
            name: candidate.name.clone(),
            description: candidate.description.clone(),
            kind: MemberKind::Agent(agent.clone()),
            tools: MemberTools::ReadOnly,
            skills: false,
            max_iterations: MEMBER_MAX_ITERATIONS,
            timeout: MEMBER_TIMEOUT,
            output_schema: None,
        };
        Ok(Arc::new(MemberAgent::new(&spec, agent)))
    }

    async fn authorize(
        &self,
        member: &TeamMemberSpec,
        candidate: &TeamAgentDescriptor,
    ) -> Result<bool> {
        if candidate.binding.starts_with(OWN_BINDING_PREFIX) {
            return Ok(self.own.contains_key(&member.name) && candidate.name == member.name);
        }
        match &self.external {
            Some(external) if self.discovered.contains(&member.name) => {
                external.authorize(member, candidate).await
            }
            _ => Ok(false),
        }
    }
}

/// A member as its delegations run it (spec 3.1 §7.10, §9.3).
///
/// Records `harness.member.spawned` before the member starts and `harness.member.finished`
/// when it ends. The member's state and artifact writes stay in its own isolated session:
/// they are kept in a state overlay the member reads and removed from the events its caller
/// merges, so nothing a member writes reaches the parent (`state_write_keys: []`).
struct MemberAgent {
    name: String,
    description: String,
    kind: &'static str,
    timeout_ms: u64,
    inner: Arc<dyn Agent>,
}

impl MemberAgent {
    fn new(spec: &MemberSpec, inner: Arc<dyn Agent>) -> Self {
        Self {
            name: spec.name.clone(),
            description: spec.description.clone(),
            kind: spec.kind_label(),
            timeout_ms: u64::try_from(spec.timeout.as_millis()).unwrap_or(u64::MAX),
            inner,
        }
    }
}

#[async_trait]
impl Agent for MemberAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    fn capabilities(&self) -> AgentCapabilities {
        self.inner.capabilities()
    }

    fn supports_agent_transfer(&self) -> bool {
        false
    }

    fn topology(&self) -> Option<AgentTopology> {
        self.inner.topology()
    }

    fn configure_run(&self, _agent_name: &str, config: &mut RunConfig) {
        self.inner.configure_run(self.inner.name(), config);
    }

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        self.inner.screen_user_input(content).await
    }

    async fn close(&self) {
        self.inner.close().await;
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let acting = ctx.acting_context();
        let audit = ctx.run_config().audit.clone().map(|trail| {
            HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
        });
        let budget = ctx.run_config().budget_tracker.as_ref().map(|tracker| {
            let usage = tracker.usage();
            json!({
                "modelCalls": usage.model_calls,
                "totalTokens": usage.total_tokens,
                "costMicroUsd": usage.cost_micro_usd,
                "toolCalls": usage.tool_calls,
            })
        });
        let parent = acting.delegation_chain.last().map(|subject| subject.id.clone());
        let member_id = acting.agent.id.to_string();
        tracing::info!(
            harness.member = %self.name,
            member.kind = self.kind,
            delegation.depth = ctx.delegation_depth(),
            "member delegation started"
        );
        if let Some(audit) = &audit {
            audit
                .record(
                    "harness.member.spawned",
                    json!({
                        "parentAgentId": parent,
                        "memberAgentId": member_id,
                        "member": self.name,
                        "kind": self.kind,
                        "depth": ctx.delegation_depth(),
                        "timeoutMs": self.timeout_ms,
                        "budget": budget,
                    }),
                )
                .await?;
        }
        let started = Instant::now();
        let scoped = Arc::new(ScopedContext::new(ctx.clone()).with_state_overlay());
        let mut finish = FinishRecord {
            audit,
            member: self.name.clone(),
            member_id,
            kind: self.kind,
            started,
            outcome: None,
        };
        // The `harness.member` span of spec 3.1 §7.13, around everything the member does.
        #[cfg(feature = "observe")]
        let span = crate::observe::member_span(&self.name, ctx.invocation_id());
        #[cfg(not(feature = "observe"))]
        let span = tracing::Span::none();
        let mut inner = match self
            .inner
            .run(scoped.clone() as Arc<dyn InvocationContext>)
            .instrument(span.clone())
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                finish.outcome = Some("failed");
                finish.record().await;
                return Err(error);
            }
        };
        Ok(Box::pin(async_stream::stream! {
            let mut held = false;
            while let Some(item) = inner.next().instrument(span.clone()).await {
                match item {
                    Ok(mut event) => {
                        if let Some(overlay) = scoped.overlay() {
                            overlay.absorb(&event.actions.state_delta);
                        }
                        event.actions.state_delta.clear();
                        event.actions.artifact_delta.clear();
                        held |= event.actions.tool_confirmation.is_some();
                        yield Ok(event);
                    }
                    Err(error) => {
                        finish.outcome = Some("failed");
                        finish.record().await;
                        yield Err(error);
                        return;
                    }
                }
            }
            finish.outcome = Some(if held { "held" } else { "completed" });
            finish.record().await;
        }))
    }
}

/// The `harness.member.finished` record of one delegation; a delegation dropped before it
/// ends, such as by its edge timeout, is recorded as `abandoned`.
struct FinishRecord {
    audit: Option<HarnessAudit>,
    member: String,
    member_id: String,
    kind: &'static str,
    started: Instant,
    outcome: Option<&'static str>,
}

impl FinishRecord {
    fn fields(&self, outcome: &str) -> Value {
        json!({
            "memberAgentId": self.member_id,
            "member": self.member,
            "kind": self.kind,
            "outcome": outcome,
            "durationMs": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
    }

    async fn record(&mut self) {
        let outcome = self.outcome.unwrap_or("completed");
        tracing::info!(harness.member = %self.member, member.outcome = outcome, "member delegation finished");
        if let Some(audit) = self.audit.take()
            && let Err(error) = audit.record("harness.member.finished", self.fields(outcome)).await
        {
            tracing::warn!(error = %error, "could not record harness.member.finished");
        }
    }
}

impl Drop for FinishRecord {
    fn drop(&mut self) {
        if let Some(audit) = self.audit.take() {
            let fields = self.fields("abandoned");
            tracing::info!(harness.member = %self.member, "member delegation abandoned");
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(error) = audit.record("harness.member.finished", fields).await {
                        tracing::warn!(error = %error, "could not record harness.member.finished");
                    }
                });
            }
        }
    }
}

#[cfg(feature = "team-acp")]
mod acp {
    //! ACP members (spec 3.1 §7.10): an ACP-compatible coding agent whose permission
    //! requests are decided on the governed path by the harness policy.

    use std::sync::Arc;

    use adk_acp::{AcpAgentConfig, PermissionDecision, PermissionPolicy, PermissionRequest};
    use adk_core::{
        AdkError, Agent, ApprovalScope, Content, Event, EventStream, GovernedCall,
        InvocationContext, Result, ToolAuthorization, ToolEffect, ToolGate, ToolOrigin,
    };
    use async_trait::async_trait;
    use serde_json::{Value, json};

    /// Registered as `Read` for an ACP member's read and search permission requests.
    pub(super) const ACP_READ: &str = "acp_read";
    /// Registered as `Edit` for an ACP member's edit, delete and move requests with a path.
    pub(super) const ACP_EDIT: &str = "acp_edit";

    pub(super) struct AcpMember {
        name: String,
        description: String,
        config: AcpAgentConfig,
    }

    impl AcpMember {
        pub(super) fn new(name: &str, description: &str, config: AcpAgentConfig) -> Self {
            Self { name: name.to_string(), description: description.to_string(), config }
        }
    }

    fn request_path(request: &PermissionRequest) -> Option<String> {
        let input = request.raw_input.as_ref()?;
        ["path", "file_path", "filePath", "abs_path", "absolutePath", "target_path"]
            .iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    }

    /// The governed call an ACP permission request is decided as.
    pub(super) fn governed_call(member: &str, request: &PermissionRequest) -> GovernedCall {
        use adk_acp::agent_client_protocol::schema::v1::ToolKind as AcpKind;
        let kind = format!("{:?}", request.kind).to_lowercase();
        let path = request_path(request);
        let mut args =
            json!({ "title": request.title, "kind": kind, "rawInput": request.raw_input });
        let (tool, effect, origin) = match (&request.kind, path) {
            (AcpKind::Read | AcpKind::Search, path) => {
                if let Some(path) = path {
                    args["path"] = Value::String(path);
                }
                (ACP_READ.to_string(), ToolEffect::ReadOnly, None)
            }
            (AcpKind::Edit | AcpKind::Delete | AcpKind::Move, Some(path)) => {
                args["path"] = Value::String(path);
                (ACP_EDIT.to_string(), ToolEffect::NonIdempotent, None)
            }
            _ => {
                let tool = format!("acp_{kind}");
                let origin = ToolOrigin::new(
                    format!("acp:{member}"),
                    &json!({ "name": tool, "kind": kind }),
                    None,
                );
                (tool, ToolEffect::NonIdempotent, Some(origin))
            }
        };
        let mut call = GovernedCall::new(tool, format!("acp-{}", request.tool_call_id), args);
        call.effect = effect;
        call.origin = origin;
        call
    }

    /// Decides one permission request on the governed path, under the member's run
    /// config without its approval store: a call an ACP session cannot wait for is
    /// decided live by the confirmation handler, or refused.
    async fn decide(
        ctx: Arc<dyn InvocationContext>,
        member: String,
        request: PermissionRequest,
    ) -> PermissionDecision {
        let mut config = ctx.run_config().clone();
        config.approval_store = None;
        config.approval_service = None;
        let gate = ToolGate::new(
            &config,
            &member,
            ctx.invocation_id(),
            ApprovalScope::new(ctx.app_name(), ctx.user_id(), ctx.session_id()),
        )
        .with_acting_context(ctx.acting_context());
        let call = governed_call(&member, &request);
        let tool = call.tool_name.clone();
        match adk_core::authorize_tool_call(&gate, call).await {
            Ok(ToolAuthorization::Execute { .. }) => {
                tracing::info!(harness.member = %member, tool.name = %tool, "acp permission allowed");
                PermissionDecision::AllowOnce
            }
            Ok(ToolAuthorization::Refuse { reason, .. }) => {
                tracing::info!(harness.member = %member, tool.name = %tool, reason = %reason, "acp permission refused");
                PermissionDecision::Deny
            }
            Ok(ToolAuthorization::Pending { reason, .. }) => {
                tracing::info!(harness.member = %member, tool.name = %tool, reason = %reason, "acp permission needs a decision no one can give live; refused");
                PermissionDecision::Deny
            }
            Err(error) => {
                tracing::warn!(harness.member = %member, tool.name = %tool, error = %error, "acp permission refused");
                PermissionDecision::Deny
            }
        }
    }

    #[async_trait]
    impl Agent for AcpMember {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            ctx.run_config().check_governance()?;
            let prompt: String = ctx
                .user_content()
                .parts
                .iter()
                .filter_map(|part| part.text())
                .collect::<Vec<_>>()
                .join("\n");
            let member = self.name.clone();
            let bridge_ctx = ctx.clone();
            let policy = PermissionPolicy::async_custom(move |request| {
                decide(bridge_ctx.clone(), member.clone(), request)
            });
            let config = self.config.clone();
            let name = self.name.clone();
            let invocation = ctx.invocation_id().to_string();
            Ok(Box::pin(async_stream::stream! {
                match adk_acp::prompt_agent_with_policy(&config, &prompt, Arc::new(policy)).await {
                    Ok(text) => {
                        let mut event = Event::new(&invocation);
                        event.author = name.clone();
                        event.llm_response.content = Some(Content::new("model").with_text(text));
                        yield Ok(event);
                    }
                    Err(error) => {
                        yield Err(AdkError::agent(format!("ACP member '{name}' failed: {error}")));
                    }
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_explore_and_general_unless_replaced() {
        let plan = TeamPlan::prepare("harness", Vec::new(), true, None, None, &HashSet::new())
            .unwrap()
            .unwrap();
        assert_eq!(plan.names(), vec!["explore".to_string(), "general".to_string()]);
        let mut custom = MemberSpec::explore();
        custom.description = "mine".into();
        let plan = TeamPlan::prepare("harness", vec![custom], true, None, None, &HashSet::new())
            .unwrap()
            .unwrap();
        assert_eq!(plan.names(), vec!["general".to_string(), "explore".to_string()]);
        assert!(
            TeamPlan::prepare("harness", Vec::new(), false, None, None, &HashSet::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn member_names_cannot_collide() {
        let reserved: HashSet<String> = ["bash".to_string()].into_iter().collect();
        let mut named = MemberSpec::explore();
        named.name = "bash".into();
        assert!(TeamPlan::prepare("harness", vec![named], false, None, None, &reserved).is_err());
        let mut harness = MemberSpec::explore();
        harness.name = "harness".into();
        assert!(TeamPlan::prepare("harness", vec![harness], false, None, None, &reserved).is_err());
        assert!(
            TeamPlan::prepare(
                "harness",
                vec![MemberSpec::explore(), MemberSpec::explore()],
                false,
                None,
                None,
                &reserved
            )
            .is_err()
        );
        let mut untimed = MemberSpec::general();
        untimed.timeout = Duration::ZERO;
        assert!(TeamPlan::prepare("harness", vec![untimed], false, None, None, &reserved).is_err());
    }
}
