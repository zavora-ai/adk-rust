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
//! | Learning | [`PlaybookLearning`](crate::learning::PlaybookLearning) over the review, the experiments and the ladder: proposals in `data/learning/proposals.json` and the owner's cards, playbook versions in `data/playbooks/`; its weekly trigger (spec 3.5 §7.15) |
//! | Leads | The contact book and nurture counts in `data/edge.db` ([`SqliteLeadRecords`]), scored against the kind's threshold; the email outbox, when configured, reading consent from the edge's receipts |
//! | Upkeep | [`UpkeepCheck`] over the edge's lockfile and the certificate, when configured |
//! | Repository | [`RepositoryEntry`](crate::repository::RepositoryEntry) from `repository.json`: its signed hooks on the edge, each push a kernel run, its decision cards in the kernel's approvals (spec 3.5 §7.8.2) |
//! | Durable state | In `data/edge.db`: the back office's drafts and requests (`SqliteSiteDraftStore`), quotes, orders and the bookings of `calendar.json`'s slots ([`SqliteCommerceStore`], [`StoredCalendar`]), the owner's open, deferred and waiting alarms ([`SqliteAlarmStore`]), and the edge's rate-limit windows |
//! | Packs | The built-in capability packs and those installed in `packs/`, verified against `packs/trust.json` ([`PackDir::load`](crate::packs::installed::PackDir::load)): their capabilities on the edge, their playbooks in the agents' workspaces (spec 3.5 §7.23) |
//! | Audit trail | `data/audit.jsonl` (`adk-core`'s `FileAuditRecorder`): the pipeline's, the edge's, the update engine's, the alarms', the review's, the experiments', the ladder's, the tracker's and the signals' records (spec 3.5 §9.3) |

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

use super::checks::{PaymentLinks, ReleaseDelegation, metered_trail};
use super::kernel::KernelStores;
use super::project::{self, KeyFile, ProjectPaths};
use super::updates::{SiteAlarms, UpdateCards, UpdateDriver, UpdateJournal};
use crate::alarms::{AlarmRouter, SqliteAlarmStore};
use crate::edge::adapters::{
    DeferredAnnouncer, InboxThreads, KernelSignals, ReceiptEntrySigner, ReleaseSearch,
};
use crate::edge::commerce::{SqliteCommerceStore, StoredCalendar};
use crate::edge::handlers::{Calendar, PaymentDesk};
use crate::edge::{SiteEdge, SiteEdgeParts, consent_config, site_router, stream_router};
use crate::experiments::{
    ExperimentGenerator, ExperimentRunner, FileExperimentStore, RepublishOnStop,
};
use crate::incident::{GuardianProbe, IncidentPath};
use crate::kinds::{KindTemplate, template};
use crate::leads::{LeadScorer, Leads, SqliteLeadRecords};
use crate::measurement::{
    AeoKpi, AeoTracker, AgentSearchKpi, BeaconKpi, CommerceKpi, MeteredSpendLedger, SpendLedgerKpi,
};
use crate::quotes::{QuotedReviews, ReviewQuotes};
use crate::review::ReviewComposer;
use crate::team::AgentRole;
use crate::upkeep::{AdvisorySource, CertificateSource, UpkeepCheck};

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
    /// A calendar; otherwise the CalDAV calendar in `caldav.json`, otherwise `calendar.json`,
    /// otherwise no slot is open.
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
    /// The site's email: nurture sequences and the outbox are off without it (spec 3.5 §5 S8).
    pub email: Option<EmailOptions>,
    /// How long a page the site drafts on its own waits before it can go live; 24 hours by
    /// default (spec 3.5 §5 S10).
    pub publish_delay: Option<chrono::Duration>,
    /// The edge's `Cargo.lock` and where its advisories come from, for the hosting job's
    /// upgrade proposals (spec 3.5 §7.20).
    pub advisories: Option<(std::path::PathBuf, Arc<dyn AdvisorySource>)>,
    /// Where the certificate's expiry is read, for renewal proposals (spec 3.5 §7.20).
    pub certificate: Option<Arc<dyn CertificateSource>>,
    /// The owner's wallet the Finance agent pays running costs from (spec 3.5 §7.14); no
    /// wallet tools without it.
    pub wallet: Option<adk_payments::wallet::Wallet>,
    /// The Guardian's probe (spec 3.5 §7.20, §7.8 "Probes", A-I9): the resolver that proves
    /// the owner's domain claim again on every run; no probe without it.
    pub probe: Option<ProbeOptions>,
    /// The kernel's chat pairings: a paired account's message is the owner's, so the Inbox
    /// does not read it (spec 3.5 A-I24); without them the Inbox reads every chat message
    /// handed to it.
    pub channel_bindings: Option<Arc<dyn adk_kernel::channels::ChannelBindingStore>>,
    /// The chat platforms the site talks on (spec 3.5 §10 X12): a served site runs a channel
    /// gateway over them — a paired account's messages are turns of the Steward, a
    /// stranger's go to the Inbox — and its edge serves their routes, such as WhatsApp's
    /// webhook. Pairings are kept in [`channel_bindings`](Self::channel_bindings), else in
    /// the kernel's store.
    pub chat: Option<Arc<dyn super::chat::ChatAdapters>>,
}

/// What the Guardian's probe of a served site needs beyond the project (spec 3.5 §7.20,
/// §7.8 "Probes", A-I9): the resolver that proves the owner's domain claim again on every
/// run, and the claim, the client and what to probe when the defaults do not fit.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_site::migrate::claim::StaticDnsResolver;
/// use adk_website::deploy::ProbeOptions;
///
/// let options = ProbeOptions::new(Arc::new(StaticDnsResolver::new()));
/// assert!(options.claim.is_none(), "the project's own claim file by default");
/// ```
#[derive(Clone)]
pub struct ProbeOptions {
    /// The resolver the claim is proven with on every run.
    pub resolver: Arc<dyn adk_site::migrate::claim::DnsResolver>,
    /// The owner's claim of the site's domain; when not set, the project's
    /// `data/claims/{domain}.json`, which `adk-rust site migrate` keeps. A claim on another
    /// domain is not used.
    pub claim: Option<adk_site::migrate::claim::DomainClaim>,
    /// The client that reads the public origin; when not set, a client that reaches only
    /// the site's origin.
    pub http: Option<Arc<adk_site::migrate::http::HttpClient>>,
    /// What to probe; the site's domain and origin with the prober's defaults when not set.
    pub config: Option<adk_site::migrate::probe::ProbeConfig>,
}

impl ProbeOptions {
    /// A probe proving the claim with `resolver`, with every other part from the project.
    pub fn new(resolver: Arc<dyn adk_site::migrate::claim::DnsResolver>) -> Self {
        Self { resolver, claim: None, http: None, config: None }
    }

    /// A probe proving the claim with the system's resolver.
    pub fn system() -> Self {
        Self::new(Arc::new(adk_site::migrate::claim::SystemDnsResolver))
    }
}

impl std::fmt::Debug for ProbeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeOptions").field("claim", &self.claim).finish_non_exhaustive()
    }
}

/// The site's email (spec 3.5 §7.13): the driver, the address it sends from, and the
/// domain-authentication check bulk sends wait for.
#[derive(Clone)]
pub struct EmailOptions {
    /// The driver.
    pub driver: Arc<dyn adk_drivers::email::EmailDriver>,
    /// The address the site sends from.
    pub from: adk_drivers::email::Mailbox,
    /// The DNS lookup and sender policy SPF, DKIM and DMARC are checked against.
    pub domain_auth:
        Option<(Arc<dyn adk_drivers::email::TxtLookup>, adk_drivers::email::SenderPolicy)>,
}

impl std::fmt::Debug for EmailOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailOptions").field("from", &self.from).finish_non_exhaustive()
    }
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

/// What [`guardian`] builds the Guardian's probe from.
struct GuardianParts<'a> {
    probe: &'a ProbeOptions,
    paths: &'a ProjectPaths,
    domain: &'a str,
    site: &'a SiteRef,
    host: Arc<DirectoryHost>,
    release_key: adk_auth::signed_audit::NoteVerifier,
    audit: (Arc<dyn adk_core::audit_log::AuditRecorder>, &'a str),
    incidents: Arc<IncidentPath>,
}

/// The Guardian's probe of the served site (spec 3.5 §7.20, A-I9): the owner's claim on the
/// site's domain — [`ProbeOptions::claim`], else the project's claim file — proven again with
/// the resolver on every run; `None`, with a warning, when the site has no claim.
fn guardian(parts: GuardianParts<'_>) -> Result<Option<Arc<GuardianProbe>>> {
    use adk_site::migrate::claim::DomainClaim;
    let GuardianParts { probe, paths, domain, site, host, release_key, audit, incidents } = parts;
    let file = paths.data().join("claims").join(format!("{domain}.json"));
    let claim = match probe.claim.clone() {
        Some(claim) => Some(claim),
        None => match std::fs::read(&file) {
            Ok(bytes) => Some(
                serde_json::from_slice::<DomainClaim>(&bytes)
                    .map_err(|e| config(format!("{}: not a domain claim: {e}", file.display())))?,
            ),
            Err(_) => None,
        },
    }
    .filter(|claim| claim.domain.eq_ignore_ascii_case(domain));
    let Some(claim) = claim else {
        tracing::warn!(
            site.domain = domain,
            "the Guardian does not probe: the site has no domain claim; `adk-rust site migrate` makes one in data/claims/"
        );
        return Ok(None);
    };
    let origin = Url::parse(&site.origin).map_err(config)?;
    let http = match &probe.http {
        Some(http) => http.clone(),
        None => {
            let only =
                std::collections::BTreeSet::from([adk_site::migrate::http::origin_of(&origin)]);
            Arc::new(
                adk_site::migrate::http::HttpClient::builder()
                    .egress(adk_site::migrate::http::EgressScope::Only(only))
                    .build()
                    .map_err(config)?,
            )
        }
    };
    let config = probe
        .config
        .clone()
        .unwrap_or_else(|| adk_site::migrate::probe::ProbeConfig::new(domain, origin));
    let (recorder, owner) = audit;
    let acting = adk_core::ActingContext::derived(
        &site.name,
        AgentRole::Guardian.as_str(),
        owner,
        adk_core::SubjectKind::Service,
    );
    Ok(Some(Arc::new(GuardianProbe {
        claim,
        resolver: probe.resolver.clone(),
        host,
        http,
        trusted: vec![release_key],
        config,
        audit: Some((recorder, acting)),
        incidents,
    })))
}

/// The owner's amount thresholds (spec 3.5 §7.4, §7.6): the `amountThresholds` of the charter
/// in force, else of the drafted one, else `approvals.json`. They are applied to the edge's
/// declarations, which the capability manifest publishes, so a committed change takes effect
/// when the site next starts.
fn amount_thresholds(
    committed: Option<&Charter>,
    drafted: Option<&Charter>,
    paths: &ProjectPaths,
) -> Result<crate::packs::ApprovalThresholds> {
    match committed.or(drafted).filter(|charter| !charter.amount_thresholds.is_empty()) {
        Some(charter) => {
            let thresholds = charter.amount_thresholds.iter().fold(
                crate::packs::ApprovalThresholds::new(),
                |thresholds, (capability, amount)| thresholds.above(capability.clone(), *amount),
            );
            thresholds.validate()?;
            Ok(thresholds)
        }
        None => project::approval_thresholds(paths),
    }
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
    /// The capability packs it loaded (spec 3.5 §7.23).
    pub packs: crate::packs::installed::PackSet,
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
    /// The site's leads: scores, consent for nurture, one handoff each (spec 3.5 §5 S8).
    pub leads: Arc<Leads>,
    /// The reviews the site may quote, with the owner's approvals (spec 3.5 S2).
    pub quotes: Arc<ReviewQuotes>,
    /// The site's email outbox, when the site has email (spec 3.5 §7.13).
    pub outbox: Option<Arc<adk_drivers::email::EmailOutbox>>,
    /// The incident path over the edge's status, the alarms and the ladder (spec 3.5 §7.20).
    pub incidents: Arc<IncidentPath>,
    /// The Inbox, when the site has a model and its kind runs one (spec 3.5 §7.19): it reads
    /// the site's email while the site serves, and the chat messages of unpaired accounts
    /// handed to it.
    pub inbox: Option<Arc<crate::inbox::SiteInbox>>,
    /// The Guardian's probe, when the site has [`SiteOptions::probe`] and a domain claim; the
    /// kernel runs it every five minutes (spec 3.5 §7.20).
    pub guardian: Option<Arc<GuardianProbe>>,
    /// Files and git as an entry point, when the project has a `repository.json`: its hooks
    /// on the edge at `/hooks/{name}` (spec 3.5 §7.8.2).
    pub repository: Option<Arc<crate::repository::RepositoryEntry>>,
    /// The playbook-learning job: proposals from the week's outcomes, decided by the owner
    /// (spec 3.5 §7.15).
    pub learning: Arc<crate::learning::PlaybookLearning>,
    pub(crate) reporting: Reporting,
    pub(crate) publish_delay: chrono::Duration,
    pub(crate) wallet: Option<adk_payments::wallet::Wallet>,
    /// The edge's signals into the back office, which wake the kernel's processes once the
    /// kernel serving the site exists.
    pub(crate) signals: Arc<KernelSignals>,
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

        // The site's audit trail survives a restart (spec 3.5 §9.3).
        let recorder: Arc<dyn adk_core::audit_log::AuditRecorder> = Arc::new(
            adk_core::audit_log::FileAuditRecorder::open(paths.data().join("audit.jsonl"))?
                .with_log_id(&domain),
        );
        let trail = adk_core::audit_log::AuditTrail::new(recorder.clone());
        let pool = sqlx::SqlitePool::connect(&paths.database("edge")).await.map_err(config)?;
        // The back office's drafts and requests, kept beside the edge's stores (§7.8.2).
        let drafts = adk_harness::website::SqliteSiteDraftStore::new(pool.clone());
        drafts.migrate().await?;
        let audit = adk_session::SqliteAuditLogStore::new(&paths.database("site-log")).await?;
        audit.migrate().await?;
        let signer = Ed25519ReleaseSigner::from_seed(&domain, release.seed()).map_err(config)?;
        let release_key: adk_auth::signed_audit::NoteVerifier = {
            use adk_site::signer::ReleaseSigner as _;
            signer.verifier_key().parse().map_err(|e| config(format!("release key: {e:?}")))?
        };
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
        let verify_edge = Arc::new(crate::edge::adapters::DeferredFetcher::default());
        let quotes = Arc::new(ReviewQuotes::open(paths.data().join("reviews.json"))?);
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
        .drafts(Arc::new(drafts))
        .audit(trail.clone())
        .gate(
            template
                .gate(&site.origin)
                .with_extension(Arc::new(delegation))
                // Pages quote a found review only as its owner approved (S2).
                .with_extension(Arc::new(QuotedReviews::new(quotes.clone())))
                .with_extension(Arc::new(super::checks::preview_verification(
                    &project,
                    host.clone(),
                    verify_edge.clone(),
                ))),
        )
        .announcer(announcer.clone());
        if let Some(previous) = previous {
            builder = builder.previous(previous);
        }
        let runtime = builder.build();
        runtime.restore_drafts().await?;

        let partitions = SitePartitions::new(&site);
        let reporting = drafted.as_ref().map_or_else(
            || Reporting { review: Cadence::Weekly, routes: Vec::new(), quiet_hours: None },
            |charter| charter.reporting.clone(),
        );
        let alarm_store = SqliteAlarmStore::new(pool.clone());
        alarm_store.migrate().await?;
        let mut router = AlarmRouter::new(site.id, &reporting)
            .with_store(Arc::new(alarm_store))
            .with_audit(trail.clone());
        for (sink, thread) in &options.channels {
            router = router.with_sink(sink.clone(), thread.clone());
        }
        let alarms = Arc::new(router);
        let ladder = AutonomyLadder::new(stores.autonomy.clone())
            .with_charters(charters.clone())
            .with_audit(trail.clone());
        if let Some(current) = charters.current(site.id).await.map_err(config)? {
            crate::jobs::register(&ladder, &current.charter).await?;
        }

        // The contact book, kept beside the edge's leads so the people behind them are still
        // reachable after a restart, and erased with them (spec 3.5 §7.5, S8).
        let records = Arc::new(SqliteLeadRecords::new(pool.clone()));
        records.migrate().await?;
        let edge_stores = SqliteEdgeStores::new(pool.clone()).with_site(site.name.clone());
        edge_stores.migrate().await.map_err(config)?;
        let served_stores = edge_stores.edge_stores();
        let leads = Arc::new(
            Leads::new(
                served_stores.leads.clone(),
                records.clone(),
                served_stores.consent.clone(),
                LeadScorer::default(),
            )
            .with_charters(charters.clone(), site.id),
        );
        let reply_template = options.reply_template.clone().unwrap_or_else(|| "reply".into());
        let outbox = options.email.as_ref().map(|email| {
            let disclosure = adk_drivers::Disclosure::new(&project.config.name)
                .or_else(|_| adk_drivers::Disclosure::new(&domain))
                .map_err(config)?;
            let mut builder = adk_drivers::email::EmailOutbox::builder(
                email.driver.clone(),
                email.from.clone(),
                disclosure,
            )
            .consent(leads.clone())
            // The Inbox answers an email's thread with the reply template's body (§7.19).
            .templates(
                adk_drivers::ReplyTemplates::new().template(reply_template.clone(), "{body}"),
            );
            if let Some((lookup, policy)) = &email.domain_auth {
                builder = builder.domain_auth(lookup.clone(), policy.clone());
            }
            Ok::<_, AdkError>(Arc::new(builder.build()))
        });
        let outbox = outbox.transpose()?;
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
        // Quotes, orders and bookings outlive a restart (spec 3.5 §7.5, §7.9).
        let commerce = SqliteCommerceStore::new(pool.clone()).with_site(site.name.clone());
        commerce.migrate().await?;
        let commerce: Arc<dyn crate::edge::commerce::CommerceStore> = Arc::new(commerce);
        let caldav = project::caldav(&paths)?.map(|c| Arc::new(c) as Arc<dyn Calendar>);
        let calendar = match &options.calendar {
            Some(calendar) => Some(calendar.clone()),
            None if caldav.is_some() => caldav,
            None => project::slots(&paths)?.map(|slots| {
                Arc::new(StoredCalendar::new(slots, commerce.clone())) as Arc<dyn Calendar>
            }),
        };
        let payments = match &options.payments {
            Some(desk) => Some(desk.clone()),
            None => project::payment_links(&paths)?
                .map(|links| Arc::new(PaymentLinks::new(links)) as Arc<dyn PaymentDesk>),
        };
        // In-thread replies go back through the channel a message came in on: the inbox
        // drivers, the site's email, and the chat channels the owner's sinks serve (§7.19).
        let mut repliers: Vec<Arc<dyn ThreadReplier>> = options.repliers.clone();
        if let Some(outbox) = &outbox {
            repliers.push(outbox.clone());
        }
        let mut chats = std::collections::BTreeSet::new();
        // The site's own chat platforms too (spec 3.5 §10 X12), so the Inbox answers a
        // stranger in their thread.
        let platforms = options.chat.iter().flat_map(|chat| chat.sinks());
        let sinks: Vec<Arc<dyn ChannelSink>> =
            options.channels.iter().map(|(sink, _)| sink.clone()).chain(platforms).collect();
        for sink in &sinks {
            if chats.insert(sink.channel().to_string()) {
                repliers.push(Arc::new(crate::inbox::ChannelReplier::new(
                    sink.clone(),
                    reply_template.clone(),
                )));
            }
        }
        let threads = (!repliers.is_empty()).then(|| {
            Arc::new(InboxThreads::new(Arc::new(ReplyInThread::new(repliers)), reply_template))
                as Arc<dyn adk_server::awp::ThreadSink>
        });
        let signals = partitions
            .signal_sink(stores.signals.clone(), stores.agenda.clone())
            .with_audit(trail.clone());
        let edge_signals = Arc::new(KernelSignals::new(signals));
        // The edge's rate-limit windows are shared through `data/edge.db` (spec 3.5 §7.9).
        let limits = options
            .limits
            .as_ref()
            .map_or_else(adk_server::awp::EdgeLimits::default, |limits| limits())
            .store(Arc::new(edge_stores.clone()));
        // The installed packs verify again on every start; one that does not is not loaded.
        let packs = crate::packs::installed::PackDir::new(paths.root()).load()?;
        let public_tools = Arc::new(std::sync::OnceLock::new());
        let edge = SiteEdge::build_with_packs(
            SiteEdgeParts {
                template: template.clone(),
                runtime: runtime.clone(),
                keys: EdgeKeys { card: card_signer, receipt: receipt_signer.clone() },
                edge_url: Url::parse(&edge_url).map_err(config)?,
                stores: served_stores,
                signals: edge_signals.clone(),
                search: Arc::new(ReleaseSearch::new(Arc::new(
                    adk_harness::website::SiteSearch::new(),
                ))),
                contacts: records,
                calendar,
                payments,
                thresholds: amount_thresholds(
                    charters.current(site.id).await.map_err(config)?.map(|v| v.charter).as_ref(),
                    drafted.as_ref(),
                    &paths,
                )?,
                approvals: Some(stores.approvals.clone()),
                tiers: Some(tiers),
                consent: Some(consent_config(&template, policy, subject_key)),
                threads,
                quote_validity: chrono::Duration::days(7),
                action_ledger: Some(Arc::new(crate::measurement::MeteredActionLedger::new(
                    stores.actions.clone(),
                    counts.clone(),
                ))),
                audit: Some(metered_trail(recorder.clone(), counts.clone())),
                beacon: None,
                limits: Some(limits),
                public_agent: options.model.clone().map(|model| {
                    let concierge = crate::edge::concierge(
                        &crate::team::SiteTeam::new(site.clone(), &template),
                        &partitions,
                        stores.partitions.clone(),
                        model,
                        paths.data().join("agents/concierge"),
                    );
                    // The Inbox runs over the same public tools the edge gives the Concierge.
                    let captured = public_tools.clone();
                    Box::new(move |tools: &adk_server::awp::PublicTools| {
                        let _ = captured.set(tools.clone());
                        concierge(tools)
                    }) as crate::edge::PublicAgentFactory
                }),
            },
            commerce,
            &packs,
        )?;
        announcer.set(edge.announcer());
        let team = crate::team::SiteTeam::new(site.clone(), &template);
        let inbox = match (&options.model, public_tools.get()) {
            (Some(model), Some(tools)) if team.runs(AgentRole::Inbox) => {
                // The Inbox's jobs' playbooks are in its workspace, as every agent's are.
                crate::playbooks::install(AgentRole::Inbox, &paths.data().join("agents/inbox"))
                    .await?;
                let mut inbox = crate::inbox::SiteInbox::new(crate::inbox::InboxParts {
                    team,
                    partitions: partitions.clone(),
                    table: stores.partitions.clone(),
                    model: model.clone(),
                    tools: tools.clone(),
                    workspace: paths.data().join("agents/inbox"),
                    signals: edge_signals.clone(),
                    audit: Some(trail.clone()),
                })?;
                if let Some(bindings) = &options.channel_bindings {
                    inbox = inbox.with_bindings(bindings.clone());
                }
                Some(Arc::new(inbox))
            }
            _ => None,
        };
        let incidents = Arc::new(
            IncidentPath::new(
                runtime.clone(),
                ladder.clone(),
                alarms.clone(),
                Arc::new(edge.status()),
            )
            .with_audit(trail.clone()),
        );
        let guardian = match &options.probe {
            Some(probe) => guardian(GuardianParts {
                probe,
                paths: &paths,
                domain: &domain,
                site: &site,
                host: host.clone(),
                release_key,
                audit: (recorder.clone(), options.owner.as_str()),
                incidents: incidents.clone(),
            })?,
            None => None,
        };

        let lane = Arc::new(StreamLane::new(Arc::new(ReceiptEntrySigner::new(receipt_signer))));
        let updates = Arc::new(
            UpdateEngine::builder(runtime.shared_pipeline(), edge.live.clone())
                // Held items and deliveries outlive a restart, so the owner's cards still
                // decide them.
                .held_store(Arc::new(FileHeldStore::new(paths.journal().join("held.json"))))
                .streams(lane)
                .alarms(Arc::new(SiteAlarms::new(alarms.clone(), site.id)))
                .audit(
                    recorder.clone(),
                    adk_core::ActingContext::derived(
                        &site.name,
                        "updates",
                        &options.owner,
                        adk_core::SubjectKind::Service,
                    ),
                )
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
                .with_audit(trail.clone())
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
                .with_events(alarms.clone())
                .with_audit(trail.clone());
            if aeo {
                tracker = tracker.with_source(Arc::new(AeoKpi::new(site.id, counts.clone())));
            }
            tracker
        };
        let review = Arc::new(
            ReviewComposer::new(site.id, tracker(aeo.is_some()), ladder.clone(), charters.clone())
                .with_audit(trail.clone())
                .with_alarms(alarms.clone())
                .with_experiments(experiments.clone())
                .with_updates(updates.clone())
                .with_incidents(incidents.clone()),
        );
        let cron = options.experiments_cron.clone().unwrap_or_else(|| "0 30 3 * * *".into());
        let learning =
            Arc::new(crate::learning::PlaybookLearning::open(crate::learning::LearningParts {
                site: site.clone(),
                team: crate::team::SiteTeam::new(site.clone(), &template),
                owner: options.owner.clone(),
                composer: (*review).clone(),
                experiments: experiments.clone(),
                charters: charters.clone(),
                ladder: ladder.clone(),
                approvals: stores.approvals.clone(),
                data: paths.data(),
            })?);
        let triggers = vec![
            experiments.trigger(&site.name, &cron),
            learning.trigger(&site.name, crate::learning::LEARNING_CRON),
        ];
        let repository = crate::repository::RepositoryEntry::open(
            &paths,
            crate::repository::RepositoryParts {
                site: site.clone(),
                owner: options.owner.clone(),
                runtime: runtime.clone(),
                approvals: stores.approvals.clone(),
                signals: edge_signals.clone(),
                source: None,
            },
        )?
        .map(Arc::new);
        let mut router = site_router(&edge.edge, host, &project.config.name)
            .merge(stream_router(updates.clone()));
        if let Some(entry) = &repository {
            router = router.merge(entry.router(stores.runs.clone()));
        }
        verify_edge.set(Arc::new(adk_awp::verify::fetch::RouterFetcher::new(router.clone())));
        Ok(Self {
            tracker: tracker(aeo.is_some()),
            site,
            domain,
            template,
            paths,
            runtime,
            partitions,
            edge,
            packs,
            updates,
            driver,
            cards,
            experiments,
            ladder,
            pauses,
            alarms,
            review,
            spend,
            leads,
            quotes,
            outbox,
            incidents,
            inbox,
            guardian,
            repository,
            learning,
            reporting,
            wallet: options.wallet.clone(),
            signals: edge_signals,
            publish_delay: options.publish_delay.unwrap_or_else(|| {
                chrono::Duration::hours(crate::publish_delay::DEFAULT_PUBLISH_DELAY_HOURS)
            }),
            router,
            triggers,
            aeo,
            tasks: Vec::new(),
        })
    }

    /// The edge's signals into the back office (spec 3.5 §7.10): what a public agent or a
    /// form files, which wakes the back office's processes while the site serves.
    pub fn signals(&self) -> Arc<KernelSignals> {
        self.signals.clone()
    }

    /// The KPI tracker the site measures with, for the owner's site view (spec 3.5 §7.24).
    pub fn tracker(&self) -> &KpiTracker {
        &self.tracker
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
        // The Inbox reads the site's email while the site serves; the cursor survives a
        // restart, so no message is read twice (spec 3.5 §7.19).
        if let (Some(inbox), Some(outbox)) = (self.inbox.clone(), self.outbox.clone()) {
            let file = self.paths.data().join("inbox").join("email.cursor");
            self.tasks.push(tokio::spawn(async move {
                let mut cursor = std::fs::read_to_string(&file).ok().map(|c| c.trim().to_string());
                loop {
                    match inbox.read_email(&outbox, cursor.as_deref(), 20).await {
                        Ok(next) if next != cursor => {
                            if let Some(next) = &next
                                && let Err(error) = file
                                    .parent()
                                    .map_or(Ok(()), std::fs::create_dir_all)
                                    .and_then(|()| std::fs::write(&file, next))
                            {
                                tracing::warn!(error = %error, "the Inbox's email cursor was not kept");
                            }
                            cursor = next;
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(error = %error, "the Inbox could not read the site's email"),
                    }
                    tokio::time::sleep(idle).await;
                }
            }));
        }
        if let Some(entry) = self.repository.clone() {
            self.tasks.push(entry.spawn(idle));
        }
        self.tasks.push(self.learning.clone().spawn(idle));
        let _resolver = self.edge.edge.spawn_resolver(Duration::from_secs(5));
        // Deliveries a stopped process left in `data/edge.db` resume once their lease passes.
        let _deliveries = self.edge.edge.spawn_delivery_resumer(Duration::from_secs(30));
    }

    /// The hosting job's upkeep check (spec 3.5 §7.20), when the site has a lockfile with an
    /// advisory source or a certificate source: proposals go to `owner`'s approval inbox.
    pub fn upkeep(&self, stores: &KernelStores, options: &SiteOptions) -> Option<Arc<UpkeepCheck>> {
        if options.advisories.is_none() && options.certificate.is_none() {
            return None;
        }
        let mut check = UpkeepCheck::new(
            self.site.id,
            self.domain.clone(),
            self.site.name.clone(),
            options.owner.clone(),
            stores.approvals.clone(),
        )
        .with_alarms(self.alarms.clone());
        if let Some((lockfile, source)) = &options.advisories {
            check = check.with_advisories(lockfile, source.clone());
        }
        if let Some(source) = &options.certificate {
            check = check.with_certificate(source.clone());
        }
        Some(Arc::new(check))
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
