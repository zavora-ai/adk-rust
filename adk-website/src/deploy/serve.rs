//! Serving sites: the kernel's control plane and the agent edge (spec 3.5 §7.17, §7.22,
//! A-I20, A-I22): what `adk-website start`, `adk-website agency` and the container run.
//!
//! | Listener | Serves |
//! |---|---|
//! | Kernel (loopback by default) | The control plane, with device pairing: the setup code is claimed at `POST /api/kernel/pairing/claim` |
//! | Edge | The agent edge's routes, the stream feed and the content: the signed release on the site's `DirectoryHost`, or the holding page until the launch; in agency mode, each site's by its host (A-I22) |
//!
//! Each site is a [`RunningSite`] over its project ([`ProjectPaths`]); the kernel's stores are
//! durable in `data/kernel.db` ([`KernelStores`]). The setup code is issued while no device
//! is paired, so a restarted site does not offer a second one.
//!
//! The kernel runs each site's processes (spec 3.5 §7.20): the Guardian's probe when the site
//! has a domain claim ([`ProbeOptions`](super::ProbeOptions)), the weekly digest when the owner
//! has a channel, the hosting job's upkeep check when the site has an advisory or certificate
//! source, and — with a model — the back office ([`super::back_office`]): the Steward leading
//! the team, and each agent's jobs on their signals and schedules, under the owner's
//! standing rules in `data/kernel.db`. Agent sessions are kept in `data/kernel/sessions.db`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, Result};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{IssuedCode, Pairing};
use adk_kernel::{Kernel, KernelConfig};
use serde::Deserialize;
use tokio::task::JoinHandle;

use super::agency::{Agency, AgencyRouter};
use super::kernel::KernelStores;
use super::project::ProjectPaths;
use super::site::{RunningSite, ServedWorkflows, SiteOptions};
use crate::onboarding::SetupGate;
use crate::team::SiteTeam;
use crate::workflows::SiteWorkflows;

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// How to serve a site.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The site project.
    pub dir: PathBuf,
    /// Where the edge listens.
    pub edge: SocketAddr,
    /// Where the kernel's control plane listens; loopback unless a token or SSO guards it.
    pub kernel: SocketAddr,
    /// The owner, the edge's URL, the channels, drivers and schedules.
    pub site: SiteOptions,
}

impl ServeOptions {
    /// The site at `dir` for `owner`, the edge on `127.0.0.1:8080` and the kernel on
    /// `127.0.0.1:7340`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::deploy::ServeOptions;
    ///
    /// let options = ServeOptions::new("bakery", "amina");
    /// assert_eq!(options.edge.port(), 8080);
    /// assert_eq!(options.site.owner, "amina");
    /// ```
    pub fn new(dir: impl Into<PathBuf>, owner: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            edge: SocketAddr::from(([127, 0, 0, 1], 8080)),
            kernel: SocketAddr::from(([127, 0, 0, 1], 7340)),
            site: SiteOptions { owner: owner.into(), ..SiteOptions::default() },
        }
    }
}

/// Served sites; dropping it stops the edge's listener and the sites' background work.
pub struct Served {
    /// Where the edge listens.
    pub edge_addr: SocketAddr,
    /// Where the kernel's control plane listens.
    pub kernel_addr: SocketAddr,
    /// The setup code, also in the logs, while no device is paired (A-I20).
    pub setup_code: Option<IssuedCode>,
    /// The sites served.
    pub sites: Vec<RunningSite>,
    /// The kernel's durable stores.
    pub stores: KernelStores,
    kernel: Kernel,
    edge_task: JoinHandle<()>,
    chat: Option<adk_kernel::channels::ChannelGateway>,
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
    /// The kernel serving the sites.
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// The site's chat gateway, when [`SiteOptions::chat`](super::SiteOptions::chat) is set
    /// (spec 3.5 §10 X12): every chat platform's messages pass through it.
    pub fn chat(&self) -> Option<&adk_kernel::channels::ChannelGateway> {
        self.chat.as_ref()
    }

    /// Stops the edge, the sites' background work and the kernel with its control plane.
    ///
    /// # Errors
    ///
    /// The kernel's error on shutdown.
    pub async fn shutdown(mut self) -> Result<()> {
        self.edge_task.abort();
        if let Some(chat) = &self.chat {
            chat.stop();
        }
        drop(std::mem::take(&mut self.sites));
        self.kernel.stop().await.map_err(config)
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        self.edge_task.abort();
    }
}

/// Starts the kernel over `stores` with `sites`' workflows and triggers and device pairing on
/// its control plane, then the edge's listener serving `app`.
async fn start(
    name: &str,
    home: PathBuf,
    stores: KernelStores,
    mut sites: Vec<RunningSite>,
    options: &ServeOptions,
    app: axum::Router,
    doorbell: Option<Arc<adk_kernel::channels::ChannelDoorbell>>,
) -> Result<Served> {
    let pairing = Pairing::new(stores.devices.clone());
    // `adk-rust pair accept` pairs a chat account that asked with `/pair` (spec 3.5 §10 X12).
    let pairing = match &options.site.channel_bindings {
        Some(bindings) => pairing.with_channel_bindings(bindings.clone()),
        None => pairing,
    };
    let paired = !pairing.devices().await.map_err(config)?.is_empty();
    let setup_code = if paired {
        None
    } else {
        Some(SetupGate::new(pairing.clone(), options.site.owner.clone()).issue().await?)
    };
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(options.kernel)
        .with_pairing(pairing);
    let mut site_workflows = SiteWorkflows::new();
    let mut triggers = Vec::new();
    let sessions: Option<Arc<dyn adk_session::SessionService>> = match &options.site.model {
        Some(_) => {
            std::fs::create_dir_all(&home).map_err(config)?;
            let url = format!("sqlite://{}?mode=rwc", home.join("sessions.db").display());
            let sessions = adk_session::SqliteSessionService::new(&url).await?;
            sessions.migrate().await?;
            Some(Arc::new(sessions))
        }
        None => None,
    };
    let mut kernel_config =
        KernelConfig::default().with_app_name(name).with_home(home.clone()).with_control(control);
    // The site's proactive runs wait out the owner's quiet hours (spec 3.5 §7.21, §5 S10); an
    // agency's sites may keep different hours, and the kernel holds one set.
    if let [site] = sites.as_slice()
        && let Some(quiet) = crate::alarms::quiet_hours(&site.reporting)
    {
        kernel_config = kernel_config.with_quiet_hours(quiet);
    }
    let mut builder = Kernel::builder()
        .config(kernel_config)
        .run_store(stores.runs.clone())
        .approval_service(stores.approvals.clone())
        .partitions(stores.partitions.clone())
        .agenda_store(stores.agenda.clone())
        .watermark_store(stores.watermarks.clone());
    for site in &sites {
        use adk_kernel::charter::SiteProcessKind;
        triggers.extend(site.triggers().iter().cloned());
        let team = SiteTeam::new(site.site.clone(), &site.template);
        let partition = site.partitions.back_office().id.clone();
        // The runs of a site's processes are its own app's, so its agents act as
        // `urn:adk:agent:{site}/{agent}`, an agency's sites included.
        let app = site.site.name.as_str();
        let mut runs: Vec<SiteProcessKind> = Vec::new();
        if let Some(target) = site.digest_target(&options.site) {
            site_workflows = site_workflows.digest(site.site.id, target);
            runs.push(SiteProcessKind::Digest);
        }
        // The Guardian's probe every five minutes, when the site has a domain claim (spec
        // 3.5 §7.20, A-I9).
        if let Some(probe) = &site.guardian {
            site_workflows = site_workflows.probe(site.site.id, probe.clone());
            runs.push(SiteProcessKind::Probe);
        }
        // A push to the content repository is a run of the site's own (spec 3.5 §7.8.2).
        if let Some(entry) = &site.repository {
            site_workflows = site_workflows.repository(site.site.id, entry.clone());
        }
        // The weekly playbook learning, decided by the owner (spec 3.5 §7.15).
        site_workflows = site_workflows.learning(site.site.id, site.learning.clone());
        if let Some(check) = site.upkeep(&stores, &options.site) {
            site_workflows = site_workflows.watch(site.site.id, check);
            runs.push(SiteProcessKind::Watch);
        }
        if let (Some(model), Some(sessions)) = (&options.site.model, &sessions) {
            let team = site.back_office(&stores, model.clone()).await?;
            let hosted;
            (builder, hosted) =
                super::back_office::host(builder, &team, &partition, app, sessions.clone())?;
            tracing::info!(site = app, agents = ?hosted, "the back office is hosted");
            runs.extend([
                SiteProcessKind::Content,
                SiteProcessKind::Reviews,
                SiteProcessKind::Outreach,
                SiteProcessKind::Support,
            ]);
        }
        for process in crate::jobs::processes(&team, &site.template.jobs(), app, &partition) {
            if runs.contains(&process.kind) {
                triggers.push(process.trigger);
            }
        }
    }
    // The kernel fires each standing trigger's cron expression through one watermarked
    // source of its own, its cursor in the project's store.
    for trigger in triggers {
        builder = builder.trigger(trigger);
    }
    let workflows =
        ServedWorkflows::new(site_workflows, sites.iter().map(|site| site.experiments.clone()));
    let kernel = builder.workflows(Arc::new(workflows)).build().map_err(config)?;
    // The edge's signals wake the back office's processes (spec 3.5 §7.10, §7.20).
    for site in &sites {
        site.signals.wake(kernel.wake_router().clone(), kernel.config().app_name.clone());
    }
    kernel.start().await.map_err(config)?;
    let kernel_addr = kernel
        .control_addr()
        .await
        .ok_or_else(|| config("the kernel's control plane is not listening"))?;
    let idle = options.site.idle.unwrap_or(Duration::from_secs(60));
    for site in &mut sites {
        site.start(idle);
    }
    // The site's chat: its gateway on the kernel, and the adapters' routes on the edge.
    let (mut app, mut chat) = (app, None);
    if let (Some(adapters), [site]) = (&options.site.chat, sites.as_slice()) {
        let bindings: Arc<dyn adk_kernel::channels::ChannelBindingStore> =
            match &options.site.channel_bindings {
                Some(bindings) => bindings.clone(),
                None => stores.channel_bindings.clone(),
            };
        let (gateway, routes) =
            super::chat::start(adapters, &kernel, site, bindings, doorbell, sessions.clone())
                .await?;
        app = app.merge(routes);
        chat = Some(gateway);
    }
    let listener = tokio::net::TcpListener::bind(options.edge).await.map_err(config)?;
    let edge_addr = listener.local_addr().map_err(config)?;
    // The peer address keys anonymous callers' rate limits and conversations (spec 3.5 §7.3).
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    let edge_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, service).await {
            tracing::error!(error = %error, "the edge stopped");
        }
    });
    tracing::info!(edge = %edge_addr, kernel = %kernel_addr, sites = sites.len(), "serving");
    Ok(Served { edge_addr, kernel_addr, setup_code, sites, stores, kernel, edge_task, chat })
}

/// Serves the site at `options.dir` (spec 3.5 §7.17, §7.22): issues the setup code into the
/// logs while no device is paired, starts the kernel over its durable stores with device
/// pairing on its control plane, and serves the edge with the release behind it.
///
/// # Errors
///
/// A project that does not load, a store that does not open, or a listener that does not
/// bind.
pub async fn serve(mut options: ServeOptions) -> Result<Served> {
    let paths = ProjectPaths::new(&options.dir);
    std::fs::create_dir_all(paths.data()).map_err(config)?;
    // The site's chat gateway hears each decision the approval service takes (spec 3.5 §10
    // X12).
    let doorbell =
        options.site.chat.is_some().then(|| Arc::new(adk_kernel::channels::ChannelDoorbell::new()));
    let notifiers = doorbell.iter().map(|bell| bell.clone() as Arc<dyn adk_core::ApprovalNotifier>);
    let stores = KernelStores::open_with(
        &paths.database("kernel"),
        &options.site.owner,
        notifiers.collect(),
    )
    .await?;
    if options.site.chat.is_some() && options.site.channel_bindings.is_none() {
        options.site.channel_bindings = Some(stores.channel_bindings.clone());
    }
    let site = RunningSite::open(paths.clone(), &stores, &options.site).await?;
    site.partitions.install(&stores.partitions, adk_core::audit_log::AuditActor::default()).await?;
    let app = site.router.clone();
    let name = site.site.name.clone();
    start(&name, paths.data().join("kernel"), stores, vec![site], &options, app, doorbell).await
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
/// durable table holds each site's isolated partitions, and one edge listener that sends a
/// request only to the site its host names. `options.dir` is ignored.
///
/// # Errors
///
/// An unreadable file, a duplicate site, or [`serve`]'s errors.
pub async fn serve_agency(file: &Path, options: ServeOptions) -> Result<Served> {
    let text = tokio::fs::read_to_string(file).await.map_err(config)?;
    let listed: AgencyFile = toml::from_str(&text).map_err(config)?;
    let root = file.parent().unwrap_or(Path::new("."));
    let agency_paths = ProjectPaths::new(root);
    std::fs::create_dir_all(agency_paths.data()).map_err(config)?;
    let stores = KernelStores::open(&agency_paths.database("kernel"), &options.site.owner).await?;
    let mut agency = Agency::new(stores.partitions.clone());
    let (mut edges, mut sites) = (std::collections::BTreeMap::new(), Vec::new());
    for listed in &listed.sites {
        let paths = ProjectPaths::new(root.join(&listed.dir));
        let site = RunningSite::open(paths, &stores, &options.site).await?;
        agency.add(site.site.clone()).await?;
        if site.domain != listed.domain {
            tracing::warn!(listed = %listed.domain, project = %site.domain, "the agency file and the project name different domains");
        }
        edges.insert(listed.domain.to_ascii_lowercase(), site.router.clone());
        sites.push(site);
    }
    let app = AgencyRouter::new(edges).router();
    if options.site.chat.is_some() {
        tracing::warn!(
            "an agency's sites run no chat gateway: serve each site on its own for chat"
        );
    }
    start("agency", agency_paths.data().join("kernel"), stores, sites, &options, app, None).await
}
