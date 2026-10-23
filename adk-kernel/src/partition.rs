//! Partitions (spec 3.2 §7.8): areas of a user's life or an organisation's teams kept
//! apart, with work across them only through the coordinator and under approval.
//!
//! A [`Partition`] is a policy class plus a memory scope plus a driver set, run by its own
//! `HarnessAgent` built with the partition's mode (the `kernel` feature of `adk-harness`):
//!
//! | Part | Mechanism |
//! |---|---|
//! | Policy class | [`Partition::class`] and [`Partition::tenant`] on the `AgentPrincipal` of every agent in it, through the runner's identity source ([`Partition::identity_source`]), so `PolicyRule::for_class` rules apply |
//! | Memory | The partition's [`memory_scope`](Partition::memory_scope) is the project of its long-term facts (`FactScope.project`) and of project-scoped memory ([`Partition::scope_memory`]) |
//! | Drivers | [`Partition::drivers`]: the driver ids mounted for the partition |
//! | Crossings | [`PartitionPolicy`]: a request that reaches another partition is denied unless the coordinator makes it, then `RequireApproval`, non-waivable, audited as `kernel.partition.crossing` (K-I15) |
//!
//! The approval's unit is the crossing fingerprint within one run (§13 Q3): an approved
//! read lets the coordinator's identical reads through for the rest of that run and nothing
//! beyond it; writes stay one-shot. [`Partitions`] keeps the partition table and the
//! crossing ledger in a [`PartitionStore`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{
    AdkError, AgentPrincipal, Clock, ErrorCategory, ErrorComponent, IdentitySource, Memory,
    MemoryEntry, SystemClock,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit;
use crate::error::{KernelError, NOT_FOUND_CODE, Result};

mod crossing;

pub use crossing::{
    CROSSING_PARTITION_ARG, Crossing, CrossingAccess, CrossingFilter, CrossingOutcome,
    CrossingRecord, CrossingResource, CrossingRoute, CrossingRun, CrossingTool, PartitionPolicy,
    RecurringCrossing,
};

/// Names a partition: `work`, `home`, a team (spec 3.2 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_kernel::PartitionId;
///
/// let work = PartitionId::new("work");
/// assert_eq!(work.as_str(), "work");
/// assert_eq!(PartitionId::default().as_str(), PartitionId::DEFAULT);
/// assert_eq!(serde_json::to_value(&work).unwrap(), "work");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartitionId(String);

impl PartitionId {
    /// The partition a process runs in when none is named.
    pub const DEFAULT: &'static str = "default";

    /// A partition id.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PartitionId {
    fn default() -> Self {
        Self::new(Self::DEFAULT)
    }
}

impl fmt::Display for PartitionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for PartitionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for PartitionId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

impl From<String> for PartitionId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// A partition's default mode (spec 3.2 §7.8): what its agents may do without asking.
///
/// The `kernel` feature of `adk-harness` maps it one to one onto `HarnessMode` (§10 HK1),
/// and the values serialize as `HarnessMode`'s do (`read_only`, `plan`, `ask`, `auto`).
/// Agentrix's Observe / Suggest / Automate is ReadOnly / Ask / Auto (§4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::PartitionMode;
///
/// let mode: PartitionMode = "read_only".parse().unwrap();
/// assert_eq!(mode, PartitionMode::ReadOnly);
/// assert_eq!(mode.to_string(), "read_only");
/// assert_eq!(serde_json::to_value(PartitionMode::Auto).unwrap(), "auto");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PartitionMode {
    /// Reads and todos only.
    ReadOnly,
    /// Reads and planning; nothing that changes the workspace.
    Plan,
    /// Changes need approval unless a standing rule allows them.
    Ask,
    /// Changes run without approval, within the harness's own limits.
    Auto,
}

impl PartitionMode {
    /// The mode's name: `read_only`, `plan`, `ask` or `auto`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Plan => "plan",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }
}

impl fmt::Display for PartitionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad`, so a width or alignment in the format string applies.
        f.pad(self.as_str())
    }
}

impl FromStr for PartitionMode {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "read_only" | "readonly" => Ok(Self::ReadOnly),
            "plan" => Ok(Self::Plan),
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            other => Err(KernelError::Config {
                reason: format!(
                    "unknown partition mode `{other}`; use read_only, plan, ask or auto"
                ),
            }),
        }
    }
}

/// Whether a partition takes part in crossings (spec 3.2 §7.8).
///
/// A crossing needs both partitions to allow it: a partition set to [`Deny`](Self::Deny)
/// is isolated both ways, even from the coordinator. Agentrix's Work and Home worlds are two
/// partitions with [`CoordinatorWithApproval`](Self::CoordinatorWithApproval) (§4).
///
/// # Example
///
/// ```rust
/// use adk_kernel::CrossPartition;
///
/// assert_eq!(CrossPartition::default(), CrossPartition::Deny);
/// assert_eq!(
///     serde_json::to_value(CrossPartition::CoordinatorWithApproval).unwrap(),
///     "coordinator_with_approval"
/// );
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CrossPartition {
    /// No request crosses into or out of the partition.
    #[default]
    Deny,
    /// The coordinator may cross, each crossing under a non-waivable approval (K-I15).
    CoordinatorWithApproval,
}

impl CrossPartition {
    /// The value's name: `deny` or `coordinator_with_approval`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::CoordinatorWithApproval => "coordinator_with_approval",
        }
    }
}

impl fmt::Display for CrossPartition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for CrossPartition {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "deny" => Ok(Self::Deny),
            "coordinator_with_approval" | "coordinator" => Ok(Self::CoordinatorWithApproval),
            other => Err(KernelError::Config {
                reason: format!(
                    "unknown crossing setting `{other}`; use deny or coordinator_with_approval"
                ),
            }),
        }
    }
}

fn ask() -> PartitionMode {
    PartitionMode::Ask
}

/// A partition (spec 3.2 §7.8): a policy class plus a memory scope plus a driver set, run by
/// its own `HarnessAgent` built with [`default_mode`](Self::default_mode).
///
/// The quotas and quiet hours of §7.9 join the record with the scheduling of phase 2.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CrossPartition, Partition, PartitionMode};
///
/// let work = Partition::new("work", "work")
///     .with_mode(PartitionMode::Auto)
///     .with_driver("calendar")
///     .with_cross_partition(CrossPartition::CoordinatorWithApproval);
/// assert_eq!(work.memory_scope, "work");
/// assert_eq!(work.drivers, ["calendar"]);
/// let wire = serde_json::to_value(&work).unwrap();
/// assert_eq!(wire["defaultMode"], "auto");
/// assert_eq!(wire["crossPartition"], "coordinator_with_approval");
/// assert_eq!(serde_json::from_value::<Partition>(wire).unwrap(), work);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Partition {
    /// The partition's id: `work`, `home`, a team.
    pub id: PartitionId,
    /// `AgentPrincipal.class` of every agent in the partition.
    pub class: String,
    /// `AgentPrincipal.tenant` of every agent in the partition, on multi-tenant hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The mode of the partition's `HarnessAgent`; the harness maps it onto `HarnessMode`.
    /// Defaults to `ask`.
    #[serde(default = "ask")]
    pub default_mode: PartitionMode,
    /// The project of the partition's long-term facts (spec 3.1 §7.15.2) and of its
    /// project-scoped memory. Defaults to the id.
    pub memory_scope: String,
    /// The ids of the drivers mounted for the partition (spec 3.2 §7.7). A driver reaches
    /// the partition's agents only when it is mounted, its manifest lists the partition
    /// and its id is here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<String>,
    /// Whether the partition takes part in crossings. Defaults to [`CrossPartition::Deny`].
    #[serde(default)]
    pub cross_partition: CrossPartition,
    /// Partitions the coordinator in this partition may no longer cross into: the prompt's
    /// "deny and block crossings to *X* from this partition".
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blocked_crossings: BTreeSet<PartitionId>,
}

impl Partition {
    /// The principal attribute naming the partition of an agent
    /// ([`principal`](Self::principal)).
    pub const PRINCIPAL_ATTRIBUTE: &'static str = "adk.partition";

    /// A partition `id` of class `class` in `ask` mode, its memory scope its id, with no
    /// drivers and no crossings.
    pub fn new(id: impl Into<PartitionId>, class: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            memory_scope: id.as_str().to_string(),
            id,
            class: class.into(),
            tenant: None,
            default_mode: PartitionMode::Ask,
            drivers: Vec::new(),
            cross_partition: CrossPartition::Deny,
            blocked_crossings: BTreeSet::new(),
        }
    }

    /// Sets the tenant.
    #[must_use]
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    /// Sets the mode.
    #[must_use]
    pub fn with_mode(mut self, mode: PartitionMode) -> Self {
        self.default_mode = mode;
        self
    }

    /// Sets the memory scope.
    #[must_use]
    pub fn with_memory_scope(mut self, scope: impl Into<String>) -> Self {
        self.memory_scope = scope.into();
        self
    }

    /// Adds a driver id.
    #[must_use]
    pub fn with_driver(mut self, driver: impl Into<String>) -> Self {
        self.drivers.push(driver.into());
        self
    }

    /// Sets whether the partition takes part in crossings.
    #[must_use]
    pub fn with_cross_partition(mut self, cross_partition: CrossPartition) -> Self {
        self.cross_partition = cross_partition;
        self
    }

    /// Blocks crossings from this partition into `to`.
    #[must_use]
    pub fn with_blocked_crossing(mut self, to: impl Into<PartitionId>) -> Self {
        self.blocked_crossings.insert(to.into());
        self
    }

    /// Checks the record: a non-empty id, class and memory scope, none with control
    /// characters.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the field.
    pub fn validate(&self) -> Result<()> {
        let fields = [
            ("id", self.id.as_str()),
            ("class", self.class.as_str()),
            ("memoryScope", self.memory_scope.as_str()),
        ];
        for (field, value) in fields {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(KernelError::Config {
                    reason: format!(
                        "partition `{}`: `{field}` must be non-empty text without control \
                         characters",
                        self.id
                    ),
                });
            }
        }
        Ok(())
    }

    /// The principal the agent `agent_name` of `app_name` acts as in this partition:
    /// `urn:adk:agent:{app}/{agent}` with the partition's class and tenant, and the
    /// partition's id as the [`PRINCIPAL_ATTRIBUTE`](Self::PRINCIPAL_ATTRIBUTE) attribute.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::Partition;
    ///
    /// let principal = Partition::new("work", "work-agents").with_tenant("acme").principal("ops", "harness");
    /// assert_eq!(principal.id.as_str(), "urn:adk:agent:ops/harness");
    /// assert_eq!(principal.class.as_deref(), Some("work-agents"));
    /// assert_eq!(principal.tenant.as_deref(), Some("acme"));
    /// assert_eq!(principal.attributes[Partition::PRINCIPAL_ATTRIBUTE], "work");
    /// ```
    pub fn principal(&self, app_name: &str, agent_name: &str) -> AgentPrincipal {
        let mut principal = AgentPrincipal::derived(app_name, agent_name)
            .with_class(self.class.clone())
            .with_attribute(Self::PRINCIPAL_ATTRIBUTE, self.id.as_str());
        if let Some(tenant) = &self.tenant {
            principal = principal.with_tenant(tenant.clone());
        }
        principal
    }

    /// The runner's identity source for the partition's harness (spec 3.1 §7.14): every
    /// agent of the runner whose root is `root_agent` acts as its
    /// [`principal`](Self::principal) in this partition, so its class and tenant reach every
    /// `ToolPolicyRequest`, `ToolConfirmationRequest`, `SpendKey` and audit record.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::IdentitySource;
    /// use adk_kernel::Partition;
    ///
    /// # futures::executor::block_on(async {
    /// let source = Partition::new("home", "home").identity_source("adk-rust", "harness");
    /// assert_eq!(source.current().await.unwrap().class.as_deref(), Some("home"));
    /// let member = source.principal_for("reviewer").await.unwrap().unwrap();
    /// assert_eq!(member.id.as_str(), "urn:adk:agent:adk-rust/reviewer");
    /// assert_eq!(member.class.as_deref(), Some("home"));
    /// # });
    /// ```
    pub fn identity_source(&self, app_name: &str, root_agent: &str) -> Arc<dyn IdentitySource> {
        Arc::new(PartitionIdentity {
            partition: self.clone(),
            app_name: app_name.to_string(),
            root_agent: root_agent.to_string(),
        })
    }

    /// `memory` confined to the partition's [`memory_scope`](Self::memory_scope): searches
    /// and writes go through the memory's project scoping, and a search or write naming
    /// another project is refused (K-I15).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when `memory` does not keep projects apart
    /// (`Memory::supports_project_scoping` is `false`): a memory without project scoping
    /// would show every partition's entries to every other.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let memory = work.scope_memory(Arc::new(adk_memory::SqliteMemoryService::new(url).await?))?;
    /// let runner = Runner::builder().memory_service(memory) /* … */;
    /// ```
    pub fn scope_memory(&self, memory: Arc<dyn Memory>) -> Result<Arc<dyn Memory>> {
        if !memory.supports_project_scoping() {
            return Err(KernelError::Config {
                reason: format!(
                    "partition `{}` needs a memory that keeps projects apart: this memory \
                     does not implement project scoping, so it would show every partition's \
                     entries to every other; use a MemoryService with project support",
                    self.id
                ),
            });
        }
        Ok(Arc::new(PartitionMemory {
            inner: memory,
            partition: self.id.clone(),
            project: self.memory_scope.clone(),
        }))
    }
}

/// The identity of a partition's agents ([`Partition::identity_source`]).
#[derive(Debug)]
struct PartitionIdentity {
    partition: Partition,
    app_name: String,
    root_agent: String,
}

#[async_trait]
impl IdentitySource for PartitionIdentity {
    async fn current(&self) -> adk_core::Result<AgentPrincipal> {
        Ok(self.partition.principal(&self.app_name, &self.root_agent))
    }

    async fn principal_for(&self, agent_name: &str) -> adk_core::Result<Option<AgentPrincipal>> {
        Ok(Some(self.partition.principal(&self.app_name, agent_name)))
    }
}

/// A memory confined to one partition's project ([`Partition::scope_memory`]).
struct PartitionMemory {
    inner: Arc<dyn Memory>,
    partition: PartitionId,
    project: String,
}

impl PartitionMemory {
    fn refuse(&self, project: &str) -> AdkError {
        AdkError::new(
            ErrorComponent::Memory,
            ErrorCategory::Forbidden,
            "kernel.partition.memory_crossing",
            format!(
                "partition `{}` reads and writes only its own memory scope `{}`, not `{project}`; \
                 the coordinator reaches another partition through a crossing (spec 3.2 K-I15)",
                self.partition, self.project
            ),
        )
    }
}

#[async_trait]
impl Memory for PartitionMemory {
    async fn search(&self, query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        self.inner.search_in_project(query, &self.project).await
    }

    async fn health_check(&self) -> adk_core::Result<()> {
        self.inner.health_check().await
    }

    async fn add(&self, entry: MemoryEntry) -> adk_core::Result<()> {
        self.inner.add_to_project(entry, &self.project).await
    }

    async fn delete(&self, _query: &str) -> adk_core::Result<u64> {
        // The inner `delete` matches entries in every project.
        Err(AdkError::new(
            ErrorComponent::Memory,
            ErrorCategory::Unsupported,
            "kernel.partition.memory_delete",
            format!(
                "partition `{}` cannot delete by query: the memory's delete reaches every \
                 project; delete the partition's project through its MemoryService",
                self.partition
            ),
        ))
    }

    fn supports_project_scoping(&self) -> bool {
        true
    }

    async fn search_in_project(
        &self,
        query: &str,
        project_id: &str,
    ) -> adk_core::Result<Vec<MemoryEntry>> {
        if project_id != self.project {
            return Err(self.refuse(project_id));
        }
        self.inner.search_in_project(query, project_id).await
    }

    async fn add_to_project(&self, entry: MemoryEntry, project_id: &str) -> adk_core::Result<()> {
        if project_id != self.project {
            return Err(self.refuse(project_id));
        }
        self.inner.add_to_project(entry, project_id).await
    }
}

/// The partition table and the crossing ledger (spec 3.2 §7.8, §13 Q8), on every backend
/// [`ProcessStore`](crate::ProcessStore) has: [`InMemoryPartitionStore`](crate::InMemoryPartitionStore),
/// `SqlitePartitionStore`, `PostgresPartitionStore` and `RedisPartitionStore`, with one
/// conformance suite (`partition_store_conformance_tests!`, feature `conformance`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{InMemoryPartitionStore, Partition, PartitionId, PartitionStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryPartitionStore::new();
/// store.put(Partition::new("work", "work")).await?;
/// assert_eq!(store.get(&PartitionId::new("work")).await?.unwrap().class, "work");
/// assert_eq!(store.list().await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait PartitionStore: Send + Sync {
    /// Inserts `partition`, or replaces the record with the same id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn put(&self, partition: Partition) -> Result<()>;

    /// The partition `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn get(&self, id: &PartitionId) -> Result<Option<Partition>>;

    /// Every partition, ordered by id in byte order.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn list(&self) -> Result<Vec<Partition>>;

    /// Deletes the partition `id`; `false` when there was none. Its crossing records stay.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn delete(&self, id: &PartitionId) -> Result<bool>;

    /// Appends one crossing record. Records are never changed or deleted.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn record_crossing(&self, record: CrossingRecord) -> Result<()>;

    /// The crossing records `filter` selects, ordered by time, then id, at most
    /// `filter.limit`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>>;

    /// Records that the read with crossing-fingerprint digest `fingerprint` was approved in
    /// the durable run `run`, for the rest of that run (spec 3.2 §13 Q3). Granting twice
    /// keeps one grant.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn grant(&self, run: &str, fingerprint: &str) -> Result<()>;

    /// The fingerprint digests the durable run `run` holds grants for.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn grants(&self, run: &str) -> Result<Vec<String>>;

    /// Drops every grant of the durable run `run`, which finished; answers how many.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn release_run(&self, run: &str) -> Result<u64>;
}

/// An unknown partition, as the control plane answers it (`404`).
pub(crate) fn unknown_partition(id: &PartitionId) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::NotFound,
        NOT_FOUND_CODE,
        format!("no partition named `{id}`; list them with `adk-rust partitions`"),
    ))
}

/// How many (fingerprint, call, outcome) triples the policy remembers having recorded, so
/// the re-evaluations of one call (an edit, a prompt's re-check) record once.
const RECORDED_CAP: usize = 4096;

/// How many invocations' grants are kept in memory before they are all dropped.
const INVOCATION_GRANTS_CAP: usize = 4096;

/// The partition table and its crossings (spec 3.2 §7.8): reads and changes partitions
/// with `kernel.partition.created` and `kernel.partition.changed` records, builds each
/// partition's [`PartitionPolicy`], and keeps the crossing ledger the policy and the crossing
/// tools write and the run's approvals are read from. Cloning shares it.
///
/// # Example
///
/// ```rust
/// use adk_core::audit_log::AuditActor;
/// use adk_kernel::{CrossPartition, Partition, Partitions};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let partitions = Partitions::in_memory();
/// let by = AuditActor::default();
/// partitions.put(Partition::new("home", "home").with_cross_partition(CrossPartition::CoordinatorWithApproval), by.clone()).await?;
/// partitions.put(Partition::new("work", "work").with_cross_partition(CrossPartition::CoordinatorWithApproval), by.clone()).await?;
/// let home = partitions.block("home", "work", by).await?;
/// assert!(home.blocked_crossings.contains(&"work".into()));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Partitions {
    inner: Arc<PartitionsInner>,
}

struct PartitionsInner {
    store: Arc<dyn PartitionStore>,
    audit: OnceLock<AuditTrail>,
    clock: Arc<dyn Clock>,
    recorded: Mutex<HashSet<(String, String, CrossingOutcome)>>,
    /// The approved reads of invocations outside a durable run, by invocation id; process
    /// memory, so they end with the process if the invocation never does.
    invocation_grants: Mutex<HashMap<String, HashSet<String>>>,
}

impl fmt::Debug for Partitions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partitions")
            .field("audit", &self.inner.audit.get().is_some())
            .finish_non_exhaustive()
    }
}

impl Partitions {
    /// The partitions in `store`.
    pub fn new(store: Arc<dyn PartitionStore>) -> Self {
        Self::with_parts(store, Arc::new(SystemClock))
    }

    /// The partitions in a new [`InMemoryPartitionStore`](crate::InMemoryPartitionStore).
    pub fn in_memory() -> Self {
        Self::new(Arc::new(crate::stores::InMemoryPartitionStore::new()))
    }

    fn with_parts(store: Arc<dyn PartitionStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(PartitionsInner {
                store,
                audit: OnceLock::new(),
                clock,
                recorded: Mutex::new(HashSet::new()),
                invocation_grants: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Writes the partition and crossing records to `trail` (spec 3.2 §9.3).
    #[must_use]
    pub fn with_audit(self, trail: AuditTrail) -> Self {
        let _ = self.inner.audit.set(trail);
        self
    }

    /// Reads crossing times from `clock` instead of the system clock.
    #[must_use]
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        let partitions = Self::with_parts(Arc::clone(&self.inner.store), clock);
        if let Some(trail) = self.inner.audit.get() {
            let _ = partitions.inner.audit.set(trail.clone());
        }
        partitions
    }

    /// Sets the audit trail when none is set yet; the kernel attaches its own at build.
    pub fn attach_audit(&self, trail: AuditTrail) {
        let _ = self.inner.audit.set(trail);
    }

    /// The store.
    pub fn store(&self) -> &Arc<dyn PartitionStore> {
        &self.inner.store
    }

    /// The partition `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn get(&self, id: &PartitionId) -> Result<Option<Partition>> {
        self.inner.store.get(id).await
    }

    /// Every partition, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn list(&self) -> Result<Vec<Partition>> {
        self.inner.store.list().await
    }

    /// Creates or changes `partition`, recording `kernel.partition.created` or
    /// `kernel.partition.changed` with the fields that changed.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the record is invalid, or the store's error.
    pub async fn put(&self, partition: Partition, by: AuditActor) -> Result<()> {
        partition.validate()?;
        let before = self.inner.store.get(&partition.id).await?;
        let to = serde_json::to_value(&partition)?;
        let (action, fields, from) = match &before {
            None => (audit::PARTITION_CREATED, Vec::new(), Value::Null),
            Some(before) => {
                let from = serde_json::to_value(before)?;
                let changed: Vec<String> = match (&from, &to) {
                    (Value::Object(old), Value::Object(new)) => old
                        .keys()
                        .chain(new.keys())
                        .filter(|key| old.get(*key) != new.get(*key))
                        .cloned()
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                };
                if changed.is_empty() {
                    return Ok(());
                }
                (audit::PARTITION_CHANGED, changed, from)
            }
        };
        self.inner.store.put(partition.clone()).await?;
        let fields = json!({
            "partition": partition.id,
            "fields": fields,
            "from": from,
            "to": to,
        });
        self.record_audit(action, by, fields).await;
        Ok(())
    }

    /// Sets the mode of partition `id`.
    ///
    /// # Errors
    ///
    /// Returns a `404` error when there is no such partition, or the store's error.
    pub async fn set_mode(
        &self,
        id: impl Into<PartitionId>,
        mode: PartitionMode,
        by: AuditActor,
    ) -> Result<Partition> {
        let id = id.into();
        let partition = self.inner.store.get(&id).await?.ok_or_else(|| unknown_partition(&id))?;
        let partition = partition.with_mode(mode);
        self.put(partition.clone(), by).await?;
        Ok(partition)
    }

    /// Blocks crossings from `from` into `to`: the prompt's "deny and block crossings to
    /// *to* from this partition". The block stays until the partition record is changed.
    ///
    /// # Errors
    ///
    /// Returns a `404` error when `from` is not a partition, or the store's error.
    pub async fn block(
        &self,
        from: impl Into<PartitionId>,
        to: impl Into<PartitionId>,
        by: AuditActor,
    ) -> Result<Partition> {
        let from = from.into();
        let partition =
            self.inner.store.get(&from).await?.ok_or_else(|| unknown_partition(&from))?;
        let partition = partition.with_blocked_crossing(to);
        self.put(partition.clone(), by).await?;
        Ok(partition)
    }

    /// The crossing records `filter` selects.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>> {
        self.inner.store.crossings(filter).await
    }

    /// The policy of partition `home`'s harness ([`PartitionPolicy`]).
    pub fn policy(&self, home: impl Into<PartitionId>) -> PartitionPolicy {
        PartitionPolicy::new(self.clone(), home.into())
    }

    /// Why `crossing` may not happen, or `Ok` when it may ask for approval: both
    /// partitions exist and take part in crossings, the caller is the coordinator, and the
    /// source has not blocked the target (spec 3.2 §7.8, K-I15).
    ///
    /// # Errors
    ///
    /// The refusal's reason, also when the partition table cannot be read: a crossing
    /// fails closed.
    pub async fn admit(
        &self,
        crossing: &Crossing,
        coordinator: bool,
    ) -> std::result::Result<(), String> {
        let read = |id: &PartitionId| {
            let id = id.clone();
            async move {
                match self.inner.store.get(&id).await {
                    Ok(Some(partition)) => Ok(partition),
                    Ok(None) => Err(format!("there is no partition named `{id}`")),
                    Err(error) => Err(format!(
                        "the partition table could not be read ({error}), so the crossing is \
                         refused"
                    )),
                }
            }
        };
        let from = read(&crossing.from).await?;
        let to = read(&crossing.to).await?;
        if !coordinator {
            return Err(format!(
                "{} belongs to partition `{}`; only the coordinator may cross into another \
                 partition, and only under approval",
                crossing.label(),
                crossing.to
            ));
        }
        for partition in [&from, &to] {
            if partition.cross_partition == CrossPartition::Deny {
                return Err(format!(
                    "partition `{}` takes part in no crossings (cross_partition: deny)",
                    partition.id
                ));
            }
        }
        if from.blocked_crossings.contains(&crossing.to) {
            return Err(format!(
                "crossings to `{}` from partition `{}` are blocked",
                crossing.to, crossing.from
            ));
        }
        Ok(())
    }

    /// Whether the read `fingerprint` (a [`Crossing::fingerprint`]) was approved earlier in
    /// `run` and the run still holds the grant, so the coordinator's identical read needs no
    /// approval for the rest of the run (spec 3.2 §13 Q3). A failed read of the store answers
    /// `false`: the crossing asks again.
    pub async fn granted(&self, run: &CrossingRun, fingerprint: &str) -> bool {
        let digest = crossing::digest(fingerprint);
        match run {
            CrossingRun::Invocation(id) => self
                .inner
                .invocation_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(id)
                .is_some_and(|grants| grants.contains(&digest)),
            CrossingRun::Durable(id) => match self.inner.store.grants(id).await {
                Ok(grants) => grants.contains(&digest),
                Err(error) => {
                    tracing::warn!(error = %error, "could not read the run's crossing grants; the crossing asks again");
                    false
                }
            },
        }
    }

    /// The fingerprint digests `run` holds grants for.
    ///
    /// # Errors
    ///
    /// Returns the store's error for a durable run.
    pub async fn grants(&self, run: &CrossingRun) -> Result<Vec<String>> {
        match run {
            CrossingRun::Invocation(id) => {
                let grants = self.inner.invocation_grants.lock().unwrap_or_else(|e| e.into_inner());
                let mut digests: Vec<String> =
                    grants.get(id).map(|set| set.iter().cloned().collect()).unwrap_or_default();
                digests.sort();
                Ok(digests)
            }
            CrossingRun::Durable(id) => self.inner.store.grants(id).await,
        }
    }

    /// Drops every grant of `run`, which finished, so no later run can reuse one (spec 3.2
    /// §13 Q3): the kernel's worker calls it when a durable run ends, the partition's
    /// harness when an invocation outside a durable run ends. Answers how many it dropped.
    ///
    /// # Errors
    ///
    /// Returns the store's error for a durable run.
    pub async fn release_run(&self, run: &CrossingRun) -> Result<u64> {
        match run {
            CrossingRun::Invocation(id) => Ok(self
                .inner
                .invocation_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id)
                .map_or(0, |grants| grants.len() as u64)),
            CrossingRun::Durable(id) => self.inner.store.release_run(id).await,
        }
    }

    /// Records that `crossing` ran in `run` after its approval, and, for a read, keeps the
    /// approval the coordinator's identical reads reuse for the rest of the run. A write is
    /// never kept: it is approved one call at a time. A crossing tool calls it after the
    /// governed path authorized the call.
    ///
    /// # Errors
    ///
    /// Returns the store's error; the tool then reports the crossing unrecorded.
    pub async fn executed(
        &self,
        crossing: &Crossing,
        fingerprint: &str,
        run: &CrossingRun,
        agent: &str,
        tool: &str,
    ) -> Result<()> {
        let read = crossing.access == CrossingAccess::Read;
        if read && self.granted(run, fingerprint).await {
            return Ok(());
        }
        let record = self.crossing_record(
            crossing,
            fingerprint,
            run.as_str(),
            agent,
            tool,
            CrossingOutcome::Approved,
            None,
        );
        self.inner.store.record_crossing(record.clone()).await?;
        self.audit_crossing(&record).await;
        if read {
            match run {
                CrossingRun::Invocation(id) => {
                    let mut grants =
                        self.inner.invocation_grants.lock().unwrap_or_else(|e| e.into_inner());
                    // Dropping everything only makes later reads ask again.
                    if grants.len() > INVOCATION_GRANTS_CAP {
                        grants.clear();
                    }
                    grants.entry(id.clone()).or_default().insert(record.fingerprint);
                }
                CrossingRun::Durable(id) => self.inner.store.grant(id, &record.fingerprint).await?,
            }
        }
        Ok(())
    }

    /// The crossings since `since` that recur at least `at_least` times, by source, target,
    /// resource and name, most frequent first (spec 3.2 §7.8): what the maintenance process
    /// reports, with partition membership as the remedy. A request counts once: held, denied,
    /// or let through by its run's approval; the approval of a held request is not counted
    /// again.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn recurring_crossings(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        at_least: usize,
    ) -> Result<Vec<RecurringCrossing>> {
        let filter = CrossingFilter::default().since(since).outcomes([
            CrossingOutcome::Held,
            CrossingOutcome::Denied,
            CrossingOutcome::RunGrant,
        ]);
        let mut counts: std::collections::BTreeMap<
            (PartitionId, PartitionId, CrossingResource, String),
            usize,
        > = std::collections::BTreeMap::new();
        for record in self.inner.store.crossings(&filter).await? {
            *counts.entry((record.from, record.to, record.resource, record.name)).or_default() += 1;
        }
        let mut recurring: Vec<RecurringCrossing> = counts
            .into_iter()
            .filter(|(_, count)| *count >= at_least)
            .map(|((from, to, resource, name), count)| {
                RecurringCrossing::new(from, to, resource, name, count)
            })
            .collect();
        recurring.sort_by_key(|crossing| std::cmp::Reverse(crossing.count));
        Ok(recurring)
    }

    /// Records a decision the policy made about a crossing, once per call and outcome.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn decided(
        &self,
        crossing: &Crossing,
        fingerprint: &str,
        run: &str,
        call_id: Option<&str>,
        agent: &str,
        tool: &str,
        outcome: CrossingOutcome,
        reason: Option<String>,
    ) {
        if let Some(call_id) = call_id {
            let mut recorded = self.inner.recorded.lock().unwrap_or_else(|e| e.into_inner());
            if recorded.len() > RECORDED_CAP {
                recorded.clear();
            }
            // Keyed by the call, not the run: a prompt's re-check sees no durable run id.
            if !recorded.insert((crossing::digest(fingerprint), call_id.to_string(), outcome)) {
                return;
            }
        }
        let record = self.crossing_record(crossing, fingerprint, run, agent, tool, outcome, reason);
        if let Err(error) = self.inner.store.record_crossing(record.clone()).await {
            tracing::warn!(error = %error, "could not write a crossing record");
        }
        self.audit_crossing(&record).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn crossing_record(
        &self,
        crossing: &Crossing,
        fingerprint: &str,
        run: &str,
        agent: &str,
        tool: &str,
        outcome: CrossingOutcome,
        reason: Option<String>,
    ) -> CrossingRecord {
        CrossingRecord {
            id: uuid::Uuid::new_v4().to_string(),
            at: self.inner.clock.now(),
            run: run.to_string(),
            agent: agent.to_string(),
            from: crossing.from.clone(),
            to: crossing.to.clone(),
            resource: crossing.resource,
            name: crossing.name.clone(),
            access: crossing.access,
            tool: tool.to_string(),
            fingerprint: crossing::digest(fingerprint),
            outcome,
            reason,
        }
    }

    /// Writes `kernel.partition.crossing` (spec 3.2 §9.3): ids, names and a digest, never
    /// the call's arguments.
    async fn audit_crossing(&self, record: &CrossingRecord) {
        let actor = match adk_core::AgentId::new(record.agent.clone()) {
            Ok(agent) => AuditActor { agent: Some(agent), ..AuditActor::default() },
            Err(_) => AuditActor::default(),
        };
        let fields = json!({
            "partition": record.from,
            "to": record.to,
            "resource": record.resource,
            "name": record.name,
            "access": record.access,
            "outcome": record.outcome,
            "run": record.run,
            "tool": record.tool,
            "fingerprintDigest": record.fingerprint,
            "reason": record.reason,
            "by": record.agent,
        });
        self.record_audit(audit::PARTITION_CROSSING, actor, fields).await;
    }

    async fn record_audit(&self, action: &str, actor: AuditActor, fields: Value) {
        let Some(trail) = self.inner.audit.get() else { return };
        let mut draft =
            AuditRecordDraft::new(AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.into()), action)
                .actor(actor);
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(audit.action = action, error = %error, "could not write a partition audit record");
        }
    }
}
