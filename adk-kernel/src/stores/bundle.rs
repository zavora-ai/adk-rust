//! [`KernelStores`]: every stateful store a kernel keeps, opened together on one backend
//! (spec 3.2 §7.1, §13 Q8; spec 3.5 §7.22).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use adk_core::{
    ActionLedger, ApprovalStore, DurableRunStore, GovernanceStateStore, InMemoryActionLedger,
    InMemoryApprovalStore, InMemoryGovernanceStateStore, InMemoryRunStore, InMemoryScheduleStore,
    InMemorySpendLedger, ScheduleStore, SpendLedger,
};
use serde::{Deserialize, Serialize};

use super::{
    InMemoryAgendaStore, InMemoryAgentStore, InMemoryCronWatermarkStore, InMemoryPartitionStore,
    InMemoryProcessStore, ProcessStore,
};
use crate::agenda::AgendaStore;
use crate::agents::AgentStore;
use crate::control::{ControlStore, InMemoryControlStore};
use crate::error::{KernelError, Result};
use crate::pairing::{DeviceStore, InMemoryDeviceStore};
use crate::partition::PartitionStore;
use crate::watermark::CronWatermarkStore;

/// Every stateful store a kernel keeps, opened together on one backend (spec 3.2 §7.1, §13
/// Q8): the bundle [`KernelBuilder::stores`](crate::KernelBuilder::stores) and
/// `TenantKernelBuilder::stores` (feature `managed`) take, so a host opens one value instead
/// of assembling each store, and a kernel never keeps a store in memory by accident.
///
/// | Field | Store | Backends |
/// |---|---|---|
/// | `runs`, `schedules`, `approvals`, `actions`, `spend`, `governance` | `adk-session`'s run, schedule, approval, action-ledger, spend-ledger and kill-switch stores | every backend |
/// | `processes`, `agenda`, `partitions`, `watermarks`, `agents` | the process side table, the agenda, the partition table, cron watermarks, added agents | every backend |
/// | `drivers` (feature `drivers`), `ledger` (feature `proc`) | mounted drivers, the activity ledger | every backend |
/// | `devices`, `control`, `channel_bindings` (feature `channels`) | paired devices, settings and "While you were away" cursors, paired chat accounts | in-memory, SQLite, PostgreSQL, Redis |
/// | `charter` (feature `charter`) | charters, KPIs, autonomy, signals ([`CharterStores`](crate::charter::CharterStores)) | in-memory, SQLite, PostgreSQL, Redis |
///
/// A store with no backend on the bundle's backend is kept in memory and named by
/// [`in_memory_stores`](Self::in_memory_stores), as every store of
/// [`in_memory`](Self::in_memory) is; [`KernelBuilder::require_durable`](crate::KernelBuilder::require_durable)
/// refuses a kernel that would use one. The record describes the bundle as it was opened: a
/// field replaced afterwards keeps its entry.
///
/// [`open`](Self::open) takes the store URLs the rest of ADK-Rust takes:
///
/// | URL | Backend | Feature |
/// |---|---|---|
/// | `memory:` | [`in_memory`](Self::in_memory) | — |
/// | `sqlite://<path>?mode=rwc` | SQLite, one pool | `sqlite` (default) |
/// | `postgres://…`, `postgresql://…` | PostgreSQL, one pool | `postgres` |
/// | `redis://…`, `rediss://…` | Redis, one client | `redis` |
/// | `mongodb://…/<database>`, `mongodb+srv://…` (database `adk` when the path is empty) | MongoDB | `mongodb` |
/// | `neo4j://<user>:<password>@<host>`, `neo4j+s://…`, `bolt://…`, `bolt+s://…` | Neo4j | `neo4j` |
/// | `firestore://<project>` | Firestore, with Application Default Credentials | `firestore` |
///
/// # Example
///
/// ```rust
/// use adk_kernel::stores::KernelStores;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let memory = KernelStores::in_memory();
/// assert!(memory.is_in_memory("processes"));
/// assert!(!memory.is_durable());
///
/// let dir = tempfile::tempdir()?;
/// let url = format!("sqlite://{}?mode=rwc", dir.path().join("kernel.db").display());
/// let sqlite = KernelStores::open(&url).await?;
/// assert!(sqlite.is_durable());
/// assert_eq!(sqlite.label(), url);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct KernelStores {
    /// The durable runs every process lives in.
    pub runs: Arc<dyn DurableRunStore>,
    /// The process side table: wake sets, admission deferrals, suspensions and daily spend
    /// totals, which the wake router finds sleepers through.
    pub processes: Arc<dyn ProcessStore>,
    /// The schedules the kernel's scheduler fires.
    pub schedules: Arc<dyn ScheduleStore>,
    /// Held calls and approval requests, for the approval service the host builds over it.
    pub approvals: Arc<dyn ApprovalStore>,
    /// Exactly-once records of non-idempotent calls (K-I4).
    pub actions: Arc<dyn ActionLedger>,
    /// Committed and reserved spend.
    pub spend: Arc<dyn SpendLedger>,
    /// The kill switch's shared state, which `KernelConfig::mirrored_governance` mirrors.
    pub governance: Arc<dyn GovernanceStateStore>,
    /// The agenda (spec 3.2 §7.5).
    pub agenda: Arc<dyn AgendaStore>,
    /// The partition table and the crossing ledger (spec 3.2 §7.8).
    pub partitions: Arc<dyn PartitionStore>,
    /// Where each cron wake source left off (spec 3.2 §7.4).
    pub watermarks: Arc<dyn CronWatermarkStore>,
    /// The agents a person added (spec 3.3 §5.7).
    pub agents: Arc<dyn AgentStore>,
    /// Paired devices and their waiting codes, hashed (spec 3.3 §10 K2).
    pub devices: Arc<dyn DeviceStore>,
    /// Settings changed while the kernel runs and each device's place in "While you were
    /// away" (spec 3.3 §10 K3, K4).
    pub control: Arc<dyn ControlStore>,
    /// Mounted drivers and their parked calls (spec 3.2 §7.7).
    #[cfg(feature = "drivers")]
    pub drivers: Arc<dyn crate::drivers::DriverStore>,
    /// The content-free activity ledger (spec 3.2 §7.10).
    #[cfg(feature = "proc")]
    pub ledger: Arc<dyn crate::proc::LedgerStore>,
    /// Chat accounts paired to the owner and staff (spec 3.3 §10 K1).
    #[cfg(feature = "channels")]
    pub channel_bindings: Arc<dyn crate::channels::ChannelBindingStore>,
    /// Charters, KPIs, autonomy and signals (spec 3.5 §7.10, §7.11).
    #[cfg(feature = "charter")]
    pub charter: crate::charter::CharterStores,
    label: String,
    in_memory: BTreeSet<&'static str>,
}

impl fmt::Debug for KernelStores {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelStores")
            .field("label", &self.label)
            .field("in_memory", &self.in_memory)
            .finish_non_exhaustive()
    }
}

/// The stores a bundle on MongoDB, Firestore or Neo4j keeps in memory: those backends have
/// none of them yet.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
const SQL_AND_REDIS_ONLY: &[&str] = &[
    "devices",
    "control",
    #[cfg(feature = "channels")]
    "channel_bindings",
    #[cfg(feature = "charter")]
    "charter.charters",
    #[cfg(feature = "charter")]
    "charter.kpis",
    #[cfg(feature = "charter")]
    "charter.autonomy",
    #[cfg(feature = "charter")]
    "charter.signals",
];

impl KernelStores {
    /// The name of every store of a bundle, as [`in_memory_stores`](Self::in_memory_stores)
    /// names them.
    pub const STORES: &'static [&'static str] = &[
        "runs",
        "processes",
        "schedules",
        "approvals",
        "actions",
        "spend",
        "governance",
        "agenda",
        "partitions",
        "watermarks",
        "agents",
        "devices",
        "control",
        #[cfg(feature = "drivers")]
        "drivers",
        #[cfg(feature = "proc")]
        "ledger",
        #[cfg(feature = "channels")]
        "channel_bindings",
        #[cfg(feature = "charter")]
        "charter.charters",
        #[cfg(feature = "charter")]
        "charter.kpis",
        #[cfg(feature = "charter")]
        "charter.autonomy",
        #[cfg(feature = "charter")]
        "charter.signals",
    ];

    /// Every store in memory: nothing outlives the process, and
    /// [`KernelBuilder::require_durable`](crate::KernelBuilder::require_durable) refuses it.
    /// Its label is `memory`.
    pub fn in_memory() -> Self {
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            processes: Arc::new(InMemoryProcessStore::new()),
            schedules: Arc::new(InMemoryScheduleStore::new()),
            approvals: Arc::new(InMemoryApprovalStore::new()),
            actions: Arc::new(InMemoryActionLedger::new()),
            spend: Arc::new(InMemorySpendLedger::new(adk_core::SpendLimits::default())),
            governance: Arc::new(InMemoryGovernanceStateStore::new()),
            agenda: Arc::new(InMemoryAgendaStore::new()),
            partitions: Arc::new(InMemoryPartitionStore::new()),
            watermarks: Arc::new(InMemoryCronWatermarkStore::new()),
            agents: Arc::new(InMemoryAgentStore::new()),
            devices: Arc::new(InMemoryDeviceStore::new()),
            control: Arc::new(InMemoryControlStore::new()),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(super::InMemoryDriverStore::new()),
            #[cfg(feature = "proc")]
            ledger: Arc::new(super::InMemoryLedgerStore::new()),
            #[cfg(feature = "channels")]
            channel_bindings: Arc::new(crate::channels::InMemoryChannelBindingStore::new()),
            #[cfg(feature = "charter")]
            charter: crate::charter::CharterStores::in_memory(),
            label: "memory".into(),
            in_memory: Self::STORES.iter().copied().collect(),
        }
    }

    /// Opens and migrates every store at `url` (see the type's URL table); the label is the
    /// URL with its password redacted.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for a URL with no scheme, an unknown one, or one whose
    /// feature this build does not include, naming the feature; and
    /// [`KernelError::Store`] when the backend cannot be reached or migrated.
    pub async fn open(url: &str) -> Result<Self> {
        let url = url.trim();
        let label = redact(url);
        let scheme = url
            .split_once(':')
            .map(|(scheme, _)| scheme.to_ascii_lowercase())
            .filter(|scheme| !scheme.is_empty())
            .ok_or_else(|| unknown_scheme(&label))?;
        let stores: Self = match scheme.as_str() {
            "memory" => Self::in_memory(),
            #[cfg(feature = "sqlite")]
            "sqlite" => {
                let pool = sqlx::sqlite::SqlitePoolOptions::new()
                    .max_connections(8)
                    .connect(url)
                    .await
                    .map_err(|error| store_error("sqlite", &label, error))?;
                Self::from_sqlite_pool(pool).await?
            }
            #[cfg(feature = "postgres")]
            "postgres" | "postgresql" => {
                let pool = sqlx::PgPool::connect(url)
                    .await
                    .map_err(|error| store_error("postgres", &label, error))?;
                Self::from_postgres_pool(pool).await?
            }
            #[cfg(feature = "redis")]
            "redis" | "rediss" => {
                let processes = super::RedisProcessStore::connect(url).await?;
                Self::from_redis_client(processes.client().clone()).await?
            }
            #[cfg(feature = "mongodb")]
            "mongodb" | "mongodb+srv" => {
                let (_, path) = authority_and_path(url);
                let database = if path.is_empty() { "adk" } else { path };
                Self::from_mongo_database(super::mongodb::connect(url, database).await?).await?
            }
            #[cfg(feature = "neo4j")]
            "neo4j" | "neo4j+s" | "neo4j+ssc" | "bolt" | "bolt+s" | "bolt+ssc" => {
                let (authority, path) = authority_and_path(url);
                let (userinfo, host) =
                    authority.rsplit_once('@').ok_or_else(|| KernelError::Config {
                        reason: format!(
                            "a Neo4j store URL needs credentials: \
                             {scheme}://<user>:<password>@{authority}"
                        ),
                    })?;
                let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
                let uri = if path.is_empty() {
                    format!("{scheme}://{host}")
                } else {
                    format!("{scheme}://{host}/{path}")
                };
                Self::from_neo4j_graph(super::neo4j::connect(&uri, user, password).await?).await?
            }
            #[cfg(feature = "firestore")]
            "firestore" => {
                let (project, _) = authority_and_path(url);
                if project.is_empty() {
                    return Err(KernelError::Config {
                        reason: "a Firestore store URL names the project: firestore://<project-id>"
                            .into(),
                    });
                }
                Self::from_firestore_db(super::firestore::connect(project).await?).await?
            }
            other => {
                let feature = match other {
                    "sqlite" => "sqlite",
                    "postgres" | "postgresql" => "postgres",
                    "redis" | "rediss" => "redis",
                    "mongodb" | "mongodb+srv" => "mongodb",
                    "neo4j" | "neo4j+s" | "neo4j+ssc" | "bolt" | "bolt+s" | "bolt+ssc" => "neo4j",
                    "firestore" => "firestore",
                    _ => return Err(unknown_scheme(&label)),
                };
                return Err(KernelError::Config {
                    reason: format!(
                        "the kernel's stores at {label} need adk-kernel's `{feature}` feature, \
                         which this build does not include"
                    ),
                });
            }
        };
        Ok(stores.with_label(label))
    }

    /// Every store on `pool`, each with its own migration table, such as the pool of
    /// `harness.db` (spec 3.2 §7.1, §13 Q8). The label is `sqlite`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "sqlite")]
    pub async fn from_sqlite_pool(pool: sqlx::SqlitePool) -> Result<Self> {
        use adk_session::{
            SqliteActionLedger, SqliteApprovalStore, SqliteGovernanceStateStore, SqliteRunStore,
            SqliteScheduleStore, SqliteSpendLedger,
        };
        let runs = SqliteRunStore::from_pool(pool.clone());
        runs.migrate().await?;
        let schedules = SqliteScheduleStore::from_pool(pool.clone());
        schedules.migrate().await?;
        let approvals = SqliteApprovalStore::from_pool(pool.clone());
        approvals.migrate().await?;
        let actions = SqliteActionLedger::from_pool(pool.clone());
        actions.migrate().await?;
        let spend = SqliteSpendLedger::from_pool(pool.clone());
        spend.migrate().await?;
        let governance = SqliteGovernanceStateStore::from_pool(pool.clone());
        governance.migrate().await?;
        let processes = super::SqliteProcessStore::new(pool.clone());
        processes.migrate().await?;
        let agenda = super::SqliteAgendaStore::new(pool.clone());
        agenda.migrate().await?;
        let partitions = super::SqlitePartitionStore::new(pool.clone());
        partitions.migrate().await?;
        let watermarks = super::SqliteCronWatermarkStore::new(pool.clone());
        watermarks.migrate().await?;
        let agents = super::SqliteAgentStore::new(pool.clone());
        agents.migrate().await?;
        let devices = crate::pairing::SqliteDeviceStore::new(pool.clone());
        devices.migrate().await?;
        let control = crate::control::SqliteControlStore::new(pool.clone());
        control.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::SqliteDriverStore::new(pool.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::SqliteLedgerStore::new(pool.clone());
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        #[cfg(feature = "channels")]
        let channel_bindings = crate::channels::SqliteChannelBindingStore::new(pool.clone());
        #[cfg(feature = "channels")]
        channel_bindings.migrate().await?;
        #[cfg(feature = "charter")]
        let charter = crate::charter::CharterStores::sqlite(pool).await?;
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            devices: Arc::new(devices),
            control: Arc::new(control),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            #[cfg(feature = "channels")]
            channel_bindings: Arc::new(channel_bindings),
            #[cfg(feature = "charter")]
            charter,
            label: "sqlite".into(),
            in_memory: BTreeSet::new(),
        })
    }

    /// Every store on `pool`, each with its own migration table: the database every replica
    /// shares (spec 3.2 §13 Q8). The label is `postgres`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "postgres")]
    pub async fn from_postgres_pool(pool: sqlx::PgPool) -> Result<Self> {
        use adk_session::{
            PostgresActionLedger, PostgresApprovalStore, PostgresGovernanceStateStore,
            PostgresRunStore, PostgresScheduleStore, PostgresSpendLedger,
        };
        let runs = PostgresRunStore::from_pool(pool.clone());
        runs.migrate().await?;
        let schedules = PostgresScheduleStore::from_pool(pool.clone());
        schedules.migrate().await?;
        let approvals = PostgresApprovalStore::from_pool(pool.clone());
        approvals.migrate().await?;
        let actions = PostgresActionLedger::from_pool(pool.clone());
        actions.migrate().await?;
        let spend = PostgresSpendLedger::from_pool(pool.clone());
        spend.migrate().await?;
        let governance = PostgresGovernanceStateStore::from_pool(pool.clone());
        governance.migrate().await?;
        let processes = super::PostgresProcessStore::new(pool.clone());
        processes.migrate().await?;
        let agenda = super::PostgresAgendaStore::new(pool.clone());
        agenda.migrate().await?;
        let partitions = super::PostgresPartitionStore::new(pool.clone());
        partitions.migrate().await?;
        let watermarks = super::PostgresCronWatermarkStore::new(pool.clone());
        watermarks.migrate().await?;
        let agents = super::PostgresAgentStore::new(pool.clone());
        agents.migrate().await?;
        let devices = crate::pairing::PostgresDeviceStore::new(pool.clone());
        devices.migrate().await?;
        let control = crate::control::PostgresControlStore::new(pool.clone());
        control.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::PostgresDriverStore::new(pool.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::PostgresLedgerStore::new(pool.clone());
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        #[cfg(feature = "channels")]
        let channel_bindings = crate::channels::PostgresChannelBindingStore::new(pool.clone());
        #[cfg(feature = "channels")]
        channel_bindings.migrate().await?;
        #[cfg(feature = "charter")]
        let charter = crate::charter::CharterStores::postgres(pool).await?;
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            devices: Arc::new(devices),
            control: Arc::new(control),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            #[cfg(feature = "channels")]
            channel_bindings: Arc::new(channel_bindings),
            #[cfg(feature = "charter")]
            charter,
            label: "postgres".into(),
            in_memory: BTreeSet::new(),
        })
    }

    /// Every store on `client`, an initialised Redis client every replica shares (spec 3.2
    /// §13 Q8). The label is `redis`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "redis")]
    pub async fn from_redis_client(client: fred::prelude::Client) -> Result<Self> {
        use adk_session::{
            RedisActionLedger, RedisApprovalStore, RedisGovernanceStateStore, RedisRunStore,
            RedisScheduleStore, RedisSpendLedger,
        };
        let runs = RedisRunStore::from_client(client.clone());
        runs.migrate().await?;
        let schedules = RedisScheduleStore::from_client(client.clone());
        schedules.migrate().await?;
        let approvals = RedisApprovalStore::from_client(client.clone());
        approvals.migrate().await?;
        let actions = RedisActionLedger::from_client(client.clone());
        actions.migrate().await?;
        let spend = RedisSpendLedger::from_client(client.clone());
        spend.migrate().await?;
        let governance = RedisGovernanceStateStore::from_client(client.clone());
        governance.migrate().await?;
        let processes = super::RedisProcessStore::from_client(client.clone());
        processes.migrate().await?;
        let agenda = super::RedisAgendaStore::from_client(client.clone());
        agenda.migrate().await?;
        let partitions = super::RedisPartitionStore::from_client(client.clone());
        partitions.migrate().await?;
        let watermarks = super::RedisCronWatermarkStore::from_client(client.clone());
        watermarks.migrate().await?;
        let agents = super::RedisAgentStore::from_client(client.clone());
        agents.migrate().await?;
        let devices = crate::pairing::RedisDeviceStore::from_client(client.clone());
        devices.migrate().await?;
        let control = crate::control::RedisControlStore::from_client(client.clone());
        control.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::RedisDriverStore::from_client(client.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::RedisLedgerStore::from_client(client.clone());
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        #[cfg(feature = "channels")]
        let channel_bindings =
            crate::channels::RedisChannelBindingStore::from_client(client.clone());
        #[cfg(feature = "channels")]
        channel_bindings.migrate().await?;
        #[cfg(feature = "charter")]
        let charter = crate::charter::CharterStores::redis(client).await?;
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            devices: Arc::new(devices),
            control: Arc::new(control),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            #[cfg(feature = "channels")]
            channel_bindings: Arc::new(channel_bindings),
            #[cfg(feature = "charter")]
            charter,
            label: "redis".into(),
            in_memory: BTreeSet::new(),
        })
    }

    /// Every store on `db` (spec 3.2 §13 Q8). Devices, control settings, chat pairings and
    /// the charter stores have no MongoDB backend: they are kept in memory and named by
    /// [`in_memory_stores`](Self::in_memory_stores). The label is `mongodb`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "mongodb")]
    pub async fn from_mongo_database(db: mongodb::Database) -> Result<Self> {
        use adk_session::{
            MongoActionLedger, MongoApprovalStore, MongoGovernanceStateStore, MongoRunStore,
            MongoScheduleStore, MongoSpendLedger,
        };
        let runs = MongoRunStore::from_database(db.clone());
        runs.migrate().await?;
        let schedules = MongoScheduleStore::from_database(db.clone());
        schedules.migrate().await?;
        let approvals = MongoApprovalStore::from_database(db.clone());
        approvals.migrate().await?;
        let actions = MongoActionLedger::from_database(db.clone());
        actions.migrate().await?;
        let spend = MongoSpendLedger::from_database(db.clone());
        spend.migrate().await?;
        let governance = MongoGovernanceStateStore::from_database(db.clone());
        governance.migrate().await?;
        let processes = super::MongoProcessStore::from_database(db.clone());
        processes.migrate().await?;
        let agenda = super::MongoAgendaStore::from_database(db.clone());
        agenda.migrate().await?;
        let partitions = super::MongoPartitionStore::from_database(db.clone());
        partitions.migrate().await?;
        let watermarks = super::MongoCronWatermarkStore::from_database(db.clone());
        watermarks.migrate().await?;
        let agents = super::MongoAgentStore::from_database(db.clone());
        agents.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::MongoDriverStore::from_database(db.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::MongoLedgerStore::from_database(db);
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        let memory = Self::in_memory();
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            label: "mongodb".into(),
            in_memory: partial("MongoDB"),
            ..memory
        })
    }

    /// Every store on `db` (spec 3.2 §13 Q8). Devices, control settings, chat pairings and
    /// the charter stores have no Firestore backend: they are kept in memory and named by
    /// [`in_memory_stores`](Self::in_memory_stores). The label is `firestore`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "firestore")]
    pub async fn from_firestore_db(db: firestore::FirestoreDb) -> Result<Self> {
        use adk_session::{
            FirestoreActionLedger, FirestoreApprovalStore, FirestoreGovernanceStateStore,
            FirestoreRunStore, FirestoreScheduleStore, FirestoreSpendLedger,
        };
        let runs = FirestoreRunStore::from_db(db.clone());
        runs.migrate().await?;
        let schedules = FirestoreScheduleStore::from_db(db.clone());
        schedules.migrate().await?;
        let approvals = FirestoreApprovalStore::from_db(db.clone());
        approvals.migrate().await?;
        let actions = FirestoreActionLedger::from_db(db.clone());
        actions.migrate().await?;
        let spend = FirestoreSpendLedger::from_db(db.clone());
        spend.migrate().await?;
        let governance = FirestoreGovernanceStateStore::from_db(db.clone());
        governance.migrate().await?;
        let processes = super::FirestoreProcessStore::from_db(db.clone());
        processes.migrate().await?;
        let agenda = super::FirestoreAgendaStore::from_db(db.clone());
        agenda.migrate().await?;
        let partitions = super::FirestorePartitionStore::from_db(db.clone());
        partitions.migrate().await?;
        let watermarks = super::FirestoreCronWatermarkStore::from_db(db.clone());
        watermarks.migrate().await?;
        let agents = super::FirestoreAgentStore::from_db(db.clone());
        agents.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::FirestoreDriverStore::from_db(db.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::FirestoreLedgerStore::from_db(db);
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        let memory = Self::in_memory();
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            label: "firestore".into(),
            in_memory: partial("Firestore"),
            ..memory
        })
    }

    /// Every store on `graph` (spec 3.2 §13 Q8). Devices, control settings, chat pairings
    /// and the charter stores have no Neo4j backend: they are kept in memory and named by
    /// [`in_memory_stores`](Self::in_memory_stores). The label is `neo4j`.
    ///
    /// # Errors
    ///
    /// Returns the error of a migration that fails.
    #[cfg(feature = "neo4j")]
    pub async fn from_neo4j_graph(graph: neo4rs::Graph) -> Result<Self> {
        use adk_session::{
            Neo4jActionLedger, Neo4jApprovalStore, Neo4jGovernanceStateStore, Neo4jRunStore,
            Neo4jScheduleStore, Neo4jSpendLedger,
        };
        let runs = Neo4jRunStore::from_graph(graph.clone());
        runs.migrate().await?;
        let schedules = Neo4jScheduleStore::from_graph(graph.clone());
        schedules.migrate().await?;
        let approvals = Neo4jApprovalStore::from_graph(graph.clone());
        approvals.migrate().await?;
        let actions = Neo4jActionLedger::from_graph(graph.clone());
        actions.migrate().await?;
        let spend = Neo4jSpendLedger::from_graph(graph.clone());
        spend.migrate().await?;
        let governance = Neo4jGovernanceStateStore::from_graph(graph.clone());
        governance.migrate().await?;
        let processes = super::Neo4jProcessStore::from_graph(graph.clone());
        processes.migrate().await?;
        let agenda = super::Neo4jAgendaStore::from_graph(graph.clone());
        agenda.migrate().await?;
        let partitions = super::Neo4jPartitionStore::from_graph(graph.clone());
        partitions.migrate().await?;
        let watermarks = super::Neo4jCronWatermarkStore::from_graph(graph.clone());
        watermarks.migrate().await?;
        let agents = super::Neo4jAgentStore::from_graph(graph.clone());
        agents.migrate().await?;
        #[cfg(feature = "drivers")]
        let drivers = super::Neo4jDriverStore::from_graph(graph.clone());
        #[cfg(feature = "drivers")]
        drivers.migrate().await?;
        #[cfg(feature = "proc")]
        let ledger = super::Neo4jLedgerStore::from_graph(graph);
        #[cfg(feature = "proc")]
        ledger.migrate().await?;
        let memory = Self::in_memory();
        Ok(Self {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            schedules: Arc::new(schedules),
            approvals: Arc::new(approvals),
            actions: Arc::new(actions),
            spend: Arc::new(spend),
            governance: Arc::new(governance),
            agenda: Arc::new(agenda),
            partitions: Arc::new(partitions),
            watermarks: Arc::new(watermarks),
            agents: Arc::new(agents),
            #[cfg(feature = "drivers")]
            drivers: Arc::new(drivers),
            #[cfg(feature = "proc")]
            ledger: Arc::new(ledger),
            label: "neo4j".into(),
            in_memory: partial("Neo4j"),
            ..memory
        })
    }

    /// Names the bundle `label` in logs, errors and the kernel's inventory, such as
    /// `harness.db` or a store URL with its password redacted.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// The bundle's name: `memory`, the backend, or the label it was given.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The stores kept in memory, by name ([`STORES`](Self::STORES)): every store of
    /// [`in_memory`](Self::in_memory), and the stores the bundle's backend has none of.
    pub fn in_memory_stores(&self) -> Vec<&'static str> {
        self.in_memory.iter().copied().collect()
    }

    /// Whether the store `name` ([`STORES`](Self::STORES)) is kept in memory.
    pub fn is_in_memory(&self, name: &str) -> bool {
        self.in_memory.contains(name)
    }

    /// Whether every store outlives the process.
    pub fn is_durable(&self) -> bool {
        self.in_memory.is_empty()
    }
}

/// Where one of a kernel's stateful stores came from (spec 3.2 §7.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::stores::StoreOrigin;
///
/// assert_eq!(serde_json::to_value(StoreOrigin::Defaulted).unwrap(), "defaulted");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum StoreOrigin {
    /// Set on the builder or in the configuration.
    Set,
    /// Taken from the builder's [`KernelStores`].
    Bundle,
    /// Neither: the builder kept it in memory.
    Defaulted,
}

/// One of a kernel's stateful stores: where it came from and whether it is known to be held
/// in memory ([`Kernel::store_uses`](crate::Kernel::store_uses)).
///
/// `store` is the builder's name for it: `run_store`, `process_store`, `schedule_store`,
/// `spend_ledger`, `agenda_store`, `partitions`, `watermark_store`, `agent_store`,
/// `governance` or `control_store`. A store set on the builder is taken as durable, except
/// a kill switch that is not mirrored.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::Kernel;
/// use adk_kernel::stores::StoreOrigin;
///
/// let kernel = Kernel::builder().run_store(Arc::new(InMemoryRunStore::new())).build().unwrap();
/// let process = kernel.store_uses().iter().find(|used| used.store == "process_store").unwrap();
/// assert_eq!((process.origin, process.in_memory), (StoreOrigin::Defaulted, true));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StoreUse {
    /// The builder's name for the store.
    pub store: String,
    /// Where it came from.
    pub origin: StoreOrigin,
    /// Whether it is known to be held in memory, so its state ends with the process.
    pub in_memory: bool,
    /// The label of the bundle it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
}

impl StoreUse {
    pub(crate) fn new(store: &str, origin: StoreOrigin, in_memory: bool) -> Self {
        Self { store: store.to_string(), origin, in_memory, bundle: None }
    }
}

/// The in-memory record of a bundle on a backend without the stores of
/// [`SQL_AND_REDIS_ONLY`], logging each.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
fn partial(backend: &str) -> BTreeSet<&'static str> {
    for store in SQL_AND_REDIS_ONLY {
        tracing::warn!(
            kernel.store = *store,
            backend,
            "the store has no backend here and is kept in memory; open the kernel's stores on \
             SQLite, PostgreSQL or Redis to keep it"
        );
    }
    SQL_AND_REDIS_ONLY.iter().copied().collect()
}

/// The authority and path of `scheme://authority/path?query`.
#[cfg(any(feature = "mongodb", feature = "neo4j", feature = "firestore"))]
fn authority_and_path(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or("", |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    rest.split_once('/').unwrap_or((rest, ""))
}

/// `url` with the password of its user info replaced by `***`.
fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:***@{host}{tail}"),
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

fn unknown_scheme(label: &str) -> KernelError {
    KernelError::Config {
        reason: format!(
            "'{label}' is not a kernel store URL; use memory:, sqlite://, postgres://, redis://, \
             mongodb://, neo4j:// or firestore://"
        ),
    }
}

#[cfg(any(feature = "sqlite", feature = "postgres"))]
fn store_error(backend: &'static str, label: &str, error: sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not connect to {label}: {error}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_label_hides_the_password() {
        assert_eq!(
            redact("postgres://kernel:s3cret@db:5432/adk"),
            "postgres://kernel:***@db:5432/adk"
        );
        assert_eq!(redact("redis://localhost:6379"), "redis://localhost:6379");
        assert_eq!(redact("sqlite://data/kernel.db?mode=rwc"), "sqlite://data/kernel.db?mode=rwc");
        assert_eq!(redact("neo4j://neo4j:pa:ss@host:7687"), "neo4j://neo4j:***@host:7687");
    }

    #[test]
    fn the_memory_bundle_names_every_store() {
        let stores = KernelStores::in_memory();
        assert_eq!(stores.in_memory_stores(), {
            let mut all = KernelStores::STORES.to_vec();
            all.sort_unstable();
            all
        });
        assert_eq!(stores.label(), "memory");
        assert!(!stores.is_durable());
    }

    #[tokio::test]
    async fn an_unknown_or_missing_scheme_is_refused_naming_the_choices() {
        for url in ["kernel.db", "ftp://host/kernel", ":memory:"] {
            let error = KernelStores::open(url).await.unwrap_err();
            assert!(error.to_string().contains("sqlite://"), "{url}: {error}");
        }
    }

    #[cfg(not(feature = "firestore"))]
    #[tokio::test]
    async fn a_backend_this_build_lacks_names_its_feature() {
        let error = KernelStores::open("firestore://project").await.unwrap_err();
        assert!(error.to_string().contains("`firestore` feature"), "{error}");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn the_sqlite_bundle_keeps_every_store_durable() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("kernel.db").display());
        let stores = KernelStores::open(&url).await.unwrap();
        assert!(stores.is_durable(), "{:?}", stores.in_memory_stores());
        let stores = KernelStores::open("memory:").await.unwrap();
        assert!(!stores.is_durable());
    }
}
