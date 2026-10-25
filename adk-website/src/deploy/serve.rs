//! Serving sites: the kernel's control plane and the agent edge (spec 3.5 §7.17, §7.22,
//! A-I20, A-I22): what `adk-website start`, `adk-website agency` and the container run.
//!
//! | Listener | Serves |
//! |---|---|
//! | Kernel (loopback by default) | The control plane, with device pairing: the setup code is claimed at `POST /api/kernel/pairing/claim` |
//! | Edge | The agent edge's routes and the content: the signed release on the site's `DirectoryHost`, or the holding page until the launch; in agency mode, each site's by its host (A-I22) |
//!
//! Each site's content repository, site log, edge stores and keys live in its project
//! directory (`content/`, `data/`); the kernel's stores are in memory for now.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_auth::web_bot_auth::{HttpKeyDirectorySource, HttpSignatureVerifier};
use adk_awp::signing::jws::DocumentSigner;
use adk_core::audit_log::AuditActor;
use adk_core::{AdkError, InMemoryRunStore, Result, Url};
use adk_harness::website::{SitePartitions, SiteRef, SiteRuntime};
use adk_kernel::charter::InMemorySignalStore;
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{InMemoryDeviceStore, IssuedCode, Pairing};
use adk_kernel::{AgendaStore, InMemoryAgendaStore, Kernel, KernelConfig, Partitions};
use adk_server::awp::stores::SqliteEdgeStores;
use adk_server::awp::{ConsentPolicy, EdgeKeys, EdgeTiers, WebBotAuthTrustAssigner};
use adk_site::content::{FsContentStore, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::DirectoryHost;
use adk_site::log::SiteLog;
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::Charter;
use serde::Deserialize;
use tokio::task::JoinHandle;

use super::agency::{Agency, AgencyRouter};
use crate::contract::SiteKeys;
use crate::edge::adapters::{DeferredAnnouncer, KernelSignals, ReleaseSearch};
use crate::edge::handlers::InMemoryContactBook;
use crate::edge::{SiteEdge, SiteEdgeParts, consent_config, site_router};
use crate::kinds::template;
use crate::onboarding::SetupGate;
use crate::workflows::SiteWorkflows;

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// How to serve a site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    /// The site project.
    pub dir: PathBuf,
    /// Where the edge listens.
    pub edge: SocketAddr,
    /// Where the kernel's control plane listens; loopback unless a token or SSO guards it.
    pub kernel: SocketAddr,
    /// The edge's public URL; the site's base URL when not set.
    pub edge_url: Option<String>,
    /// The person the setup code pairs.
    pub owner: String,
}

/// Served sites; dropping it stops the edge's listener.
pub struct Served {
    /// Where the edge listens.
    pub edge_addr: SocketAddr,
    /// Where the kernel's control plane listens.
    pub kernel_addr: SocketAddr,
    /// The setup code, also in the logs (A-I20).
    pub setup_code: IssuedCode,
    /// The sites served.
    pub sites: Vec<SiteRef>,
    kernel: Kernel,
    edge_task: JoinHandle<()>,
}

impl std::fmt::Debug for Served {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Served")
            .field("edge_addr", &self.edge_addr)
            .field("kernel_addr", &self.kernel_addr)
            .field("sites", &self.sites)
            .finish_non_exhaustive()
    }
}

impl Served {
    /// Stops the edge and the kernel with its control plane.
    ///
    /// # Errors
    ///
    /// The kernel's error on shutdown.
    pub async fn shutdown(self) -> Result<()> {
        self.edge_task.abort();
        self.kernel.stop().await.map_err(config)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.edge_task.abort();
    }
}

/// The 32-byte seed in `path`, made on first use with owner-only permissions.
async fn seed(path: &Path) -> Result<[u8; 32]> {
    if let Ok(bytes) = tokio::fs::read(path).await
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return Ok(seed);
    }
    let mut fresh = [0u8; 32];
    getrandom::fill(&mut fresh).map_err(config)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(config)?;
    }
    tokio::fs::write(path, fresh).await.map_err(config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(config)?;
    }
    Ok(fresh)
}

/// A site opened from its project directory: its runtime, its keys and its public router.
struct OpenSite {
    site: SiteRef,
    domain: String,
    router: axum::Router,
}

/// Opens the site project at `dir`: its runtime over the content repository, a SQLite site
/// log and a `DirectoryHost`, and its edge over SQLite stores with signals into `agenda`.
async fn open_site(
    dir: &Path,
    edge_url: Option<String>,
    agenda: Arc<dyn AgendaStore>,
) -> Result<(OpenSite, SitePartitions)> {
    let project = SiteProject::load(dir, chrono::Utc::now()).await.map_err(config)?;
    let domain = project.config.domain.clone();
    let charter: Option<Charter> = tokio::fs::read(dir.join("charter.draft.json"))
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let kind = charter.as_ref().map_or(awp_types::SiteKind::Business, |c| c.kind);
    let id = charter.as_ref().map_or_else(uuid::Uuid::new_v4, |c| c.site_id);
    let site = SiteRef::new(id, domain.replace('.', "-"), project.config.base_url());
    let data = dir.join("data");
    let release_seed = seed(&data.join("keys/release.seed")).await?;
    let keys = SiteKeys::from_seeds(
        &domain,
        seed(&data.join("keys/root.seed")).await?,
        release_seed,
        seed(&data.join("keys/edge.seed")).await?,
    )?;
    let sqlite = |name: &str| format!("sqlite://{}?mode=rwc", data.join(name).display());
    let audit = adk_session::SqliteAuditLogStore::new(&sqlite("site.db")).await?;
    audit.migrate().await?;
    let signer = Ed25519ReleaseSigner::from_seed(&domain, release_seed).map_err(config)?;
    let log = SiteLog::open(Arc::new(audit), &domain, Arc::new(signer)).await.map_err(config)?;
    let host = Arc::new(DirectoryHost::new(dir.join("host")));
    let announcer = Arc::new(DeferredAnnouncer::default());
    let runtime = SiteRuntime::builder(
        site.clone(),
        project.clone(),
        Arc::new(FsContentStore::new(dir)),
        Arc::new(BuiltinGenerator),
        host.clone(),
        Arc::new(log),
        data.join("work"),
    )
    .announcer(announcer.clone())
    .build();
    let site_partitions = SitePartitions::new(&site);

    let pool = sqlx::SqlitePool::connect(&sqlite("edge.db")).await.map_err(config)?;
    let stores = SqliteEdgeStores::new(pool).with_site(site.name.clone());
    stores.migrate().await.map_err(config)?;
    // Consent policy version 1 is the charter's public terms (spec 3.5 §7.5).
    let terms = tokio::fs::read(dir.join("public-terms.json")).await.unwrap_or_default();
    let policy = ConsentPolicy { digest: adk_site::release::file_digest(&terms), ttl: None };
    let subject_key = seed(&data.join("keys/subject.seed")).await?.to_vec();
    let edge_url = edge_url.unwrap_or_else(|| project.config.base_url());
    let signer: Arc<dyn DocumentSigner> = keys.edge.clone();
    let directory = HttpKeyDirectorySource::new().map_err(config)?;
    let tiers = EdgeTiers::builder()
        .verifier(WebBotAuthTrustAssigner::new(
            HttpSignatureVerifier::builder(Arc::new(directory)).build(),
        ))
        .build();
    let signals = site_partitions.signal_sink(Arc::new(InMemorySignalStore::new()), agenda);
    let site_edge = SiteEdge::build(SiteEdgeParts {
        template: template(kind),
        runtime,
        keys: EdgeKeys { card: signer.clone(), receipt: signer },
        edge_url: Url::parse(&edge_url).map_err(config)?,
        stores: stores.edge_stores(),
        signals: Arc::new(KernelSignals::new(signals)),
        search: Arc::new(ReleaseSearch::new(Arc::new(adk_harness::website::SiteSearch::new()))),
        contacts: Arc::new(InMemoryContactBook::default()),
        calendar: None,
        payments: None,
        approvals: None,
        tiers: Some(tiers),
        consent: Some(consent_config(&template(kind), policy, subject_key)),
        threads: None,
        quote_validity: chrono::Duration::days(7),
    })?;
    announcer.set(site_edge.announcer());
    let _resolver = site_edge.edge.spawn_resolver(std::time::Duration::from_secs(5));
    let router = site_router(&site_edge.edge, host, &project.config.name);
    Ok((OpenSite { site, domain, router }, site_partitions))
}

/// What the kernel and the edge's listener start with.
struct Launch<'a> {
    name: String,
    home: PathBuf,
    table: Partitions,
    agenda: Arc<InMemoryAgendaStore>,
    edge: SocketAddr,
    kernel: SocketAddr,
    owner: &'a str,
}

/// Starts the kernel with device pairing on its control plane, then the edge's listener
/// serving `app`; issues the setup code into the logs (A-I20).
async fn start(launch: Launch<'_>, app: axum::Router, sites: Vec<SiteRef>) -> Result<Served> {
    let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    let setup_code = SetupGate::new(pairing.clone(), launch.owner).issue().await?;
    // The kernel binds its control plane itself; a port of 0 is resolved first so the
    // address it listens on is known.
    let kernel_addr = if launch.kernel.port() == 0 {
        std::net::TcpListener::bind(launch.kernel).and_then(|l| l.local_addr()).map_err(config)?
    } else {
        launch.kernel
    };
    let control =
        ControlPlaneConfig::default().without_socket().with_http(kernel_addr).with_pairing(pairing);
    let kernel = Kernel::builder()
        .config(
            KernelConfig::default()
                .with_app_name(launch.name)
                .with_home(launch.home)
                .with_control(control),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .partitions(launch.table)
        .agenda_store(launch.agenda)
        .workflows(Arc::new(SiteWorkflows::new()))
        .build()
        .map_err(config)?;
    kernel.start().await.map_err(config)?;
    let listener = tokio::net::TcpListener::bind(launch.edge).await.map_err(config)?;
    let edge_addr = listener.local_addr().map_err(config)?;
    let edge_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::error!(error = %error, "the edge stopped");
        }
    });
    tracing::info!(edge = %edge_addr, kernel = %kernel_addr, sites = sites.len(), "serving");
    Ok(Served { edge_addr, kernel_addr, setup_code, sites, kernel, edge_task })
}

/// Serves the site at `options.dir` (spec 3.5 §7.17, §7.22): issues the setup code into the
/// logs, starts the kernel with device pairing on its control plane, and serves the edge
/// with the content behind it.
///
/// # Errors
///
/// A project that does not load, a store that does not open, or a listener that does not
/// bind.
pub async fn serve(options: ServeOptions) -> Result<Served> {
    let table = Partitions::in_memory();
    let agenda = Arc::new(InMemoryAgendaStore::new());
    let (opened, partitions) =
        open_site(&options.dir, options.edge_url.clone(), agenda.clone()).await?;
    partitions.install(&table, AuditActor::default()).await?;
    let launch = Launch {
        name: opened.site.name.clone(),
        home: options.dir.join("data/kernel"),
        table,
        agenda,
        edge: options.edge,
        kernel: options.kernel,
        owner: &options.owner,
    };
    start(launch, opened.router, vec![opened.site]).await
}

/// An agency's file: the sites it runs, each a project directory relative to the file
/// (spec 3.5 §7.22).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgencyFile {
    /// The sites.
    #[serde(rename = "site")]
    pub sites: Vec<AgencySite>,
}

/// One site of an agency.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgencySite {
    /// The domain it is served at.
    pub domain: String,
    /// Its project directory, relative to the agency file.
    pub dir: PathBuf,
}

/// Serves every site `file` lists from one harness (spec 3.5 §7.22, A-I22): one kernel whose
/// table holds each site's isolated partitions, and one edge listener that sends a request
/// only to the site its host names.
///
/// # Errors
///
/// An unreadable file, a duplicate site, or [`serve`]'s errors.
pub async fn serve_agency(
    file: &Path,
    edge: SocketAddr,
    kernel: SocketAddr,
    owner: &str,
) -> Result<Served> {
    let text = tokio::fs::read_to_string(file).await.map_err(config)?;
    let listed: AgencyFile = toml::from_str(&text).map_err(config)?;
    let root = file.parent().unwrap_or(Path::new("."));
    let table = Partitions::in_memory();
    let agenda = Arc::new(InMemoryAgendaStore::new());
    let mut agency = Agency::new(table.clone());
    let (mut edges, mut sites) = (std::collections::BTreeMap::new(), Vec::new());
    for listed in &listed.sites {
        let (opened, _) = open_site(&root.join(&listed.dir), None, agenda.clone()).await?;
        agency.add(opened.site.clone()).await?;
        edges.insert(listed.domain.to_ascii_lowercase(), opened.router);
        if opened.domain != listed.domain {
            tracing::warn!(listed = %listed.domain, project = %opened.domain, "the agency file and the project name different domains");
        }
        sites.push(opened.site);
    }
    let app = AgencyRouter::new(edges).router();
    let launch = Launch {
        name: "agency".into(),
        home: root.join("data/kernel"),
        table,
        agenda,
        edge,
        kernel,
        owner,
    };
    start(launch, app, sites).await
}
