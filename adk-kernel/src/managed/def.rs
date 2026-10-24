//! The kernel block of a managed agent definition (spec 3.2 §7.13, §10 M1).

use std::collections::{BTreeMap, BTreeSet};

use adk_agent::ambient::CronTrigger;
use adk_core::schedule::MissedFirePolicy;
use adk_managed::types::ManagedAgentDef;
use serde::{Deserialize, Serialize};

use crate::agenda::ProactivityPolicy;
use crate::consolidation::ConsolidationTick;
use crate::drivers::{DriverManifest, DriverSandbox};
use crate::error::{KernelError, Result};
use crate::partition::{CrossPartition, Partition, PartitionId, PartitionMode};
use crate::sched::QueueClass;
use crate::wake::Wake;

/// The [`ManagedAgentDef::metadata`] key a [`KernelDef`] travels under, beside the
/// harness's `adk.harness` (spec 3.2 §7.13, §10 M1).
///
/// The block is JSON in a string value, as the harness block is, so a store or service that
/// keeps a `ManagedAgentDef` — the managed state store's session records included — keeps
/// it unchanged.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::managed::KERNEL_DEF_METADATA_KEY, "adk.kernel");
/// ```
pub const KERNEL_DEF_METADATA_KEY: &str = "adk.kernel";

/// The user a wake set's processes run for when it names none (spec 3.2 §7.13).
pub const DEFAULT_WAKE_USER: &str = "kernel";

/// The values of [`PartitionDef::cross_partition`], as the partition record names them
/// (spec 3.2 §7.8).
pub const CROSS_PARTITION_VALUES: [&str; 2] = ["deny", "coordinator_with_approval"];

/// The kernel part of a hosted definition: partitions and their coordinator, drivers, the
/// proactivity policy and wake sets (spec 3.2 §7.8, §7.13, §10 M1).
///
/// A `KernelDef` travels as JSON under [`KERNEL_DEF_METADATA_KEY`] in a
/// [`ManagedAgentDef`]'s metadata, beside the harness block, so the managed runtime stores
/// and rebuilds it like any other definition, and a [`KernelResolver`](super::KernelResolver)
/// hosts the definition on its tenant's kernel. The block is written in the kernel's own
/// camelCase wire shape — its wakes, driver manifests and proactivity policy are the
/// kernel's types — and an unknown key of the block, a partition or a wake set refuses it.
///
/// | Field | In the tenant's kernel |
/// |---|---|
/// | `wakeSets` | standing triggers: each wake starts a governed run of the definition's agent |
/// | `proactivity` | the tenant kernel's [`ProactivityPolicy`]: open proposals, the daily proactive budget, quiet hours, how proposals are approved; the consolidation tick runs only when the block sets `consolidation` (§13 Q1) |
/// | `drivers` | real [`DriverManifest`]s, validated; with the tenant's driver supervisor (`TenantKernelBuilder::drivers`), each one a declared partition mounts is proposed in the tenant's approval inbox and mounted when a person approves it, then served to that partition's harness (spec 3.2 §7.7) |
/// | `partitions`, `coordinator` | as `[[kernel.partitions]]` and `kernel.coordinator` in `harness.toml`: each partition in the tenant's partition table, its tenant the hosting tenant, with an agent of its own built by the tenant's [`PartitionAgentResolver`](super::PartitionAgentResolver) — one harness per partition — as the target `<definition>@<partition>`; the coordinator's agent, else the first partition's by id, also serves the definition's own target. Without a partition resolver the partitions are recorded and the definition's agent runs in the default partition |
///
/// A definition comes from a tenant, so the block cannot widen what the operator allows: a
/// driver is never `unsandboxed` and pins no declaration digest of its own, a credential
/// reaches a driver only as a `secret://` reference, a wake set never runs on the
/// `interactive` queue a person's turn holds nor watches the host's files, and no field lets
/// an agenda item of origin `Consolidation` start without a person's approval (K-I23).
///
/// # Example
///
/// ```rust
/// use adk_kernel::Wake;
/// use adk_kernel::managed::{KernelDef, WakeSetDef};
/// use adk_managed::types::{ManagedAgentDef, ModelRef};
///
/// let kernel = KernelDef::new().with_wake_set(WakeSetDef::new(
///     "morning",
///     [WakeSetDef::cron("0 0 9 * * *")],
///     "Summarize what changed overnight.",
/// ));
/// let def = kernel.apply_to(ManagedAgentDef::new("digest", ModelRef::Shorthand("gemini-3.5-flash-lite".into())));
/// // The definition crosses a store as JSON, as a session record does.
/// let def: ManagedAgentDef = serde_json::from_value(serde_json::to_value(&def)?)?;
/// assert_eq!(KernelDef::from_agent_def(&def)?, Some(kernel));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct KernelDef {
    /// The partitions the definition's agents run in (spec 3.2 §7.8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partitions: Vec<PartitionDef>,
    /// The partition whose agent is the coordinator, the one that may cross into another
    /// partition under a non-waivable approval (spec 3.2 §7.8, K-I15); one of
    /// [`partitions`](Self::partitions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<PartitionId>,
    /// The drivers the definition asks to mount (spec 3.2 §7.7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<DriverManifest>,
    /// The proactivity policy (spec 3.2 §7.5); `None` leaves the operator's, with the
    /// consolidation tick off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactivity: Option<ProactivityPolicy>,
    /// The wake sets that start the definition's agent (spec 3.2 §7.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_sets: Vec<WakeSetDef>,
}

impl KernelDef {
    /// An empty block: no partitions, drivers or wake sets, no proactivity policy.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a partition.
    #[must_use]
    pub fn with_partition(mut self, partition: PartitionDef) -> Self {
        self.partitions.push(partition);
        self
    }

    /// Makes `partition`'s agent the coordinator (spec 3.2 §7.8).
    #[must_use]
    pub fn with_coordinator(mut self, partition: impl Into<PartitionId>) -> Self {
        self.coordinator = Some(partition.into());
        self
    }

    /// The partition whose agent serves the definition's own target: the coordinator's,
    /// else the first partition's by id; `None` without partitions (spec 3.2 §7.8).
    pub fn primary_partition(&self) -> Option<PartitionId> {
        self.coordinator
            .clone()
            .or_else(|| self.partitions.iter().map(|partition| partition.id.clone()).min())
    }

    /// Adds a driver manifest.
    #[must_use]
    pub fn with_driver(mut self, driver: DriverManifest) -> Self {
        self.drivers.push(driver);
        self
    }

    /// Sets the proactivity policy.
    #[must_use]
    pub fn with_proactivity(mut self, proactivity: ProactivityPolicy) -> Self {
        self.proactivity = Some(proactivity);
        self
    }

    /// Adds a wake set.
    #[must_use]
    pub fn with_wake_set(mut self, wake_set: WakeSetDef) -> Self {
        self.wake_sets.push(wake_set);
        self
    }

    /// The kernel block `def` carries, validated, or `None` when its metadata holds no
    /// [`KERNEL_DEF_METADATA_KEY`] (spec 3.2 §7.13, §10 M1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the block is not valid JSON, has an unknown key,
    /// or fails [`validate`](Self::validate); the message names the definition and the key.
    pub fn from_agent_def(def: &ManagedAgentDef) -> Result<Option<Self>> {
        let Some(text) =
            def.metadata.as_ref().and_then(|metadata| metadata.get(KERNEL_DEF_METADATA_KEY))
        else {
            return Ok(None);
        };
        let block: Self = serde_json::from_str(text).map_err(|error| KernelError::Config {
            reason: format!(
                "metadata `{KERNEL_DEF_METADATA_KEY}` of definition '{}' is not a kernel \
                 block: {error}",
                def.name
            ),
        })?;
        block.validate().map_err(|error| match error {
            KernelError::Config { reason } => KernelError::Config {
                reason: format!(
                    "metadata `{KERNEL_DEF_METADATA_KEY}` of definition '{}': {reason}",
                    def.name
                ),
            },
            other => other,
        })?;
        Ok(Some(block))
    }

    /// `def` with this block in its metadata under [`KERNEL_DEF_METADATA_KEY`], replacing
    /// any block it carried; the caller's other keys, `adk.harness` included, stay.
    ///
    /// The block is not validated here; [`from_agent_def`](Self::from_agent_def) validates
    /// it where it is read.
    pub fn apply_to(&self, mut def: ManagedAgentDef) -> ManagedAgentDef {
        let text = serde_json::to_string(self).expect("a kernel block always serializes");
        def.metadata
            .get_or_insert_with(BTreeMap::new)
            .insert(KERNEL_DEF_METADATA_KEY.to_string(), text);
        def
    }

    /// The consolidation tick the block turns on, or `None`: the tick is off in the managed
    /// form unless the definition's policy sets it (spec 3.2 §7.5, §13 Q1).
    pub fn consolidation(&self) -> Option<&ConsolidationTick> {
        self.proactivity.as_ref().and_then(|proactivity| proactivity.consolidation.as_ref())
    }

    /// The partition a wake set's processes run in: its own, or the default partition.
    pub(crate) fn partition_of(wake_set: &WakeSetDef) -> PartitionId {
        wake_set.partition.clone().unwrap_or_default()
    }

    /// Checks every rule of the block (spec 3.2 §7.4, §7.5, §7.7, §7.8).
    ///
    /// | Part | Refused when |
    /// |---|---|
    /// | partitions | an id or class is blank or repeated, an id has a character other than letters, digits, `-` and `_`, a mounted driver is not one of the block's, `crossPartition` is unknown, a blocked crossing names the partition itself |
    /// | coordinator | it names a partition the block does not declare |
    /// | drivers | a manifest fails `DriverManifest::validate`, an id is repeated, a partition is not declared, the sandbox is `unsandboxed`, a declaration digest is pinned |
    /// | proactivity | a share, score or fraction of the consolidation tick is outside its range, or its schedule is blank |
    /// | wake sets | a name is blank, repeated or not an identifier, `anyOf` or `input` is empty, a wake is not a cron, webhook or queue wake, a cron expression does not parse, a hook or source is blank, the partition is not declared, the queue is `interactive`, the user is blank |
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the first rule broken and how to fix it.
    pub fn validate(&self) -> Result<()> {
        let mut declared = BTreeSet::new();
        let drivers: BTreeSet<&str> =
            self.drivers.iter().map(|driver| driver.id.as_str()).collect();
        for partition in &self.partitions {
            partition.validate(&drivers)?;
            if !declared.insert(partition.id.clone()) {
                return Err(invalid(format!("partition `{}` is declared twice", partition.id)));
            }
        }
        if let Some(coordinator) = &self.coordinator
            && !declared.contains(coordinator)
        {
            return Err(invalid(format!(
                "coordinator `{coordinator}` is not a partition the block declares; name one \
                 of `partitions`"
            )));
        }
        let mut partitions = declared;
        partitions.insert(PartitionId::default());
        let mut seen = BTreeSet::new();
        for driver in &self.drivers {
            validate_driver(driver, &partitions)?;
            if !seen.insert(driver.id.as_str()) {
                return Err(invalid(format!("driver `{}` is declared twice", driver.id)));
            }
        }
        if let Some(tick) = self.consolidation() {
            validate_tick(tick)?;
        }
        let mut names = BTreeSet::new();
        for wake_set in &self.wake_sets {
            wake_set.validate(&partitions)?;
            if !names.insert(wake_set.name.as_str()) {
                return Err(invalid(format!("wake set `{}` is declared twice", wake_set.name)));
            }
        }
        Ok(())
    }
}

/// A driver manifest a tenant may ask for: the manifest's own rules, then the managed form's.
fn validate_driver(driver: &DriverManifest, partitions: &BTreeSet<PartitionId>) -> Result<()> {
    driver.validate()?;
    if driver.sandbox == DriverSandbox::Unsandboxed {
        return Err(invalid(format!(
            "driver `{}`: a definition's driver runs sandboxed; `unsandboxed` drivers are the \
             operator's to configure, so use `os` or `container`",
            driver.id
        )));
    }
    if !driver.declaration_digests.is_empty() {
        return Err(invalid(format!(
            "driver `{}`: declaration digests are pinned when a person approves the mount, \
             never by a definition; leave `declarationDigests` out",
            driver.id
        )));
    }
    for partition in &driver.partitions {
        if !partitions.contains(partition) {
            return Err(invalid(format!(
                "driver `{}` serves partition `{partition}`, which the block does not declare",
                driver.id
            )));
        }
    }
    Ok(())
}

/// The ranges of a consolidation tick's shares, scores and fractions.
fn validate_tick(tick: &ConsolidationTick) -> Result<()> {
    let within = |field: &str, value: f32, zero_allowed: bool| {
        let low_ok = if zero_allowed { value >= 0.0 } else { value > 0.0 };
        if low_ok && value <= 1.0 {
            Ok(())
        } else {
            let range = if zero_allowed { "[0, 1]" } else { "(0, 1]" };
            Err(invalid(format!(
                "proactivity.consolidation.{field} is {value}; it must be in {range}"
            )))
        }
    };
    within("budgetShare", tick.budget_share, false)?;
    within("dedupScore", tick.dedup_score, true)?;
    within("expireConfidence", tick.expire_confidence, true)?;
    within("maxLossFraction", tick.max_loss_fraction, false)?;
    if tick.schedule.as_deref().is_some_and(|schedule| schedule.trim().is_empty()) {
        return Err(invalid(
            "proactivity.consolidation.schedule is blank; leave it out to admit ticks on new \
             material",
        ));
    }
    Ok(())
}

/// A partition of a hosted definition, as plain data in the partition record's shape (spec
/// 3.2 §7.8).
///
/// The tenant is the hosting kernel's, never the definition's, so the block has no
/// `tenant` key.
///
/// # Example
///
/// ```rust
/// use adk_kernel::PartitionMode;
/// use adk_kernel::managed::PartitionDef;
///
/// let work: PartitionDef = serde_json::from_value(serde_json::json!({
///     "id": "work",
///     "class": "work",
///     "defaultMode": "auto",
///     "crossPartition": "coordinator_with_approval",
/// }))?;
/// assert_eq!(work.default_mode, PartitionMode::Auto);
/// assert_eq!(work.memory_scope(), "work");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct PartitionDef {
    /// The partition's id: `work`, `home`, a team.
    pub id: PartitionId,
    /// `AgentPrincipal.class` of every agent in the partition.
    pub class: String,
    /// The mode of the partition's agents (default `ask`).
    #[serde(default = "ask")]
    pub default_mode: PartitionMode,
    /// The project of the partition's long-term facts and project-scoped memory; defaults to
    /// the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<String>,
    /// The ids of the block's drivers mounted for the partition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<String>,
    /// [`CROSS_PARTITION_VALUES`]: `deny` (default) or `coordinator_with_approval`.
    #[serde(default = "deny")]
    pub cross_partition: String,
    /// Partitions the coordinator in this partition may not cross into.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blocked_crossings: BTreeSet<PartitionId>,
}

fn ask() -> PartitionMode {
    PartitionMode::Ask
}

fn deny() -> String {
    CROSS_PARTITION_VALUES[0].to_string()
}

impl PartitionDef {
    /// A partition `id` of class `class`, in `ask` mode, denying crossings.
    pub fn new(id: impl Into<PartitionId>, class: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            class: class.into(),
            default_mode: PartitionMode::Ask,
            memory_scope: None,
            drivers: Vec::new(),
            cross_partition: deny(),
            blocked_crossings: BTreeSet::new(),
        }
    }

    /// Sets the mode.
    #[must_use]
    pub fn with_mode(mut self, mode: PartitionMode) -> Self {
        self.default_mode = mode;
        self
    }

    /// Mounts the block's driver `id` for the partition.
    #[must_use]
    pub fn with_driver(mut self, id: impl Into<String>) -> Self {
        self.drivers.push(id.into());
        self
    }

    /// The memory scope: the one set, or the id.
    pub fn memory_scope(&self) -> &str {
        self.memory_scope.as_deref().unwrap_or(self.id.as_str())
    }

    /// The partition record of this partition on `tenant`'s kernel (spec 3.2 §7.8): the
    /// tenant is the hosting tenant, never the definition's.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::managed::PartitionDef;
    ///
    /// let work = PartitionDef::new("work", "work-agents").with_driver("calendar").to_partition("tenant-a")?;
    /// assert_eq!(work.tenant.as_deref(), Some("tenant-a"));
    /// assert_eq!((work.memory_scope.as_str(), work.drivers.as_slice()), ("work", ["calendar".to_string()].as_slice()));
    /// # Ok::<(), adk_kernel::KernelError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `crossPartition` is unknown.
    pub fn to_partition(&self, tenant: &str) -> Result<Partition> {
        let mut partition = Partition::new(self.id.clone(), self.class.clone())
            .with_tenant(tenant)
            .with_mode(self.default_mode)
            .with_memory_scope(self.memory_scope())
            .with_cross_partition(self.cross_partition.parse::<CrossPartition>()?);
        for driver in &self.drivers {
            partition = partition.with_driver(driver.clone());
        }
        for blocked in &self.blocked_crossings {
            partition = partition.with_blocked_crossing(blocked.clone());
        }
        Ok(partition)
    }

    fn validate(&self, drivers: &BTreeSet<&str>) -> Result<()> {
        name("partition id", self.id.as_str())?;
        if self.class.trim().is_empty() {
            return Err(invalid(format!("partition `{}` needs a `class`", self.id)));
        }
        if self.memory_scope.as_deref().is_some_and(|scope| scope.trim().is_empty()) {
            return Err(invalid(format!(
                "partition `{}` has a blank `memoryScope`; leave it out to use the id",
                self.id
            )));
        }
        for driver in &self.drivers {
            if !drivers.contains(driver.as_str()) {
                return Err(invalid(format!(
                    "partition `{}` mounts driver `{driver}`, which the block does not declare",
                    self.id
                )));
            }
        }
        if !CROSS_PARTITION_VALUES.contains(&self.cross_partition.as_str()) {
            return Err(invalid(format!(
                "partition `{}`: crossPartition `{}` is unknown; use {}",
                self.id,
                self.cross_partition,
                CROSS_PARTITION_VALUES.join(" or ")
            )));
        }
        if self.blocked_crossings.contains(&self.id) {
            return Err(invalid(format!(
                "partition `{}` blocks crossings to itself; list other partitions",
                self.id
            )));
        }
        Ok(())
    }
}

/// A wake set of a hosted definition: wakes that each start a governed run of the
/// definition's agent with `input` (spec 3.2 §7.4, §7.13).
///
/// A wake set is standing: every firing starts a process, keyed by the wake's own key, so
/// a repeated delivery starts nothing (K-I3). `overlap` says what a firing does while the
/// set's previous process is unfinished. A definition's standing wakes are cron, webhook and
/// queue wakes: a file-change wake would watch the host's files, and approval, run-finished,
/// time, budget and manual wakes name one sleeping process. The hosting application serves
/// a webhook through `webhook_wake_hook` and a queue through `queue_wake_ingress` on the
/// tenant's run store.
///
/// # Example
///
/// ```rust
/// use adk_kernel::Wake;
/// use adk_kernel::managed::{WakeOverlap, WakeSetDef};
///
/// let set: WakeSetDef = serde_json::from_value(serde_json::json!({
///     "name": "orders",
///     "anyOf": [{ "webhook": { "hook": "orders" } }, { "cron": { "expr": "0 0 * * * *", "missed": "skip" } }],
///     "input": "Triage the new orders.",
/// }))?;
/// assert_eq!(set.any_of[0], Wake::Webhook { hook: "orders".into() });
/// assert_eq!(set.overlap, WakeOverlap::Skip);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct WakeSetDef {
    /// The set's name, unique in the definition.
    pub name: String,
    /// The wakes; any one starts a process.
    pub any_of: Vec<Wake>,
    /// The text each process starts with.
    pub input: String,
    /// The partition its processes run in; the default partition when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// The queue its processes run on: `background` (default), `maintenance` or a named
    /// queue; never `interactive`, which a person's turn holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// The user its processes run for, as a `Service` subject; [`DEFAULT_WAKE_USER`] when
    /// unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// What a firing does while the set's previous process is unfinished.
    #[serde(default)]
    pub overlap: WakeOverlap,
}

impl WakeSetDef {
    /// A set `name` that starts a process with `input` when any of `wakes` fires.
    pub fn new(
        name: impl Into<String>,
        wakes: impl IntoIterator<Item = Wake>,
        input: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            any_of: wakes.into_iter().collect(),
            input: input.into(),
            partition: None,
            queue: None,
            user: None,
            overlap: WakeOverlap::default(),
        }
    }

    /// A cron wake on the six-field expression `expr` (seconds first) that skips missed
    /// occurrences.
    pub fn cron(expr: impl Into<String>) -> Wake {
        Wake::Cron { expr: expr.into(), missed: MissedFirePolicy::Skip }
    }

    /// Runs the set's processes in `partition`.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// Runs the set's processes on `queue`.
    #[must_use]
    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    /// Runs the set's processes for `user`.
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Sets what a firing does while the previous process is unfinished.
    #[must_use]
    pub fn with_overlap(mut self, overlap: WakeOverlap) -> Self {
        self.overlap = overlap;
        self
    }

    /// The queue class the set's processes run on.
    pub fn queue_class(&self) -> QueueClass {
        self.queue.as_deref().map(QueueClass::from_name).unwrap_or(QueueClass::Background)
    }

    /// The user the set's processes run for.
    pub fn user(&self) -> &str {
        self.user.as_deref().unwrap_or(DEFAULT_WAKE_USER)
    }

    fn validate(&self, partitions: &BTreeSet<PartitionId>) -> Result<()> {
        name("wake set name", &self.name)?;
        if self.any_of.is_empty() {
            return Err(invalid(format!("wake set `{}` needs at least one wake", self.name)));
        }
        if self.input.trim().is_empty() {
            return Err(invalid(format!(
                "wake set `{}` needs an `input`: the text its processes start with",
                self.name
            )));
        }
        let partition = KernelDef::partition_of(self);
        if !partitions.contains(&partition) {
            return Err(invalid(format!(
                "wake set `{}` runs in partition `{partition}`, which the block does not declare",
                self.name
            )));
        }
        if self.queue_class() == QueueClass::Interactive {
            return Err(invalid(format!(
                "wake set `{}`: the `interactive` queue is reserved for a person's turn; use \
                 `background` or `maintenance`",
                self.name
            )));
        }
        if self.queue.as_deref().is_some_and(|queue| queue.trim().is_empty())
            || self.user.as_deref().is_some_and(|user| user.trim().is_empty())
        {
            return Err(invalid(format!(
                "wake set `{}` has a blank `queue` or `user`; leave it out for the default",
                self.name
            )));
        }
        for wake in &self.any_of {
            validate_wake(&self.name, wake)?;
        }
        Ok(())
    }
}

/// One wake of a definition's wake set: a cron, webhook or queue wake.
fn validate_wake(set: &str, wake: &Wake) -> Result<()> {
    match wake {
        Wake::Cron { expr, missed } => {
            CronTrigger::new(expr).map_err(|error| {
                invalid(format!(
                    "wake set `{set}`: `{expr}` is not a six-field cron expression (seconds \
                     first): {error}"
                ))
            })?;
            if *missed == MissedFirePolicy::CatchUp(0) {
                return Err(invalid(format!(
                    "wake set `{set}`: `missed.catchUp` must be at least 1"
                )));
            }
            Ok(())
        }
        Wake::Webhook { hook } => name("webhook hook", hook),
        Wake::Queue { source } if source.trim().is_empty() => {
            Err(invalid(format!("wake set `{set}` names a blank queue source")))
        }
        Wake::Queue { .. } => Ok(()),
        Wake::FileChange { .. } => Err(invalid(format!(
            "wake set `{set}`: a definition does not watch the host's files; use a cron, \
             webhook or queue wake"
        ))),
        other => Err(invalid(format!(
            "wake set `{set}`: `{}` names one sleeping process, not a standing wake; use a \
             cron, webhook or queue wake",
            serde_json::to_value(other)
                .ok()
                .and_then(|value| match value {
                    serde_json::Value::String(kind) => Some(kind),
                    serde_json::Value::Object(map) => map.keys().next().cloned(),
                    _ => None,
                })
                .unwrap_or_default()
        ))),
    }
}

/// What a wake set's firing does while its previous process is unfinished (spec 3.2 §7.9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum WakeOverlap {
    /// The firing starts nothing.
    #[default]
    Skip,
    /// The firing's process queues behind the unfinished one.
    Queue,
}

fn invalid(reason: impl Into<String>) -> KernelError {
    KernelError::Config { reason: reason.into() }
}

/// An identifier a target, trigger or process names: ASCII letters, digits, `-` and `_`.
fn name(what: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "{what} `{value}` must be ASCII letters, digits, `-` and `_`, and not empty"
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use adk_managed::types::ModelRef;
    use adk_tool::mcp::manager::McpServerConfig;
    use serde_json::json;

    use super::*;
    use crate::agenda::ProactiveApproval;

    fn def() -> ManagedAgentDef {
        ManagedAgentDef::new("digest", ModelRef::Shorthand("gemini-3.5-flash-lite".into()))
    }

    fn morning() -> WakeSetDef {
        WakeSetDef::new("morning", [WakeSetDef::cron("0 0 9 * * *")], "Summarize the night.")
    }

    fn webhook(hook: &str) -> Wake {
        Wake::Webhook { hook: hook.into() }
    }

    fn manifest(id: &str) -> DriverManifest {
        let server: McpServerConfig =
            serde_json::from_value(json!({ "command": format!("mcp-{id}") })).unwrap();
        DriverManifest::new(id, server).unwrap()
    }

    fn reason(error: KernelError) -> String {
        match error {
            KernelError::Config { reason } => reason,
            other => panic!("expected a configuration error, got {other}"),
        }
    }

    #[test]
    fn the_block_travels_beside_the_harness_block_and_other_keys() {
        let mut wire = def();
        wire.metadata = Some(BTreeMap::from([
            ("adk.harness".to_string(), r#"{"mode":"plan"}"#.to_string()),
            ("team".to_string(), "a".to_string()),
        ]));
        let kernel = KernelDef::new().with_wake_set(morning()).with_driver(manifest("email"));
        let wire = kernel.apply_to(wire);
        let metadata = wire.metadata.clone().unwrap();
        assert_eq!(metadata["adk.harness"], r#"{"mode":"plan"}"#);
        assert_eq!(metadata["team"], "a");
        let wire: ManagedAgentDef =
            serde_json::from_value(serde_json::to_value(&wire).unwrap()).unwrap();
        assert_eq!(KernelDef::from_agent_def(&wire).unwrap(), Some(kernel));
    }

    #[test]
    fn a_definition_without_a_block_has_no_kernel_part() {
        assert_eq!(KernelDef::from_agent_def(&def()).unwrap(), None);
    }

    #[test]
    fn applying_a_block_again_replaces_it() {
        let first = KernelDef::new().with_wake_set(morning());
        let second = KernelDef::new();
        let wire = second.apply_to(first.apply_to(def()));
        assert_eq!(KernelDef::from_agent_def(&wire).unwrap(), Some(second));
    }

    #[test]
    fn a_malformed_block_names_the_definition_and_the_key() {
        let mut wire = def();
        wire.metadata =
            Some(BTreeMap::from([(KERNEL_DEF_METADATA_KEY.to_string(), "{".to_string())]));
        let reason = reason(KernelDef::from_agent_def(&wire).unwrap_err());
        assert!(reason.contains("adk.kernel") && reason.contains("'digest'"), "{reason}");
    }

    #[test]
    fn the_block_carries_the_kernels_own_types() {
        let policy = ProactivityPolicy::default()
            .with_quiet_hours("22:00-07:00".parse().unwrap())
            .with_approval(ProactiveApproval::AlwaysAsk);
        let kernel = KernelDef::new()
            .with_partition(PartitionDef::new("work", "work").with_driver("email"))
            .with_driver(DriverManifest { partitions: vec!["work".into()], ..manifest("email") })
            .with_proactivity(policy.clone())
            .with_wake_set(morning().with_partition("work"));
        kernel.validate().unwrap();
        let wire = serde_json::to_value(&kernel).unwrap();
        assert_eq!(wire["partitions"][0]["defaultMode"], "ask");
        assert_eq!(wire["partitions"][0]["crossPartition"], "deny");
        assert_eq!(wire["drivers"][0]["server"]["command"], "mcp-email");
        assert_eq!(wire["proactivity"]["approval"], "always_ask");
        assert_eq!(wire["wakeSets"][0]["anyOf"][0]["cron"]["missed"], "skip");
        let back: KernelDef = serde_json::from_value(wire).unwrap();
        assert_eq!(back, kernel);
        assert_eq!(back.proactivity, Some(policy));
    }

    #[test]
    fn the_consolidation_tick_is_off_unless_the_block_sets_it() {
        assert!(KernelDef::new().consolidation().is_none());
        let quiet = KernelDef::new().with_proactivity(ProactivityPolicy::default());
        assert!(quiet.consolidation().is_none(), "§13 Q1: off in the managed form by default");
        let on = KernelDef::new().with_proactivity(
            ProactivityPolicy::default().with_consolidation(ConsolidationTick::default()),
        );
        assert!(on.consolidation().is_some());
    }

    #[test]
    fn a_tenant_block_cannot_widen_what_the_operator_allows() {
        let mut pinned = manifest("email");
        pinned.declaration_digests.insert("send".into(), "sha256:abc".into());
        let mut credential = manifest("github");
        credential.server.env.insert("GITHUB_TOKEN".into(), "ghp_plaintext".into());
        let cases = [
            (
                KernelDef::new().with_driver(DriverManifest {
                    sandbox: DriverSandbox::Unsandboxed,
                    ..manifest("shell")
                }),
                "unsandboxed",
            ),
            (KernelDef::new().with_driver(pinned), "declarationDigests"),
            (KernelDef::new().with_driver(credential), "secret://"),
            (KernelDef::new().with_wake_set(morning().with_queue("interactive")), "interactive"),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new(
                    "files",
                    [Wake::FileChange {
                        root: "/".into(),
                        glob: "*".into(),
                        debounce: Duration::from_millis(500),
                    }],
                    "x",
                )),
                "host's files",
            ),
        ];
        for (kernel, needle) in cases {
            let reason = reason(kernel.validate().unwrap_err());
            assert!(reason.contains(needle), "expected `{needle}` in: {reason}");
        }
    }

    #[test]
    fn every_rule_names_what_to_fix() {
        let cases: Vec<(KernelDef, &str)> = vec![
            (KernelDef::new().with_wake_set(morning()).with_wake_set(morning()), "twice"),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new(
                    "bad",
                    [WakeSetDef::cron("nope")],
                    "x",
                )),
                "six-field",
            ),
            (KernelDef::new().with_wake_set(WakeSetDef::new("none", [], "x")), "at least one wake"),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new("blank", [webhook("h")], " ")),
                "input",
            ),
            (KernelDef::new().with_wake_set(morning().with_partition("home")), "home"),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new("manual", [Wake::Manual], "x")),
                "`manual` names one sleeping process",
            ),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new(
                    "approval",
                    [Wake::Approval { fingerprint: "f".into() }],
                    "x",
                )),
                "`approval` names one sleeping process",
            ),
            (
                KernelDef::new()
                    .with_partition(PartitionDef::new("work", "work").with_driver("email")),
                "email",
            ),
            (
                KernelDef::new().with_partition(PartitionDef {
                    cross_partition: "always".into(),
                    ..PartitionDef::new("work", "work")
                }),
                "coordinator_with_approval",
            ),
            (
                KernelDef::new().with_driver(DriverManifest {
                    partitions: vec!["home".into()],
                    ..manifest("email")
                }),
                "partition `home`",
            ),
            (
                KernelDef::new().with_driver(manifest("email")).with_driver(manifest("email")),
                "twice",
            ),
            (
                KernelDef::new().with_proactivity(
                    ProactivityPolicy::default()
                        .with_consolidation(ConsolidationTick::default().with_budget_share(1.5)),
                ),
                "budgetShare",
            ),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new(
                    "catch",
                    [Wake::Cron {
                        expr: "0 0 9 * * *".into(),
                        missed: MissedFirePolicy::CatchUp(0),
                    }],
                    "x",
                )),
                "catchUp",
            ),
            (
                KernelDef::new().with_wake_set(WakeSetDef::new("sp ace", [webhook("h")], "x")),
                "ASCII",
            ),
        ];
        for (kernel, needle) in cases {
            let reason = reason(kernel.validate().unwrap_err());
            assert!(reason.contains(needle), "expected `{needle}` in: {reason}");
        }
    }

    #[test]
    fn wake_sets_default_to_the_background_queue_and_the_kernel_user() {
        assert_eq!(morning().queue_class(), QueueClass::Background);
        assert_eq!(morning().with_queue("maintenance").queue_class(), QueueClass::Maintenance);
        assert_eq!(morning().user(), DEFAULT_WAKE_USER);
        assert_eq!(morning().with_user("ana").user(), "ana");
    }
}
