//! The glue between the harness and the Agent Kernel (spec 3.2 §10 HK1, HK3; feature
//! `kernel`).
//!
//! `adk-kernel` hosts any `Arc<Runner>` and reads no harness type (spec 3.2 §6 D-1); this
//! module gives it the harness:
//!
//! | Item | Spec |
//! |---|---|
//! | [`Harness::kernel`]: a `KernelBuilder` with the harness runner as the app's and the `harness.goal` targets | §10 HK1 |
//! | `PartitionMode` → [`HarnessMode`], one to one | §10 HK1, §7.8 |
//! | Goal progress, the loop guard's verdict and each session's usage for the kernel's process view | §7.3, §7.10, §10 HK3 |
//! | Parking a goal that stopped making progress, `Waiting` and suspended manually, with `harness.goal.stalled` | §7.3, §10 HK3 |
//! | Decisions on the harness's approval service waking the kernel's approval sleepers | §7.4 |
//! | `propose_followup`, class `Todo`: an agent's follow-up as a `Proposed` agenda item | §7.5 |
//! | The standing rules the kernel's daily review checks for renewal | §9.1 |

use std::sync::{Arc, OnceLock, RwLock};

use adk_core::{
    AdkError, ApprovalCall, ApprovalDecision, ApprovalDelegation, ApprovalEventKind,
    ApprovalNotification, ApprovalNotifier, ApprovalRequest, ApprovalService, ApprovalState,
    ApprovalStore, ApprovalTick, ErrorCategory, ErrorComponent, InboxPage, InboxQuery, Result,
    RunCheckpointRef, RunRecord, SubjectRef, Tool, ToolContext, ToolEffect,
};
use adk_kernel::{
    AgendaHandle, AgendaItem, AgendaOrigin, ApprovalWakeSource, GoalProgress, KernelBuilder,
    KernelConfig, PartitionMode, PlanRef, ProcessHandle, ProgressSource, ProposalOutcome, RuleAge,
    RunUsage, StandingRuleSource, Wake,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::approvals::StandingRuleStore;
use crate::goal::{GoalBook, GoalParker, GoalStore};
use crate::harness::Harness;
use crate::mode::{HarnessMode, ModeRecord, ModeSource, ModeStore};
use crate::policy::has_harness_layer;
use crate::tools::PlanStep;

impl From<PartitionMode> for HarnessMode {
    /// A partition's mode is the harness mode of the same name (spec 3.2 §10 HK1); a mode a
    /// later kernel adds maps to the strictest, `ReadOnly`.
    fn from(mode: PartitionMode) -> Self {
        match mode {
            PartitionMode::ReadOnly => HarnessMode::ReadOnly,
            PartitionMode::Plan => HarnessMode::Plan,
            PartitionMode::Ask => HarnessMode::Ask,
            PartitionMode::Auto => HarnessMode::Auto,
            _ => HarnessMode::ReadOnly,
        }
    }
}

impl Harness {
    /// A kernel builder that hosts this harness (spec 3.2 §10 HK1): the harness runner as
    /// the app's target (`adk-rust` in the CLI) and as `harness.goal`, with the harness's
    /// session service, approval service, governance control, audit trail and spend ledger,
    /// its run store when it has one, and a progress source for goal progress, the loop
    /// guard's verdict, each session's usage and the parking of a stalled goal.
    ///
    /// The caller adds the process store, the `KernelConfig` and the control plane. The
    /// governance control is set with `KernelBuilder::governance`, so a later `config(..)`
    /// keeps it. Decisions made through the harness's approval service wake the kernel's
    /// `Wake::Approval` sleepers; a caller that also decides through the service it passed
    /// to the harness builder hands that service out as
    /// `harness.agent().runtime().approval_service()` instead. Goals land on the kernel's
    /// `background` queue when the harness was given
    /// `with_goal_queue(adk_kernel::QueueClass::BACKGROUND)`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let kernel = harness
    ///     .with_goal_queue(adk_kernel::QueueClass::BACKGROUND)
    ///     .kernel()
    ///     .process_store(Arc::new(adk_kernel::InMemoryProcessStore::new()))
    ///     .build()?;
    /// kernel.start().await?;
    /// ```
    pub fn kernel(&self) -> KernelBuilder {
        let runtime = self.agent().runtime();
        let (target, runner) = self.goal_target();
        let progress = Arc::new(HarnessProgress {
            goals: runtime.goal_store().clone(),
            book: runtime.goals.until.book().clone(),
            #[cfg(feature = "observe")]
            trails: runtime.run_trails.clone(),
            processes: RwLock::new(None),
        });
        runtime.goals.until.book().set_parker(progress.clone());
        let source = runtime.kernel_approvals.bind();
        let mut builder = adk_kernel::Kernel::builder()
            .config(KernelConfig::default().with_app_name(self.app_name()))
            .agent(self.app_name(), runner.clone())
            .agent(target, runner.clone())
            .session_service(self.sessions().clone())
            .governance(runner.governance().clone())
            .progress_source(progress)
            .wake_source(source)
            .agenda_handle(runtime.kernel_agenda.clone())
            .standing_rules(Arc::new(HarnessRules(runtime.standing_rules.clone())));
        if let Some(service) = runtime.approval_service() {
            builder = builder.approval_service(service.clone());
        }
        if let Some(trail) = runner.run_config().audit.clone().or_else(|| runtime.audit.clone()) {
            builder = builder.audit(trail);
        }
        if let Some(ledger) = runtime.spend_ledger.clone() {
            builder = builder.spend_ledger(ledger);
        }
        if let Some(runs) = self.run_store() {
            builder = builder.run_store(runs.clone());
        }
        builder
    }
}

/// The session a durable run continues, as the worker names it.
fn session_of(run: &RunRecord) -> String {
    match (&run.checkpoint, &run.session_id) {
        (
            Some(
                RunCheckpointRef::Session { session_id, .. }
                | RunCheckpointRef::AwaitingApproval { session_id, .. },
            ),
            _,
        ) => session_id.clone(),
        (_, Some(session_id)) => session_id.clone(),
        _ => format!("run-{}", run.run_id),
    }
}

/// What the harness tells the kernel about the processes it runs (spec 3.2 §7.3, §10 HK3).
struct HarnessProgress {
    goals: Arc<dyn GoalStore>,
    book: Arc<GoalBook>,
    #[cfg(feature = "observe")]
    trails: Arc<crate::observe::RunTrails>,
    processes: RwLock<Option<ProcessHandle>>,
}

#[async_trait]
impl ProgressSource for HarnessProgress {
    async fn goal_progress(&self, run: &RunRecord) -> adk_kernel::Result<Option<GoalProgress>> {
        let session = session_of(run);
        let Some(record) = self.goals.get(&run.app_name, &run.user_id, &session).await? else {
            return Ok(None);
        };
        let mut progress = GoalProgress::new(record.state.iteration, record.state.max_iterations);
        let track = self.book.track(&record.run_id);
        if let Some(track) = &track {
            let elapsed = Utc::now() - track.started_at;
            progress = progress.with_duration(elapsed.to_std().unwrap_or_default());
        }
        if let Some(exit) = record.state.last_check {
            let tail = track.and_then(|track| track.last_tail).unwrap_or_default();
            progress = progress.with_last_check(exit, tail);
        }
        Ok(Some(progress))
    }

    #[cfg(feature = "observe")]
    async fn loop_guard(&self, run: &RunRecord) -> adk_kernel::Result<Option<String>> {
        Ok(self.trails.verdict(&run.app_name, &run.user_id, &session_of(run)))
    }

    #[cfg(feature = "observe")]
    async fn usage(&self, run: &RunRecord) -> adk_kernel::Result<Option<RunUsage>> {
        Ok(self
            .trails
            .session_usage(&run.app_name, &run.user_id, &session_of(run))
            .map(|(model_calls, tool_calls, cost)| RunUsage::new(model_calls, tool_calls, cost)))
    }

    fn attach(&self, processes: ProcessHandle) {
        *self.processes.write().unwrap_or_else(|e| e.into_inner()) = Some(processes);
    }
}

#[async_trait]
impl GoalParker for HarnessProgress {
    async fn park(&self, run_id: &str) -> Result<bool> {
        let processes = self.processes.read().unwrap_or_else(|e| e.into_inner()).clone();
        match processes {
            Some(processes) => processes.suspend(run_id).await.map_err(Into::into),
            None => Ok(false),
        }
    }
}

/// The notifier the harness's approval service reports outcomes to under a kernel: a no-op
/// until [`Harness::kernel`] binds the kernel's `ApprovalWakeSource` (spec 3.2 §7.4).
#[derive(Default)]
pub(crate) struct LateApprovalNotifier {
    source: OnceLock<Arc<ApprovalWakeSource>>,
    bound: OnceLock<Arc<dyn ApprovalNotifier>>,
}

impl std::fmt::Debug for LateApprovalNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LateApprovalNotifier").field("bound", &self.bound.get().is_some()).finish()
    }
}

impl LateApprovalNotifier {
    /// The kernel's approval wake source, created and bound on first use; every kernel built
    /// from the harness shares it.
    fn bind(&self) -> Arc<ApprovalWakeSource> {
        let source = self.source.get_or_init(|| Arc::new(ApprovalWakeSource::new())).clone();
        let _ = self.bound.set(source.clone());
        source
    }

    /// Binds `notifier`, for tests that stand in for the kernel.
    #[cfg(test)]
    fn bind_to(&self, notifier: Arc<dyn ApprovalNotifier>) {
        let _ = self.bound.set(notifier);
    }

    async fn notify(&self, request: &ApprovalRequest, decision: Option<ApprovalDecision>) {
        let Some(notifier) = self.bound.get() else { return };
        let kind = match request.state {
            ApprovalState::Approved | ApprovalState::Rejected | ApprovalState::Responded => {
                ApprovalEventKind::Resolved
            }
            ApprovalState::Expired => ApprovalEventKind::Expired,
            ApprovalState::Cancelled => ApprovalEventKind::Cancelled,
            ApprovalState::Pending => return,
        };
        let notification = ApprovalNotification {
            id: format!("{}:{}", request.id, request.version),
            kind,
            request: request.clone(),
            decision,
            escalation_step: None,
            at: Utc::now(),
        };
        if let Err(error) = notifier.notify(&notification).await {
            tracing::warn!(approval.id = %request.id, error = %error, "could not tell the kernel about an approval outcome");
        }
    }
}

/// The harness's approval service under a kernel: forwards every call and reports each
/// request that reached its outcome through it to the late-bound notifier.
#[derive(Debug)]
pub(crate) struct NotifyingApprovals {
    inner: Arc<dyn ApprovalService>,
    late: Arc<LateApprovalNotifier>,
}

impl NotifyingApprovals {
    pub(crate) fn wrap(
        inner: Arc<dyn ApprovalService>,
        late: &Arc<LateApprovalNotifier>,
    ) -> Arc<dyn ApprovalService> {
        Arc::new(Self { inner, late: Arc::clone(late) })
    }
}

#[async_trait]
impl ApprovalService for NotifyingApprovals {
    async fn submit(&self, request: ApprovalRequest) -> Result<ApprovalRequest> {
        self.inner.submit(request).await
    }

    async fn submit_call(&self, call: ApprovalCall) -> Result<Option<ApprovalRequest>> {
        self.inner.submit_call(call).await
    }

    async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.inner.get(id).await
    }

    async fn inbox(&self, query: InboxQuery) -> Result<InboxPage> {
        self.inner.inbox(query).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        let request = self.inner.decide(id, expected_version, decision.clone()).await?;
        self.late.notify(&request, Some(decision)).await;
        Ok(request)
    }

    async fn delegate(&self, delegation: ApprovalDelegation) -> Result<ApprovalDelegation> {
        self.inner.delegate(delegation).await
    }

    async fn revoke_delegation(&self, id: &str) -> Result<bool> {
        self.inner.revoke_delegation(id).await
    }

    async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }

    async fn cancel(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
        let request = self.inner.cancel(id, reason).await?;
        self.late.notify(&request, None).await;
        Ok(request)
    }

    async fn tick(&self, now: DateTime<Utc>) -> Result<ApprovalTick> {
        let tick = self.inner.tick(now).await?;
        for id in &tick.expired {
            if let Ok(Some(request)) = self.inner.get(id).await {
                self.late.notify(&request, None).await;
            }
        }
        Ok(tick)
    }

    fn approval_store(&self) -> Option<Arc<dyn ApprovalStore>> {
        self.inner.approval_store()
    }
}

/// The harness's standing rules as the kernel's daily review reads them (spec 3.2 §9.1):
/// ids, owners, grant times and expiries, nothing else.
struct HarnessRules(Arc<dyn StandingRuleStore>);

#[async_trait]
impl StandingRuleSource for HarnessRules {
    async fn rules(&self, app: &str, user: &str) -> adk_kernel::Result<Vec<RuleAge>> {
        let rules = self.0.list(app, user).await?;
        Ok(rules
            .into_iter()
            .map(|rule| {
                let age = RuleAge::new(rule.id, user, rule.created_at);
                match rule.expires_at {
                    Some(at) => age.with_expiry(at),
                    None => age,
                }
            })
            .collect())
    }
}

/// The name of the tool an agent proposes follow-up work with (spec 3.2 §7.5).
pub(crate) const PROPOSE_FOLLOWUP: &str = "propose_followup";

/// The most characters of a follow-up's title.
const TITLE_MAX_CHARS: usize = 200;
/// The most characters of a follow-up's rationale.
const RATIONALE_MAX_CHARS: usize = 2_000;

/// The kernel's mode for a session in `mode`; the one-to-one inverse of
/// `From<PartitionMode> for HarnessMode`.
fn partition_mode(mode: HarnessMode) -> PartitionMode {
    match mode {
        HarnessMode::ReadOnly => PartitionMode::ReadOnly,
        HarnessMode::Plan => PartitionMode::Plan,
        HarnessMode::Ask => PartitionMode::Ask,
        HarnessMode::Auto => PartitionMode::Auto,
    }
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.followup.invalid",
        message,
    )
}

/// `propose_followup { title, rationale, when?, steps? }` (spec 3.2 §7.5): proposes work
/// for later as an agenda item in state `Proposed`, its only effect. Registered as class
/// `Todo`, so it is allowed in every mode; running the item is governed by the kernel's
/// agenda — held in ReadOnly and Plan, put to the inbox in Ask, scheduled in Auto — and by
/// its quiet hours and daily budget.
///
/// The follow-up runs in a session of its own whose mode is recorded as this session's at
/// the time of the proposal, so its process never runs above the proposing run's mode
/// (K-I7). Without a kernel hosting the harness the tool answers that none runs.
pub(crate) struct ProposeFollowupTool {
    agenda: AgendaHandle,
    modes: (Arc<dyn ModeStore>, HarnessMode),
}

impl ProposeFollowupTool {
    pub(crate) fn new(
        agenda: AgendaHandle,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        Self { agenda, modes: (mode_store, default_mode) }
    }

    async fn mode(&self, ctx: &dyn ToolContext) -> HarnessMode {
        let (store, default_mode) = &self.modes;
        match store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await {
            Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
            Ok(None) => *default_mode,
            Err(error) => {
                tracing::warn!(error = %error, "mode store unavailable; the follow-up is proposed read-only");
                HarnessMode::ReadOnly
            }
        }
    }
}

#[async_trait]
impl Tool for ProposeFollowupTool {
    fn name(&self) -> &str {
        PROPOSE_FOLLOWUP
    }

    fn description(&self) -> &str {
        "Propose follow-up work to do later, outside this conversation: a title, why it is \
         worth doing, optionally when to do it and the steps. The proposal goes on the \
         agenda; it runs only once approved (by the user, or by an Auto-mode policy), outside \
         quiet hours and within the daily budget. Proposing does nothing else."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "What to do, in a short line." },
                "rationale": { "type": "string", "description": "Why it is worth doing, and what triggers it." },
                "when": {
                    "type": "object",
                    "description": "When to run it; omit to run once approved. One of: {\"at\": \"<RFC 3339 time>\"}, {\"cron\": {\"expr\": \"<cron>\", \"missed\": \"skip\"}}, {\"fileChange\": {\"root\": \"<dir>\", \"glob\": \"<pattern>\", \"debounceMs\": 500}}, {\"runFinished\": {\"pid\": \"<process id>\"}}."
                },
                "steps": {
                    "type": "array",
                    "description": "The plan, when there is one.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string" },
                            "instruction": { "type": "string" },
                            "depends_on": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["id", "title", "instruction"]
                    }
                }
            },
            "required": ["title", "rationale"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        // A resumed run never proposes the same follow-up twice.
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let config = ctx
            .run_config()
            .ok_or_else(|| AdkError::tool("propose_followup runs only inside a harness run"))?;
        if !config.tool_policy.as_deref().is_some_and(has_harness_layer) {
            return Err(AdkError::tool(
                "propose_followup refuses to run without the harness policy in the run config",
            ));
        }
        let text = |field: &str, max: usize| -> Result<String> {
            let value = args
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid(format!("propose_followup needs a non-empty '{field}'")))?;
            if value.chars().count() > max {
                return Err(invalid(format!("'{field}' is longer than {max} characters")));
            }
            Ok(value.to_string())
        };
        let title = text("title", TITLE_MAX_CHARS)?;
        let rationale = text("rationale", RATIONALE_MAX_CHARS)?;
        let when: Option<Wake> = match args.get("when") {
            None | Some(Value::Null) => None,
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|error| {
                invalid(format!("'when' is not a wake this kernel knows: {error}"))
            })?),
        };
        let steps: Option<Vec<PlanStep>> = match args.get("steps") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                serde_json::from_value(value.clone())
                    .map_err(|error| invalid(format!("'steps' are not plan steps: {error}")))?,
            ),
        };
        let Some(agenda) = self.agenda.get() else {
            return Ok(json!({
                "error": "no agent kernel hosts this harness, so nothing can be proposed for later; \
                          the user can start one with `adk-rust up`"
            }));
        };
        let mode = self.mode(ctx.as_ref()).await;
        let pid = config
            .durable_run_id
            .clone()
            .unwrap_or_else(|| format!("invocation:{}", ctx.invocation_id()));
        let mut item = AgendaItem::new(
            uuid::Uuid::new_v4().to_string(),
            SubjectRef::human(ctx.user_id()),
            title,
            rationale,
            AgendaOrigin::Agent { pid, confidence: 1.0 },
            Utc::now(),
        );
        if let Some(when) = when {
            item = item.with_when(when);
        }
        if let Some(steps) = steps.filter(|steps| !steps.is_empty()) {
            let steps: Vec<Value> =
                steps.iter().filter_map(|step| serde_json::to_value(step).ok()).collect();
            let digest =
                crate::fsutil::hex_digest(Value::from(steps.clone()).to_string().as_bytes());
            item = item.with_plan(PlanRef::new(format!("sha256:{digest}"), steps));
        }
        // The follow-up's session starts in this session's mode, before any process can
        // read it, so the follow-up never runs above the proposing run's mode (K-I7).
        let (store, _) = &self.modes;
        store
            .set(
                ctx.app_name(),
                ctx.user_id(),
                &adk_kernel::agenda::item_session(&item.id),
                ModeRecord::new(mode, ModeSource::SystemTighten),
            )
            .await?;
        let outcome = agenda.propose(item, partition_mode(mode)).await.map_err(AdkError::from)?;
        Ok(match outcome {
            ProposalOutcome::Held { item } => json!({
                "proposed": item.id,
                "state": "proposed",
                "note": format!("held on the agenda for the user: this session is in {mode} mode"),
            }),
            ProposalOutcome::AwaitingApproval { item, request_id } => json!({
                "proposed": item.id,
                "state": "proposed",
                "awaitingApproval": request_id,
            }),
            ProposalOutcome::Scheduled { item, pid } => json!({
                "proposed": item.id,
                "state": "scheduled",
                "pid": pid,
            }),
            ProposalOutcome::Rejected { reason } => json!({
                "error": format!("the follow-up was not proposed: {reason}"),
            }),
            other => json!({ "outcome": other }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use adk_core::{
        ApprovalPolicy, ApprovalScope, ApproverSelector, InMemoryApprovalStore,
        StoreApprovalService, ToolConfirmationRequest,
    };

    use super::*;

    #[test]
    fn partition_modes_map_to_the_harness_mode_of_the_same_name() {
        let pairs = [
            (PartitionMode::ReadOnly, HarnessMode::ReadOnly),
            (PartitionMode::Plan, HarnessMode::Plan),
            (PartitionMode::Ask, HarnessMode::Ask),
            (PartitionMode::Auto, HarnessMode::Auto),
        ];
        for (partition, harness) in pairs {
            assert_eq!(HarnessMode::from(partition), harness);
            assert_eq!(partition.as_str(), harness.to_string());
        }
    }

    #[derive(Debug, Default)]
    struct Seen(Mutex<Vec<ApprovalEventKind>>);

    #[async_trait]
    impl ApprovalNotifier for Seen {
        async fn notify(&self, notification: &ApprovalNotification) -> Result<()> {
            self.0.lock().unwrap().push(notification.kind);
            Ok(())
        }
    }

    #[tokio::test]
    async fn an_outcome_through_the_harness_service_reaches_the_bound_notifier() {
        let late = Arc::new(LateApprovalNotifier::default());
        let store = Arc::new(InMemoryApprovalStore::new());
        let service =
            NotifyingApprovals::wrap(Arc::new(StoreApprovalService::new(store.clone())), &late);
        let scope = ApprovalScope::new("app", "alice", "s1");
        let call = ToolConfirmationRequest {
            tool_name: "transfer".into(),
            function_call_id: Some("call-1".into()),
            args: serde_json::json!({ "amount": 10 }),
            acting: None,
            origin: None,
        };
        store.record_pending(&scope, &call).await.unwrap();
        let policy = ApprovalPolicy::new([ApproverSelector::human("bob")]);
        let first = service
            .submit(ApprovalRequest::new("req-1", scope.clone(), &call, policy.clone()))
            .await
            .unwrap();
        // Unbound: an outcome reaches no one, and nothing fails.
        service.cancel(&first.id, None).await.unwrap();

        let seen = Arc::new(Seen::default());
        late.bind_to(seen.clone());
        let second =
            service.submit(ApprovalRequest::new("req-2", scope, &call, policy)).await.unwrap();
        service
            .decide(&second.id, second.version, ApprovalDecision::approve(SubjectRef::human("bob")))
            .await
            .unwrap();
        assert_eq!(*seen.0.lock().unwrap(), [ApprovalEventKind::Resolved]);
    }
}
