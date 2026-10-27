//! The catalogue of screen actions, the agenda they act on, and the one narrow scope a handler
//! gets (spec 3.3 §2 item 7, S-I4, S-I10, S-I11).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use adk_core::audit_log::AuditTrail;
use adk_core::principal::SubjectRef;
use adk_core::{ApprovalRouter, ApprovalStore, Clock, GovernanceControl, RunState, SystemClock};
use adk_kernel::{
    Agenda, AgendaFilter, AgendaHandle, AgendaItem, AgendaState, AgendaStateKind, Kernel,
    PartitionId, ProcessFilter, ProcessKind, ProcessTable, Wake, WakeSet,
};
use adk_session::SessionService;
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Local, Utc};
use serde_json::Value;

use super::policy::{VerbHints, VerbPolicy, VerbRouter};
use super::tools::{VerbClasses, VerbToolset};
use super::{
    ALLOWED_OBJECTS, GRANT_ID, VerbDone, VerbError, VerbSpec, VerbView, check_name, snooze, words,
};
use crate::agents::GrantedUse;
use crate::answers::AnswerLog;

/// The kernel's agenda as screen actions read and change it (spec 3.3 §2 item 7): over
/// `Kernel::agenda()` in process, or a host's own reach.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::verbs::{AgendaAccess, KernelAgenda};
///
/// # fn example(kernel: &adk_kernel::Kernel) {
/// let agenda: Arc<dyn AgendaAccess> = Arc::new(KernelAgenda::of(kernel));
/// # let _ = agenda;
/// # }
/// ```
#[async_trait]
pub trait AgendaAccess: Send + Sync {
    /// The open items of `subject` that are scheduled or running, in `area` when one is
    /// named, oldest first.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    async fn items(
        &self,
        subject: &str,
        area: Option<&PartitionId>,
    ) -> Result<Vec<AgendaItem>, VerbError>;

    /// The item `id`, whoever's it is.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    async fn item(&self, id: &str) -> Result<Option<AgendaItem>, VerbError>;

    /// Whether `item`'s process runs, or is queued to run next: about to start, or started. A
    /// process queued only to park on its wake has not started.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    async fn started(&self, item: &AgendaItem) -> Result<bool, VerbError>;

    /// Has item `id`'s sleeping process wake on `wake` instead, as `by` (`Agenda::schedule`).
    ///
    /// # Errors
    ///
    /// The kernel's refusal, such as an item whose process started, or the store's error.
    async fn reschedule(
        &self,
        id: &str,
        wake: WakeSet,
        by: &SubjectRef,
    ) -> Result<AgendaItem, VerbError>;
}

/// The agenda of a kernel in this process (spec 3.2 §7.5).
///
/// [`of`](Self::of) reads a running kernel; [`new`](Self::new) reads through an
/// [`AgendaHandle`] bound when the kernel is built, so a host can give the agents their tools
/// before the kernel that hosts them exists, without the tools keeping it alive.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::{AgendaHandle, InMemoryProcessStore, ProcessTable};
/// use adk_surface::verbs::KernelAgenda;
///
/// # fn example(runs: Arc<dyn adk_core::DurableRunStore>) {
/// let handle = AgendaHandle::new();
/// let processes = ProcessTable::new(runs, Arc::new(InMemoryProcessStore::new()));
/// let agenda = KernelAgenda::new(handle.clone(), processes);
/// // ... KernelBuilder::agenda_handle(handle) binds it when the kernel is built.
/// # let _ = agenda;
/// # }
/// ```
#[derive(Clone)]
pub struct KernelAgenda {
    agenda: Slot,
    processes: ProcessTable,
}

#[derive(Clone)]
enum Slot {
    Handle(AgendaHandle),
    Bound(Agenda),
}

impl fmt::Debug for KernelAgenda {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bound = match &self.agenda {
            Slot::Handle(handle) => handle.get().is_some(),
            Slot::Bound(_) => true,
        };
        f.debug_struct("KernelAgenda").field("bound", &bound).finish_non_exhaustive()
    }
}

impl KernelAgenda {
    /// The agenda of `kernel`.
    pub fn of(kernel: &Kernel) -> Self {
        Self { agenda: Slot::Bound(kernel.agenda().clone()), processes: kernel.processes().clone() }
    }

    /// The agenda of the kernel `handle` is bound to when it is built, with its processes in
    /// `processes`.
    pub fn new(handle: AgendaHandle, processes: ProcessTable) -> Self {
        Self { agenda: Slot::Handle(handle), processes }
    }

    fn agenda(&self) -> Result<Agenda, VerbError> {
        match &self.agenda {
            Slot::Bound(agenda) => Ok(agenda.clone()),
            Slot::Handle(handle) => handle.get().ok_or_else(|| {
                VerbError::refused("Coming up isn't available: the kernel is not running")
            }),
        }
    }
}

#[async_trait]
impl AgendaAccess for KernelAgenda {
    async fn items(
        &self,
        subject: &str,
        area: Option<&PartitionId>,
    ) -> Result<Vec<AgendaItem>, VerbError> {
        let mut filter =
            AgendaFilter::default().states([AgendaStateKind::Scheduled, AgendaStateKind::Running]);
        if let Some(area) = area {
            filter = filter.partition(area.clone());
        }
        let items = self.agenda()?.list(&filter).await?;
        Ok(items.into_iter().filter(|item| item.subject.id == subject).collect())
    }

    async fn item(&self, id: &str) -> Result<Option<AgendaItem>, VerbError> {
        Ok(self.agenda()?.get(id).await?)
    }

    async fn started(&self, item: &AgendaItem) -> Result<bool, VerbError> {
        let filter = ProcessFilter::default().partition(item.partition.clone());
        let processes = self.processes.ps(filter).await?;
        // A process queued with its wake set parks on it when it is claimed: it has not
        // started. One queued without a wake runs next.
        Ok(processes.iter().any(|process| {
            process.meta.agenda_item.as_deref() == Some(item.id.as_str())
                && process.meta.kind == ProcessKind::Proposed
                && match process.run.state {
                    RunState::Running => true,
                    RunState::Queued => process.meta.wake.is_none(),
                    _ => false,
                }
        }))
    }

    async fn reschedule(
        &self,
        id: &str,
        wake: WakeSet,
        by: &SubjectRef,
    ) -> Result<AgendaItem, VerbError> {
        Ok(self.agenda()?.schedule(id, wake, by).await?)
    }
}

/// No agenda: a process that only draws the cards of another process's actions.
struct NoAgenda;

impl NoAgenda {
    fn refused() -> VerbError {
        VerbError::refused("Coming up isn't available here — use the home")
    }
}

#[async_trait]
impl AgendaAccess for NoAgenda {
    async fn items(&self, _: &str, _: Option<&PartitionId>) -> Result<Vec<AgendaItem>, VerbError> {
        Err(Self::refused())
    }

    async fn item(&self, _: &str) -> Result<Option<AgendaItem>, VerbError> {
        Err(Self::refused())
    }

    async fn started(&self, _: &AgendaItem) -> Result<bool, VerbError> {
        Err(Self::refused())
    }

    async fn reschedule(
        &self,
        _: &str,
        _: WakeSet,
        _: &SubjectRef,
    ) -> Result<AgendaItem, VerbError> {
        Err(Self::refused())
    }
}

/// Runs one screen action (spec 3.3 §2 item 7).
///
/// A handler gets a [`VerbScope`] and nothing else: no approval service, agent table, rules,
/// modes or governance, so no action can answer a decision, change an agent, a rule, a mode or
/// Stop all, or resume work (S-I4, S-I10, S-I11).
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{VerbDone, VerbError, VerbHandler, VerbScope};
/// use serde_json::Value;
///
/// /// Lists the titles in Coming up.
/// struct Titles;
///
/// #[async_trait::async_trait]
/// impl VerbHandler for Titles {
///     async fn describe(&self, _scope: &VerbScope, _args: &Value) -> Result<String, VerbError> {
///         Ok("List what is coming up".into())
///     }
///
///     async fn run(&self, scope: &VerbScope, _args: &Value) -> Result<VerbDone, VerbError> {
///         let titles: Vec<String> = scope.items().await?.into_iter().map(|item| item.title).collect();
///         Ok(VerbDone::new("titles", titles.join(", ")))
///     }
/// }
/// ```
#[async_trait]
pub trait VerbHandler: Send + Sync {
    /// What the call with `args` would do, in kernel text read from the agenda, for the card's
    /// heading: "Move News digest to 19:00".
    ///
    /// # Errors
    ///
    /// The refusal the call would meet: something not found, not possible now, or arguments
    /// the action refuses. An agent's call that fails here is refused before any card.
    async fn describe(&self, scope: &VerbScope, args: &Value) -> Result<String, VerbError>;

    /// Runs the call with `args`.
    ///
    /// # Errors
    ///
    /// Why it was refused, or the kernel's error.
    async fn run(&self, scope: &VerbScope, args: &Value) -> Result<VerbDone, VerbError>;
}

/// What a handler may read and change: the caller's own agenda items — for an agent, only its
/// own area's — and moving one's wake later (spec 3.3 §2 item 7, S-I8).
///
/// It holds no handle to the approval service, the agent table, standing rules, modes or
/// governance:
///
/// ```rust,compile_fail
/// # async fn example(scope: adk_surface::verbs::VerbScope) {
/// // A handler cannot reach the inbox.
/// scope.approvals().decide("apr_1", 1, todo!()).await;
/// # }
/// ```
#[derive(Clone)]
pub struct VerbScope {
    subject: SubjectRef,
    area: Option<PartitionId>,
    agenda: Arc<dyn AgendaAccess>,
    now: DateTime<Utc>,
    offset: FixedOffset,
}

impl fmt::Debug for VerbScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerbScope")
            .field("subject", &self.subject)
            .field("area", &self.area)
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

impl VerbScope {
    /// The person whose items these are.
    pub fn subject(&self) -> &SubjectRef {
        &self.subject
    }

    /// The area an agent's call is confined to; `None` for a person, who sees every area.
    pub fn area(&self) -> Option<&PartitionId> {
        self.area.as_ref()
    }

    /// Now, as the registry's clock reads it.
    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }

    /// The offset times are shown at.
    pub fn offset(&self) -> FixedOffset {
        self.offset
    }

    /// `at` as a person reads it: "19:00" today, "Tue 18:00" on another day.
    pub fn time_text(&self, at: DateTime<Utc>) -> String {
        let local = at.with_timezone(&self.offset);
        let today = self.now.with_timezone(&self.offset).date_naive();
        if local.date_naive() == today {
            local.format("%H:%M").to_string()
        } else {
            local.format("%a %H:%M").to_string()
        }
    }

    /// The caller's scheduled and running items, oldest first.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    pub async fn items(&self) -> Result<Vec<AgendaItem>, VerbError> {
        self.agenda.items(&self.subject.id, self.area.as_ref()).await
    }

    /// The item `id`, when it is the caller's (and in the agent's area).
    ///
    /// # Errors
    ///
    /// [`VerbError::NotFound`] for an item that does not exist or is not the caller's — the
    /// same answer, so a caller learns nothing of other people's items (S-I8).
    pub async fn item(&self, id: &str) -> Result<AgendaItem, VerbError> {
        let item = self.agenda.item(id).await?;
        item.filter(|item| self.sees(item)).ok_or_else(|| VerbError::not_found(id))
    }

    fn sees(&self, item: &AgendaItem) -> bool {
        item.subject.id == self.subject.id
            && self.area.as_ref().is_none_or(|area| *area == item.partition)
    }

    /// Whether `item`'s process is queued or running.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    pub async fn started(&self, item: &AgendaItem) -> Result<bool, VerbError> {
        self.agenda.started(item).await
    }

    /// Moves item `id`'s one wake from `from` to the later `to`, the only change a screen
    /// action makes: the item stays scheduled; only when it wakes changes.
    ///
    /// # Errors
    ///
    /// [`VerbError::NotFound`] for an item not the caller's; [`VerbError::Refused`] when it is
    /// not scheduled, does not wait for one time, does not wait for `from` any more ("It moved
    /// since you saw this"), `to` is not later than `from`, it ends before `to`, or its process
    /// started; the kernel's error.
    pub async fn move_wake(
        &self,
        id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<AgendaItem, VerbError> {
        let item = self.item(id).await?;
        let wake = match &item.state {
            AgendaState::Scheduled { wake } => wake,
            AgendaState::Running { .. } => return Err(VerbError::refused(words::ALREADY_STARTED)),
            AgendaState::Proposed | AgendaState::Approved => {
                return Err(VerbError::refused(words::NOT_SCHEDULED));
            }
            _ => return Err(VerbError::refused(words::ENDED)),
        };
        let Some(current) = single_time(wake) else {
            return Err(VerbError::refused(words::NOT_ONE_TIME));
        };
        if current != from {
            return Err(VerbError::refused(words::IT_MOVED));
        }
        if to <= from {
            return Err(VerbError::invalid("a screen action only moves a wake later"));
        }
        if let Some(ends) = item.expires_at.filter(|ends| to > *ends) {
            return Err(VerbError::refused(words::ends_at(&self.time_text(ends))));
        }
        if self.agenda.started(&item).await? {
            return Err(VerbError::refused(words::ALREADY_STARTED));
        }
        self.agenda.reschedule(id, WakeSet::any_of([Wake::At(to)]), &self.subject).await
    }
}

/// The one time `wake` waits for, when it waits for exactly one and nothing else.
pub(crate) fn single_time(wake: &WakeSet) -> Option<DateTime<Utc>> {
    match wake.any_of.as_slice() {
        [Wake::At(at)] => Some(*at),
        _ => None,
    }
}

/// One action and what runs it.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) spec: VerbSpec,
    pub(crate) handler: Arc<dyn VerbHandler>,
}

/// The screen actions a home offers (spec 3.3 §2 item 7, §6.1), over the kernel's agenda.
///
/// The first catalogue is built in: `coming_up`, which lists the caller's Coming up with the
/// two times each item may move to, and `snooze`, which moves one by an hour or to the same
/// time tomorrow. A host adds more with [`VerbRegistryBuilder::verb`], within the object kinds
/// allowed ([`ALLOWED_OBJECTS`]) and never under a [`RESERVED`](super::RESERVED) name.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::verbs::{KernelAgenda, VerbRegistry};
///
/// # fn example(kernel: &adk_kernel::Kernel, approvals: Arc<dyn adk_core::ApprovalStore>) -> Result<(), adk_surface::verbs::VerbError> {
/// let registry = VerbRegistry::builder("adk-rust", Arc::new(KernelAgenda::of(kernel)))
///     .approval_store(approvals)
///     .build()?;
/// let names: Vec<String> = registry.views().into_iter().map(|view| view.name).collect();
/// assert_eq!(names, ["coming_up", "snooze"]);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct VerbRegistry {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) app: String,
    pub(crate) entries: Vec<Entry>,
    pub(crate) agenda: Arc<dyn AgendaAccess>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) offset: FixedOffset,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) approvals: Option<Arc<dyn ApprovalStore>>,
    pub(crate) governance: Option<GovernanceControl>,
    pub(crate) sessions: Option<Arc<dyn SessionService>>,
    pub(crate) answers: Option<Arc<dyn AnswerLog>>,
    pub(crate) names: HashMap<String, String>,
}

impl fmt::Debug for VerbRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerbRegistry")
            .field("app", &self.inner.app)
            .field(
                "verbs",
                &self.inner.entries.iter().map(|entry| &entry.spec.name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Builds a [`VerbRegistry`].
pub struct VerbRegistryBuilder {
    app: String,
    agenda: Arc<dyn AgendaAccess>,
    entries: Vec<Entry>,
    clock: Arc<dyn Clock>,
    offset: FixedOffset,
    audit: Option<AuditTrail>,
    approvals: Option<Arc<dyn ApprovalStore>>,
    governance: Option<GovernanceControl>,
    sessions: Option<Arc<dyn SessionService>>,
    answers: Option<Arc<dyn AnswerLog>>,
    names: HashMap<String, String>,
}

impl fmt::Debug for VerbRegistryBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerbRegistryBuilder").field("app", &self.app).finish_non_exhaustive()
    }
}

impl VerbRegistryBuilder {
    /// Adds the action `spec`, run by `handler`, after the catalogue; checked by
    /// [`build`](Self::build).
    #[must_use]
    pub fn verb(mut self, spec: VerbSpec, handler: Arc<dyn VerbHandler>) -> Self {
        self.entries.push(Entry { spec, handler });
        self
    }

    /// Reads now from `clock` (default: the system clock).
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Shows times at `offset` east of UTC instead of the host's time zone.
    #[must_use]
    pub fn utc_offset(mut self, offset: FixedOffset) -> Self {
        self.offset = offset;
        self
    }

    /// Records every governed call and every action taken on `trail` (spec 3.3 §11).
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Counts each agent's waiting requests in `store`, the approval service's store, so an
    /// agent has at most [`MAX_WAITING`](super::MAX_WAITING) at once, and finds the decision
    /// that let an agent's call run, for its record. The store, not the service, because the
    /// service routes through [`VerbRegistry::router`].
    #[must_use]
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approvals = Some(store);
        self
    }

    /// Refuses a person's actions while `control` is frozen, except those that only delay
    /// work (owner question 23).
    #[must_use]
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Reads the asking turn from `sessions`, so an agent's card names what it read first
    /// ("Finance read 1 web page (example.com)").
    #[must_use]
    pub fn sessions(mut self, sessions: Arc<dyn SessionService>) -> Self {
        self.sessions = Some(sessions);
        self
    }

    /// Reads where a person answered an agent's request from `answers`, for its record: "you
    /// said yes on your phone".
    #[must_use]
    pub fn answers(mut self, answers: Arc<dyn AnswerLog>) -> Self {
        self.answers = Some(answers);
        self
    }

    /// Names the agent `name` as `display_name` in refusals: "Finance already has 3 changes
    /// waiting for you".
    #[must_use]
    pub fn agent_name(mut self, name: impl Into<String>, display_name: impl Into<String>) -> Self {
        self.names.insert(name.into(), display_name.into());
        self
    }

    /// The registry.
    ///
    /// # Errors
    ///
    /// [`VerbError::Invalid`] for a name outside `^[a-z][a-z0-9_]{0,58}$`, a second action of
    /// one name, or an object kind outside [`ALLOWED_OBJECTS`]; [`VerbError::Reserved`] for a
    /// reserved name.
    pub fn build(self) -> Result<VerbRegistry, VerbError> {
        let mut seen = std::collections::HashSet::new();
        for entry in &self.entries {
            let spec = &entry.spec;
            check_name(&spec.name)?;
            if !seen.insert(spec.name.clone()) {
                return Err(VerbError::invalid(format!(
                    "two screen actions are called `{}`; give each its own name",
                    spec.name
                )));
            }
            if let Some(kind) = spec.objects.iter().find(|kind| !ALLOWED_OBJECTS.contains(kind)) {
                return Err(VerbError::invalid(format!(
                    "`{}` acts on {kind:?}, which no screen action may act on yet",
                    spec.name
                )));
            }
        }
        Ok(VerbRegistry {
            inner: Arc::new(Inner {
                app: self.app,
                entries: self.entries,
                agenda: self.agenda,
                clock: self.clock,
                offset: self.offset,
                audit: self.audit,
                approvals: self.approvals,
                governance: self.governance,
                sessions: self.sessions,
                answers: self.answers,
                names: self.names,
            }),
        })
    }
}

impl VerbRegistry {
    /// A builder for the app `app` over `agenda`, with the first catalogue: `coming_up` and
    /// `snooze`.
    pub fn builder(app: impl Into<String>, agenda: Arc<dyn AgendaAccess>) -> VerbRegistryBuilder {
        VerbRegistryBuilder {
            app: app.into(),
            agenda,
            entries: snooze::catalogue(),
            clock: Arc::new(SystemClock),
            offset: *Local::now().offset(),
            audit: None,
            approvals: None,
            governance: None,
            sessions: None,
            answers: None,
            names: HashMap::new(),
        }
    }

    /// A builder of the first catalogue in `app` with no agenda, for a process that draws the
    /// cards and lists the grant of actions another process takes, such as a terminal attached
    /// to `adk-rust up` (spec 3.3 §7.1): its [`hints`](Self::hints) and [`grant`](Self::grant)
    /// are those of the process that hosts the actions, and every action it would run is
    /// refused.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_surface::verbs::VerbRegistry;
    ///
    /// let drawn = VerbRegistry::catalogue("adk-rust").build().unwrap();
    /// assert_eq!(drawn.origin(), "verbs:adk-rust");
    /// let _hints = drawn.hints();
    /// ```
    pub fn catalogue(app: impl Into<String>) -> VerbRegistryBuilder {
        Self::builder(app, Arc::new(NoAgenda))
    }

    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }

    /// The app the actions belong to.
    pub fn app_name(&self) -> &str {
        &self.inner.app
    }

    /// The publisher of the action tools: `verbs:<app>`.
    pub fn origin(&self) -> String {
        super::verbs_origin(&self.inner.app)
    }

    /// The action `name`.
    pub fn spec(&self, name: &str) -> Option<&VerbSpec> {
        self.entry(name).map(|entry| &entry.spec)
    }

    pub(crate) fn entry(&self, name: &str) -> Option<&Entry> {
        self.inner.entries.iter().find(|entry| entry.spec.name == name)
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.inner.entries
    }

    /// The action an agent calls as `tool`, `verb_<name>`.
    pub(crate) fn entry_for_tool(&self, tool: &str) -> Option<&Entry> {
        tool.strip_prefix(super::VERB_TOOL_PREFIX).and_then(|name| self.entry(name))
    }

    /// The catalogue, as `GET /surface/verbs` lists it.
    pub fn views(&self) -> Vec<VerbView> {
        self.inner.entries.iter().map(|entry| VerbView::from(&entry.spec)).collect()
    }

    /// What `user`'s action may read and change: every area for a person, only `area` for an
    /// agent.
    pub(crate) fn scope(&self, user: &str, area: Option<PartitionId>) -> VerbScope {
        VerbScope {
            subject: SubjectRef::human(user),
            area,
            agenda: Arc::clone(&self.inner.agenda),
            now: self.inner.clock.now(),
            offset: self.inner.offset,
        }
    }

    /// The display name of the agent `name`: as [`VerbRegistryBuilder::agent_name`] set it,
    /// else the name with its first letter upper-cased.
    pub(crate) fn display_name(&self, name: &str) -> String {
        if let Some(display) = self.inner.names.get(name) {
            return display.clone();
        }
        let mut chars = name.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        }
    }

    /// The tools of the actions agents may ask for, for an agent working in `area`
    /// (spec 3.3 §2 item 7): attach with `HarnessAgentBuilder::toolset`.
    pub fn toolset(&self, area: impl Into<PartitionId>) -> VerbToolset {
        VerbToolset::new(self.clone(), area.into())
    }

    /// The classes of the action tools, by their declared effects: attach with
    /// `HarnessAgentBuilder::tool_classes`.
    pub fn classes(&self) -> VerbClasses {
        VerbClasses::new(self.clone())
    }

    /// The operator-layer policy of an agent working in `area`: attach with
    /// `HarnessAgentBuilder::add_operator_policy`, so the strictest decision keeps it over
    /// modes and standing rules.
    pub fn policy(&self, area: impl Into<PartitionId>) -> VerbPolicy {
        VerbPolicy::new(self.clone(), Some(area.into()))
    }

    /// The approval router of the action tools' cards, with `fallback` for every other call:
    /// pass it to `StoreApprovalService::with_router`.
    pub fn router(&self, fallback: impl ApprovalRouter + 'static) -> VerbRouter {
        VerbRouter::new(self.clone(), Arc::new(fallback))
    }

    /// The card hints of the action tools' cards: pass them to
    /// `SurfaceEngineBuilder::card_hints`.
    pub fn hints(&self) -> VerbHints {
        VerbHints::new(self.clone())
    }

    /// What a host grants every agent it gives the action tools, for
    /// `SurfaceEngineBuilder::grant` (spec 3.3 §5.7, S-I12, S-I13).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # fn example(registry: adk_surface::verbs::VerbRegistry) {
    /// assert_eq!(registry.grant().text, "Ask to change things on your screens — always asks you first");
    /// # }
    /// ```
    pub fn grant(&self) -> GrantedUse {
        GrantedUse::new(GRANT_ID, words::GRANTED)
    }
}
