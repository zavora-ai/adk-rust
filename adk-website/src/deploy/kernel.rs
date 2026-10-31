//! The served kernel's durable stores (spec 3.5 §7.17, §7.22): one SQLite database,
//! `data/kernel.db`, so a restart keeps the charters, the jobs on the ladder, the KPIs, the
//! signals and the agenda, the approvals and the owner's paired devices, the runs — weekly
//! reviews among them — and the process table, so a process asleep on a wake set, quiet
//! hours or a spent budget still wakes after a restart.
//!
//! | Store | SQLite |
//! |---|---|
//! | Runs | `adk_session::SqliteRunStore` |
//! | Processes: wake sets, admission deferrals, suspensions and daily spend totals | `adk_kernel::SqliteProcessStore` |
//! | Schedules the kernel's scheduler fires | `adk_session::SqliteScheduleStore` |
//! | Approvals | `adk_session::SqliteApprovalStore`, through `StoreApprovalService` |
//! | Action and spend ledgers | `adk_session::SqliteActionLedger`, `SqliteSpendLedger` |
//! | Agenda, partitions, cron watermarks | `adk_kernel::SqliteAgendaStore`, `SqlitePartitionStore`, `SqliteCronWatermarkStore` |
//! | Devices | `adk_kernel::pairing::SqliteDeviceStore` |
//! | Charters, KPIs, autonomy, signals | `adk_kernel::charter::{SqliteCharterStore, SqliteKpiStore, SqliteAutonomyStore, SqliteSignalStore}` |
//! | The owner's standing rules | `adk_harness::SqliteStandingRuleStore`: what `adk-rust website jobs promote` grants and the served back office reads |

use std::sync::Arc;

use adk_core::{
    AdkError, ApprovalPolicy, ApprovalService, ApproverSelector, Result, StoreApprovalService,
};
use adk_kernel::charter::{
    SqliteAutonomyStore, SqliteCharterStore, SqliteKpiStore, SqliteSignalStore,
};
use adk_kernel::pairing::SqliteDeviceStore;
use adk_kernel::{
    Partitions, SqliteAgendaStore, SqliteCronWatermarkStore, SqlitePartitionStore,
    SqliteProcessStore,
};
use adk_session::{
    SqliteActionLedger, SqliteApprovalStore, SqliteRunStore, SqliteScheduleStore, SqliteSpendLedger,
};

fn store(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the kernel's stores could not open: {error}"))
}

/// The kernel's durable stores in one SQLite database.
#[derive(Clone)]
pub struct KernelStores {
    /// Runs, weekly reviews among them.
    pub runs: Arc<SqliteRunStore>,
    /// The process table: each process's wake set, admission deferral, suspension and daily
    /// spend totals, which the wake router finds sleepers through.
    pub processes: Arc<SqliteProcessStore>,
    /// The schedules the kernel's scheduler fires.
    pub schedules: Arc<SqliteScheduleStore>,
    /// Approvals: held calls and the owner's decision cards.
    pub approvals: Arc<dyn ApprovalService>,
    /// The agenda signals land on.
    pub agenda: Arc<SqliteAgendaStore>,
    /// The partition table.
    pub partitions: Partitions,
    /// The owner's paired devices.
    pub devices: Arc<SqliteDeviceStore>,
    /// Charter versions.
    pub charters: Arc<SqliteCharterStore>,
    /// KPI readings, baselines and targets.
    pub kpis: Arc<SqliteKpiStore>,
    /// The jobs' autonomy levels.
    pub autonomy: Arc<SqliteAutonomyStore>,
    /// Signals.
    pub signals: Arc<SqliteSignalStore>,
    /// Exactly-once records of the edge's non-idempotent calls (A-I7).
    pub actions: Arc<SqliteActionLedger>,
    /// Committed spend.
    pub spend: Arc<SqliteSpendLedger>,
    /// Where each cron trigger left off, so a restart fires the ticks it missed once.
    pub watermarks: Arc<SqliteCronWatermarkStore>,
    /// The owner's standing rules: a promotion grants them, and the back office's harnesses
    /// read them at every call (spec 3.5 §7.11, A-I13).
    pub rules: Arc<adk_harness::SqliteStandingRuleStore>,
    /// The chat accounts paired to the owner and staff, and the pairing requests that wait
    /// (spec 3.5 §10 X12).
    pub channel_bindings: Arc<adk_kernel::channels::SqliteChannelBindingStore>,
}

impl std::fmt::Debug for KernelStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelStores").finish_non_exhaustive()
    }
}

impl KernelStores {
    /// Opens and migrates the stores at `url`, such as `sqlite://data/kernel.db?mode=rwc`;
    /// approvals route to `owner`.
    ///
    /// # Errors
    ///
    /// A database that does not open or migrate.
    pub async fn open(url: &str, owner: &str) -> Result<Self> {
        Self::open_with(url, owner, Vec::new()).await
    }

    /// Opens the stores as [`open`](Self::open) does, the approval service ringing
    /// `notifiers`, such as the doorbell of the site's chat gateway (spec 3.5 §10 X12).
    ///
    /// # Errors
    ///
    /// A database that does not open or migrate.
    pub async fn open_with(
        url: &str,
        owner: &str,
        notifiers: Vec<Arc<dyn adk_core::ApprovalNotifier>>,
    ) -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(store)?;
        let runs = Arc::new(SqliteRunStore::from_pool(pool.clone()));
        runs.migrate().await?;
        let processes = Arc::new(SqliteProcessStore::new(pool.clone()));
        processes.migrate().await.map_err(store)?;
        let schedules = Arc::new(SqliteScheduleStore::from_pool(pool.clone()));
        schedules.migrate().await?;
        let approval_store = Arc::new(SqliteApprovalStore::from_pool(pool.clone()));
        approval_store.migrate().await?;
        let mut service = StoreApprovalService::new(approval_store)
            .with_router(ApprovalPolicy::new([ApproverSelector::human(owner)]));
        for notifier in notifiers {
            service = service.with_notifier(notifier);
        }
        let approvals: Arc<dyn ApprovalService> = Arc::new(service);
        let agenda = Arc::new(SqliteAgendaStore::new(pool.clone()));
        agenda.migrate().await.map_err(store)?;
        let partition_store = Arc::new(SqlitePartitionStore::new(pool.clone()));
        partition_store.migrate().await.map_err(store)?;
        let devices = Arc::new(SqliteDeviceStore::new(pool.clone()));
        devices.migrate().await.map_err(store)?;
        let charters = Arc::new(SqliteCharterStore::new(pool.clone()));
        charters.migrate().await.map_err(store)?;
        let kpis = Arc::new(SqliteKpiStore::new(pool.clone()));
        kpis.migrate().await.map_err(store)?;
        let autonomy = Arc::new(SqliteAutonomyStore::new(pool.clone()));
        autonomy.migrate().await.map_err(store)?;
        let signals = Arc::new(SqliteSignalStore::new(pool.clone()));
        signals.migrate().await.map_err(store)?;
        let actions = Arc::new(SqliteActionLedger::from_pool(pool.clone()));
        actions.migrate().await?;
        let spend = Arc::new(SqliteSpendLedger::from_pool(pool.clone()));
        spend.migrate().await?;
        let watermarks = Arc::new(SqliteCronWatermarkStore::new(pool.clone()));
        watermarks.migrate().await.map_err(store)?;
        let channel_bindings =
            Arc::new(adk_kernel::channels::SqliteChannelBindingStore::new(pool.clone()));
        channel_bindings.migrate().await.map_err(store)?;
        let rules = Arc::new(adk_harness::SqliteStandingRuleStore::new(pool));
        rules.migrate().await?;
        Ok(Self {
            runs,
            processes,
            schedules,
            approvals,
            agenda,
            partitions: Partitions::new(partition_store),
            devices,
            charters,
            kpis,
            autonomy,
            signals,
            actions,
            spend,
            watermarks,
            rules,
            channel_bindings,
        })
    }
}
