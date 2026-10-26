//! One running site, assembled from its project and the kernel's stores (spec 3.5 §7.8.3,
//! §7.9, §7.12, §7.17, §7.21): the runtime, the edge, the update engine, experiments and
//! measurement, the weekly review and the owner's alarms.
//!
//! | Part | Built from |
//! |---|---|
//! | Runtime | The content repository, `ExperimentGenerator` around the built-in generator, the SQLite site log signed with `data/keys/release.jwk`, the `DirectoryHost`, the history, the charter store, the kind's launch gate with [`ReleaseDelegation`] |
//! | Edge | The kind's capabilities with the packs' handlers under the owner's `approvals.json` thresholds; SQLite edge stores, event log and beacon counts in `data/edge.db`, with the deliveries a restart left resumed; the kernel's approvals and signals; the consent policy; `MeteredActionLedger` and `EdgeMeter`; inbox threads, the calendar and a payment desk (`SiteOptions::payments`, such as an AP2 desk, else the `payments.json` links) when configured; `card.jwk` and `receipt.jwk` |
//! | Updates | `UpdateEngine` over the runtime's pipeline, the edge's live store and the stream lane signed with the receipt key, its held items and deliveries in `data/updates/held.json`; the feeds in `feeds.json`; [`UpdateDriver`] with its journal and cards |
//! | Experiments | `ExperimentRunner` with `BeaconKpi`, the job pauses, the ladder, the alarms and `RepublishOnStop`; its daily trigger |
//! | Measurement | `KpiTracker` with the beacon, commerce, spend and agent-search sources (and AEO with search providers), their counts in `data/edge.db` |
//! | Review | `ReviewComposer` with the alarms, the experiments and the updates |

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use adk_core::{AdkError, Result, SpendLedger, Url};
use adk_drivers::{ReplyInThread, ThreadReplier};
use adk_harness::web::SearchProvider;
use adk_harness::website::{SitePartitions, SiteRef, SiteRuntime};
use adk_kernel::channels::ChannelSink;
use adk_kernel::charter::{AutonomyLadder, CharterStore, JobPauses, KpiStore, KpiTracker};
use adk_kernel::wake::Trigger;
use adk_server::awp::stores::SqliteEdgeStores;
use adk_server::awp::{BeaconStore, ConsentPolicy, EdgeKeys, EdgeTiers, WebBotAuthTrustAssigner};
use adk_site::content::{FileContentHistory, FsContentStore, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::signer::Ed25519ReleaseSigner;
use adk_site::stream::StreamLane;
use adk_site::updates::{FileHeldStore, UpdateEngine};
use awp_types::{Cadence, Charter, Reporting, SiteKind};
use tokio::task::JoinHandle;

use super::checks::{PaymentLinks, ReleaseDelegation, metered_audit};
use super::kernel::KernelStores;
use super::project::{self, KeyFile, ProjectPaths};
use super::updates::{SiteAlarms, UpdateCards, UpdateDriver, UpdateJournal};
use crate::alarms::AlarmRouter;
use crate::edge::adapters::{
    DeferredAnnouncer, InboxThreads, KernelSignals, ReceiptEntrySigner, ReleaseSearch,
};
use crate::edge::handlers::{Calendar, InMemoryCalendar, InMemoryContactBook, PaymentDesk};
use crate::edge::{SiteEdge, SiteEdgeParts, consent_config, site_router, stream_router};
use crate::experiments::{
    ExperimentGenerator, ExperimentRunner, FileExperimentStore, RepublishOnStop,
};
use crate::kinds::{KindTemplate, template};
use crate::measurement::{
    AeoKpi, AeoTracker, AgentSearchKpi, BeaconKpi, CommerceKpi, MeteredSpendLedger, SpendLedgerKpi,
};
use crate::review::ReviewComposer;
use crate::team::AgentRole;

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// What a site is served with beyond its project.
#[derive(Clone, Default)]
pub struct SiteOptions {
    /// The edge's public URL; the site's base URL when not set.
    pub edge_url: Option<String>,
    /// The person the owner's decision cards and approvals go to.
    pub owner: String,
    /// The owner's channels and threads, for alarms and the weekly review.
    pub channels: Vec<(Arc<dyn ChannelSink>, String)>,
    /// The inbox drivers' repliers; in-thread replies are off without them.
    pub repliers: Vec<Arc<dyn ThreadReplier>>,
    /// The playbook template in-thread replies use.
    pub reply_template: Option<String>,
    /// A payment desk; otherwise `payments.json`, otherwise payments are refused.
    pub payments: Option<Arc<dyn PaymentDesk>>,
    /// A calendar; otherwise `calendar.json`, otherwise no slot is open.
    pub calendar: Option<Arc<dyn Calendar>>,
    /// `web_search` providers for AEO tracking, with the questions to ask.
    pub search: Vec<Arc<dyn SearchProvider>>,
    /// The questions AEO tracking asks.
    pub questions: Vec<String>,
    /// The experiments' daily look, a six-field cron expression; `0 30 3 * * *` by default.
    pub experiments_cron: Option<String>,
    /// How often the update engine and measurement wake at the least; a minute by default.
    pub idle: Option<Duration>,
    /// The model the Concierge answers visitors with; no public agent without one.
    pub model: Option<Arc<dyn adk_core::Llm>>,
    /// The edge's rate limits, made for each site.
    pub limits: Option<Arc<dyn Fn() -> adk_server::awp::EdgeLimits + Send + Sync>>,
}

impl std::fmt::Debug for SiteOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteOptions").field("owner", &self.owner).finish_non_exhaustive()
    }
}

/// A site's id and kind: the committed charter's or the drafted one's, else a business site
/// with a new id.
fn identity(paths: &ProjectPaths) -> (uuid::Uuid, SiteKind, Option<Charter>) {
    ["charter.json", "charter.draft.json"]
        .iter()
        .filter_map(|name| std::fs::read(paths.root().join(name)).ok())
        .find_map(|bytes| serde_json::from_slice::<Charter>(&bytes).ok())
        .map_or_else(
            || (uuid::Uuid::new_v4(), SiteKind::Business, None),
            |charter| (charter.site_id, charter.kind, Some(charter)),
        )
}

/// A key file's id for a key made now: `{name}-{year}-{month}`.
fn month_kid(name: &str) -> String {
    format!("{name}-{}", chrono::Utc::now().format("%Y-%m"))
}

/// The 32-byte secret at `path`, made on first use, readable by its owner only.
fn secret(path: &Path) -> Result<[u8; 32]> {
    if let Ok(bytes) = std::fs::read(path)
        && let Ok(secret) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return Ok(secret);
    }
    let mut fresh = [0u8; 32];
    getrandom::fill(&mut fresh).map_err(config)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(config)?;
    }
    std::fs::write(path, fresh).map_err(config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(config)?;
    }
    Ok(fresh)
}

/// One running site.
pub struct RunningSite {
    /// The site.
    pub site: SiteRef,
    /// Its domain.
    pub domain: String,
    /// Its kind's template.
    pub template: KindTemplate,
    /// Its project.
    pub paths: ProjectPaths,
    /// The runtime the back office acts on.
    pub runtime: Arc<SiteRuntime>,
    /// Its partitions.
    pub partitions: SitePartitions,
    /// The edge.
    pub edge: SiteEdge,
    /// The update engine.
    pub updates: Arc<UpdateEngine>,
    /// The engine's driver: journal and cards.
    pub driver: Arc<UpdateDriver>,
    /// Held deliveries and items as the owner's decision cards.
    pub cards: Arc<UpdateCards>,
    /// Experiments.
    pub experiments: Arc<ExperimentRunner>,
    /// The ladder of the site's jobs.
    pub ladder: AutonomyLadder,
    /// The job pauses guardrails set.
    pub pauses: Arc<JobPauses>,
    /// The owner's alarms.
    pub alarms: Arc<AlarmRouter>,
    /// The weekly review.
    pub review: Arc<ReviewComposer>,
    /// The spend ledger agents' runs commit to, metered for the `spend` KPI source.
    pub spend: Arc<MeteredSpendLedger>,
    /// The router serving the edge, the stream feed and the release.
    pub router: axum::Router,
    triggers: Vec<Trigger>,
    tracker: KpiTracker,
    aeo: Option<AeoTracker>,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for RunningSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningSite").field("site", &self.site).finish_non_exhaustive()
    }
}

impl Drop for RunningSite {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl RunningSite {
    /// Opens the site at `paths` over the kernel's `stores` (spec 3.5 §7.17): continues after
    /// the release its host serves, and brings a committed charter's jobs onto the ladder.
    ///
    /// # Errors
    ///
    /// A project that does not load, a key file that is not valid, or a store that does not
    /// open.
    pub async fn open(
        paths: ProjectPaths,
        stores: &KernelStores,
        options: &SiteOptions,
    ) -> Result<Self> {
        let (id, kind, drafted) = identity(&paths);
        let template = template(kind);
        let mut project =
            SiteProject::load(paths.root(), chrono::Utc::now()).await.map_err(config)?;
        project.documents = project::documents(&paths)?;
        let domain = project.config.domain.clone();
        let site = SiteRef::new(id, domain.replace('.', "-"), project.config.base_url());
        let edge_url = options.edge_url.clone().unwrap_or_else(|| project.config.base_url());

        let release = KeyFile::load_or_create(&paths.key_file("release"), &month_kid("release"))?;
        let card = KeyFile::load_or_create(&paths.key_file("card"), &month_kid("card"))?;
        let receipt = match paths.key_file("receipt") {
            path if path.exists() => KeyFile::read(&path)?,
            _ => KeyFile::read(&paths.key_file("card"))?,
        };
        let release_signer = Ed25519DocumentSigner::from_seed(release.kid.clone(), release.seed())
            .map_err(config)?;
        let card_signer: Arc<dyn DocumentSigner> = Arc::new(
            Ed25519DocumentSigner::from_seed(card.kid.clone(), card.seed()).map_err(config)?,
        );
        let receipt_signer: Arc<dyn DocumentSigner> = Arc::new(
            Ed25519DocumentSigner::from_seed(receipt.kid.clone(), receipt.seed())
                .map_err(config)?,
        );

        let audit = adk_session::SqliteAuditLogStore::new(&paths.database("site-log")).await?;
        audit.migrate().await?;
        let signer = Ed25519ReleaseSigner::from_seed(&domain, release.seed()).map_err(config)?;
        let log = Arc::new(
            SiteLog::open(Arc::new(audit), &domain, Arc::new(signer)).await.map_err(config)?,
        );
        let host = Arc::new(DirectoryHost::new(paths.host_dir()));
        let previous = project::previous_release(host.as_ref(), log.size().await).await?;
        let experiment_store = Arc::new(FileExperimentStore::new(paths.experiments()));
        let generator = Arc::new(ExperimentGenerator::new(
            Arc::new(BuiltinGenerator),
            experiment_store.clone(),
            site.id,
            format!("{edge_url}/awp/v2/beacon"),
        ));
        let delegation = ReleaseDelegation::new(
            release.kid.clone(),
            release_signer.public_key(),
            "data/keys/release.jwk",
        );
        let announcer = Arc::new(DeferredAnnouncer::default());
        let charters: Arc<dyn CharterStore> = stores.charters.clone();
        let mut builder = SiteRuntime::builder(
            site.clone(),
            project.clone(),
            Arc::new(FsContentStore::new(paths.root())),
            generator,
            host.clone(),
            log,
            paths.work(),
        )
        .history(Arc::new(FileContentHistory::new(paths.history())))
        .charters(charters.clone())
        .gate(template.gate(&site.origin).with_extension(Arc::new(delegation)))
        .announcer(announcer.clone());
        if let Some(previous) = previous {
            builder = builder.previous(previous);
        }
        let runtime = builder.build();

        let partitions = SitePartitions::new(&site);
        let reporting = drafted.as_ref().map_or_else(
            || Reporting { review: Cadence::Weekly, routes: Vec::new(), quiet_hours: None },
            |charter| charter.reporting.clone(),
        );
        let mut router = AlarmRouter::new(site.id, &reporting);
        for (sink, thread) in &options.channels {
            router = router.with_sink(sink.clone(), thread.clone());
        }
        let alarms = Arc::new(router);
        let ladder = AutonomyLadder::new(stores.autonomy.clone()).with_charters(charters.clone());
        if let Some(current) = charters.current(site.id).await.map_err(config)? {
            crate::jobs::register(&ladder, &current.charter).await?;
        }

        let pool = sqlx::SqlitePool::connect(&paths.database("edge")).await.map_err(config)?;
        let edge_stores = SqliteEdgeStores::new(pool).with_site(site.name.clone());
        edge_stores.migrate().await.map_err(config)?;
        // Counts the edge's meters write and the measurement sources read, kept beside the
        // edge's own beacon counts in a namespace of their own, so both survive a restart.
        let counts: Arc<dyn BeaconStore> =
            Arc::new(edge_stores.clone().with_site(format!("{}#measurement", site.name)));
        let terms = std::fs::read(paths.root().join("public-terms.json")).unwrap_or_default();
        let policy = ConsentPolicy { digest: adk_site::release::file_digest(&terms), ttl: None };
        let subject_key = secret(&paths.data().join("keys/subject.key"))?.to_vec();
        let directory = adk_auth::web_bot_auth::HttpKeyDirectorySource::new().map_err(config)?;
        let tiers = EdgeTiers::builder()
            .verifier(WebBotAuthTrustAssigner::new(
                adk_auth::web_bot_auth::HttpSignatureVerifier::builder(Arc::new(directory)).build(),
            ))
            .build();
        let calendar = match &options.calendar {
            Some(calendar) => Some(calendar.clone()),
            None => project::slots(&paths)?
                .map(|slots| Arc::new(InMemoryCalendar::new(slots)) as Arc<dyn Calendar>),
        };
        let payments = match &options.payments {
            Some(desk) => Some(desk.clone()),
            None => project::payment_links(&paths)?
                .map(|links| Arc::new(PaymentLinks::new(links)) as Arc<dyn PaymentDesk>),
        };
        let threads = (!options.repliers.is_empty()).then(|| {
            Arc::new(InboxThreads::new(
                Arc::new(ReplyInThread::new(options.repliers.clone())),
                options.reply_template.clone().unwrap_or_else(|| "reply".into()),
            )) as Arc<dyn adk_server::awp::ThreadSink>
        });
        let signals = partitions.signal_sink(stores.signals.clone(), stores.agenda.clone());
        let edge = SiteEdge::build(SiteEdgeParts {
            template: template.clone(),
            runtime: runtime.clone(),
            keys: EdgeKeys { card: card_signer, receipt: receipt_signer.clone() },
            edge_url: Url::parse(&edge_url).map_err(config)?,
            stores: edge_stores.edge_stores(),
            signals: Arc::new(KernelSignals::new(signals)),
            search: Arc::new(ReleaseSearch::new(Arc::new(adk_harness::website::SiteSearch::new()))),
            contacts: Arc::new(InMemoryContactBook::default()),
            calendar,
            payments,
            thresholds: project::approval_thresholds(&paths)?,
            approvals: Some(stores.approvals.clone()),
            tiers: Some(tiers),
            consent: Some(consent_config(&template, policy, subject_key)),
            threads,
            quote_validity: chrono::Duration::days(7),
            action_ledger: Some(Arc::new(crate::measurement::MeteredActionLedger::new(
                stores.actions.clone(),
                counts.clone(),
            ))),
            audit: Some(metered_audit(counts.clone())),
            beacon: None,
            limits: options.limits.as_ref().map(|limits| limits()),
            public_agent: options.model.clone().map(|model| {
                crate::edge::concierge(
                    &crate::team::SiteTeam::new(site.clone(), &template),
                    &partitions,
                    stores.partitions.clone(),
                    model,
                    paths.data().join("agents/concierge"),
                )
            }),
        })?;
        announcer.set(edge.announcer());

        let lane = Arc::new(StreamLane::new(Arc::new(ReceiptEntrySigner::new(receipt_signer))));
        let updates = Arc::new(
            UpdateEngine::builder(runtime.shared_pipeline(), edge.live.clone())
                // Held items and deliveries outlive a restart, so the owner's cards still
                // decide them.
                .held_store(Arc::new(FileHeldStore::new(paths.journal().join("held.json"))))
                .streams(lane)
                .alarms(Arc::new(SiteAlarms::new(alarms.clone(), site.id)))
                .build()
                .map_err(config)?,
        );
        for feed in project::feeds(&paths)? {
            updates.add_feed(feed.mapping, feed.bounds).await.map_err(config)?;
        }
        let cards = Arc::new(UpdateCards::new(
            updates.clone(),
            stores.approvals.clone(),
            site.id,
            site.name.clone(),
            options.owner.clone(),
        ));
        let driver = Arc::new(UpdateDriver::new(
            updates.clone(),
            UpdateJournal::new(paths.journal()),
            Some(cards.clone()),
        ));

        let pauses = Arc::new(JobPauses::new());
        let kpis: Arc<dyn KpiStore> = stores.kpis.clone();
        let experiments = Arc::new(
            ExperimentRunner::new(site.id, charters.clone(), experiment_store)
                .with_source(Arc::new(BeaconKpi::new(site.id, edge.edge.beacon())))
                .with_kpis(kpis.clone())
                .with_control(pauses.clone())
                .with_ladder(ladder.clone())
                .with_events(alarms.clone())
                .with_stop_hook(Arc::new(RepublishOnStop::new(
                    runtime.clone(),
                    AgentRole::Growth.principal(&site),
                ))),
        );
        let inner_spend: Arc<dyn SpendLedger> = stores.spend.clone();
        let spend = Arc::new(MeteredSpendLedger::new(inner_spend, counts.clone()));
        let aeo = (!options.search.is_empty()).then(|| {
            let mut tracker = AeoTracker::new(site.id, domain.clone(), counts.clone());
            for provider in &options.search {
                tracker = tracker.provider(provider.clone());
            }
            for question in &options.questions {
                tracker = tracker.question(question.clone());
            }
            tracker
        });
        let tracker = |aeo: bool| {
            let mut tracker = KpiTracker::new(site.id, charters.clone(), kpis.clone())
                .with_source(Arc::new(BeaconKpi::new(site.id, edge.edge.beacon())))
                .with_source(Arc::new(CommerceKpi::new(site.id, counts.clone())))
                .with_source(Arc::new(SpendLedgerKpi::new(site.id, counts.clone())))
                .with_source(Arc::new(AgentSearchKpi::new(site.id, counts.clone())))
                .with_control(pauses.clone())
                .with_ladder(ladder.clone())
                .with_events(alarms.clone());
            if aeo {
                tracker = tracker.with_source(Arc::new(AeoKpi::new(site.id, counts.clone())));
            }
            tracker
        };
        let review = Arc::new(
            ReviewComposer::new(site.id, tracker(aeo.is_some()), ladder.clone(), charters.clone())
                .with_alarms(alarms.clone())
                .with_experiments(experiments.clone())
                .with_updates(updates.clone()),
        );
        let cron = options.experiments_cron.clone().unwrap_or_else(|| "0 30 3 * * *".into());
        let triggers = vec![experiments.trigger(&site.name, &cron)];
        let router = site_router(&edge.edge, host, &project.config.name)
            .merge(stream_router(updates.clone()));
        Ok(Self {
            tracker: tracker(aeo.is_some()),
            site,
            domain,
            template,
            paths,
            runtime,
            partitions,
            edge,
            updates,
            driver,
            cards,
            experiments,
            ladder,
            pauses,
            alarms,
            review,
            spend,
            router,
            triggers,
            aeo,
            tasks: Vec::new(),
        })
    }

    /// The kernel triggers the site's own processes run on: the experiments' daily look.
    pub fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }

    /// Starts the site's background work: the update engine on its schedule, measurement
    /// every `idle` (AEO once a day), and the edge's trust resolver.
    pub fn start(&mut self, idle: Duration) {
        self.tasks.push(self.driver.clone().spawn(idle));
        let tracker = self.tracker.clone();
        let aeo = self.aeo.take();
        self.tasks.push(tokio::spawn(async move {
            let mut last_aeo: Option<chrono::NaiveDate> = None;
            loop {
                if let Err(error) = tracker.tick().await {
                    tracing::warn!(error = %error, "the KPI tracker's tick failed");
                }
                let today = chrono::Utc::now().date_naive();
                if let Some(aeo) = &aeo
                    && last_aeo != Some(today)
                {
                    match aeo.run().await {
                        Ok(_) => last_aeo = Some(today),
                        Err(error) => tracing::warn!(error = %error, "AEO tracking failed"),
                    }
                }
                tokio::time::sleep(idle).await;
            }
        }));
        let _resolver = self.edge.edge.spawn_resolver(Duration::from_secs(5));
        // Deliveries a stopped process left in `data/edge.db` resume once their lease passes.
        let _deliveries = self.edge.edge.spawn_delivery_resumer(Duration::from_secs(30));
    }

    /// The owner's channel for the weekly review, when one is configured.
    pub(crate) fn digest_target(
        &self,
        options: &SiteOptions,
    ) -> Option<crate::workflows::DigestTarget> {
        options.channels.first().map(|(sink, thread)| crate::workflows::DigestTarget {
            composer: self.review.clone(),
            sink: sink.clone(),
            thread: thread.clone(),
        })
    }
}

/// The served workflows: the site's model-free jobs and each site's experiments look, by
/// the site a run names.
pub struct ServedWorkflows {
    sites: crate::workflows::SiteWorkflows,
    experiments: BTreeMap<uuid::Uuid, Arc<ExperimentRunner>>,
}

impl ServedWorkflows {
    /// `sites` with each runner's look.
    pub fn new(
        sites: crate::workflows::SiteWorkflows,
        experiments: impl IntoIterator<Item = Arc<ExperimentRunner>>,
    ) -> Self {
        Self {
            sites,
            experiments: experiments.into_iter().map(|runner| (runner.site(), runner)).collect(),
        }
    }
}

#[async_trait::async_trait]
impl adk_server::background::WorkflowExecutor for ServedWorkflows {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        workflow_id == crate::experiments::EXPERIMENTS_WORKFLOW
            || self.sites.has_workflow(workflow_id)
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: adk_server::background::WorkflowState,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> std::result::Result<serde_json::Value, String> {
        if workflow_id != crate::experiments::EXPERIMENTS_WORKFLOW {
            return self.sites.execute(workflow_id, input, cancel_token).await;
        }
        let site = input
            .get("site")
            .and_then(serde_json::Value::as_str)
            .and_then(|site| site.parse::<uuid::Uuid>().ok())
            .ok_or_else(|| format!("a `{workflow_id}` run names its site"))?;
        let runner = self
            .experiments
            .get(&site)
            .ok_or_else(|| format!("no experiments run for site {site}"))?;
        runner.execute(workflow_id, input, cancel_token).await
    }
}
