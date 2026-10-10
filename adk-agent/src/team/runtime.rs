use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adk_core::{BudgetTracker, BudgetUsage, Event, Part, Result, RunBudget};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    RelationshipKind, TeamBudget, TeamError, TeamLifecycleContext, TeamLifecycleDecision,
    TeamLifecycleHook, TeamLifecycleOutcome, TeamRuntimeError, TeamTerminationPolicy,
    lifecycle::TeamLifecycleManager,
};

/// Session state key containing the latest serializable team execution receipt.
pub const TEAM_EXECUTION_STATE_KEY: &str = "__adk_team_execution_v1";
/// Event metadata key containing the stable root team invocation identifier.
pub const TEAM_ROOT_INVOCATION_KEY: &str = "adk.team.root_invocation_id";
/// Event metadata key containing the causal relationship execution identifier.
pub const TEAM_EDGE_ID_KEY: &str = "adk.team.edge_id";

/// Aggregate resource usage recorded for a team invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamExecutionUsage {
    /// Non-partial events emitted by all members and nested delegates.
    pub events: u64,
    /// Model calls made by members and nested delegates.
    pub model_requests: u64,
    /// Tool calls dispatched by members and nested delegates.
    pub tool_calls: u64,
    /// Total reported input and output tokens.
    pub tokens: u64,
    /// Cost in millionths of a US dollar, from the providers' priced usage.
    pub cost_microusd: u64,
    /// Delegation relationship executions.
    pub delegations: u64,
    /// Handoff relationship executions.
    pub handoffs: u64,
}

/// Lifecycle state of a team or relationship execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TeamExecutionStatus {
    /// Execution is active.
    Running,
    /// Execution reached a normal terminal response.
    Completed,
    /// A configured clean termination condition matched.
    Terminated,
    /// A member or relationship failed.
    Failed,
    /// A hard team budget was exceeded.
    BudgetExceeded,
}

/// One causal relationship execution in a team invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamEdgeExecution {
    /// Unique execution identifier.
    pub id: String,
    /// Parent relationship execution for nested delegation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Relationship source member.
    pub from: String,
    /// Relationship target member.
    pub to: String,
    /// Relationship control-flow semantics.
    pub kind: RelationshipKind,
    /// One-based attempt number.
    pub attempt: u32,
    /// Unix timestamp in milliseconds.
    pub started_at_ms: u64,
    /// Unix timestamp in milliseconds when terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    /// Terminal or active state.
    pub status: TeamExecutionStatus,
    /// Failure message when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Serializable execution receipt for one root team invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamExecutionSnapshot {
    /// Team root name.
    pub team: String,
    /// Stable root invocation identifier.
    pub invocation_id: String,
    /// Frozen member-to-binding roster used for this execution.
    pub roster: Vec<ResolvedTeamMember>,
    /// Unix timestamp in milliseconds.
    pub started_at_ms: u64,
    /// Most recent update timestamp in milliseconds.
    pub updated_at_ms: u64,
    /// Current execution state.
    pub status: TeamExecutionStatus,
    /// Aggregate resource usage.
    pub usage: TeamExecutionUsage,
    /// Causal relationship executions in start order.
    pub edges: Vec<TeamEdgeExecution>,
    /// Reason for termination or failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Immutable member binding captured when a team is compiled or discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTeamMember {
    /// Portable member name in [`super::TeamSpec`].
    pub member: String,
    /// Concrete registry or agent binding identifier.
    pub binding: String,
    /// Capabilities frozen at resolution time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Frozen provider or semantic version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Frozen immutable content/configuration digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Frozen trust labels used during resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_labels: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum EventDisposition {
    Continue,
    Terminate,
}

pub(crate) struct TeamEdgeStart<'a> {
    pub(crate) execution_id: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) from: &'a str,
    pub(crate) to: &'a str,
    pub(crate) kind: RelationshipKind,
    pub(crate) attempt: u32,
}

/// Finished invocations whose receipts stay readable through
/// `execution_snapshot`; older finished receipts are evicted. Every receipt is
/// also persisted to session state under [`TEAM_EXECUTION_STATE_KEY`].
pub(crate) const RETAINED_FINISHED_INVOCATIONS: usize = 64;

/// Runtime state of one root team invocation.
struct TeamInvocation {
    snapshot: Mutex<TeamExecutionSnapshot>,
    /// Shared counters for the team budget, chained to the run budget.
    tracker: Mutex<Option<Arc<BudgetTracker>>>,
    /// Team streams of this invocation currently alive.
    active_streams: Mutex<usize>,
}

pub(crate) struct TeamRuntimeRegistry {
    team: String,
    roster: Vec<ResolvedTeamMember>,
    budget: TeamBudget,
    termination: TeamTerminationPolicy,
    lifecycle: TeamLifecycleManager,
    invocations: RwLock<HashMap<String, Arc<TeamInvocation>>>,
    finished: Mutex<VecDeque<String>>,
}

impl TeamRuntimeRegistry {
    pub(crate) fn team_name(&self) -> &str {
        &self.team
    }

    pub(crate) fn new(
        team: String,
        roster: Vec<ResolvedTeamMember>,
        budget: TeamBudget,
        termination: TeamTerminationPolicy,
        hooks: Vec<Arc<dyn TeamLifecycleHook>>,
    ) -> Self {
        Self {
            team,
            roster,
            budget,
            termination,
            lifecycle: TeamLifecycleManager::new(hooks),
            invocations: RwLock::new(HashMap::new()),
            finished: Mutex::new(VecDeque::new()),
        }
    }

    pub(crate) async fn before_lifecycle(
        &self,
        context: &TeamLifecycleContext,
    ) -> Result<TeamLifecycleDecision> {
        self.lifecycle.before(context).await
    }

    pub(crate) async fn after_lifecycle(
        &self,
        context: &TeamLifecycleContext,
        outcome: &TeamLifecycleOutcome,
    ) -> Result<()> {
        self.lifecycle.after(context, outcome).await
    }

    pub(crate) fn snapshot(&self, invocation_id: &str) -> Option<TeamExecutionSnapshot> {
        let ledger = self
            .invocations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(invocation_id)
            .cloned()?;
        Some(ledger.synced_snapshot())
    }

    pub(crate) fn snapshots(&self) -> Vec<TeamExecutionSnapshot> {
        self.invocations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .map(|ledger| ledger.synced_snapshot())
            .collect()
    }

    /// Returns the invocation's team budget tracker, creating it on first use.
    ///
    /// `run_tracker` is the run budget the team runs under; the team tracker
    /// counts against it, so members stop at whichever limit is reached first.
    pub(crate) fn budget_tracker(
        &self,
        invocation_id: &str,
        run_tracker: Option<Arc<BudgetTracker>>,
    ) -> Arc<BudgetTracker> {
        let ledger = self.ledger(invocation_id);
        self.tracker_for(&ledger, run_tracker)
    }

    fn tracker_for(
        &self,
        ledger: &TeamInvocation,
        run_tracker: Option<Arc<BudgetTracker>>,
    ) -> Arc<BudgetTracker> {
        let mut slot = ledger.tracker.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(tracker) = slot.as_ref() {
            return tracker.clone();
        }
        let snapshot = ledger.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        // A restored receipt continues from its recorded usage and start time.
        let usage = BudgetUsage {
            model_calls: snapshot.usage.model_requests,
            total_tokens: snapshot.usage.tokens,
            cost_micro_usd: snapshot.usage.cost_microusd,
            tool_calls: snapshot.usage.tool_calls,
            elapsed: Duration::from_millis(now_ms().saturating_sub(snapshot.started_at_ms)),
        };
        let tracker = Arc::new(BudgetTracker::resume(self.budget.run_budget(), usage, run_tracker));
        *slot = Some(tracker.clone());
        tracker
    }

    /// Marks one team stream of the invocation as alive.
    pub(crate) fn enter(self: &Arc<Self>, invocation_id: &str) -> ActiveTeamStream {
        let ledger = self.ledger(invocation_id);
        *ledger.active_streams.lock().unwrap_or_else(|error| error.into_inner()) += 1;
        ActiveTeamStream { runtime: self.clone(), invocation_id: invocation_id.to_string() }
    }

    /// Ends one team stream; the invocation finishes when none remain and no
    /// handoff is in flight.
    fn leave(&self, invocation_id: &str) {
        let Some(ledger) = self
            .invocations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(invocation_id)
            .cloned()
        else {
            return;
        };
        let mut active = ledger.active_streams.lock().unwrap_or_else(|error| error.into_inner());
        *active = active.saturating_sub(1);
        if *active > 0 || ledger.has_running_edges() {
            return;
        }
        drop(active);
        // Counters move into the receipt; a later resume builds a fresh tracker from it.
        ledger.synced_snapshot();
        ledger.tracker.lock().unwrap_or_else(|error| error.into_inner()).take();
        let mut finished = self.finished.lock().unwrap_or_else(|error| error.into_inner());
        if !finished.iter().any(|id| id == invocation_id) {
            finished.push_back(invocation_id.to_string());
        }
        while finished.len() > RETAINED_FINISHED_INVOCATIONS {
            let Some(evicted) = finished.pop_front() else {
                break;
            };
            let mut invocations =
                self.invocations.write().unwrap_or_else(|error| error.into_inner());
            if invocations.get(&evicted).is_some_and(|ledger| ledger.is_idle()) {
                invocations.remove(&evicted);
            }
        }
    }

    pub(crate) fn restore(
        &self,
        snapshot: TeamExecutionSnapshot,
    ) -> std::result::Result<(), TeamError> {
        if snapshot.team != self.team {
            return Err(TeamError::IncompatibleExecutionSnapshot(format!(
                "snapshot belongs to team '{}', expected '{}'",
                snapshot.team, self.team
            )));
        }
        if snapshot.roster != self.roster {
            return Err(TeamError::IncompatibleExecutionSnapshot(
                "the frozen member roster does not match".to_string(),
            ));
        }
        if snapshot.invocation_id.trim().is_empty() {
            return Err(TeamError::IncompatibleExecutionSnapshot(
                "invocationId must not be empty".to_string(),
            ));
        }
        self.invocations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .insert(snapshot.invocation_id.clone(), Arc::new(TeamInvocation::new(snapshot)));
        Ok(())
    }

    pub(crate) fn resume_handoff_target(&self, invocation_id: &str) -> Result<Option<String>> {
        let Some(snapshot) = self.snapshot(invocation_id) else {
            return Ok(None);
        };
        let Some(active) =
            snapshot.edges.iter().rev().find(|edge| edge.status == TeamExecutionStatus::Running)
        else {
            return Ok(None);
        };
        match active.kind {
            RelationshipKind::Handoff => Ok(Some(active.to.clone())),
            RelationshipKind::Delegate => Err(TeamRuntimeError::UnsafeResume(format!(
                "cannot replay unresolved delegation '{}' from '{}' to '{}'; inspect CompiledTeam::resume_plan and use a checkpoint-aware durable host",
                active.id, active.from, active.to
            ))
            .into()),
        }
    }

    fn ledger(&self, invocation_id: &str) -> Arc<TeamInvocation> {
        if let Some(ledger) = self
            .invocations
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(invocation_id)
            .cloned()
        {
            return ledger;
        }
        let now = now_ms();
        let ledger = Arc::new(TeamInvocation::new(TeamExecutionSnapshot {
            team: self.team.clone(),
            invocation_id: invocation_id.to_string(),
            roster: self.roster.clone(),
            started_at_ms: now,
            updated_at_ms: now,
            status: TeamExecutionStatus::Running,
            usage: TeamExecutionUsage::default(),
            edges: Vec::new(),
            reason: None,
        }));
        self.invocations
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .entry(invocation_id.to_string())
            .or_insert_with(|| ledger.clone())
            .clone()
    }

    pub(crate) fn check_budget(&self, invocation_id: &str) -> Result<()> {
        let ledger = self.ledger(invocation_id);
        let tracker = self.tracker_for(&ledger, None);
        let mut snapshot = ledger.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        sync_usage(&mut snapshot, &tracker);
        if let Err(exceeded) = tracker.check() {
            snapshot.status = TeamExecutionStatus::BudgetExceeded;
            snapshot.reason = Some(exceeded.to_string());
            snapshot.updated_at_ms = now_ms();
            return Err(exceeded.into());
        }
        if let Some(reason) = self.budget_violation(&snapshot) {
            snapshot.status = TeamExecutionStatus::BudgetExceeded;
            snapshot.reason = Some(reason.clone());
            snapshot.updated_at_ms = now_ms();
            return Err(TeamRuntimeError::BudgetExceeded(reason).into());
        }
        Ok(())
    }

    pub(crate) fn start_edge(
        &self,
        invocation_id: &str,
        start: TeamEdgeStart<'_>,
    ) -> Result<String> {
        self.check_budget(invocation_id)?;
        let ledger = self.ledger(invocation_id);
        let mut snapshot = ledger.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        snapshot.status = TeamExecutionStatus::Running;
        snapshot.reason = None;
        match start.kind {
            RelationshipKind::Delegate => snapshot.usage.delegations += 1,
            RelationshipKind::Handoff => snapshot.usage.handoffs += 1,
        }
        let id = start.execution_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        snapshot.edges.push(TeamEdgeExecution {
            id: id.clone(),
            parent_id: start.parent_id,
            from: start.from.to_string(),
            to: start.to.to_string(),
            kind: start.kind,
            attempt: start.attempt,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            status: TeamExecutionStatus::Running,
            error: None,
        });
        snapshot.updated_at_ms = now_ms();
        if let Some(reason) = self.budget_violation(&snapshot) {
            snapshot.status = TeamExecutionStatus::BudgetExceeded;
            snapshot.reason = Some(reason.clone());
            return Err(TeamRuntimeError::BudgetExceeded(reason).into());
        }
        Ok(id)
    }

    pub(crate) fn finish_edge(&self, invocation_id: &str, id: &str, error: Option<String>) {
        let ledger = self.ledger(invocation_id);
        let mut snapshot = ledger.snapshot.lock().unwrap_or_else(|failure| failure.into_inner());
        if let Some(edge) = snapshot.edges.iter_mut().find(|edge| edge.id == id) {
            edge.finished_at_ms = Some(now_ms());
            edge.status = if error.is_some() {
                TeamExecutionStatus::Failed
            } else {
                TeamExecutionStatus::Completed
            };
            edge.error = error;
        }
        snapshot.updated_at_ms = now_ms();
    }

    pub(crate) fn record_event(
        &self,
        invocation_id: &str,
        edge_id: Option<&str>,
        event: &mut Event,
    ) -> Result<EventDisposition> {
        let ledger = self.ledger(invocation_id);
        let tracker = self.tracker_for(&ledger, None);
        let mut snapshot = ledger.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        let already_recorded = event
            .provider_metadata
            .get(TEAM_ROOT_INVOCATION_KEY)
            .is_some_and(|root| root == invocation_id);
        // Partial streaming chunks repeat their usage and are never counted.
        if !already_recorded && !event.llm_response.partial {
            snapshot.usage.events += 1;
        }
        // Model calls metered by their call site are already in the tracker; this
        // counts usage from members that call models directly and audits the limits.
        let tracker_violation =
            if already_recorded { tracker.exceeded() } else { tracker.record_event(event).err() };
        sync_usage(&mut snapshot, &tracker);
        snapshot.updated_at_ms = now_ms();
        event
            .provider_metadata
            .insert(TEAM_ROOT_INVOCATION_KEY.to_string(), invocation_id.to_string());
        if let Some(edge_id) = edge_id {
            event.provider_metadata.insert(TEAM_EDGE_ID_KEY.to_string(), edge_id.to_string());
        }

        // Tracker violations keep their `budget.*` code and limit details.
        if let Some(exceeded) = tracker_violation {
            snapshot.status = TeamExecutionStatus::BudgetExceeded;
            snapshot.reason = Some(exceeded.to_string());
            persist_snapshot_if_absent(event, &snapshot);
            return Err(exceeded.into());
        }
        if let Some(reason) = self.budget_violation(&snapshot) {
            snapshot.status = TeamExecutionStatus::BudgetExceeded;
            snapshot.reason = Some(reason.clone());
            persist_snapshot_if_absent(event, &snapshot);
            return Err(TeamRuntimeError::BudgetExceeded(reason).into());
        }

        let termination = self.termination_reason(event);
        if let Some(reason) = &termination {
            snapshot.status = TeamExecutionStatus::Terminated;
            snapshot.reason = Some(reason.clone());
        } else if event.is_final_response()
            && edge_id.is_none_or(|id| {
                snapshot
                    .edges
                    .iter()
                    .find(|edge| edge.id == id)
                    .is_none_or(|edge| edge.kind != RelationshipKind::Delegate)
            })
        {
            snapshot.status = TeamExecutionStatus::Completed;
        }
        persist_snapshot_if_absent(event, &snapshot);
        Ok(termination.map_or(EventDisposition::Continue, |_| EventDisposition::Terminate))
    }

    pub(crate) fn fail(&self, invocation_id: &str, error: &adk_core::AdkError) {
        let ledger = self.ledger(invocation_id);
        let mut snapshot = ledger.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        snapshot.status = if error.category == adk_core::ErrorCategory::ResourceExhausted {
            TeamExecutionStatus::BudgetExceeded
        } else {
            TeamExecutionStatus::Failed
        };
        snapshot.reason = Some(error.to_string());
        snapshot.updated_at_ms = now_ms();
    }

    fn termination_reason(&self, event: &Event) -> Option<String> {
        if self.termination.stop_on_escalation && event.actions.escalate {
            return Some("team terminated on escalation".to_string());
        }
        if event.is_final_response()
            && self.termination.final_authors.iter().any(|author| author == &event.author)
        {
            return Some(format!("team terminated after final response from '{}'", event.author));
        }
        for marker in &self.termination.text_markers {
            let matched = event
                .content()
                .into_iter()
                .flat_map(|content| &content.parts)
                .any(|part| matches!(part, Part::Text { text } if text.contains(marker)));
            if matched {
                return Some(format!("team termination marker matched: {marker}"));
            }
        }
        None
    }

    /// Limits the budget tracker does not own: events, delegations and handoffs.
    fn budget_violation(&self, snapshot: &TeamExecutionSnapshot) -> Option<String> {
        let usage = &snapshot.usage;
        let limits = [
            ("events", usage.events, self.budget.max_events),
            ("delegations", usage.delegations, self.budget.max_delegations),
            ("handoffs", usage.handoffs, self.budget.max_handoffs),
        ];
        let (name, used, limit) =
            limits.into_iter().find(|(_, used, limit)| limit.is_some_and(|max| *used > max))?;
        Some(format!(
            "team budget exceeded for {name}: used {used}, maximum {}",
            limit.unwrap_or_default()
        ))
    }
}

impl TeamInvocation {
    fn new(snapshot: TeamExecutionSnapshot) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
            tracker: Mutex::new(None),
            active_streams: Mutex::new(0),
        }
    }

    /// The receipt with its usage refreshed from the live tracker.
    fn synced_snapshot(&self) -> TeamExecutionSnapshot {
        let tracker = self.tracker.lock().unwrap_or_else(|error| error.into_inner()).clone();
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(tracker) = tracker {
            sync_usage(&mut snapshot, &tracker);
        }
        snapshot.clone()
    }

    fn has_running_edges(&self) -> bool {
        self.snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .edges
            .iter()
            .any(|edge| edge.status == TeamExecutionStatus::Running)
    }

    fn is_idle(&self) -> bool {
        *self.active_streams.lock().unwrap_or_else(|error| error.into_inner()) == 0
            && !self.has_running_edges()
    }
}

/// Keeps a team invocation active while a team stream is alive.
pub(crate) struct ActiveTeamStream {
    runtime: Arc<TeamRuntimeRegistry>,
    invocation_id: String,
}

impl Drop for ActiveTeamStream {
    fn drop(&mut self) {
        self.runtime.leave(&self.invocation_id);
    }
}

impl TeamBudget {
    /// The limits the budget tracker enforces before each model and tool call.
    pub(crate) fn run_budget(&self) -> RunBudget {
        let mut budget = RunBudget::new();
        if let Some(max) = self.max_model_requests {
            budget = budget.max_model_calls(max);
        }
        if let Some(max) = self.max_tokens {
            budget = budget.max_total_tokens(max);
        }
        if let Some(max) = self.max_cost_microusd {
            budget = budget.max_cost_micro_usd(max);
        }
        if let Some(max) = self.max_tool_calls {
            budget = budget.max_tool_calls(max);
        }
        if let Some(max) = self.max_wall_time_ms {
            budget = budget.max_wall_time(Duration::from_millis(max));
        }
        if self.allow_unpriced_models {
            budget = budget.allow_unpriced_models();
        }
        budget
    }
}

fn sync_usage(snapshot: &mut TeamExecutionSnapshot, tracker: &BudgetTracker) {
    let usage = tracker.usage();
    snapshot.usage.model_requests = usage.model_calls;
    snapshot.usage.tool_calls = usage.tool_calls;
    snapshot.usage.tokens = usage.total_tokens;
    snapshot.usage.cost_microusd = usage.cost_micro_usd;
}

fn persist_snapshot(event: &mut Event, snapshot: &TeamExecutionSnapshot) {
    if let Ok(value) = serde_json::to_value(snapshot) {
        event.actions.state_delta.insert(TEAM_EXECUTION_STATE_KEY.to_string(), value);
    }
}

fn persist_snapshot_if_absent(event: &mut Event, snapshot: &TeamExecutionSnapshot) {
    if !event.actions.state_delta.contains_key(TEAM_EXECUTION_STATE_KEY) {
        persist_snapshot(event, snapshot);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;

    fn registry(budget: TeamBudget, termination: TeamTerminationPolicy) -> TeamRuntimeRegistry {
        TeamRuntimeRegistry::new(
            "portable_team".to_string(),
            vec![ResolvedTeamMember {
                member: "root".to_string(),
                binding: "root-v1".to_string(),
                capabilities: vec!["route".to_string()],
                version: None,
                digest: None,
                trust_labels: Vec::new(),
            }],
            budget,
            termination,
            Vec::new(),
        )
    }

    #[test]
    fn enforces_aggregate_event_budget_and_persists_receipt() {
        let runtime = registry(
            TeamBudget { max_events: Some(1), ..TeamBudget::default() },
            TeamTerminationPolicy::default(),
        );
        let mut first = Event::new("invocation");
        assert!(matches!(
            runtime.record_event("invocation", None, &mut first),
            Ok(EventDisposition::Continue)
        ));
        assert!(first.actions.state_delta.contains_key(TEAM_EXECUTION_STATE_KEY));

        let mut second = Event::new("invocation");
        let error = runtime.record_event("invocation", None, &mut second).unwrap_err();
        assert!(error.to_string().contains("events"));
        assert_eq!(error.code, "agent.team.budget_exceeded");
        assert_eq!(
            runtime.snapshot("invocation").unwrap().status,
            TeamExecutionStatus::BudgetExceeded
        );
    }

    #[test]
    fn records_causal_edges_and_terminates_on_marker() {
        let runtime = registry(
            TeamBudget::default(),
            TeamTerminationPolicy {
                text_markers: vec!["APPROVED".to_string()],
                ..TeamTerminationPolicy::default()
            },
        );
        let parent = runtime
            .start_edge(
                "invocation",
                TeamEdgeStart {
                    execution_id: Some("edge-1".to_string()),
                    parent_id: None,
                    from: "root",
                    to: "a",
                    kind: RelationshipKind::Delegate,
                    attempt: 1,
                },
            )
            .unwrap();
        let child = runtime
            .start_edge(
                "invocation",
                TeamEdgeStart {
                    execution_id: Some("edge-2".to_string()),
                    parent_id: Some(parent.clone()),
                    from: "a",
                    to: "b",
                    kind: RelationshipKind::Handoff,
                    attempt: 1,
                },
            )
            .unwrap();
        runtime.finish_edge("invocation", &child, None);
        runtime.finish_edge("invocation", &parent, None);
        let mut event = Event::new("invocation");
        event.author = "b".to_string();
        event.llm_response.content = Some(Content::new("model").with_text("APPROVED"));
        assert!(matches!(
            runtime.record_event("invocation", Some(&child), &mut event),
            Ok(EventDisposition::Terminate)
        ));
        let snapshot = runtime.snapshot("invocation").unwrap();
        assert_eq!(snapshot.status, TeamExecutionStatus::Terminated);
        assert_eq!(snapshot.edges[1].parent_id.as_deref(), Some("edge-1"));
        assert_eq!(event.provider_metadata.get(TEAM_EDGE_ID_KEY), Some(&child));
    }

    #[test]
    fn restores_only_matching_frozen_rosters() {
        let source = registry(TeamBudget::default(), TeamTerminationPolicy::default());
        source.check_budget("resume-me").unwrap();
        let snapshot = source.snapshot("resume-me").unwrap();

        let restored = registry(TeamBudget::default(), TeamTerminationPolicy::default());
        restored.restore(snapshot.clone()).unwrap();
        assert_eq!(restored.snapshot("resume-me"), Some(snapshot.clone()));

        let other = TeamRuntimeRegistry::new(
            "portable_team".to_string(),
            vec![ResolvedTeamMember {
                member: "root".to_string(),
                binding: "root-v2".to_string(),
                capabilities: vec!["route".to_string()],
                version: None,
                digest: None,
                trust_labels: Vec::new(),
            }],
            TeamBudget::default(),
            TeamTerminationPolicy::default(),
            Vec::new(),
        );
        assert!(matches!(
            other.restore(snapshot),
            Err(TeamError::IncompatibleExecutionSnapshot(_))
        ));
    }

    #[test]
    fn resumes_handoffs_but_refuses_unsafe_delegate_replay() {
        let handoff = registry(TeamBudget::default(), TeamTerminationPolicy::default());
        handoff
            .start_edge(
                "handoff-run",
                TeamEdgeStart {
                    execution_id: Some("handoff-edge".to_string()),
                    parent_id: None,
                    from: "root",
                    to: "specialist",
                    kind: RelationshipKind::Handoff,
                    attempt: 1,
                },
            )
            .unwrap();
        assert_eq!(
            handoff.resume_handoff_target("handoff-run").unwrap().as_deref(),
            Some("specialist")
        );

        let delegate = registry(TeamBudget::default(), TeamTerminationPolicy::default());
        delegate
            .start_edge(
                "delegate-run",
                TeamEdgeStart {
                    execution_id: Some("delegate-edge".to_string()),
                    parent_id: None,
                    from: "root",
                    to: "worker",
                    kind: RelationshipKind::Delegate,
                    attempt: 1,
                },
            )
            .unwrap();
        let error = delegate.resume_handoff_target("delegate-run").unwrap_err();
        assert!(error.to_string().contains("cannot replay unresolved delegation"));
        assert_eq!(error.code, "agent.team.resume_unsafe");
    }
}
