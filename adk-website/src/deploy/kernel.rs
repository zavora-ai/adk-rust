//! The served site's durable stores (spec 3.5 §7.17, §7.22): one SQLite database,
//! `data/kernel.db`, so a restart keeps the charters, the jobs on the ladder, the KPIs, the
//! signals and the agenda, the approvals and the owner's paired devices, the runs — weekly
//! reviews among them — the process table, so a process asleep on a wake set, quiet hours or
//! a spent budget still wakes after a restart, the kill switch and the control plane's
//! settings.
//!
//! | Store | SQLite |
//! |---|---|
//! | Every kernel store: runs, processes, schedules, approvals, action and spend ledgers, the kill switch, agenda, partitions, cron watermarks, added agents, devices, control settings, drivers, the activity ledger, chat pairings, charters, KPIs, autonomy and signals | [`adk_kernel::stores::KernelStores`] on the database's pool ([`SiteStores::kernel`]) |
//! | Approvals routed to the owner | `StoreApprovalService` over [`KernelStores::approvals`](adk_kernel::stores::KernelStores::approvals) ([`SiteStores::approvals`]) |
//! | The partition table | [`Partitions`] over [`KernelStores::partitions`](adk_kernel::stores::KernelStores::partitions) ([`SiteStores::partitions`]) |
//! | The owner's standing rules | `adk_harness::SqliteStandingRuleStore`: what `adk-rust website jobs promote` grants and the served back office reads |

use std::sync::Arc;

use adk_core::{
    AdkError, ApprovalPolicy, ApprovalService, ApproverSelector, Result, StoreApprovalService,
};
use adk_kernel::Partitions;
use adk_kernel::stores::KernelStores;

fn store(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("the site's stores could not open: {error}"))
}

/// The served site's durable stores in one SQLite database: the kernel's, and the site's
/// approval routing, partition table and standing rules over them.
#[derive(Clone)]
pub struct SiteStores {
    /// Every store the kernel keeps, on the database's pool.
    pub kernel: KernelStores,
    /// Approvals: held calls and the owner's decision cards, routed to the owner.
    pub approvals: Arc<dyn ApprovalService>,
    /// The partition table.
    pub partitions: Partitions,
    /// The owner's standing rules: a promotion grants them, and the back office's harnesses
    /// read them at every call (spec 3.5 §7.11, A-I13).
    pub rules: Arc<adk_harness::SqliteStandingRuleStore>,
    /// The kill switch, mirrored over the kernel's governance store: the kernel and every
    /// site share it, so a freeze stops agent runs and holds every release, a person's in
    /// the desk included (#1280).
    pub governance: adk_core::GovernanceControl,
}

impl std::fmt::Debug for SiteStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteStores").field("kernel", &self.kernel).finish_non_exhaustive()
    }
}

impl SiteStores {
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
        let kernel =
            KernelStores::from_sqlite_pool(pool.clone()).await.map_err(store)?.with_label(url);
        let mut service = StoreApprovalService::new(kernel.approvals.clone())
            .with_router(ApprovalPolicy::new([ApproverSelector::human(owner)]))
            // The site charters decide who holds a role, never a surface's claim (A-I23).
            .with_directory(Arc::new(adk_kernel::charter::CharterDirectory::new(
                kernel.charter.charters.clone(),
            )));
        for notifier in notifiers {
            service = service.with_notifier(notifier);
        }
        let rules = Arc::new(adk_harness::SqliteStandingRuleStore::new(pool));
        rules.migrate().await?;
        Ok(Self {
            approvals: Arc::new(service),
            partitions: Partitions::new(kernel.partitions.clone()),
            governance: adk_kernel::KernelConfig::mirrored_governance(kernel.governance.clone()),
            kernel,
            rules,
        })
    }
}
