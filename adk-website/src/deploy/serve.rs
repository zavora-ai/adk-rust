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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, Result};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::pairing::{IssuedCode, Pairing};
use adk_kernel::wake::Wake;
use adk_kernel::{CronWakeSource, Kernel, KernelConfig, SourceWatermark};
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
    /// Stops the edge, the sites' background work and the kernel with its control plane.
    ///
    /// # Errors
    ///
    /// The kernel's error on shutdown.
    pub async fn shutdown(mut self) -> Result<()> {
        self.edge_task.abort();
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
) -> Result<Served> {
    let pairing = Pairing::new(stores.devices.clone());
    let paired = !pairing.devices().await.map_err(config)?.is_empty();
    let setup_code = if paired {
        None
    } else {
        Some(SetupGate::new(pairing.clone(), options.site.owner.clone()).issue().await?)
    };
    // The kernel binds its control plane itself; a port of 0 is resolved first so the
    // address it listens on is known.
    let kernel_addr = if options.kernel.port() == 0 {
        std::net::TcpListener::bind(options.kernel).and_then(|l| l.local_addr()).map_err(config)?
    } else {
        options.kernel
    };
    let control =
        ControlPlaneConfig::default().without_socket().with_http(kernel_addr).with_pairing(pairing);
    let mut site_workflows = SiteWorkflows::new();
    let mut triggers = Vec::new();
    let mut builder = Kernel::builder()
        .config(KernelConfig::default().with_app_name(name).with_home(home).with_control(control))
        .run_store(stores.runs.clone())
        .approval_service(stores.approvals.clone())
        .partitions(stores.partitions.clone())
        .agenda_store(stores.agenda.clone());
    for site in &sites {
        triggers.extend(site.triggers().iter().cloned());
        if let Some(target) = site.digest_target(&options.site) {
            site_workflows = site_workflows.digest(site.site.id, target);
            let team = SiteTeam::new(site.site.clone(), &site.template);
            let partition = site.partitions.back_office().id.clone();
            for process in crate::jobs::processes(&team, &site.template.jobs(), name, &partition) {
                if process.kind == adk_kernel::charter::SiteProcessKind::Digest {
                    triggers.push(process.trigger);
                }
            }
        }
    }
    // A standing trigger's cron expression fires through its own watermarked source.
    for trigger in triggers {
        if let Wake::Cron { expr, missed } = &trigger.wake {
            let watermark = SourceWatermark::new(stores.watermarks.clone(), trigger.name.clone());
            let source = CronWakeSource::watermarked(
                trigger.name.clone(),
                expr,
                *missed,
                Arc::new(watermark),
            )
            .map_err(config)?;
            builder = builder.wake_source(Arc::new(source));
        }
        builder = builder.trigger(trigger);
    }
    let workflows =
        ServedWorkflows::new(site_workflows, sites.iter().map(|site| site.experiments.clone()));
    let kernel = builder.workflows(Arc::new(workflows)).build().map_err(config)?;
    kernel.start().await.map_err(config)?;
    let idle = options.site.idle.unwrap_or(Duration::from_secs(60));
    for site in &mut sites {
        site.start(idle);
    }
    let listener = tokio::net::TcpListener::bind(options.edge).await.map_err(config)?;
    let edge_addr = listener.local_addr().map_err(config)?;
    let edge_task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::error!(error = %error, "the edge stopped");
        }
    });
    tracing::info!(edge = %edge_addr, kernel = %kernel_addr, sites = sites.len(), "serving");
    Ok(Served { edge_addr, kernel_addr, setup_code, sites, stores, kernel, edge_task })
}

/// Serves the site at `options.dir` (spec 3.5 §7.17, §7.22): issues the setup code into the
/// logs while no device is paired, starts the kernel over its durable stores with device
/// pairing on its control plane, and serves the edge with the release behind it.
///
/// # Errors
///
/// A project that does not load, a store that does not open, or a listener that does not
/// bind.
pub async fn serve(options: ServeOptions) -> Result<Served> {
    let paths = ProjectPaths::new(&options.dir);
    std::fs::create_dir_all(paths.data()).map_err(config)?;
    let stores = KernelStores::open(&paths.database("kernel"), &options.site.owner).await?;
    let site = RunningSite::open(paths.clone(), &stores, &options.site).await?;
    site.partitions.install(&stores.partitions, adk_core::audit_log::AuditActor::default()).await?;
    let app = site.router.clone();
    let name = site.site.name.clone();
    start(&name, paths.data().join("kernel"), stores, vec![site], &options, app).await
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
    start("agency", agency_paths.data().join("kernel"), stores, sites, &options, app).await
}
