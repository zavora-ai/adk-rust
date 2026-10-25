//! The kernel's audited agent table (spec 3.3 §5.7, §10 K7): every change to an agent a
//! person adds goes through it, recorded before it acts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, RwLock, Weak};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, AgentId, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk,
    ApprovalScope, ApprovalService, ApprovalState, ApproverSelector, ErrorCategory, ErrorComponent,
    InboxQuery, RunState, ToolConfirmationRequest,
};
use adk_server::background::WorkflowExecutor;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agenda::{Agenda, AgendaFilter};
use crate::agents::AGENT_CREATE_TOOL_NAME;
use crate::agents::caller::AgentCaller;
use crate::agents::change::{AgentAction, AgentChange, AgentPreview, AgentView, EndedWork};
use crate::agents::check::{area_limits, changed_fields, kernel_assessment, over_ceiling, widened};
use crate::agents::factory::{AgentAssessment, AgentChoices, AgentFactory};
use crate::agents::record::{
    AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, RESERVED_AGENT_NAME, agent_id_for,
};
use crate::agents::store::AgentStore;
use crate::agents::targets::{Added, Targets};
use crate::audit;
use crate::bus::KernelEvent;
use crate::drivers::EffectClass;
use crate::error::{
    AGENT_INVALID_CODE, AGENT_NOT_FOUND_CODE, AGENT_OUT_OF_DATE_CODE, AGENT_REFUSED_CODE,
    AGENT_UNCONFIRMED_CODE, KernelError, Result,
};
use crate::partition::{Partition, PartitionId, PartitionMode};
use crate::process::{ProcessHandle, ProcessKind, ProcessTable, Suspension};
use crate::stores::ProcessFilter;
use crate::supervise::Supervision;

/// The approver group of the inbox `adk-rust up` serves: the control plane's `operator`
/// scope.
const OPERATOR_GROUP: &str = "operator";

/// The most suggestions one agent may have waiting at once (spec 3.3 §5.7).
pub const MAX_OPEN_SUGGESTIONS: usize = 3;

fn agent_error(
    category: ErrorCategory,
    code: &'static str,
    message: impl Into<String>,
) -> KernelError {
    KernelError::Core(AdkError::new(ErrorComponent::Server, category, code, message.into()))
}

fn refused(message: impl Into<String>) -> KernelError {
    agent_error(ErrorCategory::Forbidden, AGENT_REFUSED_CODE, message)
}

fn invalid(message: impl Into<String>) -> KernelError {
    agent_error(ErrorCategory::InvalidInput, AGENT_INVALID_CODE, message)
}

fn not_found(id: &str) -> KernelError {
    agent_error(
        ErrorCategory::NotFound,
        AGENT_NOT_FOUND_CODE,
        format!("no agent `{id}`; list them with `adk-rust agents` or `GET /api/kernel/agents`"),
    )
}

/// The inbox request id of the suggestion `record`: `agent-<id>-<digest[..12]>`.
pub fn suggestion_request_id(record: &AgentRecord) -> String {
    format!("agent-{}-{}", record.id, &record.digest[..record.digest.len().min(12)])
}

/// The agent that suggests an agent, and where (spec 3.3 §5.7): `propose_agent`'s caller.
///
/// # Example
///
/// ```rust
/// use adk_core::AgentId;
/// use adk_kernel::agents::AgentProposer;
///
/// let proposer = AgentProposer::new(AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap(), "alice", "s-1")
///     .with_run("run-7");
/// assert_eq!(proposer.run.as_deref(), Some("run-7"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentProposer {
    /// The suggesting agent.
    pub agent: AgentId,
    /// The person whose session it is; the suggestion waits in their inbox.
    pub user: String,
    /// The session it suggests in.
    pub session: String,
    /// The durable run it suggests from, when it runs as a kernel process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
}

impl AgentProposer {
    /// `agent` suggesting in `user`'s session `session`.
    pub fn new(agent: AgentId, user: impl Into<String>, session: impl Into<String>) -> Self {
        Self { agent, user: user.into(), session: session.into(), run: None }
    }

    /// Sets the durable run.
    #[must_use]
    pub fn with_run(mut self, run: impl Into<String>) -> Self {
        self.run = Some(run.into());
        self
    }
}

/// A suggestion the table keeps (spec 3.3 §5.7): the suggested agent and its inbox request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgentProposal {
    /// The suggested agent, `Proposed`.
    pub agent: AgentRecord,
    /// Its inbox request.
    pub request_id: String,
    /// The risk the inbox shows.
    pub risk: ApprovalRisk,
    /// Whether the same suggestion was already waiting, so nothing new was added.
    pub merged: bool,
}

/// What [`Agents::apply`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum AgentOutcome {
    /// The agent was added.
    Created {
        /// It.
        agent: AgentRecord,
    },
    /// The agent was changed.
    Changed {
        /// It, as it is now.
        agent: AgentRecord,
    },
    /// The agent was paused.
    Paused {
        /// It.
        agent: AgentRecord,
    },
    /// The agent was resumed.
    Resumed {
        /// It.
        agent: AgentRecord,
    },
    /// The agent was removed.
    Removed {
        /// Its tombstone.
        agent: AgentRecord,
        /// What ended.
        ended: EndedWork,
    },
}

/// What this replica remembers between ticks.
#[derive(Default)]
struct Local {
    /// Why an agent could not be built, by id, with the version that failed.
    cant_start: BTreeMap<String, (u64, String)>,
    /// Suggestions whose approval by someone other than a person was already logged.
    ignored: BTreeSet<String>,
}

/// What the kernel gives its agent table when it builds it.
pub(crate) struct AgentsParts {
    pub(crate) sup: Arc<Supervision>,
    pub(crate) processes: ProcessTable,
    pub(crate) agenda: Agenda,
    pub(crate) targets: Arc<Targets>,
    pub(crate) store: Arc<dyn AgentStore>,
    pub(crate) factory: Option<Arc<dyn AgentFactory>>,
    pub(crate) approvals: Option<Arc<dyn ApprovalService>>,
    /// The agent targets the host registered at build.
    pub(crate) statics: BTreeSet<String>,
    pub(crate) workflows: Option<Arc<dyn WorkflowExecutor>>,
    #[cfg(feature = "drivers")]
    pub(crate) drivers: Option<crate::drivers::DriverSupervisor>,
    #[cfg(feature = "proc")]
    pub(crate) ledger: Option<crate::proc::Ledger>,
}

pub(crate) struct AgentsInner {
    pub(crate) sup: Arc<Supervision>,
    pub(crate) processes: ProcessTable,
    pub(crate) agenda: Agenda,
    pub(crate) targets: Arc<Targets>,
    pub(crate) store: Arc<dyn AgentStore>,
    pub(crate) factory: Option<Arc<dyn AgentFactory>>,
    pub(crate) approvals: Option<Arc<dyn ApprovalService>>,
    pub(crate) statics: BTreeSet<String>,
    pub(crate) workflows: Option<Arc<dyn WorkflowExecutor>>,
    #[cfg(feature = "drivers")]
    pub(crate) drivers: Option<crate::drivers::DriverSupervisor>,
    #[cfg(feature = "proc")]
    pub(crate) ledger: Option<crate::proc::Ledger>,
    local: Mutex<Local>,
}

/// The kernel's audited agent table (spec 3.3 §5.7, §10 K7), from
/// [`Kernel::agents`](crate::Kernel::agents): the one front door for adding, changing,
/// pausing and removing the agents a person adds, whichever surface asks.
///
/// Its rules:
///
/// | Rule | Spec |
/// |---|---|
/// | Only a person changes an agent; a device only answers suggestions | S-I11, decision 16 |
/// | A change previews first and applies only with its preview's digest | §5.7 |
/// | High risk or money needs presence: a paired local terminal or "Confirm it's you" | §7.2, decision 22 |
/// | A definition stays within its area: its mode, limit and mounted drivers | §5.7, K-I13 |
/// | A suggestion stays within its suggester, checked when made and again when approved | S-I12 |
/// | Every change is recorded (`kernel.agent.*`) before it acts, and refused when the record cannot be written | S-I18, K-I16 |
/// | A removed agent's id is never given again | S-I14 |
///
/// Cloning shares the table.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentAction, AgentCaller, AgentDefinition, Presence};
///
/// # async fn example(kernel: adk_kernel::Kernel) -> adk_kernel::Result<()> {
/// let caller = AgentCaller::new(SubjectRef::human("alice"), "terminal", "keyboard")
///     .with_presence(Presence::PairedLocal);
/// let add = AgentAction::Create { definition: AgentDefinition::new("Finance", "default"), replaces: None };
/// let preview = kernel.agents().preview(&caller, &add).await?;
/// println!("{} — {:?} risk; confirm with {}", preview.id, preview.risk, preview.code());
/// kernel.agents().apply(&caller, add, &preview.digest).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Agents {
    inner: Arc<AgentsInner>,
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agents")
            .field("factory", &self.inner.factory.is_some())
            .field("hosted", &self.inner.targets.hosted_ids())
            .finish_non_exhaustive()
    }
}

/// A handle on a kernel's agent table that does not keep the kernel alive, bound when the
/// kernel is built (spec 3.3 §10 K7): the harness's `propose_agent` tool exists before the
/// kernel that hosts it, as `propose_followup`'s [`AgendaHandle`](crate::AgendaHandle) does.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::AgentsHandle;
///
/// let handle = AgentsHandle::new();
/// assert!(handle.get().is_none(), "no kernel was built with it");
/// ```
#[derive(Clone, Default)]
pub struct AgentsHandle {
    slot: Arc<RwLock<Weak<AgentsInner>>>,
}

impl std::fmt::Debug for AgentsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsHandle").field("bound", &self.get().is_some()).finish()
    }
}

impl AgentsHandle {
    /// An unbound handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// The agent table of the kernel last built with this handle, while that kernel lives.
    pub fn get(&self) -> Option<Agents> {
        let slot = self.slot.read().unwrap_or_else(|e| e.into_inner());
        slot.upgrade().map(|inner| Agents { inner })
    }

    pub(crate) fn bind(&self, agents: &Agents) {
        *self.slot.write().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&agents.inner);
    }
}

/// What a change would do, worked out once for its preview and its apply.
struct Plan {
    preview: AgentPreview,
    current: Option<AgentRecord>,
    next: Option<AgentRecord>,
    replaces: Option<AgentRecord>,
}

impl Agents {
    pub(crate) fn new(parts: AgentsParts) -> Self {
        Self {
            inner: Arc::new(AgentsInner {
                sup: parts.sup,
                processes: parts.processes,
                agenda: parts.agenda,
                targets: parts.targets,
                store: parts.store,
                factory: parts.factory,
                approvals: parts.approvals,
                statics: parts.statics,
                workflows: parts.workflows,
                #[cfg(feature = "drivers")]
                drivers: parts.drivers,
                #[cfg(feature = "proc")]
                ledger: parts.ledger,
                local: Mutex::default(),
            }),
        }
    }

    /// The store the agents live in.
    pub fn store(&self) -> &Arc<dyn AgentStore> {
        &self.inner.store
    }

    /// Every agent but tombstones and refused suggestions, ordered by id, each with why
    /// this kernel cannot build it when it cannot ("Can't start").
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn list(&self) -> Result<Vec<AgentView>> {
        let records = self.inner.store.list().await?;
        Ok(records
            .into_iter()
            .filter(|record| {
                !matches!(record.state, AgentRecordState::Removed | AgentRecordState::Refused)
            })
            .map(|record| self.view(record))
            .collect())
    }

    /// The agent `id`, tombstones included, or `None`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn get(&self, id: &str) -> Result<Option<AgentView>> {
        Ok(self.inner.store.get(id).await?.map(|record| self.view(record)))
    }

    fn view(&self, record: AgentRecord) -> AgentView {
        let local = self.inner.local.lock().unwrap_or_else(|e| e.into_inner());
        let reason = local
            .cant_start
            .get(&record.id)
            .filter(|(version, _)| *version == record.version && record.state.is_hosted())
            .map(|(_, reason)| reason.clone());
        drop(local);
        match reason {
            Some(reason) => AgentView::new(record).with_cant_start(reason),
            None => AgentView::new(record),
        }
    }

    /// What a person may choose for an agent on this host: the models whose key it has, and
    /// the packs.
    ///
    /// # Errors
    ///
    /// Returns the factory's error; an empty choice when the host builds no agents.
    pub async fn choices(&self) -> Result<AgentChoices> {
        match &self.inner.factory {
            Some(factory) => factory.choices().await,
            None => Ok(AgentChoices::default()),
        }
    }

    /// What `action` would do, without doing it: the agent's abilities and risk as it would
    /// be, what it changes, what a removal ends, and the digest that applies it (spec 3.3
    /// §5.7).
    ///
    /// # Errors
    ///
    /// Returns `kernel.agent.refused` when `caller` may change no agent, `kernel.agent.invalid`
    /// for a definition that breaks a rule, `kernel.agent.not_found`,
    /// `kernel.agent.out_of_date` for a change against an older version, or a store error.
    pub async fn preview(
        &self,
        caller: &AgentCaller,
        action: &AgentAction,
    ) -> Result<AgentPreview> {
        if let Some(reason) = caller.refusal() {
            return Err(refused(reason));
        }
        Ok(self.plan(action).await?.preview)
    }

    /// Applies `action` for `caller` when `digest` is its preview's (spec 3.3 §5.7), writing
    /// its `kernel.agent.*` record first.
    ///
    /// # Errors
    ///
    /// As [`preview`](Self::preview); `kernel.agent.unconfirmed` when `digest` is not the
    /// preview's; `kernel.agent.refused` for high risk or money without presence; the audit
    /// trail's error under `AuditFailureMode::Block`, in which case nothing changed.
    pub async fn apply(
        &self,
        caller: &AgentCaller,
        action: AgentAction,
        digest: &str,
    ) -> Result<AgentOutcome> {
        if let Some(reason) = caller.refusal() {
            self.record_refused(Some(caller), action.id(), None, &reason).await;
            return Err(refused(reason));
        }
        let plan = match self.plan(&action).await {
            Ok(plan) => plan,
            Err(error) => {
                let message = error.to_string();
                self.record_refused(Some(caller), action.id(), definition_name(&action), &message)
                    .await;
                return Err(error);
            }
        };
        if digest != plan.preview.digest {
            return Err(agent_error(
                ErrorCategory::Conflict,
                AGENT_UNCONFIRMED_CODE,
                format!(
                    "this {} is not the one that was previewed; preview it again and confirm \
                     with its code ({})",
                    plan.preview.action,
                    plan.preview.code()
                ),
            ));
        }
        if plan.preview.needs_presence() && !caller.presence.is_present() {
            let reason = "this change is high risk: confirm it's you, or make it on the \
                          kernel's own terminal (spec 3.3 §7.2)";
            self.record_refused(Some(caller), Some(&plan.preview.id), None, reason).await;
            return Err(refused(reason));
        }
        match action {
            AgentAction::Create { .. } => self.create_planned(caller, plan).await,
            AgentAction::Change { .. } => self.change_planned(caller, plan).await,
            AgentAction::Pause { .. } => self.pause_planned(caller, plan).await,
            AgentAction::Resume { .. } => self.resume_planned(caller, plan).await,
            AgentAction::Remove { .. } => self.remove_planned(caller, plan).await,
        }
    }

    /// Adds an agent by hand: [`apply`](Self::apply) of [`AgentAction::Create`].
    ///
    /// # Errors
    ///
    /// As [`apply`](Self::apply).
    pub async fn create(
        &self,
        caller: &AgentCaller,
        definition: AgentDefinition,
        digest: &str,
    ) -> Result<AgentRecord> {
        match self.apply(caller, AgentAction::Create { definition, replaces: None }, digest).await?
        {
            AgentOutcome::Created { agent } => Ok(agent),
            other => Err(KernelError::Config { reason: format!("unexpected outcome {other:?}") }),
        }
    }

    /// Changes agent `id` made against `expected_version`: [`apply`](Self::apply) of
    /// [`AgentAction::Change`].
    ///
    /// # Errors
    ///
    /// As [`apply`](Self::apply).
    pub async fn change(
        &self,
        caller: &AgentCaller,
        id: &str,
        expected_version: u64,
        definition: AgentDefinition,
        digest: &str,
    ) -> Result<AgentRecord> {
        let action = AgentAction::Change { id: id.to_string(), expected_version, definition };
        match self.apply(caller, action, digest).await? {
            AgentOutcome::Changed { agent } => Ok(agent),
            other => Err(KernelError::Config { reason: format!("unexpected outcome {other:?}") }),
        }
    }

    /// Pauses agent `id`: its work stops at its next step, and it takes no new work until
    /// it is resumed. Pausing only makes an agent do less, so it needs no digest.
    ///
    /// # Errors
    ///
    /// As [`apply`](Self::apply), without the digest check.
    pub async fn pause(&self, caller: &AgentCaller, id: &str) -> Result<AgentRecord> {
        let action = AgentAction::Pause { id: id.to_string() };
        let digest = action.digest();
        match self.apply(caller, action, &digest).await? {
            AgentOutcome::Paused { agent } => Ok(agent),
            other => Err(KernelError::Config { reason: format!("unexpected outcome {other:?}") }),
        }
    }

    /// Resumes agent `id`, waking only the work its pause stopped.
    ///
    /// # Errors
    ///
    /// As [`apply`](Self::apply), without the digest check.
    pub async fn resume(&self, caller: &AgentCaller, id: &str) -> Result<AgentRecord> {
        let action = AgentAction::Resume { id: id.to_string() };
        let digest = action.digest();
        match self.apply(caller, action, &digest).await? {
            AgentOutcome::Resumed { agent } => Ok(agent),
            other => Err(KernelError::Config { reason: format!("unexpected outcome {other:?}") }),
        }
    }

    /// Removes agent `id` asked against `expected_version`, returning what ended:
    /// [`apply`](Self::apply) of [`AgentAction::Remove`].
    ///
    /// # Errors
    ///
    /// As [`apply`](Self::apply).
    pub async fn remove(
        &self,
        caller: &AgentCaller,
        id: &str,
        expected_version: u64,
        digest: &str,
    ) -> Result<EndedWork> {
        let action = AgentAction::Remove { id: id.to_string(), expected_version };
        match self.apply(caller, action, digest).await? {
            AgentOutcome::Removed { ended, .. } => Ok(ended),
            other => Err(KernelError::Config { reason: format!("unexpected outcome {other:?}") }),
        }
    }

    /// Suggests `definition` for `proposer`, an agent (spec 3.3 §5.7, S-I12): checks it
    /// against the suggester's limits now and its area's, keeps it `Proposed`, and puts one
    /// `kernel:agent.create` request in the inbox, which only a person's approval of the
    /// same definition applies. The same suggestion waiting already is returned merged; at
    /// most [`MAX_OPEN_SUGGESTIONS`] wait per suggesting agent.
    ///
    /// # Errors
    ///
    /// `kernel.agent.refused` for a suggestion over its suggester's or area's limits, too
    /// many waiting, or a kernel without an inbox or a factory; `kernel.agent.invalid` for a
    /// definition that breaks a rule; a store or audit error.
    pub async fn propose(
        &self,
        definition: AgentDefinition,
        proposer: &AgentProposer,
    ) -> Result<AgentProposal> {
        let definition = definition.normalized();
        let name = definition.display_name.clone();
        let result = self.propose_checked(definition, proposer).await;
        if let Err(error) = &result {
            let fields = json!({
                "name": name,
                "reason": error.to_string(),
                "suggestingAgent": proposer.agent,
                "session": proposer.session,
                "run": proposer.run,
            });
            if let Err(error) =
                self.record(audit::AGENT_REFUSED, None, Some(&proposer.agent), fields).await
            {
                tracing::warn!(error = %error, "could not record a refused suggestion");
            }
        }
        result
    }

    async fn propose_checked(
        &self,
        definition: AgentDefinition,
        proposer: &AgentProposer,
    ) -> Result<AgentProposal> {
        let inner = &self.inner;
        let Some(approvals) = inner.approvals.clone() else {
            return Err(refused("this kernel has no inbox, so no agent can be suggested"));
        };
        let Some(factory) = inner.factory.clone() else {
            return Err(refused("this kernel builds no agents of a person's own"));
        };
        definition.validate()?;
        let records = inner.store.list().await?;
        let digest = definition.digest();
        let mine = |record: &&AgentRecord| {
            matches!(&record.origin, AgentOrigin::Suggested { agent, .. } if *agent == proposer.agent)
                && record.state == AgentRecordState::Proposed
        };
        if let Some(waiting) = records.iter().filter(mine).find(|record| record.digest == digest) {
            let request_id = suggestion_request_id(waiting);
            let risk = match approvals.get(&request_id).await? {
                Some(request) => request.risk,
                None => self.assess(waiting).await.risk,
            };
            return Ok(AgentProposal { agent: waiting.clone(), request_id, risk, merged: true });
        }
        if records.iter().filter(mine).count() >= MAX_OPEN_SUGGESTIONS {
            return Err(refused(format!(
                "{MAX_OPEN_SUGGESTIONS} suggested agents already wait for a person; answer them \
                 first"
            )));
        }
        let (unattended, area) = self.suggesting_process(proposer).await?;
        if let Some(area) = area
            && area != definition.area
        {
            return Err(refused(format!(
                "a suggested agent works in its suggester's area `{area}`, not `{}`",
                definition.area
            )));
        }
        self.check_names(&definition, &records, None, None)?;
        let partition = self.area(&definition.area).await?;
        let over_area = area_limits(&definition, &partition);
        if !over_area.is_empty() {
            return Err(refused(format!("it would exceed its area: {}", over_area.join("; "))));
        }
        let Some(ceiling) = factory.ceiling(&proposer.agent, &proposer.session).await? else {
            return Err(refused(format!("{} may not suggest agents", proposer.agent)));
        };
        let over = over_ceiling(&definition, &ceiling);
        if !over.is_empty() {
            return Err(refused(format!(
                "{} can't give {} {}: a suggested agent never does more than its suggester \
                 (spec 3.3 S-I12)",
                proposer.agent,
                definition.display_name,
                over.join(", ")
            )));
        }
        let id = self.allocate_id(&definition.display_name, &records, None);
        let origin = AgentOrigin::Suggested {
            agent: proposer.agent.clone(),
            session: proposer.session.clone(),
            run: proposer.run.clone(),
            unattended,
        };
        let now = inner.sup.clock.now();
        let record = AgentRecord::new(
            id,
            definition.clone(),
            AgentRecordState::Proposed,
            origin,
            SubjectRef::agent(&proposer.agent),
            now,
        );
        let assessment = self.assess(&record).await;
        let request_id = suggestion_request_id(&record);
        self.record(
            audit::AGENT_PROPOSED,
            None,
            Some(&proposer.agent),
            json!({
                "id": record.id,
                "digest": record.digest,
                "risk": assessment.risk,
                "money": assessment.money,
                "area": record.area,
                "suggestingAgent": proposer.agent,
                "session": proposer.session,
                "run": proposer.run,
                "unattended": unattended,
            }),
        )
        .await?;
        if !inner.store.put_if_version(record.clone(), 0).await? {
            return Err(agent_error(
                ErrorCategory::Conflict,
                AGENT_OUT_OF_DATE_CODE,
                format!("the id `{}` was taken meanwhile; suggest it again", record.id),
            ));
        }
        let args = serde_json::to_value(&definition)?;
        let call = ToolConfirmationRequest::new(AGENT_CREATE_TOOL_NAME, args.clone());
        let scope = ApprovalScope::new(
            inner.sup.app_name.clone(),
            proposer.user.clone(),
            proposer.session.clone(),
        );
        // The person or the operators decide; the default forbid_self_approval keeps the
        // suggesting agent from approving its own suggestion.
        let policy = ApprovalPolicy::new([
            ApproverSelector::human(proposer.user.clone()),
            ApproverSelector::group(OPERATOR_GROUP),
        ]);
        let mut request = ApprovalRequest::new(request_id.clone(), scope, &call, policy)
            .with_redacted_args(args)
            .with_requester(SubjectRef::agent(&proposer.agent))
            .with_summary(format!("Add an agent called {}", record.display_name))
            .with_risk(assessment.risk)
            .with_allowed_actions([ApprovalAction::Approve, ApprovalAction::Reject]);
        if let Some(run) = &proposer.run {
            request = request.with_run_id(run.clone());
        }
        approvals.submit(request).await?;
        inner.sup.publish(KernelEvent::Agent {
            id: record.id.clone(),
            change: AgentChange::Proposed,
            reason: None,
        });
        Ok(AgentProposal { agent: record, request_id, risk: assessment.risk, merged: false })
    }

    /// Whether the suggesting run is work the person did not start at a screen: a process
    /// that is not interactive, or a turn a chat message started (decision 20); and the area
    /// it runs in.
    async fn suggesting_process(
        &self,
        proposer: &AgentProposer,
    ) -> Result<(bool, Option<PartitionId>)> {
        let Some(pid) = &proposer.run else { return Ok((false, None)) };
        let sup = &self.inner.sup;
        let Some(run) = sup.runs.get(pid).await? else { return Ok((false, None)) };
        let meta = match sup.processes.get(pid).await? {
            Some(meta) => meta,
            None => sup.inferred_meta(&run),
        };
        let chat = run.input.get("channel").is_some();
        Ok((chat || meta.kind != ProcessKind::Interactive, Some(meta.partition)))
    }

    /// The partition `id`; the default area exists even when the table holds no record of
    /// it, without limits of its own.
    async fn area(&self, id: &PartitionId) -> Result<Partition> {
        match self.inner.sup.partitions.get(id).await? {
            Some(partition) => Ok(partition),
            None if id.as_str() == PartitionId::DEFAULT => {
                Ok(Partition::new(PartitionId::DEFAULT, PartitionId::DEFAULT)
                    .with_mode(PartitionMode::Auto))
            }
            None => Err(invalid(format!("there is no area named `{id}`"))),
        }
    }

    /// Refuses a name another agent holds, a reserved one, or the primary agent's.
    fn check_names(
        &self,
        definition: &AgentDefinition,
        records: &[AgentRecord],
        own: Option<&str>,
        replaces: Option<&str>,
    ) -> Result<()> {
        let name = definition.display_name.trim();
        let mut reserved = vec![RESERVED_AGENT_NAME.to_string(), self.inner.sup.app_name.clone()];
        if let Some(factory) = &self.inner.factory {
            reserved.extend(factory.reserved_names());
        }
        if reserved.iter().any(|taken| taken.trim().eq_ignore_ascii_case(name)) {
            return Err(invalid(format!("\"{name}\" is reserved; choose another name")));
        }
        let lower = name.to_lowercase();
        if let Some(record) = records.iter().find(|record| {
            record.state.holds_its_name()
                && Some(record.id.as_str()) != own
                && Some(record.id.as_str()) != replaces
                && record.display_name.trim().to_lowercase() == lower
        }) {
            return Err(invalid(if record.state == AgentRecordState::Proposed {
                format!(
                    "an agent called {name} is waiting for your answer; add or refuse that \
                     suggestion first"
                )
            } else {
                format!(
                    "you already have an agent called {}; choose another name",
                    record.display_name
                )
            }));
        }
        Ok(())
    }

    /// The id a new agent named `name` gets: never one any record ever had, a reserved one,
    /// a target the host registered, or a workflow it runs (S-I14).
    fn allocate_id(&self, name: &str, records: &[AgentRecord], reuse: Option<&str>) -> String {
        let inner = &self.inner;
        let ever: BTreeSet<&str> = records
            .iter()
            .map(|record| record.id.as_str())
            .filter(|id| Some(*id) != reuse)
            .collect();
        let reserved: BTreeSet<String> = inner
            .factory
            .as_ref()
            .map(|factory| factory.reserved_ids().into_iter().collect())
            .unwrap_or_default();
        agent_id_for(name, |id| {
            ever.contains(id)
                || reserved.contains(id)
                || inner.statics.contains(id)
                || id == inner.sup.app_name
                || inner.workflows.as_ref().is_some_and(|workflows| workflows.has_workflow(id))
        })
    }

    /// The effect classes of each driver `record` uses, from the driver store.
    async fn driver_classes(&self, record: &AgentRecord) -> Vec<(String, Vec<EffectClass>)> {
        #[cfg(feature = "drivers")]
        if let Some(drivers) = &self.inner.drivers {
            let mut classes = Vec::new();
            for driver in &record.uses.drivers {
                if let Ok(Some(stored)) = drivers.store().get(driver).await {
                    classes
                        .push((driver.clone(), stored.manifest.tools.values().copied().collect()));
                }
            }
            return classes;
        }
        let _ = record;
        Vec::new()
    }

    /// The factory's assessment of `record` with the kernel's own on top: the higher risk
    /// wins.
    async fn assess(&self, record: &AgentRecord) -> AgentAssessment {
        let drivers = self.driver_classes(record).await;
        let own = kernel_assessment(&record.definition(), &record.origin, &drivers);
        match &self.inner.factory {
            Some(factory) => factory.assess(record).merge(own),
            None => own,
        }
    }

    async fn plan(&self, action: &AgentAction) -> Result<Plan> {
        let inner = &self.inner;
        let now = inner.sup.clock.now();
        let digest = action.digest();
        match action {
            AgentAction::Create { definition, replaces } => {
                if inner.factory.is_none() {
                    return Err(refused(
                        "this kernel builds no agents of a person's own: its host registered no \
                         agent factory",
                    ));
                }
                let definition = definition.clone().normalized();
                definition.validate()?;
                let records = inner.store.list().await?;
                let replaced = match replaces {
                    Some(id) => match records.iter().find(|record| record.id == *id) {
                        Some(record) if record.state == AgentRecordState::Proposed => {
                            Some(record.clone())
                        }
                        _ => {
                            return Err(invalid(format!(
                                "`{id}` is not a suggestion waiting for an answer"
                            )));
                        }
                    },
                    None => None,
                };
                let replaced_id = replaced.as_ref().map(|record| record.id.as_str());
                self.check_names(&definition, &records, None, replaced_id)?;
                let partition = self.area(&definition.area).await?;
                let over = area_limits(&definition, &partition);
                if !over.is_empty() {
                    return Err(refused(format!("it would exceed its area: {}", over.join("; "))));
                }
                let id = self.allocate_id(&definition.display_name, &records, replaced_id);
                let caller_unknown = SubjectRef::human("");
                let next = AgentRecord::new(
                    id.clone(),
                    definition.clone(),
                    AgentRecordState::Active,
                    AgentOrigin::ByHand,
                    caller_unknown,
                    now,
                );
                let assessment = self.assess(&next).await;
                let mut preview = AgentPreview::new(id, "create", digest, assessment);
                preview.widens = true;
                preview.now = Some(definition);
                Ok(Plan { preview, current: None, next: Some(next), replaces: replaced })
            }
            AgentAction::Change { id, expected_version, definition } => {
                let current = self.current(id, Some(*expected_version)).await?;
                if !current.state.is_hosted() {
                    return Err(invalid(format!(
                        "{} is {}, so it cannot be changed",
                        current.display_name, current.state
                    )));
                }
                let definition = definition.clone().normalized();
                if definition.area != current.area {
                    return Err(invalid(format!(
                        "an agent's area is fixed; add a new agent in `{}` instead",
                        definition.area
                    )));
                }
                definition.validate()?;
                let records = inner.store.list().await?;
                self.check_names(&definition, &records, Some(id), None)?;
                let partition = self.area(&current.area).await?;
                let over = area_limits(&definition, &partition);
                if !over.is_empty() {
                    return Err(refused(format!("it would exceed its area: {}", over.join("; "))));
                }
                let was = current.definition();
                let mut next = current.clone().with_definition(definition.clone());
                next.version = current.version + 1;
                next.updated_at = now;
                let more = widened(&was, &definition);
                let assessment = self.assess(&next).await;
                let mut preview = AgentPreview::new(id, "change", digest, assessment);
                if more.is_empty() {
                    preview.risk = ApprovalRisk::Low;
                }
                preview.widens = !more.is_empty();
                preview.fields = changed_fields(&was, &definition);
                preview.was = Some(was);
                preview.now = Some(definition);
                Ok(Plan { preview, current: Some(current), next: Some(next), replaces: None })
            }
            AgentAction::Pause { id } | AgentAction::Resume { id } => {
                let current = self.current(id, None).await?;
                let pausing = matches!(action, AgentAction::Pause { .. });
                let from =
                    if pausing { AgentRecordState::Active } else { AgentRecordState::Paused };
                if current.state != from {
                    return Err(invalid(format!(
                        "{} is {}, so it cannot be {}",
                        current.display_name,
                        current.state,
                        if pausing { "paused" } else { "resumed" }
                    )));
                }
                let mut next = current.clone().with_state(if pausing {
                    AgentRecordState::Paused
                } else {
                    AgentRecordState::Active
                });
                next.version = current.version + 1;
                next.updated_at = now;
                let assessment = AgentAssessment::new(ApprovalRisk::Low);
                let preview = AgentPreview::new(id, action.name(), digest, assessment);
                Ok(Plan { preview, current: Some(current), next: Some(next), replaces: None })
            }
            AgentAction::Remove { id, expected_version } => {
                let current = self.current(id, Some(*expected_version)).await?;
                if !matches!(
                    current.state,
                    AgentRecordState::Active
                        | AgentRecordState::Paused
                        | AgentRecordState::Removing
                ) {
                    return Err(invalid(format!(
                        "{} is {}, so it cannot be removed",
                        current.display_name, current.state
                    )));
                }
                let ended = self.ended_work(&current).await?;
                let assessment = AgentAssessment::new(ApprovalRisk::Low);
                let mut preview = AgentPreview::new(id, "remove", digest, assessment);
                preview.ended = Some(ended);
                Ok(Plan { preview, current: Some(current), next: None, replaces: None })
            }
        }
    }

    /// The stored record `id`, at `version` when one is asked.
    async fn current(&self, id: &str, version: Option<u64>) -> Result<AgentRecord> {
        let current = self.inner.store.get(id).await?.ok_or_else(|| not_found(id))?;
        if let Some(version) = version
            && current.version != version
        {
            return Err(out_of_date(&current, version));
        }
        Ok(current)
    }

    async fn create_planned(&self, caller: &AgentCaller, plan: Plan) -> Result<AgentOutcome> {
        let inner = &self.inner;
        let mut record = plan.next.expect("a create plans its record");
        record.created_by = caller.by.clone();
        record.approved_by = Some(caller.by.clone());
        self.record(
            audit::AGENT_CREATED,
            Some(caller),
            None,
            json!({
                "id": record.id,
                "version": record.version,
                "digest": record.digest,
                "risk": plan.preview.risk,
                "money": plan.preview.money,
                "origin": record.origin,
                "replaces": plan.replaces.as_ref().map(|replaced| &replaced.id),
            }),
        )
        .await?;
        // Over the suggestion it replaces when it keeps that suggestion's id.
        let expected = match &plan.replaces {
            Some(replaced) if replaced.id == record.id => replaced.version,
            _ => 0,
        };
        if expected > 0 {
            record.version = expected + 1;
        }
        if !inner.store.put_if_version(record.clone(), expected).await? {
            return Err(agent_error(
                ErrorCategory::Conflict,
                AGENT_OUT_OF_DATE_CODE,
                format!("the id `{}` was taken meanwhile; add the agent again", record.id),
            ));
        }
        if let Some(replaced) = &plan.replaces {
            self.end_suggestion(replaced, "You changed it and added it").await;
            if replaced.id != record.id {
                let _ = inner.store.delete(&replaced.id).await;
            }
        }
        inner.sup.publish(KernelEvent::Agent {
            id: record.id.clone(),
            change: AgentChange::Created,
            reason: None,
        });
        self.host(&record).await;
        Ok(AgentOutcome::Created { agent: record })
    }

    async fn change_planned(&self, caller: &AgentCaller, plan: Plan) -> Result<AgentOutcome> {
        let inner = &self.inner;
        let current = plan.current.expect("a change plans from the stored record");
        let next = plan.next.expect("a change plans its record");
        self.record(
            audit::AGENT_CHANGED,
            Some(caller),
            None,
            json!({
                "id": next.id,
                "version": next.version,
                "fields": plan.preview.fields,
                "from": plan.preview.was,
                "to": plan.preview.now,
                "risk": plan.preview.risk,
                "widens": plan.preview.widens,
            }),
        )
        .await?;
        // A change that lets the agent do more ends its waiting decisions first, as loosening
        // a mode does (spec 3.3 §5.7).
        if plan.preview.widens {
            let ended = self.ended_work(&current).await?;
            let note = format!("{} was changed to do more", current.display_name);
            self.cancel_decisions(&ended.decisions, &note).await;
        }
        if !inner.store.put_if_version(next.clone(), current.version).await? {
            let now = inner.store.get(&current.id).await?.ok_or_else(|| not_found(&current.id))?;
            return Err(out_of_date(&now, current.version));
        }
        inner.sup.publish(KernelEvent::Agent {
            id: next.id.clone(),
            change: AgentChange::Changed,
            reason: Some(plan.preview.fields.join(", ")),
        });
        self.host(&next).await;
        Ok(AgentOutcome::Changed { agent: next })
    }

    async fn pause_planned(&self, caller: &AgentCaller, plan: Plan) -> Result<AgentOutcome> {
        let inner = &self.inner;
        let current = plan.current.expect("a pause plans from the stored record");
        let next = plan.next.expect("a pause plans its record");
        self.record(audit::AGENT_PAUSED, Some(caller), None, json!({ "id": next.id })).await?;
        if !inner.store.put_if_version(next.clone(), current.version).await? {
            let now = inner.store.get(&current.id).await?.ok_or_else(|| not_found(&current.id))?;
            return Err(out_of_date(&now, current.version));
        }
        inner.targets.set_paused(&next.id, true);
        self.suspend_work(&next).await;
        inner.sup.publish(KernelEvent::Agent {
            id: next.id.clone(),
            change: AgentChange::Paused,
            reason: None,
        });
        Ok(AgentOutcome::Paused { agent: next })
    }

    async fn resume_planned(&self, caller: &AgentCaller, plan: Plan) -> Result<AgentOutcome> {
        let inner = &self.inner;
        let current = plan.current.expect("a resume plans from the stored record");
        let next = plan.next.expect("a resume plans its record");
        self.record(audit::AGENT_RESUMED, Some(caller), None, json!({ "id": next.id })).await?;
        if !inner.store.put_if_version(next.clone(), current.version).await? {
            let now = inner.store.get(&current.id).await?.ok_or_else(|| not_found(&current.id))?;
            return Err(out_of_date(&now, current.version));
        }
        inner.targets.set_paused(&next.id, false);
        let paused = Suspension::Paused { agent: next.id.clone() };
        // A queued process the pause caught before its claim runs at its claim again; a
        // parked one is woken, and a sleeper among them sleeps on its own wake set again.
        let filter = ProcessFilter::default().target(next.id.clone()).states([RunState::Queued]);
        for process in inner.processes.ps(filter).await.unwrap_or_default() {
            if process.meta.suspended_by.as_ref() == Some(&paused) && process.meta.wake.is_none() {
                let mut meta = process.meta;
                meta.suspended_by = None;
                meta.suspend_requested = false;
                inner.sup.put_meta(meta).await;
            }
        }
        inner.sup.wake_suspended(&paused).await;
        inner.sup.nudge();
        inner.sup.publish(KernelEvent::Agent {
            id: next.id.clone(),
            change: AgentChange::Resumed,
            reason: None,
        });
        Ok(AgentOutcome::Resumed { agent: next })
    }

    async fn remove_planned(&self, caller: &AgentCaller, plan: Plan) -> Result<AgentOutcome> {
        let inner = &self.inner;
        let current = plan.current.expect("a removal plans from the stored record");
        let ended = plan.preview.ended.clone().unwrap_or_default();
        self.record(
            audit::AGENT_REMOVING,
            Some(caller),
            None,
            json!({ "id": current.id, "ended": ended }),
        )
        .await?;
        let removing = if current.state == AgentRecordState::Removing {
            current.clone()
        } else {
            let mut removing = current.clone().with_state(AgentRecordState::Removing);
            removing.version = current.version + 1;
            removing.updated_at = inner.sup.clock.now();
            if !inner.store.put_if_version(removing.clone(), current.version).await? {
                let now =
                    inner.store.get(&current.id).await?.ok_or_else(|| not_found(&current.id))?;
                return Err(out_of_date(&now, current.version));
            }
            inner.sup.publish(KernelEvent::Agent {
                id: removing.id.clone(),
                change: AgentChange::Removing,
                reason: None,
            });
            removing
        };
        let (tombstone, ended) = self.finish_removal(&removing, Some(caller)).await?;
        Ok(AgentOutcome::Removed { agent: tombstone, ended })
    }

    /// Ends what the `Removing` agent `record` had, step by step, each step safe to repeat,
    /// then writes its tombstone (spec 3.3 §5.7, S-I16): its processes (routines and
    /// sleepers included) are killed, its waiting decisions cancelled, its follow-ups
    /// withdrawn, its rules revoked, its target retired.
    async fn finish_removal(
        &self,
        record: &AgentRecord,
        caller: Option<&AgentCaller>,
    ) -> Result<(AgentRecord, EndedWork)> {
        let inner = &self.inner;
        let mut ended = self.ended_work(record).await?;
        inner.targets.set_paused(&record.id, false);
        for pid in ended.work.iter().chain(&ended.routines) {
            if let Err(error) =
                inner.processes.kill(pid, &format!("{} was removed", record.display_name)).await
            {
                tracing::debug!(process.pid = %pid, error = %error, "could not kill a removed agent's process");
            }
        }
        let at = record.updated_at.with_timezone(&chrono::Local).format("%H:%M");
        let note = format!("You removed {} at {at}", record.display_name);
        self.cancel_decisions(&ended.decisions, &note).await;
        let by = caller.map_or_else(|| SubjectRef::service("kernel"), |caller| caller.by.clone());
        for item in &ended.follow_ups {
            if let Err(error) = inner.agenda.reject(item, &by, Some(&note)).await {
                tracing::debug!(agenda.item = %item, error = %error, "could not withdraw a removed agent's follow-up");
            }
        }
        if let Some(factory) = &inner.factory {
            ended.rules = factory.retired(record).await?;
        }
        inner.targets.retire(&record.id);
        inner.local.lock().unwrap_or_else(|e| e.into_inner()).cant_start.remove(&record.id);
        let mut tombstone = record.clone().with_state(AgentRecordState::Removed);
        tombstone.version = record.version + 1;
        tombstone.updated_at = inner.sup.clock.now();
        self.record(audit::AGENT_REMOVED, caller, None, json!({ "id": record.id, "ended": ended }))
            .await?;
        if !inner.store.put_if_version(tombstone.clone(), record.version).await? {
            // Another replica finished it first.
            let now = inner.store.get(&record.id).await?.ok_or_else(|| not_found(&record.id))?;
            return Ok((now, ended));
        }
        inner.sup.publish(KernelEvent::Agent {
            id: record.id.clone(),
            change: AgentChange::Removed,
            reason: None,
        });
        Ok((tombstone, ended))
    }

    /// What removing `record` ends now: its unfinished processes, split into work and
    /// routines, the decisions its runs wait on, and its open follow-ups.
    async fn ended_work(&self, record: &AgentRecord) -> Result<EndedWork> {
        let inner = &self.inner;
        let processes =
            inner.processes.ps(ProcessFilter::default().target(record.id.clone())).await?;
        let pids: BTreeSet<String> =
            processes.iter().map(|process| process.run.run_id.clone()).collect();
        let mut ended = EndedWork::default();
        let mut sessions = BTreeSet::new();
        for process in &processes {
            sessions.insert((
                process.run.app_name.clone(),
                process.run.user_id.clone(),
                crate::supervise::run_session(&process.run),
            ));
            if process.run.state.is_terminal() {
                continue;
            }
            let routine =
                process.meta.kind == ProcessKind::Scheduled || process.meta.wake.is_some();
            if routine {
                ended.routines.push(process.run.run_id.clone());
            } else {
                ended.work.push(process.run.run_id.clone());
            }
        }
        if let Some(approvals) = &inner.approvals {
            let agent = record.agent_id(&inner.sup.app_name);
            let mut decisions = BTreeSet::new();
            for (app, user, session) in sessions {
                let query =
                    InboxQuery::for_approver(SubjectRef::human(user.clone()), [OPERATOR_GROUP])
                        .with_app_name(app)
                        .with_user_id(user)
                        .with_session_id(session)
                        .with_limit(500);
                for request in approvals.inbox(query).await?.items {
                    let acting =
                        request.acting.as_ref().is_some_and(|acting| acting.agent.id == agent);
                    let run = request.run_id.as_ref().is_some_and(|run| pids.contains(run));
                    // A follow-up's own request ends with the follow-up, counted once.
                    let follow_up = request.tool_name == crate::agenda::AGENDA_TOOL_NAME;
                    if request.state == ApprovalState::Pending && (acting || run) && !follow_up {
                        decisions.insert(request.id);
                    }
                }
            }
            ended.decisions = decisions.into_iter().collect();
        }
        for item in inner.agenda.list(&AgendaFilter::default().open()).await? {
            if item.proposer().is_some_and(|pid| pids.contains(pid))
                && matches!(
                    item.state,
                    crate::agenda::AgendaState::Proposed | crate::agenda::AgendaState::Approved
                )
            {
                ended.follow_ups.push(item.id);
            }
        }
        Ok(ended)
    }

    /// Cancels the waiting decisions `ids` with `note`, so each shows "Ended without an
    /// answer", never a yes or a no.
    async fn cancel_decisions(&self, ids: &[String], note: &str) {
        let Some(approvals) = &self.inner.approvals else { return };
        for id in ids {
            if let Err(error) = approvals.cancel(id, Some(note.to_string())).await {
                tracing::debug!(approval.id = %id, error = %error, "could not end a decision");
            }
        }
    }

    /// Marks the unfinished processes of `record` paused: a running one stops at its next
    /// step, a queued or sleeping one is held until the agent is resumed.
    async fn suspend_work(&self, record: &AgentRecord) {
        let inner = &self.inner;
        let processes = match inner
            .processes
            .ps(ProcessFilter::default().target(record.id.clone()))
            .await
        {
            Ok(processes) => processes,
            Err(error) => {
                tracing::warn!(agent.id = %record.id, error = %error, "could not list a paused agent's processes");
                return;
            }
        };
        let paused = Suspension::Paused { agent: record.id.clone() };
        for process in processes.iter().filter(|process| !process.run.state.is_terminal()) {
            if let Err(error) =
                inner.processes.suspend_as(&process.run.run_id, paused.clone()).await
            {
                tracing::debug!(process.pid = %process.run.run_id, error = %error, "could not suspend a paused agent's process");
            }
        }
    }

    /// Ends the inbox request of the suggestion `record` with `note`.
    async fn end_suggestion(&self, record: &AgentRecord, note: &str) {
        let Some(approvals) = &self.inner.approvals else { return };
        let id = suggestion_request_id(record);
        if let Ok(Some(request)) = approvals.get(&id).await
            && request.state == ApprovalState::Pending
            && let Err(error) = approvals.cancel(&id, Some(note.to_string())).await
        {
            tracing::debug!(approval.id = %id, error = %error, "could not end a suggestion");
        }
    }

    /// Builds and hosts `record` when it is active or paused and this replica does not host
    /// its version yet (spec 3.3 §5.7): the factory builds it, the kernel refuses a runner
    /// whose kill switch is not its own (S-I15) or whose root agent is not the record's id,
    /// attaches its progress source and the ledger, and swaps it in. A build error keeps the
    /// record and shows "Can't start".
    async fn host(&self, record: &AgentRecord) {
        let inner = &self.inner;
        if !record.state.is_hosted() {
            return;
        }
        inner.targets.expect(&record.id);
        inner.targets.set_paused(&record.id, record.state == AgentRecordState::Paused);
        if inner.targets.hosted(&record.id).is_some_and(|hosted| hosted.version == record.version) {
            return;
        }
        let failed = inner
            .local
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cant_start
            .get(&record.id)
            .is_some_and(|(version, _)| *version == record.version);
        if failed {
            return;
        }
        match self.build(record).await {
            Ok(()) => {
                inner.local.lock().unwrap_or_else(|e| e.into_inner()).cant_start.remove(&record.id);
                inner.sup.publish(KernelEvent::Agent {
                    id: record.id.clone(),
                    change: AgentChange::Hosted,
                    reason: None,
                });
            }
            Err(reason) => {
                tracing::warn!(agent.id = %record.id, reason = %reason, "an added agent cannot start");
                inner
                    .local
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .cant_start
                    .insert(record.id.clone(), (record.version, reason.clone()));
                inner.sup.publish(KernelEvent::Agent {
                    id: record.id.clone(),
                    change: AgentChange::CantStart,
                    reason: Some(reason),
                });
            }
        }
    }

    async fn build(&self, record: &AgentRecord) -> std::result::Result<(), String> {
        let inner = &self.inner;
        if inner.statics.contains(&record.id) || record.validate().is_err() {
            return Err(format!("`{}` is not an id an added agent may have", record.id));
        }
        let Some(factory) = &inner.factory else {
            return Err("this kernel builds no agents of a person's own".to_string());
        };
        let hosted = factory.build(record).await.map_err(|error| AdkError::from(error).message)?;
        if !hosted.runner.governance().same_as(&inner.sup.governance) {
            return Err(
                "its kill switch is not the kernel's, so Stop all would not reach it (spec 3.3 \
                 S-I15)"
                    .to_string(),
            );
        }
        if hosted.runner.root_agent_name() != record.id {
            return Err(format!(
                "its root agent is `{}`, not `{}`, so it would act as another agent",
                hosted.runner.root_agent_name(),
                record.id
            ));
        }
        if let Some(progress) = &hosted.progress {
            progress.attach(ProcessHandle::new(&inner.sup));
            #[cfg(feature = "proc")]
            if let Some(ledger) = &inner.ledger {
                progress.attach_ledger(Arc::new(ledger.clone()));
            }
        }
        inner.targets.host(
            &record.id,
            hosted.runner,
            Added {
                area: record.area.clone(),
                progress: hosted.progress,
                version: record.version,
                daily_limit: record.daily_limit_micro_usd,
            },
        );
        Ok(())
    }

    /// Reconciles this replica's hosted agents with the store (spec 3.3 §5.7): builds and
    /// hosts active and paused agents not hosted at their version, retires the rest, and
    /// finishes interrupted removals.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub(crate) async fn reconcile(&self) -> Result<()> {
        let inner = &self.inner;
        let records = inner.store.list().await?;
        for record in &records {
            match record.state {
                AgentRecordState::Active | AgentRecordState::Paused => self.host(record).await,
                AgentRecordState::Removing => {
                    if let Err(error) = self.finish_removal(record, None).await {
                        tracing::warn!(agent.id = %record.id, error = %error, "could not finish removing an agent; the next tick retries");
                    }
                }
                AgentRecordState::Proposed => self.settle(record).await,
                AgentRecordState::Removed | AgentRecordState::Refused => {}
            }
        }
        // Read again: a suggestion settled above is live now.
        let live: BTreeSet<String> = inner
            .store
            .list()
            .await?
            .into_iter()
            .filter(|record| record.state.is_hosted())
            .map(|record| record.id)
            .collect();
        for id in inner.targets.hosted_ids() {
            if !live.contains(&id) {
                inner.targets.retire(&id);
            }
        }
        Ok(())
    }

    /// One scheduler tick (spec 3.3 §5.7): applies the inbox's answers to suggestions and
    /// reconciles hosting with the store.
    pub(crate) async fn tick(&self) {
        if let Err(error) = self.reconcile().await {
            tracing::debug!(error = %error, "agent table tick failed");
        }
    }

    /// Applies the inbox's answer to the suggestion `record`: a person's approval of the
    /// same definition, still within the suggester's and the area's limits, adds the agent;
    /// any other approval is logged and ignored; a rejection deletes the suggestion, whose
    /// id was never used.
    async fn settle(&self, record: &AgentRecord) {
        let inner = &self.inner;
        let Some(approvals) = &inner.approvals else { return };
        let request_id = suggestion_request_id(record);
        let request = match approvals.get(&request_id).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(approval.id = %request_id, error = %error, "could not read a suggestion's request");
                return;
            }
        };
        match request.state {
            ApprovalState::Pending => {}
            ApprovalState::Approved => {
                let approver = request.decisions.last().map(|decision| decision.approver.clone());
                match approver {
                    Some(approver) if approver.kind == SubjectKind::Human => {
                        if let Err(error) = self.approve(record, &request, approver).await {
                            tracing::warn!(agent.id = %record.id, error = %error, "could not add an approved agent; the next tick retries");
                        }
                    }
                    other => {
                        let first = inner
                            .local
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .ignored
                            .insert(request_id.clone());
                        if first {
                            tracing::warn!(agent.id = %record.id, approver = ?other, "a suggested agent's approval by someone other than a person was ignored (spec 3.3 S-I11)");
                            let fields = json!({
                                "id": record.id,
                                "reason": "an approver who is not a person",
                                "approver": other,
                            });
                            let _ = self.record(audit::AGENT_REFUSED, None, None, fields).await;
                        }
                    }
                }
            }
            ApprovalState::Rejected
            | ApprovalState::Responded
            | ApprovalState::Cancelled
            | ApprovalState::Expired => {
                let reason = request
                    .note
                    .clone()
                    .or_else(|| request.decisions.last().and_then(|d| d.reason.clone()))
                    .unwrap_or_else(|| "not added".to_string());
                let fields = json!({ "id": record.id, "reason": reason, "approvalId": request.id });
                if self.record(audit::AGENT_REFUSED, None, None, fields).await.is_ok()
                    && matches!(inner.store.delete(&record.id).await, Ok(true))
                {
                    inner.sup.publish(KernelEvent::Agent {
                        id: record.id.clone(),
                        change: AgentChange::Refused,
                        reason: Some(reason),
                    });
                }
            }
        }
    }

    /// Adds the approved suggestion `record`, or ends it refused.
    async fn approve(
        &self,
        record: &AgentRecord,
        request: &ApprovalRequest,
        approver: SubjectRef,
    ) -> Result<()> {
        let inner = &self.inner;
        let approved: Option<AgentDefinition> =
            serde_json::from_value(request.args_redacted.clone()).ok();
        let mut refusal = match approved {
            Some(definition) if definition.digest() == record.digest => None,
            _ => Some("the approved definition is not the one suggested".to_string()),
        };
        if refusal.is_none()
            && let AgentOrigin::Suggested { agent, session, .. } = &record.origin
        {
            let definition = record.definition();
            let ceiling = match &inner.factory {
                Some(factory) => factory.ceiling(agent, session).await?,
                None => None,
            };
            refusal = match ceiling {
                None => Some(format!("{agent} may no longer suggest agents")),
                Some(ceiling) => {
                    let over = over_ceiling(&definition, &ceiling);
                    (!over.is_empty()).then(|| {
                        format!(
                            "the agent that suggested it can no longer give it {}",
                            over.join(", ")
                        )
                    })
                }
            };
            if refusal.is_none() {
                let over = area_limits(&definition, &self.area(&record.area).await?);
                if !over.is_empty() {
                    refusal = Some(format!("it would exceed its area: {}", over.join("; ")));
                }
            }
        }
        let now = inner.sup.clock.now();
        if let Some(reason) = refusal {
            self.record(
                audit::AGENT_REFUSED,
                None,
                None,
                json!({ "id": record.id, "reason": reason, "approvalId": request.id }),
            )
            .await?;
            let mut refused_record = record.clone().with_state(AgentRecordState::Refused);
            refused_record.version = record.version + 1;
            refused_record.updated_at = now;
            if inner.store.put_if_version(refused_record, record.version).await? {
                let text = format!("{} wasn't added: {reason}.", record.display_name);
                self.notify_session(record, &request.scope, &text).await;
                inner.sup.publish(KernelEvent::Agent {
                    id: record.id.clone(),
                    change: AgentChange::Refused,
                    reason: Some(reason),
                });
            }
            return Ok(());
        }
        let assessment = self.assess(record).await;
        let mut active =
            record.clone().with_state(AgentRecordState::Active).with_approved_by(approver.clone());
        active.version = record.version + 1;
        active.updated_at = now;
        let actor = AgentCaller::new(approver, "inbox", "approval");
        self.record(
            audit::AGENT_CREATED,
            Some(&actor),
            None,
            json!({
                "id": active.id,
                "version": active.version,
                "digest": active.digest,
                "risk": assessment.risk,
                "money": assessment.money,
                "origin": active.origin,
                "approvalId": request.id,
            }),
        )
        .await?;
        if !inner.store.put_if_version(active.clone(), record.version).await? {
            // Another replica added it first.
            return Ok(());
        }
        inner.sup.publish(KernelEvent::Agent {
            id: active.id.clone(),
            change: AgentChange::Created,
            reason: None,
        });
        self.host(&active).await;
        Ok(())
    }

    /// Writes `text` as a system notice in the session the suggestion was made in.
    async fn notify_session(&self, record: &AgentRecord, scope: &ApprovalScope, text: &str) {
        let Some(sessions) = &self.inner.sup.sessions else { return };
        let identity = match (
            adk_core::AppName::try_from(scope.app_name.as_str()),
            adk_core::UserId::try_from(scope.user_id.as_str()),
            adk_core::SessionId::try_from(scope.session_id.as_str()),
        ) {
            (Ok(app), Ok(user), Ok(session)) => adk_core::AdkIdentity::new(app, user, session),
            _ => return,
        };
        let event =
            adk_core::history::system_notice_event(format!("kernel-agent-{}", record.id), text);
        if let Err(error) = sessions
            .append_event_for_identity(adk_session::AppendEventRequest { identity, event })
            .await
        {
            tracing::debug!(agent.id = %record.id, error = %error, "could not tell the suggesting session");
        }
    }

    /// Writes a `kernel.agent.*` record before the change it records (S-I18, K-I16).
    ///
    /// # Errors
    ///
    /// The trail's error under `AuditFailureMode::Block`; the change is then refused.
    async fn record(
        &self,
        action: &str,
        caller: Option<&AgentCaller>,
        agent: Option<&AgentId>,
        fields: Value,
    ) -> Result<()> {
        let sup = &self.inner.sup;
        let Some(trail) = &sup.audit else { return Ok(()) };
        let mut actor = match caller {
            Some(caller) => AuditActor::subject(caller.by.clone()),
            None => AuditActor::default(),
        };
        actor.agent = agent.cloned();
        actor.instance = Some(sup.kernel_id.clone());
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.to_string()),
            action,
        )
        .actor(actor);
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        if let Some(caller) = caller {
            draft = draft
                .ext("by", json!(caller.by))
                .ext("surface", json!(caller.surface))
                .ext("how", json!(caller.how))
                .ext("device", json!(caller.device))
                .ext("presence", json!(caller.presence))
                .ext("transport", json!(caller.transport))
                .ext("peerPid", json!(caller.peer_pid));
        }
        trail.record(draft).await.map_err(KernelError::Core)
    }

    /// Records a refused change; a failed record is logged, the refusal stands anyway.
    async fn record_refused(
        &self,
        caller: Option<&AgentCaller>,
        id: Option<&str>,
        name: Option<&str>,
        reason: &str,
    ) {
        let fields = json!({ "id": id, "name": name, "reason": reason });
        if let Err(error) = self.record(audit::AGENT_REFUSED, caller, None, fields).await {
            tracing::warn!(error = %error, "could not record a refused agent change");
        }
    }
}

fn definition_name(action: &AgentAction) -> Option<&str> {
    match action {
        AgentAction::Create { definition, .. } | AgentAction::Change { definition, .. } => {
            Some(definition.display_name.as_str())
        }
        _ => None,
    }
}

/// The refusal of a change made against `asked` when `current` is newer.
fn out_of_date(current: &AgentRecord, asked: u64) -> KernelError {
    let at = current.updated_at.with_timezone(&chrono::Local).format("%H:%M");
    agent_error(
        ErrorCategory::Conflict,
        AGENT_OUT_OF_DATE_CODE,
        format!(
            "{} was changed at {at} (version {}); this change was made against version \
             {asked}, so look at it again first",
            current.display_name, current.version
        ),
    )
}
