//! [`CharterKit`]: one call that installs a site's jobs, KPI tracker and signals on a kernel
//! (spec 3.5 §7.10, §7.11, §7.19, §7.20; #1278).

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::NewRun;
use serde_json::Value;
use uuid::Uuid;

use super::signal::SignalStore;
use super::site::SiteProcessKind;
use super::tracker::KpiTracker;
use crate::inventory::InventoryWarning;
use crate::kernel::KernelBuilder;
use crate::partition::PartitionId;
use crate::wake::Wake;

/// Installs a site's charter machinery on a kernel (spec 3.5 §7.10, §7.11, §7.19, §7.20):
/// the product binds each [`SiteProcessKind`] it runs to its own target with
/// [`job`](Self::job), and [`install`](Self::install) registers on a [`KernelBuilder`]:
///
/// | Part | What `install` registers |
/// |---|---|
/// | Each bound kind | Its standing trigger ([`SiteProcessKind::trigger`]) on its default wake, or the one [`job_on`](Self::job_on) names, in the back-office partition, running the bound target with the bound input as the kit's user |
/// | The KPI tracker ([`tracker`](Self::tracker)) | [`KernelBuilder::kpi_tracker`]: its `kernel.kpi` trigger on the kit's cron and its executor, so the tracker is a kernel process — in `ps`, with a cron watermark, held during a freeze |
/// | Signals | [`KernelBuilder::signals`] over the kit's signal store in the back-office partition, so [`Kernel::signal_sink`](crate::Kernel::signal_sink) files the edge's signals and they wake the signal kinds |
/// | Each kind not bound | An inventory warning `charter.job_unbound` naming the kind, with the reason [`skip`](Self::skip) gave |
///
/// The kernel recognises the kinds and the product does their work: the kit never picks a
/// target for a kind.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_kernel::Kernel;
/// use adk_kernel::charter::{CharterKit, InMemorySignalStore, SiteProcessKind};
///
/// let site = uuid::Uuid::new_v4();
/// let kit = CharterKit::new(site, "bakery", "bakery.back-office", Arc::new(InMemorySignalStore::new()))
///     .job(SiteProcessKind::Probe, "website.guardian.probe", serde_json::json!({ "site": site }))
///     .skip(SiteProcessKind::Digest, "the owner has no channel");
/// let kernel = kit.install(Kernel::builder().run_store(Arc::new(InMemoryRunStore::new()))).build()?;
/// let triggers: Vec<&str> = kernel.wake_router().triggers().iter().map(|t| t.name.as_str()).collect();
/// assert_eq!(triggers, ["site.probe"]);
/// assert!(kernel.signal_sink("bakery.back-office").is_ok());
/// let unbound = kernel.inventory().warnings.into_iter().filter(|w| w.code == CharterKit::JOB_UNBOUND).count();
/// assert_eq!(unbound, SiteProcessKind::ALL.len() - 1);
/// # Ok::<(), adk_kernel::KernelError>(())
/// ```
#[derive(Clone)]
pub struct CharterKit {
    site: Uuid,
    app: String,
    user: String,
    back_office: PartitionId,
    signals: Arc<dyn SignalStore>,
    jobs: BTreeMap<SiteProcessKind, Job>,
    skipped: BTreeMap<SiteProcessKind, String>,
    tracker: Option<KpiTracker>,
    tracker_cron: String,
}

/// One bound kind.
#[derive(Clone)]
struct Job {
    target: String,
    input: Value,
    wake: Option<Wake>,
}

impl std::fmt::Debug for CharterKit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharterKit")
            .field("site", &self.site)
            .field("app", &self.app)
            .field("back_office", &self.back_office)
            .field("jobs", &self.jobs.keys().collect::<Vec<_>>())
            .field("tracker", &self.tracker.is_some())
            .finish_non_exhaustive()
    }
}

impl CharterKit {
    /// The inventory warning of a kind no job is bound to.
    pub const JOB_UNBOUND: &'static str = "charter.job_unbound";

    /// The tracker's default cron: hourly, on the hour, in UTC. The tracker reads each KPI
    /// on its own cadence, daily at the most often.
    pub const DEFAULT_TRACKER_CRON: &'static str = "0 0 * * * *";

    /// A kit for site `site` of app `app`, its jobs running in the back-office partition
    /// `back_office`, its signals filed through `signals`; the runs act for the user `site`.
    pub fn new(
        site: Uuid,
        app: impl Into<String>,
        back_office: impl Into<PartitionId>,
        signals: Arc<dyn SignalStore>,
    ) -> Self {
        Self {
            site,
            app: app.into(),
            user: "site".into(),
            back_office: back_office.into(),
            signals,
            jobs: BTreeMap::new(),
            skipped: BTreeMap::new(),
            tracker: None,
            tracker_cron: Self::DEFAULT_TRACKER_CRON.into(),
        }
    }

    /// Runs the jobs' processes for `user`, a `Service` subject.
    #[must_use]
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = user.into();
        self
    }

    /// Binds `kind` to `target`, an agent or workflow the kernel serves, its runs starting
    /// with `input`, on the kind's default wake ([`SiteProcessKind::default_wake`]). Binding a
    /// kind again replaces it.
    #[must_use]
    pub fn job(mut self, kind: SiteProcessKind, target: impl Into<String>, input: Value) -> Self {
        self.skipped.remove(&kind);
        self.jobs.insert(kind, Job { target: target.into(), input, wake: None });
        self
    }

    /// Binds `kind` as [`job`](Self::job) does, started by `wake` instead of its default.
    #[must_use]
    pub fn job_on(
        mut self,
        kind: SiteProcessKind,
        target: impl Into<String>,
        input: Value,
        wake: Wake,
    ) -> Self {
        self.skipped.remove(&kind);
        self.jobs.insert(kind, Job { target: target.into(), input, wake: Some(wake) });
        self
    }

    /// Leaves `kind` unbound for `reason`, such as "the owner has no channel": the kernel's
    /// inventory names it with the reason.
    #[must_use]
    pub fn skip(mut self, kind: SiteProcessKind, reason: impl Into<String>) -> Self {
        self.jobs.remove(&kind);
        self.skipped.insert(kind, reason.into());
        self
    }

    /// Runs `tracker`, the site's KPI tracker, as the kernel's `kernel.kpi` process.
    #[must_use]
    pub fn tracker(mut self, tracker: KpiTracker) -> Self {
        self.tracker = Some(tracker);
        self
    }

    /// Runs the tracker on the six-field UTC cron expression `expr` instead of
    /// [`DEFAULT_TRACKER_CRON`](Self::DEFAULT_TRACKER_CRON).
    #[must_use]
    pub fn tracker_cron(mut self, expr: impl Into<String>) -> Self {
        self.tracker_cron = expr.into();
        self
    }

    /// The kinds bound to a job.
    pub fn bound(&self) -> Vec<SiteProcessKind> {
        self.jobs.keys().copied().collect()
    }

    /// Registers the kit on `builder` (see the type's table).
    pub fn install(self, mut builder: KernelBuilder) -> KernelBuilder {
        let bound = self.bound();
        for (kind, job) in self.jobs {
            let wake = job.wake.unwrap_or_else(|| kind.default_wake(&self.back_office));
            let run =
                NewRun::new(kind.queue().as_str(), &self.app, &self.user, job.target, job.input);
            builder = builder.trigger(kind.trigger(wake, run, self.back_office.clone()));
        }
        for kind in SiteProcessKind::ALL.into_iter().filter(|kind| !bound.contains(kind)) {
            let reason = self.skipped.get(&kind).map_or("no job is bound to it", String::as_str);
            builder = builder.inventory_warning(
                InventoryWarning::new(
                    Self::JOB_UNBOUND,
                    format!("site {} runs no `{kind}` process: {reason}", self.site),
                )
                .with_subject(kind.as_str()),
            );
        }
        if let Some(tracker) = self.tracker {
            builder = builder.kpi_tracker(tracker, &self.app, &self.tracker_cron);
        }
        builder.signals(self.signals, self.back_office)
    }
}
