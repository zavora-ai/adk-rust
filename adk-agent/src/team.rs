//! Portable, validated multi-agent team composition.
//!
//! A [`TeamSpec`] is data: it names members, identifies the coordinator, and
//! declares exact directed relationships. Compilation binds those names to any
//! existing [`Agent`] implementations and produces a [`CompiledTeam`] root that
//! can be passed directly to a Runner.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[cfg(feature = "team-tools")]
use std::sync::{Mutex, OnceLock};

use adk_core::{Agent, EventStream, InvocationContext, Result, RunConfig};
use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::Instrument;

mod blackboard;
mod discovery;
mod evaluation;
mod lifecycle;
mod runtime;
mod templates;
pub use blackboard::{
    BlackboardHistoryPolicy, BlackboardPolicy, BlackboardSchedule, BlackboardSpec,
    BlackboardTransition, CompiledBlackboardTeam,
};
pub use discovery::{
    StaticTeamAgentRegistry, TeamAgentDescriptor, TeamAgentHealth, TeamAgentRegistry,
};
pub use evaluation::{
    TeamExecutionAnalysis, TeamReplayError, analyze_team_execution, validate_team_replay,
};
pub use lifecycle::{
    TeamLifecycleContext, TeamLifecycleDecision, TeamLifecycleHook, TeamLifecycleOutcome,
    TeamLifecyclePhase,
};
use runtime::{EventDisposition, TeamEdgeStart, TeamRuntimeRegistry};
pub use runtime::{
    ResolvedTeamMember, TEAM_EDGE_ID_KEY, TEAM_EXECUTION_STATE_KEY, TEAM_ROOT_INVOCATION_KEY,
    TeamEdgeExecution, TeamExecutionSnapshot, TeamExecutionStatus, TeamExecutionUsage,
};
pub use templates::{TeamArchitectureTemplate, TeamManagerBranch, WorkflowArchitectureTemplate};

#[cfg(feature = "team-tools")]
use adk_core::{RuntimeToolset, Tool, Toolset};
#[cfg(feature = "team-tools")]
use adk_tool::{
    AgentTool, AgentToolFailureMode, AgentToolSessionSnapshot, AgentToolStateMergePolicy,
};

/// A portable team definition independent of concrete model or runtime types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamSpec {
    /// Stable name of the executable team root.
    pub name: String,
    /// Human-readable purpose of the team.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Member that handles the initial request.
    pub coordinator: String,
    /// Named members expected at compilation time.
    pub members: Vec<TeamMemberSpec>,
    /// Exact directed handoff and delegation relationships.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<TeamRelationship>,
    /// Runtime policies enforced by the compiled team.
    #[serde(default)]
    pub policy: TeamPolicy,
}

/// Serializable metadata for one team member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamMemberSpec {
    /// Name used to bind the member to an [`Agent`].
    pub name: String,
    /// Optional portable description for design tools and documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Capabilities a discovered binding must provide.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<String>,
    /// Registry trust, version, and integrity constraints for this member.
    #[serde(default)]
    pub registry: TeamRegistryRequirement,
}

impl TeamMemberSpec {
    /// Creates member metadata for `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            required_capabilities: Vec::new(),
            registry: TeamRegistryRequirement::default(),
        }
    }

    /// Adds a portable description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Requires all named capabilities when resolving this member dynamically.
    pub fn with_capabilities(
        mut self,
        capabilities: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.required_capabilities = capabilities.into_iter().map(Into::into).collect();
        self
    }

    /// Applies discovery-time trust and integrity constraints.
    pub fn with_registry_requirement(mut self, requirement: TeamRegistryRequirement) -> Self {
        self.registry = requirement;
        self
    }
}

/// Portable governance constraints for dynamically discovered bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamRegistryRequirement {
    /// Exact semantic or provider version required from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Exact immutable content/configuration digest required from the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// At least one of these trust labels must be advertised.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_labels: Vec<String>,
    /// Reject degraded candidates as well as unavailable ones.
    #[serde(default)]
    pub require_healthy: bool,
}

/// One exact directed relationship between two team members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamRelationship {
    /// Calling or transferring member.
    pub from: String,
    /// Target member.
    pub to: String,
    /// Control-flow semantics of this edge.
    pub kind: RelationshipKind,
    /// Input, output, safety, and failure contract for this exact edge.
    #[serde(default)]
    pub policy: RelationshipPolicy,
}

impl TeamRelationship {
    /// Creates a directed relationship.
    pub fn new(from: impl Into<String>, to: impl Into<String>, kind: RelationshipKind) -> Self {
        Self { from: from.into(), to: to.into(), kind, policy: RelationshipPolicy::default() }
    }

    /// Applies a contract to this relationship.
    pub fn with_policy(mut self, policy: RelationshipPolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// Contract enforced for one exact relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipPolicy {
    /// Optional JSON Schema for arguments supplied to the relationship tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Optional JSON Schema validated against a delegate's returned value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Override the team-wide context policy for this edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<TeamContextPolicy>,
    /// History projection applied to delegated child sessions.
    #[serde(default)]
    pub history: TeamHistoryPolicy,
    /// Exact state keys copied to delegated child sessions. Empty means all
    /// keys allowed by the selected context policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_keys: Vec<String>,
    /// Exact state keys a delegated member may write back. Empty allows all keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_write_keys: Vec<String>,
    /// Merge behavior for delegated state writes.
    #[serde(default)]
    pub state_merge: TeamStateMergePolicy,
    /// Allowed artifact-name prefixes written by a delegate. Empty allows all names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_prefixes: Vec<String>,
    /// Durable recovery contract for an interrupted delegation.
    #[serde(default)]
    pub resume: TeamResumePolicy,
    /// Per-attempt timeout in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Whether invoking this edge requires a tool confirmation decision.
    #[serde(default)]
    pub approval: RelationshipApprovalPolicy,
    /// Failure handling for this edge.
    #[serde(default)]
    pub failure: RelationshipFailureStrategy,
    /// Optional circuit breaker for repeatedly failing delegate calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub circuit_breaker: Option<CircuitBreakerPolicy>,
}

/// Conversation history exposed to a delegated member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum TeamHistoryPolicy {
    /// Derive history behavior from the selected context policy.
    #[default]
    Inherit,
    /// Do not copy conversation history.
    None,
    /// Copy the full available history.
    Full,
    /// Copy at most the most recent `max_events` messages.
    Last {
        /// Maximum number of messages copied.
        max_events: usize,
    },
}

/// Approval requirement for a relationship invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub enum RelationshipApprovalPolicy {
    /// Invoke without a relationship-specific approval gate.
    #[default]
    Never,
    /// Reuse the runtime's exact-call tool confirmation mechanism.
    Required,
}

/// Failure handling for an exact relationship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", tag = "strategy")]
pub enum RelationshipFailureStrategy {
    /// Use the team-wide failure policy.
    #[default]
    Inherit,
    /// Propagate the member failure.
    Propagate,
    /// Return a structured error to the delegating member.
    ReturnError,
    /// Retry the same target with bounded attempts and backoff.
    Retry {
        /// Total attempts, including the initial attempt.
        max_attempts: u32,
        /// Delay between attempts in milliseconds.
        #[serde(default)]
        backoff_ms: u64,
    },
    /// Transfer the failed operation to another explicitly declared target.
    Fallback {
        /// Exact fallback member name.
        target: String,
    },
    /// Retry first, then use an explicitly declared fallback target.
    RetryThenFallback {
        /// Total attempts against the primary target.
        max_attempts: u32,
        /// Delay between attempts in milliseconds.
        #[serde(default)]
        backoff_ms: u64,
        /// Exact fallback member name.
        target: String,
    },
}

/// Opens an edge circuit after repeated failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CircuitBreakerPolicy {
    /// Consecutive failures required to open the circuit.
    pub failure_threshold: u32,
    /// Cooldown before a half-open probe, in milliseconds.
    pub reset_after_ms: u64,
}

/// Control-flow semantics for a team relationship.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum RelationshipKind {
    /// Invoke the target as a tool, receive its result, and resume the caller.
    Delegate,
    /// Transfer active control to the target; the caller does not resume.
    Handoff,
}

/// Policies applied uniformly by a compiled team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamPolicy {
    /// Maximum handoffs in one Runner invocation.
    pub max_transfer_depth: u32,
    /// Maximum nested agent-as-tool calls.
    pub max_delegation_depth: u32,
    /// Maximum simultaneous delegate calls from a member.
    pub max_concurrent_delegations: usize,
    /// Context exposed to delegated members.
    pub context: TeamContextPolicy,
    /// How member failures surface to callers.
    pub failure: TeamFailurePolicy,
    /// Aggregate resource limits for one root invocation.
    #[serde(default)]
    pub budget: TeamBudget,
    /// Conditions that terminate a running team cleanly.
    #[serde(default)]
    pub termination: TeamTerminationPolicy,
}

impl Default for TeamPolicy {
    fn default() -> Self {
        Self {
            max_transfer_depth: 8,
            max_delegation_depth: 4,
            max_concurrent_delegations: 4,
            context: TeamContextPolicy::Shared,
            failure: TeamFailurePolicy::Propagate,
            budget: TeamBudget::default(),
            termination: TeamTerminationPolicy::default(),
        }
    }
}

/// Aggregate limits enforced across a root team invocation and its delegates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamBudget {
    /// Maximum emitted events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_events: Option<u64>,
    /// Maximum model responses carrying usage metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_requests: Option<u64>,
    /// Maximum tool calls observed in model events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    /// Maximum total input and output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Maximum estimated cost in millionths of a US dollar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_microusd: Option<u64>,
    /// Maximum delegation relationship executions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegations: Option<u64>,
    /// Maximum handoff relationship executions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_handoffs: Option<u64>,
    /// Maximum wall-clock duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_time_ms: Option<u64>,
}

/// Clean termination conditions independent from hard resource budgets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamTerminationPolicy {
    /// End when an event requests human escalation.
    pub stop_on_escalation: bool,
    /// End after a final response authored by one of these members.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub final_authors: Vec<String>,
    /// End when emitted text contains one of these exact markers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_markers: Vec<String>,
}

impl Default for TeamTerminationPolicy {
    fn default() -> Self {
        Self { stop_on_escalation: true, final_authors: Vec::new(), text_markers: Vec::new() }
    }
}

/// Context sharing policy for agent-as-tool delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TeamContextPolicy {
    /// Start delegates with an empty history/state snapshot and no memory.
    Isolated,
    /// Snapshot parent history/state and forward memory, artifacts, and shared state.
    Shared,
}

/// Merge policy for state returned by an exact delegation edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub enum TeamStateMergePolicy {
    /// Reject writes whose parent value changed after delegation began.
    #[default]
    RejectConflicts,
    /// Apply child writes with last-writer-wins semantics.
    Overwrite,
}

/// Recovery contract for a delegation that was running when execution stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", tag = "strategy")]
pub enum TeamResumePolicy {
    /// Never replay an unresolved delegation automatically.
    #[default]
    FailClosed,
    /// Require the target to advertise checkpoint resume and let the durable
    /// host provide a continuation token stored under this state key.
    RequireCheckpoint {
        /// Session-state key holding the opaque target continuation token.
        token_state_key: String,
    },
}

/// Safe action a durable host can take for a restored execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum TeamResumePlan {
    /// No relationship was active; start or continue with the coordinator.
    Coordinator,
    /// Resume control at an active handoff target.
    Handoff {
        /// Active relationship execution.
        edge_id: String,
        /// Member that owns control.
        target: String,
    },
    /// Resume an unresolved delegate through its checkpoint-aware host adapter.
    DelegateCheckpoint {
        /// Active relationship execution.
        edge_id: String,
        /// Delegated member.
        target: String,
        /// State key containing the opaque continuation token.
        token_state_key: String,
    },
    /// The active delegation cannot be replayed safely.
    UnsafeDelegate {
        /// Active relationship execution.
        edge_id: String,
        /// Delegated member.
        target: String,
        /// Explanation suitable for an operator or durable scheduler.
        reason: String,
    },
}

/// Failure behavior for team relationships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TeamFailurePolicy {
    /// Surface member and timeout failures as failed tool or agent executions.
    Propagate,
    /// Return delegate failures as structured tool results; handoff failures still propagate.
    ReturnDelegateError,
}

/// Validation or compilation failure for a [`TeamSpec`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TeamError {
    /// A required name is empty.
    #[error("team {field} must not be empty")]
    EmptyName {
        /// Name-bearing field that failed validation.
        field: &'static str,
    },
    /// A member name occurs more than once.
    #[error("duplicate team member '{0}'")]
    DuplicateMember(String),
    /// The team root name collides with a member name.
    #[error("team name '{0}' must not also be a member name")]
    TeamNameCollision(String),
    /// The coordinator is not declared as a member.
    #[error("team coordinator '{0}' is not a declared member")]
    UnknownCoordinator(String),
    /// A relationship references a missing member.
    #[error("relationship {endpoint} references unknown member '{name}'")]
    UnknownRelationshipMember {
        /// Whether the source or target was invalid.
        endpoint: &'static str,
        /// Invalid member name.
        name: String,
    },
    /// A member points to itself.
    #[error("self relationship is not allowed for member '{0}'")]
    SelfRelationship(String),
    /// The same semantic edge occurs more than once.
    #[error("duplicate {kind:?} relationship from '{from}' to '{to}'")]
    DuplicateRelationship {
        /// Source member.
        from: String,
        /// Target member.
        to: String,
        /// Repeated relationship kind.
        kind: RelationshipKind,
    },
    /// The directed topology contains a cycle.
    #[error("team topology contains a cycle involving '{0}'")]
    CyclicTopology(String),
    /// A member cannot be reached from the coordinator.
    #[error("team member '{0}' is unreachable from the coordinator")]
    UnreachableMember(String),
    /// A depth or concurrency policy is zero.
    #[error("team policy '{0}' must be greater than zero")]
    InvalidPolicy(&'static str),
    /// A member capability requirement is empty or repeated.
    #[error("invalid capability requirement for member '{member}': {reason}")]
    InvalidMemberCapability {
        /// Member carrying the invalid requirement.
        member: String,
        /// Actionable validation failure.
        reason: String,
    },
    /// An exact relationship contract is invalid.
    #[error("invalid relationship policy from '{from}' to '{to}': {reason}")]
    InvalidRelationshipPolicy {
        /// Relationship source.
        from: String,
        /// Relationship target.
        to: String,
        /// Actionable validation failure.
        reason: String,
    },
    /// A team budget contains a zero bound.
    #[error("team budget '{0}' must be greater than zero when configured")]
    InvalidBudget(&'static str),
    /// A termination rule names an undeclared member.
    #[error("team termination final author '{0}' is not a declared member")]
    UnknownTerminationAuthor(String),
    /// No concrete agent was registered for a member.
    #[error("no agent registered for team member '{0}'")]
    MissingAgent(String),
    /// More than one concrete agent has the same name.
    #[error("duplicate agent binding for team member '{0}'")]
    DuplicateAgentBinding(String),
    /// A concrete agent was supplied but is not declared by the specification.
    #[error("agent binding '{0}' is not declared by the team")]
    UnexpectedAgentBinding(String),
    /// A bound source agent cannot enforce a declared relationship.
    #[error(
        "agent '{member}' does not support required runtime capability '{capability}' for {relationship}"
    )]
    UnsupportedAgentCapability {
        /// Portable member name.
        member: String,
        /// Missing execution-plane capability.
        capability: &'static str,
        /// Relationship that requires the capability.
        relationship: String,
    },
    /// Delegation support was not compiled into `adk-agent`.
    #[error("TeamSpec contains Delegate relationships; enable the adk-agent 'team-tools' feature")]
    DelegationFeatureDisabled,
    /// A dynamic registry operation failed.
    #[error("team registry failed: {0}")]
    Registry(String),
    /// No registry candidate satisfies a portable member requirement.
    #[error(
        "no registry candidate for team member '{member}' with required capabilities {capabilities:?}"
    )]
    NoRegistryCandidate {
        /// Portable member name.
        member: String,
        /// Required capability identifiers.
        capabilities: Vec<String>,
    },
    /// A persisted execution receipt cannot be restored into this compiled team.
    #[error("incompatible team execution snapshot: {0}")]
    IncompatibleExecutionSnapshot(String),
}

/// Structured failure produced while a compiled team is executing.
#[derive(Debug, Error)]
pub enum TeamRuntimeError {
    /// A lifecycle hook failed.
    #[error("team lifecycle hook '{hook}' failed during {phase:?}: {message}")]
    LifecycleHook {
        /// Hook name.
        hook: String,
        /// Lifecycle boundary.
        phase: TeamLifecyclePhase,
        /// Hook failure.
        message: String,
    },
    /// A lifecycle policy denied an operation.
    #[error("team policy denied {operation}: {reason}")]
    PolicyDenied {
        /// Operation being denied.
        operation: String,
        /// Auditable policy reason.
        reason: String,
    },
    /// A configured aggregate budget was exhausted.
    #[error("team budget exceeded: {0}")]
    BudgetExceeded(String),
    /// A persisted delegation cannot be replayed safely.
    #[error("unsafe team resume: {0}")]
    UnsafeResume(String),
    /// Delegated state violated its write/merge contract.
    #[error("team state policy rejected delegated output: {0}")]
    StatePolicy(String),
}

impl From<TeamRuntimeError> for adk_core::AdkError {
    fn from(error: TeamRuntimeError) -> Self {
        use adk_core::{AdkError, ErrorCategory, ErrorComponent};
        let (category, code) = match &error {
            TeamRuntimeError::LifecycleHook { .. } => {
                (ErrorCategory::Internal, "agent.team.lifecycle_hook_failed")
            }
            TeamRuntimeError::PolicyDenied { .. } => {
                (ErrorCategory::Forbidden, "agent.team.policy_denied")
            }
            TeamRuntimeError::BudgetExceeded(_) => {
                (ErrorCategory::RateLimited, "agent.team.budget_exceeded")
            }
            TeamRuntimeError::UnsafeResume(_) => {
                (ErrorCategory::Unsupported, "agent.team.resume_unsafe")
            }
            TeamRuntimeError::StatePolicy(_) => {
                (ErrorCategory::InvalidInput, "agent.team.state_policy_violation")
            }
        };
        AdkError::new(ErrorComponent::Agent, category, code, error.to_string())
    }
}

impl From<TeamError> for adk_core::AdkError {
    fn from(error: TeamError) -> Self {
        use adk_core::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails};

        let message = error.to_string();
        let (category, code) = match &error {
            TeamError::MissingAgent(_)
            | TeamError::NoRegistryCandidate { .. }
            | TeamError::UnknownRelationshipMember { .. }
            | TeamError::UnknownCoordinator(_) => {
                (ErrorCategory::NotFound, "agent.team.binding_not_found")
            }
            TeamError::UnsupportedAgentCapability { .. } | TeamError::DelegationFeatureDisabled => {
                (ErrorCategory::Unsupported, "agent.team.capability_unsupported")
            }
            TeamError::Registry(_) => (ErrorCategory::Unavailable, "agent.team.registry_failed"),
            TeamError::IncompatibleExecutionSnapshot(_) => {
                (ErrorCategory::InvalidInput, "agent.team.snapshot_incompatible")
            }
            TeamError::EmptyName { .. }
            | TeamError::DuplicateMember(_)
            | TeamError::TeamNameCollision(_)
            | TeamError::SelfRelationship(_)
            | TeamError::DuplicateRelationship { .. }
            | TeamError::CyclicTopology(_)
            | TeamError::UnreachableMember(_)
            | TeamError::InvalidPolicy(_)
            | TeamError::InvalidMemberCapability { .. }
            | TeamError::InvalidRelationshipPolicy { .. }
            | TeamError::InvalidBudget(_)
            | TeamError::UnknownTerminationAuthor(_)
            | TeamError::DuplicateAgentBinding(_)
            | TeamError::UnexpectedAgentBinding(_) => {
                (ErrorCategory::InvalidInput, "agent.team.invalid_spec")
            }
        };
        let mut details = ErrorDetails::default();
        details
            .metadata
            .insert("teamError".to_string(), serde_json::Value::String(format!("{error:?}")));
        AdkError::new(ErrorComponent::Agent, category, code, message).with_details(details)
    }
}

impl TeamSpec {
    /// Validates names, policies, edges, acyclicity, and coordinator reachability.
    pub fn validate(&self) -> std::result::Result<(), TeamError> {
        if self.name.trim().is_empty() {
            return Err(TeamError::EmptyName { field: "name" });
        }
        if self.coordinator.trim().is_empty() {
            return Err(TeamError::EmptyName { field: "coordinator" });
        }
        if self.policy.max_transfer_depth == 0 {
            return Err(TeamError::InvalidPolicy("maxTransferDepth"));
        }
        if self.policy.max_delegation_depth == 0 {
            return Err(TeamError::InvalidPolicy("maxDelegationDepth"));
        }
        if self.policy.max_concurrent_delegations == 0 {
            return Err(TeamError::InvalidPolicy("maxConcurrentDelegations"));
        }

        let mut names = BTreeSet::new();
        for member in &self.members {
            if member.name.trim().is_empty() {
                return Err(TeamError::EmptyName { field: "member.name" });
            }
            if !names.insert(member.name.as_str()) {
                return Err(TeamError::DuplicateMember(member.name.clone()));
            }
            let mut capabilities = BTreeSet::new();
            for capability in &member.required_capabilities {
                if capability.trim().is_empty() {
                    return Err(TeamError::InvalidMemberCapability {
                        member: member.name.clone(),
                        reason: "capability names must not be empty".to_string(),
                    });
                }
                if !capabilities.insert(capability.as_str()) {
                    return Err(TeamError::InvalidMemberCapability {
                        member: member.name.clone(),
                        reason: format!("duplicate capability '{capability}'"),
                    });
                }
            }
            if member.registry.version.as_ref().is_some_and(|value| value.trim().is_empty()) {
                return Err(TeamError::InvalidMemberCapability {
                    member: member.name.clone(),
                    reason: "registry version must not be empty".to_string(),
                });
            }
            if member.registry.digest.as_ref().is_some_and(|value| value.trim().is_empty()) {
                return Err(TeamError::InvalidMemberCapability {
                    member: member.name.clone(),
                    reason: "registry digest must not be empty".to_string(),
                });
            }
            let mut trust_labels = BTreeSet::new();
            for label in &member.registry.trust_labels {
                if label.trim().is_empty() || !trust_labels.insert(label.as_str()) {
                    return Err(TeamError::InvalidMemberCapability {
                        member: member.name.clone(),
                        reason: format!("invalid or duplicate registry trust label '{label}'"),
                    });
                }
            }
        }
        if !names.contains(self.coordinator.as_str()) {
            return Err(TeamError::UnknownCoordinator(self.coordinator.clone()));
        }
        if names.contains(self.name.as_str()) {
            return Err(TeamError::TeamNameCollision(self.name.clone()));
        }

        let mut edges = BTreeSet::new();
        let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for relationship in &self.relationships {
            if !names.contains(relationship.from.as_str()) {
                return Err(TeamError::UnknownRelationshipMember {
                    endpoint: "source",
                    name: relationship.from.clone(),
                });
            }
            if !names.contains(relationship.to.as_str()) {
                return Err(TeamError::UnknownRelationshipMember {
                    endpoint: "target",
                    name: relationship.to.clone(),
                });
            }
            if relationship.from == relationship.to {
                return Err(TeamError::SelfRelationship(relationship.from.clone()));
            }
            let edge = (relationship.from.as_str(), relationship.to.as_str(), relationship.kind);
            if !edges.insert(edge) {
                return Err(TeamError::DuplicateRelationship {
                    from: relationship.from.clone(),
                    to: relationship.to.clone(),
                    kind: relationship.kind,
                });
            }
            relationship.validate_policy(&names, &self.relationships)?;
            adjacency.entry(relationship.from.as_str()).or_default().push(relationship.to.as_str());
        }

        for (name, limit) in [
            ("maxEvents", self.policy.budget.max_events),
            ("maxModelRequests", self.policy.budget.max_model_requests),
            ("maxToolCalls", self.policy.budget.max_tool_calls),
            ("maxTokens", self.policy.budget.max_tokens),
            ("maxCostMicrousd", self.policy.budget.max_cost_microusd),
            ("maxDelegations", self.policy.budget.max_delegations),
            ("maxHandoffs", self.policy.budget.max_handoffs),
            ("maxWallTimeMs", self.policy.budget.max_wall_time_ms),
        ] {
            if limit == Some(0) {
                return Err(TeamError::InvalidBudget(name));
            }
        }
        for author in &self.policy.termination.final_authors {
            if !names.contains(author.as_str()) {
                return Err(TeamError::UnknownTerminationAuthor(author.clone()));
            }
        }
        if self.policy.termination.text_markers.iter().any(|marker| marker.is_empty()) {
            return Err(TeamError::InvalidPolicy("termination.textMarkers"));
        }

        fn visit<'a>(
            node: &'a str,
            adjacency: &BTreeMap<&'a str, Vec<&'a str>>,
            visiting: &mut HashSet<&'a str>,
            visited: &mut HashSet<&'a str>,
        ) -> std::result::Result<(), TeamError> {
            if visiting.contains(node) {
                return Err(TeamError::CyclicTopology(node.to_string()));
            }
            if !visited.insert(node) {
                return Ok(());
            }
            visiting.insert(node);
            for target in adjacency.get(node).into_iter().flatten() {
                visit(target, adjacency, visiting, visited)?;
            }
            visiting.remove(node);
            Ok(())
        }

        let mut visiting = HashSet::new();
        let mut reachable = HashSet::new();
        visit(&self.coordinator, &adjacency, &mut visiting, &mut reachable)?;
        if let Some(unreachable) = names.iter().find(|name| !reachable.contains(**name)) {
            return Err(TeamError::UnreachableMember((*unreachable).to_string()));
        }
        Ok(())
    }

    /// Binds this spec to concrete agents and returns an executable team root.
    ///
    /// Any [`Agent`] implementation can be a member. Delegate edges additionally
    /// require the `team-tools` feature, which lowers them to `AgentTool`s.
    pub fn compile(
        &self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
    ) -> std::result::Result<CompiledTeam, TeamError> {
        self.validate()?;
        #[cfg(not(feature = "team-tools"))]
        if self.relationships.iter().any(|edge| edge.kind == RelationshipKind::Delegate) {
            return Err(TeamError::DelegationFeatureDisabled);
        }
        self.compile_inner(agents, Vec::new())
    }

    /// Binds concrete agents and ordered async team lifecycle hooks.
    ///
    /// Generic Runner plugins continue to apply normally. These hooks add
    /// policy and observation at exact member and relationship boundaries.
    pub fn compile_with_hooks(
        &self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
        hooks: impl IntoIterator<Item = Arc<dyn TeamLifecycleHook>>,
    ) -> std::result::Result<CompiledTeam, TeamError> {
        self.validate()?;
        #[cfg(not(feature = "team-tools"))]
        if self.relationships.iter().any(|edge| edge.kind == RelationshipKind::Delegate) {
            return Err(TeamError::DelegationFeatureDisabled);
        }
        self.compile_inner(agents, hooks.into_iter().collect())
    }

    fn compile_inner(
        &self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
        hooks: Vec<Arc<dyn TeamLifecycleHook>>,
    ) -> std::result::Result<CompiledTeam, TeamError> {
        let declared: HashSet<&str> =
            self.members.iter().map(|member| member.name.as_str()).collect();
        let mut registry: HashMap<String, Arc<dyn Agent>> = HashMap::new();
        for agent in agents {
            let name = agent.name().to_string();
            if !declared.contains(name.as_str()) {
                return Err(TeamError::UnexpectedAgentBinding(name));
            }
            if registry.insert(name.clone(), agent).is_some() {
                return Err(TeamError::DuplicateAgentBinding(name));
            }
        }
        for member in &self.members {
            if !registry.contains_key(&member.name) {
                return Err(TeamError::MissingAgent(member.name.clone()));
            }
        }

        let roster: Vec<ResolvedTeamMember> = self
            .members
            .iter()
            .map(|member| ResolvedTeamMember {
                member: member.name.clone(),
                binding: member.name.clone(),
                capabilities: member.required_capabilities.clone(),
                version: None,
                digest: None,
                trust_labels: Vec::new(),
            })
            .collect();
        self.compile_registry(registry, roster, hooks)
    }

    fn compile_registry(
        &self,
        registry: HashMap<String, Arc<dyn Agent>>,
        roster: Vec<ResolvedTeamMember>,
        hooks: Vec<Arc<dyn TeamLifecycleHook>>,
    ) -> std::result::Result<CompiledTeam, TeamError> {
        for relationship in &self.relationships {
            let source = registry
                .get(&relationship.from)
                .expect("validated compilation has every member binding");
            let capabilities = source.capabilities();
            let (supported, capability) = match relationship.kind {
                RelationshipKind::Delegate => (capabilities.runtime_tools, "runtimeTools"),
                RelationshipKind::Handoff => (capabilities.handoff, "handoff"),
            };
            if !supported {
                return Err(TeamError::UnsupportedAgentCapability {
                    member: relationship.from.clone(),
                    capability,
                    relationship: format!("{:?} edge to '{}'", relationship.kind, relationship.to),
                });
            }
            if relationship.policy.approval == RelationshipApprovalPolicy::Required
                && !capabilities.relationship_confirmation
            {
                return Err(TeamError::UnsupportedAgentCapability {
                    member: relationship.from.clone(),
                    capability: "relationshipConfirmation",
                    relationship: format!(
                        "approved {:?} edge to '{}'",
                        relationship.kind, relationship.to
                    ),
                });
            }
            if matches!(relationship.policy.resume, TeamResumePolicy::RequireCheckpoint { .. }) {
                let target = registry
                    .get(&relationship.to)
                    .expect("validated compilation has every member binding");
                if !target.capabilities().checkpoint_resume {
                    return Err(TeamError::UnsupportedAgentCapability {
                        member: relationship.to.clone(),
                        capability: "checkpointResume",
                        relationship: format!(
                            "checkpoint-resumable delegation from '{}'",
                            relationship.from
                        ),
                    });
                }
            }
        }
        let runtime = Arc::new(TeamRuntimeRegistry::new(
            self.name.clone(),
            roster,
            self.policy.budget.clone(),
            self.policy.termination.clone(),
            hooks,
        ));
        #[cfg(feature = "team-tools")]
        let member_registry = Arc::new(OnceLock::new());
        let members: Vec<Arc<dyn Agent>> = self
            .members
            .iter()
            .map(|member| {
                let relationships: Vec<TeamRelationship> = self
                    .relationships
                    .iter()
                    .filter(|relationship| relationship.from == member.name)
                    .cloned()
                    .collect();
                Arc::new(TeamMemberAgent {
                    name: member.name.clone(),
                    description: member.description.clone().unwrap_or_else(|| {
                        registry
                            .get(&member.name)
                            .expect("all member bindings were checked before lowering")
                            .description()
                            .to_string()
                    }),
                    agent: registry
                        .get(&member.name)
                        .expect("all member bindings were checked before lowering")
                        .clone(),
                    relationships,
                    incoming_relationships: self
                        .relationships
                        .iter()
                        .filter(|relationship| relationship.to == member.name)
                        .cloned()
                        .collect(),
                    #[cfg(feature = "team-tools")]
                    members: member_registry.clone(),
                    #[cfg(feature = "team-tools")]
                    team_name: self.name.clone(),
                    policy: self.policy.clone(),
                    runtime: runtime.clone(),
                }) as Arc<dyn Agent>
            })
            .collect();
        #[cfg(feature = "team-tools")]
        member_registry
            .set(members.iter().map(|agent| (agent.name().to_string(), agent.clone())).collect())
            .map_err(|_| TeamError::InvalidPolicy("memberRegistry"))?;

        let coordinator_index = self
            .members
            .iter()
            .position(|member| member.name == self.coordinator)
            .expect("validated coordinator must be present");
        Ok(CompiledTeam {
            spec: self.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            coordinator_name: self.coordinator.clone(),
            coordinator: members[coordinator_index].clone(),
            members,
            handoffs: self
                .members
                .iter()
                .map(|member| {
                    let targets = self
                        .relationships
                        .iter()
                        .filter(|edge| {
                            edge.from == member.name && edge.kind == RelationshipKind::Handoff
                        })
                        .map(|edge| edge.to.clone())
                        .collect();
                    (member.name.clone(), targets)
                })
                .collect(),
            policy: self.policy.clone(),
            runtime,
        })
    }
}

impl TeamRelationship {
    fn validate_policy(
        &self,
        names: &BTreeSet<&str>,
        relationships: &[TeamRelationship],
    ) -> std::result::Result<(), TeamError> {
        let invalid = |reason: String| TeamError::InvalidRelationshipPolicy {
            from: self.from.clone(),
            to: self.to.clone(),
            reason,
        };
        if self.kind == RelationshipKind::Handoff {
            let unsupported = if self.policy.input_schema.is_some() {
                Some("inputSchema")
            } else if self.policy.output_schema.is_some() {
                Some("outputSchema")
            } else if self.policy.context.is_some() {
                Some("context")
            } else if self.policy.history != TeamHistoryPolicy::Inherit {
                Some("history")
            } else if !self.policy.state_keys.is_empty() {
                Some("stateKeys")
            } else if !self.policy.state_write_keys.is_empty() {
                Some("stateWriteKeys")
            } else if self.policy.state_merge != TeamStateMergePolicy::RejectConflicts {
                Some("stateMerge")
            } else if !self.policy.artifact_prefixes.is_empty() {
                Some("artifactPrefixes")
            } else if self.policy.resume != TeamResumePolicy::FailClosed {
                Some("resume")
            } else if self.policy.timeout_ms.is_some() {
                Some("timeoutMs")
            } else if self.policy.approval != RelationshipApprovalPolicy::Never {
                Some("approval")
            } else if self.policy.circuit_breaker.is_some() {
                Some("circuitBreaker")
            } else {
                None
            };
            if let Some(field) = unsupported {
                return Err(invalid(format!(
                    "{field} applies only to Delegate relationships; use team-wide handoff budgets or failure policy"
                )));
            }
        }
        for (label, schema) in [
            ("inputSchema", self.policy.input_schema.as_ref()),
            ("outputSchema", self.policy.output_schema.as_ref()),
        ] {
            if let Some(schema) = schema {
                jsonschema::validator_for(schema).map_err(|error| {
                    invalid(format!("{label} is not valid JSON Schema: {error}"))
                })?;
            }
        }
        if self.policy.timeout_ms == Some(0) {
            return Err(invalid("timeoutMs must be greater than zero".to_string()));
        }
        if let TeamHistoryPolicy::Last { max_events: 0 } = self.policy.history {
            return Err(invalid("history.maxEvents must be greater than zero".to_string()));
        }
        let mut state_keys = BTreeSet::new();
        for key in &self.policy.state_keys {
            adk_core::validate_state_key(key)
                .map_err(|reason| invalid(format!("invalid state key '{key}': {reason}")))?;
            if !state_keys.insert(key.as_str()) {
                return Err(invalid(format!("duplicate state key '{key}'")));
            }
        }
        let mut state_write_keys = BTreeSet::new();
        for key in &self.policy.state_write_keys {
            adk_core::validate_state_key(key)
                .map_err(|reason| invalid(format!("invalid state write key '{key}': {reason}")))?;
            if !state_write_keys.insert(key.as_str()) {
                return Err(invalid(format!("duplicate state write key '{key}'")));
            }
        }
        let mut artifact_prefixes = BTreeSet::new();
        for prefix in &self.policy.artifact_prefixes {
            if prefix.is_empty() {
                return Err(invalid("artifact prefixes must not be empty".to_string()));
            }
            if !artifact_prefixes.insert(prefix.as_str()) {
                return Err(invalid(format!("duplicate artifact prefix '{prefix}'")));
            }
        }
        if let TeamResumePolicy::RequireCheckpoint { token_state_key } = &self.policy.resume {
            adk_core::validate_state_key(token_state_key).map_err(|reason| {
                invalid(format!("invalid resume token state key '{token_state_key}': {reason}"))
            })?;
        }
        match &self.policy.failure {
            RelationshipFailureStrategy::Retry { max_attempts: 0, .. }
            | RelationshipFailureStrategy::RetryThenFallback { max_attempts: 0, .. } => {
                return Err(invalid("maxAttempts must be greater than zero".to_string()));
            }
            RelationshipFailureStrategy::Fallback { target }
            | RelationshipFailureStrategy::RetryThenFallback { target, .. } => {
                if !names.contains(target.as_str()) {
                    return Err(invalid(format!("fallback target '{target}' is not a member")));
                }
                if target == &self.to {
                    return Err(invalid(
                        "fallback target must differ from the primary target".to_string(),
                    ));
                }
                let declared = relationships.iter().any(|candidate| {
                    candidate.from == self.from
                        && candidate.to == *target
                        && candidate.kind == self.kind
                });
                if !declared {
                    return Err(invalid(format!(
                        "fallback target '{target}' must be declared as an exact {:?} edge from '{}'",
                        self.kind, self.from
                    )));
                }
            }
            RelationshipFailureStrategy::Inherit
            | RelationshipFailureStrategy::Propagate
            | RelationshipFailureStrategy::ReturnError
            | RelationshipFailureStrategy::Retry { .. } => {}
        }
        if let Some(circuit) = self.policy.circuit_breaker
            && (circuit.failure_threshold == 0 || circuit.reset_after_ms == 0)
        {
            return Err(invalid(
                "circuitBreaker failureThreshold and resetAfterMs must be greater than zero"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

/// Executable root produced by [`TeamSpec::compile`].
pub struct CompiledTeam {
    spec: TeamSpec,
    name: String,
    description: String,
    coordinator_name: String,
    coordinator: Arc<dyn Agent>,
    members: Vec<Arc<dyn Agent>>,
    handoffs: HashMap<String, Vec<String>>,
    policy: TeamPolicy,
    runtime: Arc<TeamRuntimeRegistry>,
}

impl std::fmt::Debug for CompiledTeam {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompiledTeam")
            .field("name", &self.name)
            .field("coordinator", &self.coordinator_name)
            .field("member_count", &self.members.len())
            .finish()
    }
}

impl CompiledTeam {
    /// Returns the portable source specification used to compile this root.
    pub fn spec(&self) -> &TeamSpec {
        &self.spec
    }

    /// Returns the validated portable source specification's coordinator name.
    pub fn coordinator_name(&self) -> &str {
        &self.coordinator_name
    }

    /// Returns the exact handoff targets for a member.
    pub fn handoff_targets(&self, member: &str) -> Option<&[String]> {
        self.handoffs.get(member).map(Vec::as_slice)
    }

    /// Returns the latest serializable execution receipt for a root invocation.
    pub fn execution_snapshot(&self, invocation_id: &str) -> Option<TeamExecutionSnapshot> {
        self.runtime.snapshot(invocation_id)
    }

    /// Returns all execution receipts currently retained by this compiled team.
    pub fn execution_snapshots(&self) -> Vec<TeamExecutionSnapshot> {
        self.runtime.snapshots()
    }

    /// Restores a persisted execution receipt before resuming with the same
    /// root invocation identifier.
    ///
    /// The team name and frozen roster must exactly match this compiled team,
    /// preventing a resumed execution from silently selecting different agents.
    pub fn restore_execution_snapshot(
        &self,
        snapshot: TeamExecutionSnapshot,
    ) -> std::result::Result<(), TeamError> {
        self.runtime.restore(snapshot)
    }

    /// Returns the safe durable-resume action for a retained invocation.
    ///
    /// This method never replays delegation implicitly. A checkpoint plan must
    /// be handed to the target's durable host adapter with the named opaque
    /// token; a fail-closed plan requires operator resolution.
    pub fn resume_plan(&self, invocation_id: &str) -> Option<TeamResumePlan> {
        let snapshot = self.runtime.snapshot(invocation_id)?;
        let Some(active) =
            snapshot.edges.iter().rev().find(|edge| edge.status == TeamExecutionStatus::Running)
        else {
            return Some(TeamResumePlan::Coordinator);
        };
        match active.kind {
            RelationshipKind::Handoff => Some(TeamResumePlan::Handoff {
                edge_id: active.id.clone(),
                target: active.to.clone(),
            }),
            RelationshipKind::Delegate => {
                let policy = self.spec.relationships.iter().find(|relationship| {
                    relationship.kind == RelationshipKind::Delegate
                        && relationship.from == active.from
                        && relationship.to == active.to
                });
                match policy.map(|relationship| &relationship.policy.resume) {
                    Some(TeamResumePolicy::RequireCheckpoint { token_state_key }) => {
                        Some(TeamResumePlan::DelegateCheckpoint {
                            edge_id: active.id.clone(),
                            target: active.to.clone(),
                            token_state_key: token_state_key.clone(),
                        })
                    }
                    Some(TeamResumePolicy::FailClosed) | None => {
                        Some(TeamResumePlan::UnsafeDelegate {
                            edge_id: active.id.clone(),
                            target: active.to.clone(),
                            reason: "the edge is fail-closed and has no checkpoint resume contract"
                                .to_string(),
                        })
                    }
                }
            }
        }
    }

    /// Validates and summarizes one retained execution receipt without model calls.
    pub fn analyze_execution(
        &self,
        invocation_id: &str,
    ) -> std::result::Result<Option<TeamExecutionAnalysis>, TeamReplayError> {
        let Some(snapshot) = self.runtime.snapshot(invocation_id) else {
            return Ok(None);
        };
        validate_team_replay(&self.spec, &snapshot)?;
        Ok(Some(analyze_team_execution(&self.spec, &snapshot)))
    }
}

#[async_trait]
impl Agent for CompiledTeam {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.members
    }

    fn capabilities(&self) -> adk_core::AgentCapabilities {
        self.coordinator.capabilities()
    }

    fn topology(&self) -> Option<adk_core::AgentTopology> {
        Some(adk_core::AgentTopology {
            root: self.name.clone(),
            coordinator: self.coordinator_name.clone(),
            members: self
                .members
                .iter()
                .map(|member| adk_core::AgentTopologyMember {
                    name: member.name().to_string(),
                    description: member.description().to_string(),
                    coordinator: member.name() == self.coordinator_name,
                    capabilities: member.capabilities(),
                })
                .collect(),
            relationships: self
                .spec
                .relationships
                .iter()
                .map(|relationship| adk_core::AgentTopologyRelationship {
                    from: relationship.from.clone(),
                    to: relationship.to.clone(),
                    kind: match relationship.kind {
                        RelationshipKind::Delegate => adk_core::AgentRelationshipKind::Delegate,
                        RelationshipKind::Handoff => adk_core::AgentRelationshipKind::Handoff,
                    },
                })
                .collect(),
        })
    }

    fn configure_run(&self, agent_name: &str, config: &mut RunConfig) {
        let member_name =
            if agent_name == self.name { self.coordinator_name.as_str() } else { agent_name };
        config.max_transfer_depth =
            Some(config.max_transfer_depth.map_or(self.policy.max_transfer_depth, |current| {
                current.min(self.policy.max_transfer_depth)
            }));
        if let Some(member) = self.members.iter().find(|member| member.name() == member_name) {
            member.configure_run(member_name, config);
        }
    }

    fn transfer_targets_for(&self, agent_name: &str) -> Option<Vec<String>> {
        let member_name =
            if agent_name == self.name { self.coordinator_name.as_str() } else { agent_name };
        Some(self.handoffs.get(member_name).cloned().unwrap_or_default())
    }

    fn strict_transfer_policy(&self) -> bool {
        true
    }

    async fn govern_transfer(
        &self,
        request: &adk_core::AgentTransferRequest,
    ) -> Result<adk_core::AgentTransferDecision> {
        let declared = self.spec.relationships.iter().any(|relationship| {
            relationship.kind == RelationshipKind::Handoff
                && relationship.from == request.from
                && relationship.to == request.to
        });
        if !declared {
            return Ok(adk_core::AgentTransferDecision::Deny {
                reason: "the exact handoff edge is not declared by TeamSpec".to_string(),
            });
        }
        if request.depth > self.policy.max_transfer_depth {
            return Ok(adk_core::AgentTransferDecision::Deny {
                reason: format!(
                    "handoff depth {} exceeds team maximum {}",
                    request.depth, self.policy.max_transfer_depth
                ),
            });
        }
        Ok(adk_core::AgentTransferDecision::Allow)
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let root_invocation_id = ctx.orchestration_root_invocation_id().to_string();
        let lifecycle = TeamLifecycleContext {
            team: self.name.clone(),
            invocation_id: root_invocation_id.clone(),
            phase: TeamLifecyclePhase::Team,
            member: None,
            edge_id: None,
            from: None,
            to: None,
            kind: None,
            attempt: None,
        };
        let team_span = adk_telemetry::team_run_span_with_context(
            &self.name,
            &root_invocation_id,
            ctx.session_id(),
            &self.coordinator_name,
        );
        if let TeamLifecycleDecision::Terminate { reason } =
            self.runtime.before_lifecycle(&lifecycle).instrument(team_span.clone()).await?
        {
            self.runtime
                .after_lifecycle(
                    &lifecycle,
                    &TeamLifecycleOutcome::Terminated { reason: reason.clone() },
                )
                .await?;
            return Err(TeamRuntimeError::PolicyDenied {
                operation: "team start".to_string(),
                reason,
            }
            .into());
        }
        if let Some(target) = self.runtime.resume_handoff_target(&root_invocation_id)? {
            let member = self
                .members
                .iter()
                .find(|member| member.name() == target)
                .cloned()
                .ok_or_else(|| {
                    adk_core::AdkError::new(
                        adk_core::ErrorComponent::Agent,
                        adk_core::ErrorCategory::NotFound,
                        "agent.team.resume_target_missing",
                        format!(
                            "restored handoff target '{target}' is not in the frozen team roster"
                        ),
                    )
                })?;
            let inner = member.run(ctx).await;
            return wrap_lifecycle_stream(inner, self.runtime.clone(), lifecycle, team_span).await;
        }
        let inner = self.coordinator.run(ctx).await;
        wrap_lifecycle_stream(inner, self.runtime.clone(), lifecycle, team_span).await
    }
}

async fn wrap_lifecycle_stream(
    inner: Result<EventStream>,
    runtime: Arc<TeamRuntimeRegistry>,
    lifecycle: TeamLifecycleContext,
    span: tracing::Span,
) -> Result<EventStream> {
    let mut inner = match inner {
        Ok(stream) => stream,
        Err(error) => {
            let outcome = TeamLifecycleOutcome::Failed {
                code: Some(error.code.to_string()),
                message: error.to_string(),
            };
            runtime.after_lifecycle(&lifecycle, &outcome).await?;
            span.record("team.status", "failed");
            return Err(error);
        }
    };
    Ok(Box::pin(async_stream::stream! {
        while let Some(result) = inner.next().await {
            match result {
                Ok(event) => {
                    span.in_scope(|| tracing::trace!("team execution event"));
                    yield Ok(event);
                }
                Err(error) => {
                    let outcome = TeamLifecycleOutcome::Failed {
                        code: Some(error.code.to_string()),
                        message: error.to_string(),
                    };
                    if let Err(hook_error) = runtime.after_lifecycle(&lifecycle, &outcome).await {
                        span.record("team.status", "failed");
                        yield Err(hook_error);
                    } else {
                        span.record("team.status", "failed");
                        yield Err(error);
                    }
                    return;
                }
            }
        }
        if let Err(error) = runtime
            .after_lifecycle(&lifecycle, &TeamLifecycleOutcome::Succeeded)
            .await
        {
            span.record("team.status", "failed");
            yield Err(error);
        } else {
            span.record("team.status", "completed");
        }
    }))
}

struct TeamMemberAgent {
    name: String,
    description: String,
    agent: Arc<dyn Agent>,
    relationships: Vec<TeamRelationship>,
    incoming_relationships: Vec<TeamRelationship>,
    #[cfg(feature = "team-tools")]
    members: Arc<OnceLock<HashMap<String, Arc<dyn Agent>>>>,
    #[cfg(feature = "team-tools")]
    team_name: String,
    policy: TeamPolicy,
    runtime: Arc<TeamRuntimeRegistry>,
}

#[async_trait]
impl Agent for TeamMemberAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    fn supports_agent_transfer(&self) -> bool {
        self.agent.supports_agent_transfer()
    }

    fn capabilities(&self) -> adk_core::AgentCapabilities {
        self.agent.capabilities()
    }

    fn configure_run(&self, _agent_name: &str, config: &mut RunConfig) {
        let handoff_targets: Vec<String> = self
            .relationships
            .iter()
            .filter(|relationship| relationship.kind == RelationshipKind::Handoff)
            .map(|relationship| relationship.to.clone())
            .collect();
        config.transfer_targets = handoff_targets;
        config.parent_agent = None;
        config.max_transfer_depth =
            Some(config.max_transfer_depth.map_or(self.policy.max_transfer_depth, |current| {
                current.min(self.policy.max_transfer_depth)
            }));
        #[cfg(feature = "team-tools")]
        {
            config
                .runtime_toolsets
                .retain(|runtime| !runtime.toolset().name().starts_with("__adk_team_"));
            let delegates: Vec<&TeamRelationship> = self
                .relationships
                .iter()
                .filter(|relationship| relationship.kind == RelationshipKind::Delegate)
                .collect();
            if !delegates.is_empty() {
                let members = self
                    .members
                    .get()
                    .expect("compiled team initializes member registry before execution");
                let limiter =
                    Arc::new(tokio::sync::Semaphore::new(self.policy.max_concurrent_delegations));
                let build_tool = |relationship: &TeamRelationship| -> Arc<dyn Tool> {
                    let target = members
                        .get(&relationship.to)
                        .expect("validated relationship target has a compiled binding")
                        .clone();
                    let context = relationship.policy.context.unwrap_or(self.policy.context);
                    let history = relationship.policy.history;
                    let snapshot = match (context, history) {
                        (TeamContextPolicy::Shared, _)
                        | (
                            TeamContextPolicy::Isolated,
                            TeamHistoryPolicy::Full | TeamHistoryPolicy::Last { .. },
                        ) => AgentToolSessionSnapshot::Parent,
                        (
                            TeamContextPolicy::Isolated,
                            TeamHistoryPolicy::Inherit | TeamHistoryPolicy::None,
                        ) => AgentToolSessionSnapshot::Isolated,
                    };
                    let mut tool = AgentTool::new(target)
                        .session_snapshot(snapshot)
                        .forward_artifacts(context == TeamContextPolicy::Shared)
                        .forward_shared_state(context == TeamContextPolicy::Shared)
                        .forward_events(true)
                        .forward_memory(context == TeamContextPolicy::Shared)
                        .failure_mode(AgentToolFailureMode::Propagate)
                        .max_delegation_depth(self.policy.max_delegation_depth)
                        .execute_child_handoffs(members.values().cloned());
                    if let Some(schema) = &relationship.policy.input_schema {
                        tool = tool.input_schema(schema.clone());
                    }
                    if let Some(schema) = &relationship.policy.output_schema {
                        tool = tool.output_schema(schema.clone());
                    }
                    if let Some(timeout_ms) = relationship.policy.timeout_ms {
                        tool = tool.timeout(std::time::Duration::from_millis(timeout_ms));
                    }
                    match history {
                        TeamHistoryPolicy::None => tool = tool.history_max_events(0),
                        TeamHistoryPolicy::Last { max_events } => {
                            tool = tool.history_max_events(max_events);
                        }
                        TeamHistoryPolicy::Inherit | TeamHistoryPolicy::Full => {}
                    }
                    if context == TeamContextPolicy::Isolated {
                        tool = tool.state_keys(Vec::<String>::new());
                    } else if !relationship.policy.state_keys.is_empty() {
                        tool = tool.state_keys(relationship.policy.state_keys.clone());
                    }
                    if !relationship.policy.state_write_keys.is_empty() {
                        let mut writable_keys = relationship.policy.state_write_keys.clone();
                        writable_keys.push(runtime::TEAM_EXECUTION_STATE_KEY.to_string());
                        tool = tool.output_state_keys(writable_keys);
                    }
                    tool = tool.state_merge_policy(match relationship.policy.state_merge {
                        TeamStateMergePolicy::RejectConflicts => {
                            AgentToolStateMergePolicy::RejectConflicts
                        }
                        TeamStateMergePolicy::Overwrite => AgentToolStateMergePolicy::Overwrite,
                    });
                    tool = tool.state_merge_exempt_keys([runtime::TEAM_EXECUTION_STATE_KEY]);
                    if !relationship.policy.artifact_prefixes.is_empty() {
                        tool =
                            tool.artifact_prefixes(relationship.policy.artifact_prefixes.clone());
                    }
                    Arc::new(tool)
                };
                let tools = delegates
                    .iter()
                    .map(|relationship| {
                        let fallback_target = match &relationship.policy.failure {
                            RelationshipFailureStrategy::Fallback { target }
                            | RelationshipFailureStrategy::RetryThenFallback { target, .. } => {
                                Some(target.as_str())
                            }
                            RelationshipFailureStrategy::Inherit
                            | RelationshipFailureStrategy::Propagate
                            | RelationshipFailureStrategy::ReturnError
                            | RelationshipFailureStrategy::Retry { .. } => None,
                        };
                        let fallback = fallback_target.and_then(|target| {
                            self.relationships
                                .iter()
                                .find(|candidate| {
                                    candidate.kind == RelationshipKind::Delegate
                                        && candidate.to == target
                                })
                                .map(&build_tool)
                        });
                        Arc::new(BoundedDelegateTool {
                            inner: build_tool(relationship),
                            fallback,
                            limiter: limiter.clone(),
                            relationship: (*relationship).clone(),
                            team_failure: self.policy.failure,
                            runtime: self.runtime.clone(),
                            circuit: Arc::new(Mutex::new(CircuitState::default())),
                        }) as Arc<dyn Tool>
                    })
                    .collect();
                config.runtime_toolsets.push(RuntimeToolset::new(Arc::new(TeamToolset {
                    name: format!("__adk_team_{}_{}", self.team_name, self.name),
                    tools,
                })));
            }
        }
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let root_invocation_id = ctx.orchestration_root_invocation_id().to_string();
        self.runtime.check_budget(&root_invocation_id)?;
        let member_lifecycle = TeamLifecycleContext {
            team: self.runtime.team_name().to_string(),
            invocation_id: root_invocation_id.clone(),
            phase: TeamLifecyclePhase::Member,
            member: Some(self.name.clone()),
            edge_id: ctx.orchestration_edge_id().map(str::to_string),
            from: None,
            to: None,
            kind: None,
            attempt: None,
        };
        let member_span = adk_telemetry::team_member_span_with_context(
            self.runtime.team_name(),
            &self.name,
            &root_invocation_id,
            ctx.session_id(),
        );
        if let TeamLifecycleDecision::Terminate { reason } =
            self.runtime.before_lifecycle(&member_lifecycle).instrument(member_span.clone()).await?
        {
            self.runtime
                .after_lifecycle(
                    &member_lifecycle,
                    &TeamLifecycleOutcome::Terminated { reason: reason.clone() },
                )
                .await?;
            return Err(TeamRuntimeError::PolicyDenied {
                operation: format!("member '{}' start", self.name),
                reason,
            }
            .into());
        }
        let mut config = ctx.run_config().clone();
        self.configure_run(self.name(), &mut config);
        let required_confirmations = self
            .relationships
            .iter()
            .filter(|relationship| {
                relationship.kind == RelationshipKind::Delegate
                    && relationship.policy.approval == RelationshipApprovalPolicy::Required
            })
            .map(|relationship| relationship.to.clone())
            .collect();
        let configured_ctx = Arc::new(TeamInvocationContext {
            inner: ctx.clone(),
            agent: self.agent.clone(),
            config,
            max_delegation_depth: self.policy.max_delegation_depth,
            required_confirmations,
        });
        let incoming = self.incoming_execution(ctx.as_ref(), &root_invocation_id);
        let incoming_relationship_span = incoming.as_ref().map(|(relationship, edge)| {
            adk_telemetry::team_relationship_span_with_context(
                self.runtime.team_name(),
                &relationship.from,
                &relationship.to,
                "handoff",
                &edge.id,
                &root_invocation_id,
                ctx.session_id(),
            )
        });
        let incoming_lifecycle =
            incoming.as_ref().map(|(relationship, edge)| TeamLifecycleContext {
                team: self.runtime.team_name().to_string(),
                invocation_id: root_invocation_id.clone(),
                phase: TeamLifecyclePhase::Relationship,
                member: None,
                edge_id: Some(edge.id.clone()),
                from: Some(relationship.from.clone()),
                to: Some(relationship.to.clone()),
                kind: Some(relationship.kind),
                attempt: Some(edge.attempt),
            });
        let incoming_relationship_started = std::time::Instant::now();
        let (max_attempts, backoff_ms) = incoming.as_ref().map_or((1, 0), |(relationship, _)| {
            match relationship.policy.failure {
                RelationshipFailureStrategy::Retry { max_attempts, backoff_ms }
                | RelationshipFailureStrategy::RetryThenFallback {
                    max_attempts, backoff_ms, ..
                } => (max_attempts, backoff_ms),
                RelationshipFailureStrategy::Inherit
                | RelationshipFailureStrategy::Propagate
                | RelationshipFailureStrategy::ReturnError
                | RelationshipFailureStrategy::Fallback { .. } => (1, 0),
            }
        });
        let mut immediate_error = None;
        let mut inner = None;
        for attempt in 1..=max_attempts {
            match self
                .agent
                .run(configured_ctx.clone())
                .instrument(incoming_relationship_span.clone().unwrap_or_else(tracing::Span::none))
                .await
            {
                Ok(stream) => {
                    inner = Some(stream);
                    break;
                }
                Err(error) => {
                    immediate_error = Some(error);
                    if attempt < max_attempts && backoff_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }
        let Some(mut inner) = inner else {
            let error = immediate_error.unwrap_or_else(|| {
                adk_core::AdkError::new(
                    adk_core::ErrorComponent::Agent,
                    adk_core::ErrorCategory::Internal,
                    "agent.team.member_start_failed",
                    format!("team member '{}' failed to start", self.name),
                )
            });
            if let Some((_, edge)) = &incoming {
                self.runtime.finish_edge(&root_invocation_id, &edge.id, Some(error.to_string()));
            }
            if let Some(span) = &incoming_relationship_span {
                span.record("team.status", "failed");
                span.record(
                    "team.duration_ms",
                    incoming_relationship_started.elapsed().as_millis() as u64,
                );
            }
            if let Some(lifecycle) = &incoming_lifecycle {
                self.runtime
                    .after_lifecycle(
                        lifecycle,
                        &TeamLifecycleOutcome::Failed {
                            code: Some(error.code.to_string()),
                            message: error.to_string(),
                        },
                    )
                    .await?;
            }
            self.runtime.fail(&root_invocation_id, error.to_string());
            if let Some((relationship, incoming_edge)) = &incoming {
                match &relationship.policy.failure {
                    RelationshipFailureStrategy::Fallback { target }
                    | RelationshipFailureStrategy::RetryThenFallback { target, .. } => {
                        let mut event = adk_core::Event::new(ctx.invocation_id());
                        event.author = relationship.from.clone();
                        event.actions.transfer_to_agent = Some(target.clone());
                        event.llm_response.turn_complete = true;
                        let edge_id = self.runtime.start_edge(
                            &root_invocation_id,
                            TeamEdgeStart {
                                execution_id: None,
                                parent_id: incoming_edge.parent_id.clone(),
                                from: &relationship.from,
                                to: target,
                                kind: RelationshipKind::Handoff,
                                attempt: 1,
                            },
                        )?;
                        self.runtime.record_event(
                            &root_invocation_id,
                            Some(&edge_id),
                            &mut event,
                        )?;
                        return wrap_lifecycle_stream(
                            Ok(Box::pin(futures::stream::once(async { Ok(event) }))),
                            self.runtime.clone(),
                            member_lifecycle,
                            member_span,
                        )
                        .await;
                    }
                    RelationshipFailureStrategy::ReturnError => {
                        let mut event = adk_core::Event::new(ctx.invocation_id());
                        event.author = self.name.clone();
                        event.llm_response.turn_complete = true;
                        event.llm_response.content = Some(
                            adk_core::Content::new("model").with_text(
                                serde_json::json!({
                                    "error": error.to_string(),
                                    "agent": self.name,
                                })
                                .to_string(),
                            ),
                        );
                        self.runtime.record_event(
                            &root_invocation_id,
                            Some(&incoming_edge.id),
                            &mut event,
                        )?;
                        return wrap_lifecycle_stream(
                            Ok(Box::pin(futures::stream::once(async { Ok(event) }))),
                            self.runtime.clone(),
                            member_lifecycle,
                            member_span,
                        )
                        .await;
                    }
                    RelationshipFailureStrategy::Inherit
                    | RelationshipFailureStrategy::Propagate
                    | RelationshipFailureStrategy::Retry { .. } => {}
                }
            }
            return wrap_lifecycle_stream(
                Err(error),
                self.runtime.clone(),
                member_lifecycle,
                member_span,
            )
            .await;
        };
        let relationships = self.relationships.clone();
        let member = self.name.clone();
        let inner_name = self.agent.name().to_string();
        let runtime = self.runtime.clone();
        let incoming_edge_id = incoming.as_ref().map(|(_, edge)| edge.id.clone());
        let stream = async_stream::stream! {
            let mut incoming_finished = false;
            while let Some(result) = inner.next().await {
                match result {
                    Ok(mut event) => {
                        if event.author.is_empty() || event.author == inner_name {
                            event.author.clone_from(&member);
                        }
                        let mut event_edge_id = ctx
                            .orchestration_edge_id()
                            .map(str::to_string)
                            .or_else(|| incoming_edge_id.clone());
                        if let Some(target) = &event.actions.transfer_to_agent {
                            let Some(_relationship) = relationships.iter().find(|relationship| {
                                relationship.kind == RelationshipKind::Handoff
                                    && relationship.to == *target
                            }) else {
                                let allowed = relationships
                                    .iter()
                                    .filter(|relationship| relationship.kind == RelationshipKind::Handoff)
                                    .map(|relationship| relationship.to.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                let error: adk_core::AdkError = TeamRuntimeError::PolicyDenied {
                                    operation: format!(
                                        "handoff from '{member}' to '{target}' (allowed: {allowed})"
                                    ),
                                    reason: "the exact edge is not declared".to_string(),
                                }
                                .into();
                                runtime.fail(&root_invocation_id, error.to_string());
                                yield Err(error);
                                return;
                            };
                            let proposed_edge_id = uuid::Uuid::new_v4().to_string();
                            let relationship_lifecycle = TeamLifecycleContext {
                                team: runtime.team_name().to_string(),
                                invocation_id: root_invocation_id.clone(),
                                phase: TeamLifecyclePhase::Relationship,
                                member: None,
                                edge_id: Some(proposed_edge_id.clone()),
                                from: Some(member.clone()),
                                to: Some(target.clone()),
                                kind: Some(RelationshipKind::Handoff),
                                attempt: Some(1),
                            };
                            match runtime.before_lifecycle(&relationship_lifecycle).await {
                                Ok(TeamLifecycleDecision::Continue) => {}
                                Ok(TeamLifecycleDecision::Terminate { reason }) => {
                                    if let Err(error) = runtime
                                        .after_lifecycle(
                                            &relationship_lifecycle,
                                            &TeamLifecycleOutcome::Terminated {
                                                reason: reason.clone(),
                                            },
                                        )
                                        .await
                                    {
                                        yield Err(error);
                                    } else {
                                        yield Err(TeamRuntimeError::PolicyDenied {
                                            operation: format!(
                                                "handoff from '{member}' to '{target}'"
                                            ),
                                            reason,
                                        }
                                        .into());
                                    }
                                    return;
                                }
                                Err(error) => {
                                    yield Err(error);
                                    return;
                                }
                            }
                            match runtime.start_edge(
                                &root_invocation_id,
                                TeamEdgeStart {
                                    execution_id: Some(proposed_edge_id),
                                    parent_id: event_edge_id.clone(),
                                    from: &member,
                                    to: target,
                                    kind: RelationshipKind::Handoff,
                                    attempt: 1,
                                },
                            ) {
                                Ok(edge_id) => {
                                    if let Some(incoming_id) = &incoming_edge_id {
                                        runtime.finish_edge(&root_invocation_id, incoming_id, None);
                                        incoming_finished = true;
                                        if let Some(span) = &incoming_relationship_span {
                                            span.record("team.status", "completed");
                                            span.record(
                                                "team.duration_ms",
                                                incoming_relationship_started.elapsed().as_millis() as u64,
                                            );
                                        }
                                        if let Some(lifecycle) = &incoming_lifecycle
                                            && let Err(error) = runtime
                                                .after_lifecycle(
                                                    lifecycle,
                                                    &TeamLifecycleOutcome::Succeeded,
                                                )
                                                .await
                                        {
                                            yield Err(error);
                                            return;
                                        }
                                    }
                                    event_edge_id = Some(edge_id);
                                }
                                Err(error) => {
                                    yield Err(error);
                                    return;
                                }
                            }
                        } else if event.is_final_response()
                            && let Some(incoming_id) = &incoming_edge_id
                        {
                            runtime.finish_edge(&root_invocation_id, incoming_id, None);
                            incoming_finished = true;
                            if let Some(span) = &incoming_relationship_span {
                                span.record("team.status", "completed");
                                span.record(
                                    "team.duration_ms",
                                    incoming_relationship_started.elapsed().as_millis() as u64,
                                );
                            }
                            if let Some(lifecycle) = &incoming_lifecycle
                                && let Err(error) = runtime
                                    .after_lifecycle(lifecycle, &TeamLifecycleOutcome::Succeeded)
                                    .await
                            {
                                yield Err(error);
                                return;
                            }
                        }
                        match runtime.record_event(
                            &root_invocation_id,
                            event_edge_id.as_deref(),
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
                    Err(error) => {
                        if let Some(incoming_id) = &incoming_edge_id {
                            runtime.finish_edge(
                                &root_invocation_id,
                                incoming_id,
                                Some(error.to_string()),
                            );
                        }
                        runtime.fail(&root_invocation_id, error.to_string());
                        if let Some(span) = &incoming_relationship_span {
                            span.record("team.status", "failed");
                            span.record(
                                "team.duration_ms",
                                incoming_relationship_started.elapsed().as_millis() as u64,
                            );
                        }
                        if let Some(lifecycle) = &incoming_lifecycle {
                            let outcome = TeamLifecycleOutcome::Failed {
                                code: Some(error.code.to_string()),
                                message: error.to_string(),
                            };
                            if let Err(hook_error) = runtime.after_lifecycle(lifecycle, &outcome).await {
                                yield Err(hook_error);
                                return;
                            }
                        }
                        yield Err(error);
                        return;
                    }
                }
            }
            if !incoming_finished && let Some(incoming_id) = &incoming_edge_id {
                runtime.finish_edge(&root_invocation_id, incoming_id, None);
                if let Some(span) = &incoming_relationship_span {
                    span.record("team.status", "completed");
                    span.record(
                        "team.duration_ms",
                        incoming_relationship_started.elapsed().as_millis() as u64,
                    );
                }
                if let Some(lifecycle) = &incoming_lifecycle
                    && let Err(error) = runtime
                        .after_lifecycle(lifecycle, &TeamLifecycleOutcome::Succeeded)
                        .await
                {
                    yield Err(error);
                }
            }
        };
        wrap_lifecycle_stream(
            Ok(Box::pin(stream)),
            self.runtime.clone(),
            member_lifecycle,
            member_span,
        )
        .await
    }
}

impl TeamMemberAgent {
    fn incoming_execution(
        &self,
        ctx: &dyn InvocationContext,
        root_invocation_id: &str,
    ) -> Option<(TeamRelationship, TeamEdgeExecution)> {
        let snapshot = self.runtime.snapshot(root_invocation_id).or_else(|| {
            ctx.session()
                .state()
                .get(TEAM_EXECUTION_STATE_KEY)
                .and_then(|value| serde_json::from_value::<TeamExecutionSnapshot>(value).ok())
        })?;
        let edge = snapshot
            .edges
            .iter()
            .rev()
            .find(|edge| {
                edge.kind == RelationshipKind::Handoff
                    && edge.status == TeamExecutionStatus::Running
                    && edge.to == self.name
            })?
            .clone();
        let relationship = self
            .incoming_relationships
            .iter()
            .find(|relationship| {
                relationship.kind == RelationshipKind::Handoff
                    && relationship.from == edge.from
                    && relationship.to == edge.to
            })
            .cloned()?;
        Some((relationship, edge))
    }
}

/// Context wrapper that makes member-specific policy visible even when the
/// member is invoked by `AgentTool` rather than directly by `Runner`.
struct TeamInvocationContext {
    inner: Arc<dyn InvocationContext>,
    agent: Arc<dyn Agent>,
    config: RunConfig,
    max_delegation_depth: u32,
    required_confirmations: HashSet<String>,
}

#[async_trait]
impl adk_core::ReadonlyContext for TeamInvocationContext {
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

    fn user_content(&self) -> &adk_core::Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl adk_core::CallbackContext for TeamInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
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
impl InvocationContext for TeamInvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        self.inner.memory()
    }

    fn session(&self) -> &dyn adk_core::Session {
        self.inner.session()
    }

    fn run_config(&self) -> &RunConfig {
        &self.config
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
        Some(
            self.inner.max_delegation_depth().map_or(self.max_delegation_depth, |current| {
                current.min(self.max_delegation_depth)
            }),
        )
    }

    fn orchestration_root_invocation_id(&self) -> &str {
        self.inner.orchestration_root_invocation_id()
    }

    fn orchestration_edge_id(&self) -> Option<&str> {
        self.inner.orchestration_edge_id()
    }

    fn requires_tool_confirmation(&self, tool_name: &str) -> bool {
        self.required_confirmations.contains(tool_name)
            || self.inner.requires_tool_confirmation(tool_name)
    }

    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        self.inner.get_secret(name).await
    }

    async fn get_secret_for(&self, request: &adk_core::SecretRequest) -> Result<Option<String>> {
        self.inner.get_secret_for(request).await
    }
}

#[cfg(feature = "team-tools")]
struct TeamToolset {
    name: String,
    tools: Vec<Arc<dyn Tool>>,
}

#[cfg(feature = "team-tools")]
#[async_trait]
impl Toolset for TeamToolset {
    fn name(&self) -> &str {
        &self.name
    }

    async fn tools(&self, _ctx: Arc<dyn adk_core::ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }
}

#[cfg(feature = "team-tools")]
struct BoundedDelegateTool {
    inner: Arc<dyn Tool>,
    fallback: Option<Arc<dyn Tool>>,
    limiter: Arc<tokio::sync::Semaphore>,
    relationship: TeamRelationship,
    team_failure: TeamFailurePolicy,
    runtime: Arc<TeamRuntimeRegistry>,
    circuit: Arc<Mutex<CircuitState>>,
}

#[cfg(feature = "team-tools")]
#[derive(Default)]
struct CircuitState {
    consecutive_failures: u32,
    opened_at: Option<std::time::Instant>,
}

#[cfg(feature = "team-tools")]
#[async_trait]
impl Tool for BoundedDelegateTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn declaration(&self) -> serde_json::Value {
        self.inner.declaration()
    }

    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }

    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }

    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        self.inner.parameters_schema()
    }

    fn response_schema(&self) -> Option<serde_json::Value> {
        self.inner.response_schema()
    }

    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }

    async fn execute(
        &self,
        ctx: Arc<dyn adk_core::ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let _permit = self.limiter.acquire().await.map_err(|_| {
            adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::Unavailable,
                "tool.team.delegation_limiter_closed",
                "team delegation limiter was closed",
            )
        })?;
        let root_invocation_id = ctx.orchestration_root_invocation_id().to_string();
        self.runtime.check_budget(&root_invocation_id)?;
        if let Some(schema) = &self.relationship.policy.input_schema {
            let validator = jsonschema::validator_for(schema).map_err(|error| {
                adk_core::AdkError::new(
                    adk_core::ErrorComponent::Tool,
                    adk_core::ErrorCategory::Internal,
                    "tool.team.invalid_compiled_input_schema",
                    format!("invalid relationship input schema: {error}"),
                )
            })?;
            if !validator.is_valid(&args) {
                return Err(adk_core::AdkError::new(
                    adk_core::ErrorComponent::Tool,
                    adk_core::ErrorCategory::InvalidInput,
                    "tool.team.delegate_input_invalid",
                    format!(
                        "arguments for delegation from '{}' to '{}' do not satisfy the relationship input schema",
                        self.relationship.from, self.relationship.to
                    ),
                ));
            }
        }
        if let Some(policy) = self.relationship.policy.circuit_breaker {
            let mut circuit = self.circuit.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(opened_at) = circuit.opened_at {
                if opened_at.elapsed() < std::time::Duration::from_millis(policy.reset_after_ms) {
                    return self.handle_failure(format!(
                        "delegation circuit from '{}' to '{}' is open",
                        self.relationship.from, self.relationship.to
                    ));
                }
                circuit.opened_at = None;
            }
        }

        let execution_id = ctx.orchestration_edge_id().map_or_else(
            || ctx.function_call_id().to_string(),
            |parent| format!("{parent}/{}", ctx.function_call_id()),
        );
        let lifecycle = TeamLifecycleContext {
            team: self.runtime.team_name().to_string(),
            invocation_id: root_invocation_id.clone(),
            phase: TeamLifecyclePhase::Relationship,
            member: None,
            edge_id: Some(execution_id.clone()),
            from: Some(self.relationship.from.clone()),
            to: Some(self.relationship.to.clone()),
            kind: Some(RelationshipKind::Delegate),
            attempt: Some(1),
        };
        let relationship_span = adk_telemetry::team_relationship_span_with_context(
            self.runtime.team_name(),
            &self.relationship.from,
            &self.relationship.to,
            "delegate",
            &execution_id,
            &root_invocation_id,
            ctx.session_id(),
        );
        let relationship_started = std::time::Instant::now();
        if let TeamLifecycleDecision::Terminate { reason } =
            self.runtime.before_lifecycle(&lifecycle).instrument(relationship_span.clone()).await?
        {
            self.runtime
                .after_lifecycle(
                    &lifecycle,
                    &TeamLifecycleOutcome::Terminated { reason: reason.clone() },
                )
                .await?;
            return Err(TeamRuntimeError::PolicyDenied {
                operation: format!(
                    "delegation from '{}' to '{}'",
                    self.relationship.from, self.relationship.to
                ),
                reason,
            }
            .into());
        }
        self.runtime.start_edge(
            &root_invocation_id,
            TeamEdgeStart {
                execution_id: Some(execution_id.clone()),
                parent_id: ctx.orchestration_edge_id().map(str::to_string),
                from: &self.relationship.from,
                to: &self.relationship.to,
                kind: RelationshipKind::Delegate,
                attempt: 1,
            },
        )?;
        let mut checkpoint = adk_core::Event::new(ctx.invocation_id());
        checkpoint.author = self.relationship.from.clone();
        self.runtime.record_event(&root_invocation_id, Some(&execution_id), &mut checkpoint)?;
        ctx.emit_event(checkpoint).await;

        let (max_attempts, backoff_ms) = match self.relationship.policy.failure {
            RelationshipFailureStrategy::Retry { max_attempts, backoff_ms }
            | RelationshipFailureStrategy::RetryThenFallback { max_attempts, backoff_ms, .. } => {
                (max_attempts, backoff_ms)
            }
            RelationshipFailureStrategy::Inherit
            | RelationshipFailureStrategy::Propagate
            | RelationshipFailureStrategy::ReturnError
            | RelationshipFailureStrategy::Fallback { .. } => (1, 0),
        };
        let mut last_error = None;
        for attempt in 1..=max_attempts {
            match self
                .inner
                .execute(ctx.clone(), args.clone())
                .instrument(relationship_span.clone())
                .await
            {
                Ok(value) => {
                    if let Some(schema) = &self.relationship.policy.output_schema {
                        let validator = jsonschema::validator_for(schema).map_err(|error| {
                            adk_core::AdkError::new(
                                adk_core::ErrorComponent::Tool,
                                adk_core::ErrorCategory::Internal,
                                "tool.team.invalid_compiled_output_schema",
                                format!("invalid relationship output schema: {error}"),
                            )
                        })?;
                        if !validator.is_valid(&value) {
                            last_error = Some(format!(
                                "delegate '{}' returned a value that does not satisfy the relationship output schema",
                                self.relationship.to
                            ));
                        } else {
                            self.record_success(&root_invocation_id, &execution_id, &lifecycle)
                                .await?;
                            relationship_span.record("team.status", "completed");
                            relationship_span.record(
                                "team.duration_ms",
                                relationship_started.elapsed().as_millis() as u64,
                            );
                            return Ok(value);
                        }
                    } else {
                        self.record_success(&root_invocation_id, &execution_id, &lifecycle).await?;
                        relationship_span.record("team.status", "completed");
                        relationship_span.record(
                            "team.duration_ms",
                            relationship_started.elapsed().as_millis() as u64,
                        );
                        return Ok(value);
                    }
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt < max_attempts && backoff_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
        }

        if let Some(fallback) = &self.fallback {
            match fallback.execute(ctx, args).await {
                Ok(value) => {
                    self.record_success(&root_invocation_id, &execution_id, &lifecycle).await?;
                    relationship_span.record("team.status", "completed");
                    relationship_span.record(
                        "team.duration_ms",
                        relationship_started.elapsed().as_millis() as u64,
                    );
                    return Ok(value);
                }
                Err(error) => {
                    last_error = Some(format!(
                        "primary delegate '{}' failed and fallback '{}' also failed: {error}",
                        self.relationship.to,
                        fallback.name()
                    ));
                }
            }
        }

        let error = last_error.unwrap_or_else(|| "delegated execution failed".to_string());
        self.runtime.finish_edge(&root_invocation_id, &execution_id, Some(error.clone()));
        if let Some(policy) = self.relationship.policy.circuit_breaker {
            let mut circuit = self.circuit.lock().unwrap_or_else(|failure| failure.into_inner());
            circuit.consecutive_failures = circuit.consecutive_failures.saturating_add(1);
            if circuit.consecutive_failures >= policy.failure_threshold {
                circuit.opened_at = Some(std::time::Instant::now());
            }
        }
        self.runtime
            .after_lifecycle(
                &lifecycle,
                &TeamLifecycleOutcome::Failed { code: None, message: error.clone() },
            )
            .await?;
        relationship_span.record("team.status", "failed");
        relationship_span
            .record("team.duration_ms", relationship_started.elapsed().as_millis() as u64);
        self.handle_failure(error)
    }
}

#[cfg(feature = "team-tools")]
impl BoundedDelegateTool {
    async fn record_success(
        &self,
        invocation_id: &str,
        execution_id: &str,
        lifecycle: &TeamLifecycleContext,
    ) -> Result<()> {
        self.runtime.finish_edge(invocation_id, execution_id, None);
        {
            let mut circuit = self.circuit.lock().unwrap_or_else(|error| error.into_inner());
            circuit.consecutive_failures = 0;
            circuit.opened_at = None;
        }
        self.runtime.after_lifecycle(lifecycle, &TeamLifecycleOutcome::Succeeded).await
    }

    fn handle_failure(&self, error: String) -> Result<serde_json::Value> {
        let return_error =
            matches!(self.relationship.policy.failure, RelationshipFailureStrategy::ReturnError)
                || matches!(self.relationship.policy.failure, RelationshipFailureStrategy::Inherit)
                    && self.team_failure == TeamFailurePolicy::ReturnDelegateError;
        if return_error {
            Ok(serde_json::json!({
                "error": error,
                "agent": self.relationship.to,
            }))
        } else {
            Err(adk_core::AdkError::new(
                adk_core::ErrorComponent::Tool,
                adk_core::ErrorCategory::Internal,
                "tool.team.delegation_failed",
                error,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, Event, EventStream, ReadonlyContext, Session, State};
    use async_stream::stream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubAgent(String);

    #[async_trait]
    impl Agent for StubAgent {
        fn name(&self) -> &str {
            &self.0
        }

        fn description(&self) -> &str {
            "stub"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        fn capabilities(&self) -> adk_core::AgentCapabilities {
            adk_core::AgentCapabilities {
                runtime_tools: true,
                handoff: true,
                relationship_confirmation: true,
                checkpoint_resume: false,
                shared_state: true,
                invocation_metadata: true,
            }
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            Ok(Box::pin(stream! { yield Ok(Event::new("stub")); }))
        }
    }

    struct TestState;

    impl State for TestState {
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
            "team-session"
        }

        fn app_name(&self) -> &str {
            "team-app"
        }

        fn user_id(&self) -> &str {
            "team-user"
        }

        fn state(&self) -> &dyn State {
            &TestState
        }

        fn conversation_history(&self) -> Vec<Content> {
            Vec::new()
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
            "team-invocation"
        }

        fn agent_name(&self) -> &str {
            "support_team"
        }

        fn user_id(&self) -> &str {
            "team-user"
        }

        fn app_name(&self) -> &str {
            "team-app"
        }

        fn session_id(&self) -> &str {
            "team-session"
        }

        fn branch(&self) -> &str {
            ""
        }

        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl adk_core::CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl InvocationContext for TestContext {
        fn agent(&self) -> Arc<dyn Agent> {
            panic!("agent identity is not used by this test")
        }

        fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
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

    fn valid_spec() -> TeamSpec {
        TeamSpec {
            name: "support_team".to_string(),
            description: "Routes support work".to_string(),
            coordinator: "supervisor".to_string(),
            members: vec![
                TeamMemberSpec::new("supervisor"),
                TeamMemberSpec::new("billing"),
                TeamMemberSpec::new("technical"),
            ],
            relationships: vec![
                TeamRelationship::new("supervisor", "billing", RelationshipKind::Handoff),
                TeamRelationship::new("supervisor", "technical", RelationshipKind::Handoff),
            ],
            policy: TeamPolicy::default(),
        }
    }

    #[test]
    fn validates_and_round_trips_schema() {
        let spec = valid_spec();
        spec.validate().unwrap();
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<TeamSpec>(&json).unwrap(), spec);
        let schema = schemars::schema_for!(TeamSpec);
        assert_eq!(schema.get("title").and_then(serde_json::Value::as_str), Some("TeamSpec"));
    }

    #[test]
    fn rejects_duplicates_cycles_and_unreachable_members() {
        let mut duplicate = valid_spec();
        duplicate.members.push(TeamMemberSpec::new("billing"));
        assert_eq!(duplicate.validate(), Err(TeamError::DuplicateMember("billing".to_string())));

        let mut cyclic = valid_spec();
        cyclic.relationships.push(TeamRelationship::new(
            "billing",
            "supervisor",
            RelationshipKind::Delegate,
        ));
        assert!(matches!(cyclic.validate(), Err(TeamError::CyclicTopology(_))));

        let mut unreachable = valid_spec();
        unreachable.relationships.pop();
        assert_eq!(
            unreachable.validate(),
            Err(TeamError::UnreachableMember("technical".to_string()))
        );
    }

    #[test]
    fn validates_relationship_contracts_budgets_and_fallback_edges() {
        let mut invalid_schema = valid_spec();
        invalid_schema.relationships[0].policy.input_schema = Some(serde_json::json!({"type": 7}));
        assert!(matches!(
            invalid_schema.validate(),
            Err(TeamError::InvalidRelationshipPolicy { .. })
        ));

        let mut ignored_handoff_contract = valid_spec();
        ignored_handoff_contract.relationships[0].policy.input_schema =
            Some(serde_json::json!({"type": "object"}));
        let error = ignored_handoff_contract.validate().unwrap_err();
        assert!(error.to_string().contains("applies only to Delegate"));

        let mut zero_budget = valid_spec();
        zero_budget.policy.budget.max_events = Some(0);
        assert_eq!(zero_budget.validate(), Err(TeamError::InvalidBudget("maxEvents")));

        let mut undeclared_fallback = valid_spec();
        undeclared_fallback.relationships[0].policy.failure =
            RelationshipFailureStrategy::Fallback { target: "supervisor".to_string() };
        assert!(matches!(
            undeclared_fallback.validate(),
            Err(TeamError::InvalidRelationshipPolicy { .. })
        ));
    }

    #[test]
    fn compiles_exact_handoff_allowlists() {
        let team = valid_spec()
            .compile(vec![
                Arc::new(StubAgent("supervisor".into())) as Arc<dyn Agent>,
                Arc::new(StubAgent("billing".into())),
                Arc::new(StubAgent("technical".into())),
            ])
            .unwrap();
        assert_eq!(
            team.transfer_targets_for("supervisor").unwrap(),
            vec!["billing".to_string(), "technical".to_string()]
        );
        assert_eq!(team.transfer_targets_for("billing"), Some(Vec::new()));
        assert!(team.strict_transfer_policy());
        let topology = team.topology().unwrap();
        assert_eq!(topology.root, "support_team");
        assert_eq!(topology.coordinator, "supervisor");
        assert_eq!(topology.members.len(), 3);
        assert!(topology.members[0].coordinator);
        assert_eq!(topology.relationships.len(), 2);
        assert!(topology.relationships.iter().all(|relationship| {
            relationship.kind == adk_core::AgentRelationshipKind::Handoff
                && relationship.from == "supervisor"
        }));
    }

    #[test]
    fn rejects_missing_agent_binding() {
        let error = valid_spec()
            .compile(vec![
                Arc::new(StubAgent("supervisor".into())) as Arc<dyn Agent>,
                Arc::new(StubAgent("billing".into())),
            ])
            .unwrap_err();
        assert_eq!(error, TeamError::MissingAgent("technical".to_string()));
    }

    struct EmittingAgent {
        name: String,
        target: Option<String>,
        runs: Arc<AtomicUsize>,
    }

    #[cfg(feature = "team-tools")]
    struct FlakyAgent {
        name: String,
        failures_before_success: usize,
        runs: Arc<AtomicUsize>,
    }

    #[cfg(feature = "team-tools")]
    #[async_trait]
    impl Agent for FlakyAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "deterministic failure test member"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            let attempt = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.failures_before_success {
                return Err(adk_core::AdkError::agent(format!(
                    "planned failure from '{}'",
                    self.name
                )));
            }
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name.clone();
            event.llm_response.content = Some(Content::new("model").with_text("recovered"));
            Ok(Box::pin(stream! { yield Ok(event); }))
        }
    }

    #[async_trait]
    impl Agent for EmittingAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "deterministic test member"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name.clone();
            event.actions.transfer_to_agent = self.target.clone();
            event.llm_response.content = Some(Content::new("model").with_text(&self.name));
            Ok(Box::pin(stream! { yield Ok(event); }))
        }
    }

    #[tokio::test]
    async fn handoff_transfers_control_without_invoking_target_inline() {
        let supervisor_runs = Arc::new(AtomicUsize::new(0));
        let specialist_runs = Arc::new(AtomicUsize::new(0));
        let team = TeamSpec {
            name: "handoff_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("specialist")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "specialist",
                RelationshipKind::Handoff,
            )],
            policy: TeamPolicy::default(),
        }
        .compile(vec![
            Arc::new(EmittingAgent {
                name: "supervisor".to_string(),
                target: Some("specialist".to_string()),
                runs: supervisor_runs.clone(),
            }) as Arc<dyn Agent>,
            Arc::new(EmittingAgent {
                name: "specialist".to_string(),
                target: None,
                runs: specialist_runs.clone(),
            }),
        ])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("route"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team.run(context).await.unwrap().collect::<Vec<_>>().await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].as_ref().unwrap().actions.transfer_to_agent.as_deref(),
            Some("specialist")
        );
        assert_eq!(supervisor_runs.load(Ordering::SeqCst), 1);
        assert_eq!(specialist_runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn restored_active_handoff_resumes_at_frozen_target() {
        let supervisor_runs = Arc::new(AtomicUsize::new(0));
        let specialist_runs = Arc::new(AtomicUsize::new(0));
        let team = TeamSpec {
            name: "resume_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("specialist")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "specialist",
                RelationshipKind::Handoff,
            )],
            policy: TeamPolicy::default(),
        }
        .compile([
            Arc::new(EmittingAgent {
                name: "supervisor".to_string(),
                target: Some("specialist".to_string()),
                runs: supervisor_runs.clone(),
            }) as Arc<dyn Agent>,
            Arc::new(EmittingAgent {
                name: "specialist".to_string(),
                target: None,
                runs: specialist_runs.clone(),
            }),
        ])
        .unwrap();
        team.runtime
            .start_edge(
                "team-invocation",
                TeamEdgeStart {
                    execution_id: Some("persisted-handoff".to_string()),
                    parent_id: None,
                    from: "supervisor",
                    to: "specialist",
                    kind: RelationshipKind::Handoff,
                    attempt: 1,
                },
            )
            .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("resume"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team.run(context).await.unwrap().collect::<Vec<_>>().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].as_ref().unwrap().author, "specialist");
        assert_eq!(supervisor_runs.load(Ordering::SeqCst), 0);
        assert_eq!(specialist_runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            team.execution_snapshot("team-invocation").unwrap().edges[0].status,
            TeamExecutionStatus::Completed
        );
    }

    #[tokio::test]
    async fn runner_handoff_keeps_one_completed_team_receipt() {
        let supervisor_runs = Arc::new(AtomicUsize::new(0));
        let specialist_runs = Arc::new(AtomicUsize::new(0));
        let team = Arc::new(
            TeamSpec {
                name: "runner_receipt_team".to_string(),
                description: String::new(),
                coordinator: "supervisor".to_string(),
                members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("specialist")],
                relationships: vec![TeamRelationship::new(
                    "supervisor",
                    "specialist",
                    RelationshipKind::Handoff,
                )],
                policy: TeamPolicy::default(),
            }
            .compile([
                Arc::new(EmittingAgent {
                    name: "supervisor".to_string(),
                    target: Some("specialist".to_string()),
                    runs: supervisor_runs.clone(),
                }) as Arc<dyn Agent>,
                Arc::new(EmittingAgent {
                    name: "specialist".to_string(),
                    target: None,
                    runs: specialist_runs.clone(),
                }),
            ])
            .unwrap(),
        );
        let sessions = Arc::new(adk_session::InMemorySessionService::new());
        adk_session::SessionService::create(
            sessions.as_ref(),
            adk_session::CreateRequest {
                app_name: "runner-receipt".to_string(),
                user_id: "user".to_string(),
                session_id: Some("session".to_string()),
                state: HashMap::new(),
            },
        )
        .await
        .unwrap();
        let runner = adk_runner::Runner::builder()
            .app_name("runner-receipt")
            .agent(team.clone() as Arc<dyn Agent>)
            .session_service(sessions)
            .build()
            .unwrap();
        let stream = runner
            .run_str("user", "session", Content::new("user").with_text("route"))
            .await
            .unwrap();
        let results = stream.collect::<Vec<_>>().await;
        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(supervisor_runs.load(Ordering::SeqCst), 1);
        assert_eq!(specialist_runs.load(Ordering::SeqCst), 1);
        let receipts = team.execution_snapshots();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].edges.len(), 1);
        assert_eq!(receipts[0].edges[0].status, TeamExecutionStatus::Completed);
    }

    #[tokio::test]
    async fn exact_allowlist_rejects_undeclared_handoff() {
        let team = valid_spec()
            .compile(vec![
                Arc::new(EmittingAgent {
                    name: "supervisor".to_string(),
                    target: Some("undeclared".to_string()),
                    runs: Arc::new(AtomicUsize::new(0)),
                }) as Arc<dyn Agent>,
                Arc::new(StubAgent("billing".to_string())),
                Arc::new(StubAgent("technical".to_string())),
            ])
            .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("route"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let results = team.run(context).await.unwrap().collect::<Vec<_>>().await;
        let error = results[0].as_ref().unwrap_err();
        assert_eq!(error.code, "agent.team.policy_denied");
        assert!(error.to_string().contains("exact edge is not declared"));
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn delegate_edge_lowers_to_only_the_declared_agent_tool() {
        let spec = TeamSpec {
            name: "delegate_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("researcher")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "researcher",
                RelationshipKind::Delegate,
            )],
            policy: TeamPolicy::default(),
        };
        let team = spec
            .compile(vec![
                Arc::new(StubAgent("supervisor".to_string())) as Arc<dyn Agent>,
                Arc::new(StubAgent("researcher".to_string())),
            ])
            .unwrap();
        let mut config = RunConfig::default();
        team.configure_run(team.name(), &mut config);
        assert!(config.transfer_targets.is_empty());
        assert_eq!(config.runtime_toolsets.len(), 1);
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("research"),
            config: config.clone(),
            session: TestSession,
        });
        let tools = config.runtime_toolsets[0]
            .toolset()
            .tools(context as Arc<dyn ReadonlyContext>)
            .await
            .unwrap();
        assert_eq!(tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["researcher"]);
    }

    #[cfg(feature = "team-tools")]
    struct ScriptedModel {
        responses: std::sync::Mutex<std::collections::VecDeque<adk_core::LlmResponse>>,
    }

    #[cfg(feature = "team-tools")]
    impl ScriptedModel {
        fn new(responses: Vec<adk_core::LlmResponse>) -> Self {
            Self { responses: std::sync::Mutex::new(responses.into()) }
        }

        fn text(text: &str) -> adk_core::LlmResponse {
            adk_core::LlmResponse {
                content: Some(Content::new("model").with_text(text)),
                usage_metadata: None,
                finish_reason: Some(adk_core::FinishReason::Stop),
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
            }
        }

        fn call(name: &str) -> adk_core::LlmResponse {
            adk_core::LlmResponse {
                content: Some(Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::FunctionCall {
                        name: name.to_string(),
                        args: serde_json::json!({"request": "research this"}),
                        id: Some("delegate-1".to_string()),
                        thought_signature: None,
                    }],
                }),
                ..Self::text("")
            }
        }

        fn transfer(target: &str) -> adk_core::LlmResponse {
            adk_core::LlmResponse {
                content: Some(Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::FunctionCall {
                        name: "transfer_to_agent".to_string(),
                        args: serde_json::json!({"agent_name": target}),
                        id: Some("handoff-1".to_string()),
                        thought_signature: None,
                    }],
                }),
                ..Self::text("")
            }
        }
    }

    #[cfg(feature = "team-tools")]
    #[async_trait]
    impl adk_core::Llm for ScriptedModel {
        fn name(&self) -> &str {
            "scripted-team-model"
        }

        async fn generate_content(
            &self,
            _request: adk_core::LlmRequest,
            _stream: bool,
        ) -> Result<adk_core::LlmResponseStream> {
            let response = self
                .responses
                .lock()
                .expect("script lock")
                .pop_front()
                .expect("script should have another response");
            Ok(Box::pin(stream! { yield Ok(response); }))
        }
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn delegation_returns_to_the_scripted_supervisor() {
        let supervisor = Arc::new(
            crate::LlmAgentBuilder::new("supervisor")
                .model(Arc::new(ScriptedModel::new(vec![
                    ScriptedModel::call("researcher"),
                    ScriptedModel::text("supervisor final"),
                ])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let researcher = Arc::new(
            crate::LlmAgentBuilder::new("researcher")
                .model(Arc::new(ScriptedModel::new(vec![ScriptedModel::text("research result")])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let team = TeamSpec {
            name: "scripted_delegate_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("researcher")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "researcher",
                RelationshipKind::Delegate,
            )],
            policy: TeamPolicy::default(),
        }
        .compile([supervisor, researcher])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("delegate"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let results = team.run(context).await.unwrap().collect::<Vec<_>>().await;
        let events = results.into_iter().collect::<Result<Vec<_>>>().unwrap();
        assert!(events.iter().any(|event| event.author == "researcher"));
        assert!(events.iter().all(|event| event.actions.transfer_to_agent.is_none()));
        let active_receipt = events
            .iter()
            .filter_map(|event| event.actions.state_delta.get(TEAM_EXECUTION_STATE_KEY))
            .filter_map(|value| serde_json::from_value::<TeamExecutionSnapshot>(value.clone()).ok())
            .find(|snapshot| {
                snapshot
                    .edges
                    .first()
                    .is_some_and(|edge| edge.status == TeamExecutionStatus::Running)
            })
            .expect("delegate start should emit a durable receipt before child execution");
        assert_eq!(active_receipt.edges[0].status, TeamExecutionStatus::Running);
        assert!(events.iter().any(|event| {
            event.author == "supervisor"
                && event
                    .content()
                    .into_iter()
                    .flat_map(|content| &content.parts)
                    .any(|part| part.text() == Some("supervisor final"))
        }));
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn nested_handoff_inside_delegation_is_consumed_losslessly() {
        let supervisor = Arc::new(
            crate::LlmAgentBuilder::new("supervisor")
                .model(Arc::new(ScriptedModel::new(vec![
                    ScriptedModel::call("researcher"),
                    ScriptedModel::text("supervisor resumed"),
                ])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let researcher = Arc::new(
            crate::LlmAgentBuilder::new("researcher")
                .model(Arc::new(ScriptedModel::new(vec![ScriptedModel::transfer("writer")])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let writer = Arc::new(
            crate::LlmAgentBuilder::new("writer")
                .model(Arc::new(ScriptedModel::new(vec![ScriptedModel::text("writer result")])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let team = TeamSpec {
            name: "nested_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![
                TeamMemberSpec::new("supervisor"),
                TeamMemberSpec::new("researcher"),
                TeamMemberSpec::new("writer"),
            ],
            relationships: vec![
                TeamRelationship::new("supervisor", "researcher", RelationshipKind::Delegate),
                TeamRelationship::new("researcher", "writer", RelationshipKind::Handoff),
            ],
            policy: TeamPolicy::default(),
        }
        .compile([supervisor, researcher, writer])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("delegate then hand off"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team
            .run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(events.iter().all(|event| event.actions.transfer_to_agent.is_none()));
        assert!(events.iter().any(|event| event.author == "writer"));
        assert!(events.iter().any(|event| {
            event.author == "supervisor"
                && event
                    .content()
                    .into_iter()
                    .flat_map(|content| &content.parts)
                    .any(|part| part.text() == Some("supervisor resumed"))
        }));
        let snapshot = team.execution_snapshot("team-invocation").unwrap();
        assert_eq!(snapshot.usage.delegations, 1);
        assert_eq!(snapshot.usage.handoffs, 1);
        assert_eq!(snapshot.edges[1].parent_id.as_deref(), Some("delegate-1"));
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn delegate_retry_and_exact_fallback_are_enforced() {
        let supervisor = Arc::new(
            crate::LlmAgentBuilder::new("supervisor")
                .model(Arc::new(ScriptedModel::new(vec![
                    ScriptedModel::call("primary"),
                    ScriptedModel::text("supervisor final"),
                ])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let primary_runs = Arc::new(AtomicUsize::new(0));
        let backup_runs = Arc::new(AtomicUsize::new(0));
        let primary = Arc::new(FlakyAgent {
            name: "primary".to_string(),
            failures_before_success: usize::MAX,
            runs: primary_runs.clone(),
        }) as Arc<dyn Agent>;
        let backup = Arc::new(FlakyAgent {
            name: "backup".to_string(),
            failures_before_success: 0,
            runs: backup_runs.clone(),
        }) as Arc<dyn Agent>;
        let primary_edge =
            TeamRelationship::new("supervisor", "primary", RelationshipKind::Delegate).with_policy(
                RelationshipPolicy {
                    failure: RelationshipFailureStrategy::RetryThenFallback {
                        max_attempts: 2,
                        backoff_ms: 0,
                        target: "backup".to_string(),
                    },
                    ..RelationshipPolicy::default()
                },
            );
        let team = TeamSpec {
            name: "failure_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![
                TeamMemberSpec::new("supervisor"),
                TeamMemberSpec::new("primary"),
                TeamMemberSpec::new("backup"),
            ],
            relationships: vec![
                primary_edge,
                TeamRelationship::new("supervisor", "backup", RelationshipKind::Delegate),
            ],
            policy: TeamPolicy::default(),
        }
        .compile([supervisor, primary, backup])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("use a fallback"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team
            .run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(primary_runs.load(Ordering::SeqCst), 2);
        assert_eq!(backup_runs.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|event| event.author == "backup"));
        assert!(events.iter().any(|event| event.author == "supervisor"));
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn relationship_approval_interrupts_before_delegate_execution() {
        let supervisor = Arc::new(
            crate::LlmAgentBuilder::new("supervisor")
                .model(Arc::new(ScriptedModel::new(vec![ScriptedModel::call("researcher")])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let researcher_runs = Arc::new(AtomicUsize::new(0));
        let researcher = Arc::new(FlakyAgent {
            name: "researcher".to_string(),
            failures_before_success: 0,
            runs: researcher_runs.clone(),
        }) as Arc<dyn Agent>;
        let team = TeamSpec {
            name: "approval_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("researcher")],
            relationships: vec![
                TeamRelationship::new("supervisor", "researcher", RelationshipKind::Delegate)
                    .with_policy(RelationshipPolicy {
                        approval: RelationshipApprovalPolicy::Required,
                        ..RelationshipPolicy::default()
                    }),
            ],
            policy: TeamPolicy::default(),
        }
        .compile([supervisor, researcher])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("approval required"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team
            .run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(events.iter().any(|event| event.actions.tool_confirmation.is_some()));
        assert_eq!(researcher_runs.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "team-tools")]
    #[tokio::test]
    async fn output_contract_failure_can_return_to_supervisor_as_data() {
        let supervisor = Arc::new(
            crate::LlmAgentBuilder::new("supervisor")
                .model(Arc::new(ScriptedModel::new(vec![
                    ScriptedModel::call("researcher"),
                    ScriptedModel::text("handled contract failure"),
                ])))
                .build()
                .unwrap(),
        ) as Arc<dyn Agent>;
        let researcher_runs = Arc::new(AtomicUsize::new(0));
        let researcher = Arc::new(FlakyAgent {
            name: "researcher".to_string(),
            failures_before_success: 0,
            runs: researcher_runs.clone(),
        }) as Arc<dyn Agent>;
        let team = TeamSpec {
            name: "contract_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("researcher")],
            relationships: vec![
                TeamRelationship::new("supervisor", "researcher", RelationshipKind::Delegate)
                    .with_policy(RelationshipPolicy {
                        output_schema: Some(serde_json::json!({
                            "type": "object",
                            "properties": { "score": { "type": "number" } },
                            "required": ["score"]
                        })),
                        failure: RelationshipFailureStrategy::ReturnError,
                        ..RelationshipPolicy::default()
                    }),
            ],
            policy: TeamPolicy::default(),
        }
        .compile([supervisor, researcher])
        .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("validate output"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let events = team
            .run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(researcher_runs.load(Ordering::SeqCst), 1);
        assert!(events.iter().any(|event| {
            event.author == "supervisor"
                && event
                    .content()
                    .into_iter()
                    .flat_map(|content| &content.parts)
                    .any(|part| part.text() == Some("handled contract failure"))
        }));
    }

    struct RecordingLifecycleHook {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl TeamLifecycleHook for RecordingLifecycleHook {
        fn name(&self) -> &str {
            "recording"
        }

        async fn before(&self, context: &TeamLifecycleContext) -> Result<TeamLifecycleDecision> {
            self.calls
                .lock()
                .expect("lifecycle calls lock")
                .push(format!("before:{:?}", context.phase));
            Ok(TeamLifecycleDecision::Continue)
        }

        async fn after(
            &self,
            context: &TeamLifecycleContext,
            outcome: &TeamLifecycleOutcome,
        ) -> Result<()> {
            self.calls
                .lock()
                .expect("lifecycle calls lock")
                .push(format!("after:{:?}:{outcome:?}", context.phase));
            Ok(())
        }
    }

    #[tokio::test]
    async fn invokes_team_and_member_lifecycle_hooks_around_execution() {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let team = valid_spec()
            .compile_with_hooks(
                [
                    Arc::new(StubAgent("supervisor".into())) as Arc<dyn Agent>,
                    Arc::new(StubAgent("billing".into())),
                    Arc::new(StubAgent("technical".into())),
                ],
                [Arc::new(RecordingLifecycleHook { calls: calls.clone() })
                    as Arc<dyn TeamLifecycleHook>],
            )
            .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("run hooks"),
            config: RunConfig::default(),
            session: TestSession,
        });
        team.run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let calls = calls.lock().expect("lifecycle calls lock");
        assert_eq!(calls.first().map(String::as_str), Some("before:Team"));
        assert!(calls.iter().any(|call| call == "before:Member"));
        assert!(calls.iter().any(|call| call.starts_with("after:Member:Succeeded")));
        assert!(calls.iter().any(|call| call.starts_with("after:Team:Succeeded")));
    }

    struct DenyRelationshipHook;

    #[async_trait]
    impl TeamLifecycleHook for DenyRelationshipHook {
        fn name(&self) -> &str {
            "deny-relationship"
        }

        async fn before(&self, context: &TeamLifecycleContext) -> Result<TeamLifecycleDecision> {
            if context.phase == TeamLifecyclePhase::Relationship {
                Ok(TeamLifecycleDecision::Terminate {
                    reason: "operator policy denied transition".to_string(),
                })
            } else {
                Ok(TeamLifecycleDecision::Continue)
            }
        }
    }

    #[tokio::test]
    async fn relationship_hook_denies_handoff_before_target_runs() {
        let supervisor_runs = Arc::new(AtomicUsize::new(0));
        let specialist_runs = Arc::new(AtomicUsize::new(0));
        let spec = TeamSpec {
            name: "governed_handoff".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("specialist")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "specialist",
                RelationshipKind::Handoff,
            )],
            policy: TeamPolicy::default(),
        };
        let team = spec
            .compile_with_hooks(
                [
                    Arc::new(EmittingAgent {
                        name: "supervisor".to_string(),
                        target: Some("specialist".to_string()),
                        runs: supervisor_runs.clone(),
                    }) as Arc<dyn Agent>,
                    Arc::new(EmittingAgent {
                        name: "specialist".to_string(),
                        target: None,
                        runs: specialist_runs.clone(),
                    }),
                ],
                [Arc::new(DenyRelationshipHook) as Arc<dyn TeamLifecycleHook>],
            )
            .unwrap();
        let context = Arc::new(TestContext {
            content: Content::new("user").with_text("route"),
            config: RunConfig::default(),
            session: TestSession,
        });
        let error = team
            .run(context)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find_map(std::result::Result::err)
            .expect("relationship hook should deny handoff");
        assert_eq!(error.code, "agent.team.policy_denied");
        assert_eq!(supervisor_runs.load(Ordering::SeqCst), 1);
        assert_eq!(specialist_runs.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "team-tools")]
    struct LegacyRuntimeToolBlindAgent;

    #[cfg(feature = "team-tools")]
    #[async_trait]
    impl Agent for LegacyRuntimeToolBlindAgent {
        fn name(&self) -> &str {
            "supervisor"
        }

        fn description(&self) -> &str {
            "does not consume runtime tools"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[cfg(feature = "team-tools")]
    #[test]
    fn compile_rejects_delegate_source_without_runtime_tool_capability() {
        let spec = TeamSpec {
            name: "capability_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("worker")],
            relationships: vec![TeamRelationship::new(
                "supervisor",
                "worker",
                RelationshipKind::Delegate,
            )],
            policy: TeamPolicy::default(),
        };
        assert!(matches!(
            spec.compile([
                Arc::new(LegacyRuntimeToolBlindAgent) as Arc<dyn Agent>,
                Arc::new(StubAgent("worker".into())),
            ]),
            Err(TeamError::UnsupportedAgentCapability { capability: "runtimeTools", .. })
        ));
    }

    #[test]
    fn validates_replay_and_reports_topology_coverage() {
        let team = valid_spec()
            .compile([
                Arc::new(StubAgent("supervisor".into())) as Arc<dyn Agent>,
                Arc::new(StubAgent("billing".into())),
                Arc::new(StubAgent("technical".into())),
            ])
            .unwrap();
        team.runtime
            .start_edge(
                "evaluation",
                TeamEdgeStart {
                    execution_id: Some("edge-1".to_string()),
                    parent_id: None,
                    from: "supervisor",
                    to: "billing",
                    kind: RelationshipKind::Handoff,
                    attempt: 1,
                },
            )
            .unwrap();
        team.runtime.finish_edge("evaluation", "edge-1", None);
        let analysis = team.analyze_execution("evaluation").unwrap().unwrap();
        assert_eq!(analysis.declared_relationships, 2);
        assert_eq!(analysis.covered_relationships, 1);
        assert_eq!(analysis.coverage_basis_points, 5_000);
        assert_eq!(analysis.uncovered, ["supervisor -Handoff-> technical"]);
    }

    #[cfg(feature = "team-tools")]
    struct CheckpointAgent;

    #[cfg(feature = "team-tools")]
    #[async_trait]
    impl Agent for CheckpointAgent {
        fn name(&self) -> &str {
            "worker"
        }

        fn description(&self) -> &str {
            "checkpoint-aware worker"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        fn capabilities(&self) -> adk_core::AgentCapabilities {
            adk_core::AgentCapabilities {
                runtime_tools: false,
                handoff: true,
                relationship_confirmation: false,
                checkpoint_resume: true,
                shared_state: true,
                invocation_metadata: true,
            }
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[cfg(feature = "team-tools")]
    #[test]
    fn exposes_checkpoint_resume_plan_without_replaying_delegate() {
        let spec = TeamSpec {
            name: "durable_team".to_string(),
            description: String::new(),
            coordinator: "supervisor".to_string(),
            members: vec![TeamMemberSpec::new("supervisor"), TeamMemberSpec::new("worker")],
            relationships: vec![
                TeamRelationship::new("supervisor", "worker", RelationshipKind::Delegate)
                    .with_policy(RelationshipPolicy {
                        resume: TeamResumePolicy::RequireCheckpoint {
                            token_state_key: "temp:worker_checkpoint".to_string(),
                        },
                        ..RelationshipPolicy::default()
                    }),
            ],
            policy: TeamPolicy::default(),
        };
        let team = spec
            .compile([
                Arc::new(StubAgent("supervisor".into())) as Arc<dyn Agent>,
                Arc::new(CheckpointAgent),
            ])
            .unwrap();
        team.runtime
            .start_edge(
                "durable-run",
                TeamEdgeStart {
                    execution_id: Some("delegate-edge".to_string()),
                    parent_id: None,
                    from: "supervisor",
                    to: "worker",
                    kind: RelationshipKind::Delegate,
                    attempt: 1,
                },
            )
            .unwrap();
        assert_eq!(
            team.resume_plan("durable-run"),
            Some(TeamResumePlan::DelegateCheckpoint {
                edge_id: "delegate-edge".to_string(),
                target: "worker".to_string(),
                token_state_key: "temp:worker_checkpoint".to_string(),
            })
        );
    }
}
