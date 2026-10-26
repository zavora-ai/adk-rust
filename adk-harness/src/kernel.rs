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
//! | [`driver_tool_kind`]: `EffectClass` → [`ToolKind::External`], the manifest's class over any `readOnlyHint` | §7.7, §10 HK1 |
//! | [`DriverClasses`], [`HarnessAgentBuilder::kernel_drivers`]: a partition's drivers with their classes and the kernel's driver policy | §7.7, §7.8 |
//! | [`HarnessRulePins`]: standing rules carried to re-approved driver declarations | §7.7, K-I14 |
//! | The harness's activity in the kernel's content-free ledger: each run's `intent`, each tool call with its effect class and duration, and its audit records — `harness.spend`, the loop guard, approvals, denials and the rest — through `LedgerAuditTap`; a partition's harness writes its rows in its partition | §7.10, §7.8 |

use std::sync::{Arc, OnceLock, RwLock};

use adk_core::audit_log::AuditTrail;
use adk_core::{
    AdkError, ApprovalCall, ApprovalDecision, ApprovalDelegation, ApprovalEventKind,
    ApprovalNotification, ApprovalNotifier, ApprovalRequest, ApprovalService, ApprovalState,
    ApprovalStore, ApprovalTick, ErrorCategory, ErrorComponent, InboxPage, InboxQuery, Result,
    RunCheckpointRef, RunRecord, SubjectRef, Tool, ToolApproval, ToolConfirmationDecision,
    ToolContext, ToolEffect,
};
use adk_kernel::proc::{Activity, LateLedgerSink, LedgerAuditTap, LedgerSink};
use adk_kernel::{
    AgendaHandle, AgendaItem, AgendaOrigin, ApprovalWakeSource, GoalProgress, KernelBuilder,
    KernelConfig, PartitionId, PartitionMode, PlanRef, ProcessHandle, ProgressSource,
    ProposalOutcome, RuleAge, StandingRuleSource, Wake,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::approvals::{OriginPin, StandingRuleStore};
use crate::builder::HarnessAgentBuilder;
use crate::goal::{GoalBook, GoalParker, GoalStore};
use crate::harness::Harness;
use crate::mode::{HarnessMode, ModeRecord, ModeSource, ModeStore};
use crate::policy::{OriginClasses, ToolKind, has_harness_layer};
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

/// The harness class of a driver tool of `class` (spec 3.2 §7.7, §10 HK1).
///
/// The manifest the user approved is the source of truth, so a server's own `readOnlyHint`
/// never grants anything (3.1 I12): `read` is a trusted read-only external tool, and every
/// other class a trusted external tool with effects, which the mode table asks about in
/// Ask and Auto and refuses in ReadOnly and Plan. `publish_public` and `financial` map the
/// same way; the kernel's driver policy, which [`HarnessAgentBuilder::kernel_drivers`]
/// composes with the harness's, asks before every call of them, so no mode, standing rule
/// or earlier approval allows one (K-I9).
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::{ToolKind, driver_tool_kind};
/// use adk_kernel::drivers::EffectClass;
///
/// assert_eq!(
///     driver_tool_kind(EffectClass::Read),
///     ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true }
/// );
/// assert_eq!(
///     driver_tool_kind(EffectClass::SendExternal),
///     ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: true }
/// );
/// ```
pub fn driver_tool_kind(class: adk_kernel::drivers::EffectClass) -> ToolKind {
    ToolKind::External { effect: class.tool_effect(), trusted_origin: true }
}

/// Classes the kernel's driver tools by their approved manifests (spec 3.2 §7.7).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_harness::{DriverClasses, OriginClasses};
/// use adk_kernel::drivers::DriverSupervisor;
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
/// let classes = DriverClasses::new(drivers);
/// assert_eq!(classes.class_of("driver:email", "send_draft"), None); // not mounted
/// assert_eq!(classes.class_of("mcp:email", "send_draft"), None); // not a driver
/// ```
#[derive(Debug, Clone)]
pub struct DriverClasses {
    drivers: adk_kernel::drivers::DriverSupervisor,
}

impl DriverClasses {
    /// The classes of `drivers`' mounted manifests.
    pub fn new(drivers: adk_kernel::drivers::DriverSupervisor) -> Self {
        Self { drivers }
    }
}

impl OriginClasses for DriverClasses {
    fn class_of(&self, source: &str, tool: &str) -> Option<ToolKind> {
        self.drivers.class_of(source, tool).map(driver_tool_kind)
    }
}

impl HarnessAgentBuilder {
    /// Mounts the kernel's drivers for `partition` (spec 3.2 §7.7, §7.8): the tools of the
    /// drivers whose manifest lists `partition` and whose id is in `drivers` — the
    /// partition's own driver list — classed by their manifests, with the kernel's driver
    /// policy composed with the harness's, the stricter decision winning (K-I6).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = HarnessAgent::builder("work")
    ///     .model(model)
    ///     .workspace(workspace)
    ///     .kernel_drivers(&drivers, &partition.id, &partition.drivers)
    ///     .build()?;
    /// ```
    pub fn kernel_drivers(
        self,
        drivers: &adk_kernel::drivers::DriverSupervisor,
        partition: &adk_kernel::PartitionId,
        ids: &[String],
    ) -> Self {
        self.toolset(drivers.toolset_for(partition, ids))
            .tool_classes(Arc::new(DriverClasses::new(drivers.clone())))
            .add_operator_policy(drivers.policy())
    }
}

/// Moves a person's standing rules onto a driver tool's re-approved declaration
/// (spec 3.2 §7.7, K-I14): each of the subject's rules in the app pinned exactly to the old
/// declaration of the tool is granted again, pinned to the new one.
///
/// # Example
///
/// ```rust,ignore
/// let drivers = DriverSupervisor::builder(store).rule_pins(harness.driver_rule_pins()).build();
/// ```
#[derive(Debug, Clone)]
pub struct HarnessRulePins {
    app_name: String,
    rules: Arc<dyn StandingRuleStore>,
}

impl HarnessRulePins {
    /// Re-pins the rules of `app_name` in `rules`.
    pub fn new(app_name: impl Into<String>, rules: Arc<dyn StandingRuleStore>) -> Self {
        Self { app_name: app_name.into(), rules }
    }
}

#[async_trait]
impl adk_kernel::drivers::StandingRulePins for HarnessRulePins {
    async fn repin(
        &self,
        subject: &str,
        source: &str,
        tool: &str,
        from: &str,
        to: &str,
    ) -> Result<usize> {
        let mut moved = 0;
        for mut rule in self.rules.list(&self.app_name, subject).await? {
            let pinned = matches!(
                &rule.origin,
                OriginPin::Exact { source: pinned, declaration_digest } if pinned == source && declaration_digest == from
            );
            if !pinned || rule.tool != tool {
                continue;
            }
            rule.origin =
                OriginPin::Exact { source: source.to_string(), declaration_digest: to.to_string() };
            self.rules.grant(rule).await?;
            moved += 1;
        }
        Ok(moved)
    }
}

impl Harness {
    /// Carries this harness's standing rules to re-approved driver declarations; pass it
    /// to `DriverSupervisorBuilder::rule_pins` (spec 3.2 §7.7).
    pub fn driver_rule_pins(&self) -> Arc<dyn adk_kernel::drivers::StandingRulePins> {
        Arc::new(HarnessRulePins::new(
            self.app_name(),
            self.agent().runtime().standing_rules().clone(),
        ))
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
    /// A partition's harness registers its runner as its partition's targets,
    /// `<app>@<partition>` and `harness.goal@<partition>`, bound to the partition, with the
    /// partition table its policy reads ([`host_partition`](Self::host_partition)); add the
    /// other partitions' harnesses with `host_partition` (spec 3.2 §7.8).
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
        let progress = self.kernel_progress();
        let source = runtime.kernel_approvals.bind();
        let builder = adk_kernel::Kernel::builder()
            .config(KernelConfig::default().with_app_name(self.app_name()));
        // A partition's harness runs as its partition's targets and answers for its
        // partition's processes (spec 3.2 §7.8).
        let builder = match self.agent().partition() {
            Some(_) => self.host_partition_with(builder, progress.clone()),
            None => builder.agent(self.app_name(), runner.clone()).agent(target, runner.clone()),
        };
        let mut builder = builder
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

impl Harness {
    /// What this harness tells a kernel about the processes it runs, the parker of its
    /// stalled goals.
    pub(crate) fn kernel_progress(&self) -> Arc<HarnessProgress> {
        let runtime = self.agent().runtime();
        let progress = Arc::new(HarnessProgress {
            goals: runtime.goal_store().clone(),
            book: runtime.goals.until.book().clone(),
            #[cfg(feature = "observe")]
            trails: runtime.run_trails.clone(),
            processes: RwLock::new(None),
            ledger: runtime.kernel_ledger.clone(),
            partition: self.agent().partition().map(|partition| partition.id.clone()),
        });
        runtime.goals.until.book().set_parker(progress.clone());
        progress
    }
}

/// What the harness tells the kernel about the processes it runs (spec 3.2 §7.3, §10 HK3).
pub(crate) struct HarnessProgress {
    goals: Arc<dyn GoalStore>,
    book: Arc<GoalBook>,
    #[cfg(feature = "observe")]
    trails: Arc<crate::observe::RunTrails>,
    processes: RwLock<Option<ProcessHandle>>,
    /// Where the harness writes its activity; bound to the kernel's ledger at start.
    ledger: LateLedgerSink,
    /// The partition of a partition's harness, whose ledger rows carry it (spec 3.2 §7.8).
    partition: Option<PartitionId>,
}

/// The kernel's ledger as a partition's harness writes to it: every row in the partition
/// (spec 3.2 §7.8, §7.10). The harness serves only its partition's sessions, so the
/// partition is the session's.
struct PartitionRows {
    ledger: Arc<dyn LedgerSink>,
    partition: PartitionId,
}

#[async_trait]
impl LedgerSink for PartitionRows {
    async fn record(&self, activity: Activity) -> adk_kernel::Result<()> {
        self.ledger.record(activity.with_partition(self.partition.clone())).await
    }
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
    async fn usage(&self, run: &RunRecord) -> adk_kernel::Result<Option<adk_kernel::RunUsage>> {
        Ok(self.trails.session_usage(&run.app_name, &run.user_id, &session_of(run)).map(
            |(model_calls, tool_calls, cost)| {
                adk_kernel::RunUsage::new(model_calls, tool_calls, cost)
            },
        ))
    }

    fn attach(&self, processes: ProcessHandle) {
        *self.processes.write().unwrap_or_else(|e| e.into_inner()) = Some(processes);
    }

    fn attach_ledger(&self, ledger: Arc<dyn LedgerSink>) {
        let ledger = match &self.partition {
            Some(partition) => Arc::new(PartitionRows { ledger, partition: partition.clone() })
                as Arc<dyn LedgerSink>,
            None => ledger,
        };
        if !self.ledger.bind(ledger) {
            tracing::debug!("the harness already writes to a kernel's ledger; keeping it");
        }
    }
}

/// `trail` with every record also written to the kernel's ledger once bound (spec 3.2
/// §7.10): the record first, then its content-free row.
pub(crate) fn tap_audit(trail: AuditTrail, ledger: &LateLedgerSink) -> AuditTrail {
    let tap = LedgerAuditTap::new(trail.recorder().clone(), Arc::new(ledger.clone()));
    AuditTrail::new(Arc::new(tap)).with_failure_mode(trail.failure_mode())
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

/// The rows one harness run writes to the kernel's ledger (spec 3.2 §7.10): its `intent`
/// when it starts, and each tool call when its response arrives, with the tool's effect
/// class, the time it took and, as the subject the ledger hashes, the call's fingerprint.
#[cfg(feature = "observe")]
pub(crate) struct ToolEvents {
    sink: LateLedgerSink,
    effects: Arc<std::collections::HashMap<String, String>>,
    agent: String,
    trace: Option<String>,
    started: std::collections::HashMap<String, (String, String, DateTime<Utc>)>,
}

#[cfg(feature = "observe")]
impl ToolEvents {
    /// The rows of a run of `agent` in `invocation`, or `None` until a kernel's ledger is
    /// bound.
    pub(crate) fn new(
        sink: &LateLedgerSink,
        effects: &Arc<std::collections::HashMap<String, String>>,
        agent: &str,
        invocation: &str,
    ) -> Option<Self> {
        sink.is_bound().then(|| Self {
            sink: sink.clone(),
            effects: Arc::clone(effects),
            agent: adk_kernel::proc::label(agent, 128),
            trace: ((1..=64).contains(&invocation.len())
                && invocation.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .then(|| invocation.to_string()),
            started: std::collections::HashMap::new(),
        })
    }

    async fn record(&self, activity: adk_kernel::proc::Activity) {
        let activity = match &self.trace {
            Some(trace) => activity.with_trace_id(trace.clone()),
            None => activity,
        };
        if let Err(error) = self.sink.record(activity).await {
            tracing::warn!(error = %error, "could not write a harness ledger row");
        }
    }

    /// The run started.
    pub(crate) async fn intent(&self) {
        self.record(adk_kernel::proc::Activity::new(
            adk_kernel::proc::LedgerKind::Intent,
            self.agent.clone(),
        ))
        .await;
    }

    /// Notes the calls `event` makes and writes a row for each response it carries.
    pub(crate) async fn observe(&mut self, event: &adk_core::Event) {
        if event.llm_response.partial {
            return;
        }
        let Some(content) = &event.llm_response.content else { return };
        for part in &content.parts {
            match part {
                adk_core::Part::FunctionCall { name, args, id, .. } => {
                    if self.started.len() > 1024 {
                        self.started.clear();
                    }
                    let key = id.clone().unwrap_or_else(|| name.clone());
                    let fingerprint = adk_core::tool_call_fingerprint(name, args);
                    self.started.insert(key, (name.clone(), fingerprint, event.timestamp));
                }
                adk_core::Part::FunctionResponse { function_response, id, .. } => {
                    let key = id.clone().unwrap_or_else(|| function_response.name.clone());
                    let Some((name, fingerprint, at)) = self.started.remove(&key) else {
                        continue;
                    };
                    let mut activity = adk_kernel::proc::Activity::new(
                        adk_kernel::proc::LedgerKind::ToolCall,
                        self.agent.clone(),
                    )
                    .with_duration((event.timestamp - at).to_std().unwrap_or_default())
                    .with_subject(fingerprint);
                    if let Some(effect) = self.effects.get(&name) {
                        activity = activity.with_effect(effect.clone());
                    }
                    self.record(activity).await;
                }
                _ => {}
            }
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

    /// Reports a request `Harness::decide` withdrew: resolved by the person's `decision` once
    /// it is recorded, or cancelled when none was (spec 3.3 §7.3).
    pub(crate) async fn decided_in_the_harness(
        &self,
        request: &ApprovalRequest,
        decision: Option<&ToolApproval>,
    ) {
        let mut reported = request.clone();
        if let Some(decision) = decision {
            reported.state = match decision.decision {
                ToolConfirmationDecision::Approve => ApprovalState::Approved,
                ToolConfirmationDecision::Deny => ApprovalState::Rejected,
            };
        }
        self.notify(&reported, None).await;
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

    async fn withdraw(&self, id: &str, reason: Option<String>) -> Result<ApprovalRequest> {
        // `Harness::decide` reports its withdrawal once the person's decision is recorded, so
        // no sleeper wakes before the decision exists (spec 3.3 §7.3).
        let reported_later = reason.as_deref() == Some(crate::harness::DECIDED_IN_THE_HARNESS);
        let request = self.inner.withdraw(id, reason).await?;
        if !reported_later {
            self.late.notify(&request, None).await;
        }
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
/// (K-I7). A partition's harness proposes into its partition (spec 3.2 §7.8). Without a
/// kernel hosting the harness the tool answers that none runs.
pub(crate) struct ProposeFollowupTool {
    agenda: AgendaHandle,
    modes: (Arc<dyn ModeStore>, HarnessMode),
    partition: Option<adk_kernel::PartitionId>,
}

impl ProposeFollowupTool {
    pub(crate) fn new(
        agenda: AgendaHandle,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
        partition: Option<adk_kernel::PartitionId>,
    ) -> Self {
        Self { agenda, modes: (mode_store, default_mode), partition }
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
        if let Some(partition) = &self.partition {
            item = item.with_partition(partition.clone());
        }
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

    #[derive(Debug, Default)]
    struct States(Mutex<Vec<(ApprovalEventKind, ApprovalState)>>);

    #[async_trait]
    impl ApprovalNotifier for States {
        async fn notify(&self, notification: &ApprovalNotification) -> Result<()> {
            self.0.lock().unwrap().push((notification.kind, notification.request.state));
            Ok(())
        }
    }

    /// Wave 2 review finding 21 (spec 3.3 §7.3): the request `Harness::decide` withdraws wakes
    /// no kernel sleeper until the person's decision is recorded, and is then reported with
    /// that decision; a withdrawal for a call decided another way is reported at once.
    #[tokio::test]
    async fn a_request_withdrawn_by_the_harness_is_reported_with_the_persons_decision() {
        let late = Arc::new(LateApprovalNotifier::default());
        let states = Arc::new(States::default());
        late.bind_to(states.clone());
        let store = Arc::new(InMemoryApprovalStore::new());
        let service =
            NotifyingApprovals::wrap(Arc::new(StoreApprovalService::new(store.clone())), &late);
        let scope = ApprovalScope::new("app", "alice", "s1");
        let call = ToolConfirmationRequest::new("transfer", serde_json::json!({ "amount": 10 }));
        store.record_pending(&scope, &call).await.unwrap();
        let policy = ApprovalPolicy::new([ApproverSelector::human("bob")]);
        let first = service
            .submit(ApprovalRequest::new("req-1", scope.clone(), &call, policy.clone()))
            .await
            .unwrap();

        let reason = Some(crate::harness::DECIDED_IN_THE_HARNESS.to_string());
        let withdrawn = service.withdraw(&first.id, reason).await.unwrap();
        assert!(states.0.lock().unwrap().is_empty(), "the withdrawal alone wakes nothing");
        late.decided_in_the_harness(&withdrawn, Some(&ToolApproval::approve())).await;
        assert_eq!(
            *states.0.lock().unwrap(),
            [(ApprovalEventKind::Resolved, ApprovalState::Approved)]
        );

        let second =
            service.submit(ApprovalRequest::new("req-2", scope, &call, policy)).await.unwrap();
        let note = Some("the call was decided another way (handler)".to_string());
        service.withdraw(&second.id, note).await.unwrap();
        assert_eq!(
            states.0.lock().unwrap().last(),
            Some(&(ApprovalEventKind::Cancelled, ApprovalState::Cancelled))
        );
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
