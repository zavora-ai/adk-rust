//! `IsolatedReplay`: a database dump and uploads served by containers on a network with no
//! route out, reachable only on loopback by the crawler, destroyed afterwards (spec 3.5 §7.8
//! "Sources", S3).
//!
//! | Guarantee | How |
//! |-----------|-----|
//! | The replayed site reaches nothing | Its containers sit on an internal network: no gateway, no route out |
//! | Only the crawler reaches it | A relay publishes it on `127.0.0.1` only |
//! | The crawler reaches only it | Its client allows the loopback origin and nothing else ([`EgressScope::Only`]); every request is recorded |
//! | Nothing is left running | The containers and the network are destroyed on every exit path |
//! | Nothing legacy is deleted | The dump and the uploads are mounted read-only and never removed (A-I10) |

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use url::Url;

use super::super::error::{MigrateError, Result};
use super::super::http::{ConnectionRecord, CrawlSigner, EgressScope, HttpClient, origin_of};
use super::super::inventory::{InventoryMedia, InventoryUrl, SiteInventory, SourceKind};
use super::super::{SiteSource, SourceContext};
use super::crawl::Crawl;
use super::wxr::list_uploads;

/// What to replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySpec {
    /// The legacy site's public origin, which the replayed pages link to.
    pub public_origin: String,
    /// The SQL dump.
    pub dump: PathBuf,
    /// The uploads directory (`wp-content/uploads`).
    pub uploads: PathBuf,
    /// The web image, pinned, such as `wordpress:6.8-php8.3-apache`.
    pub web_image: String,
    /// The database image, pinned, such as `mariadb:11.4`.
    pub db_image: String,
    /// How long the site may take to answer.
    #[serde(with = "seconds")]
    pub ready_timeout: Duration,
}

mod seconds {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}

impl ReplaySpec {
    /// A WordPress replay of `dump` and `uploads` for the site at `public_origin`.
    pub fn wordpress(
        public_origin: &str,
        dump: impl Into<PathBuf>,
        uploads: impl Into<PathBuf>,
    ) -> Self {
        Self {
            public_origin: public_origin.to_string(),
            dump: dump.into(),
            uploads: uploads.into(),
            web_image: "wordpress:6.8-php8.3-apache".into(),
            db_image: "mariadb:11.4".into(),
            ready_timeout: Duration::from_secs(180),
        }
    }
}

/// A running replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayContainer {
    /// The run's id.
    pub id: String,
    /// The loopback origin the crawler reaches it at, such as `http://127.0.0.1:49153`.
    pub origin: String,
    /// The internal network.
    pub network: String,
    /// The containers.
    pub containers: Vec<String>,
    /// Whether the driver confirmed the network has no route out.
    pub egress_blocked: bool,
}

/// Runs replays: Docker through its CLI ([`DockerCliDriver`]), or any other container runtime.
#[async_trait]
pub trait ContainerDriver: Send + Sync + std::fmt::Debug {
    /// Starts the replay on an internal network, publishes it on loopback only and returns
    /// when it answers.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Container`] when the runtime fails; whatever was started is destroyed.
    async fn start(&self, spec: &ReplaySpec) -> Result<ReplayContainer>;

    /// The outbound connection attempts the containers made, as the runtime observed them.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Container`] when the runtime cannot be asked.
    async fn egress_attempts(&self, replay: &ReplayContainer) -> Result<Vec<String>>;

    /// Stops and removes the containers and the network; never the dump or the uploads.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Container`] when something could not be removed.
    async fn destroy(&self, replay: &ReplayContainer) -> Result<()>;
}

/// What a replay did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayReport {
    /// The loopback origin.
    pub origin: String,
    /// Whether the network had no route out.
    pub egress_blocked: bool,
    /// Every request the crawler sent.
    pub connections: Vec<ConnectionRecord>,
    /// The requests sent anywhere but loopback; always empty.
    pub outbound_connections: Vec<ConnectionRecord>,
    /// Outbound attempts the containers made, which the network blocked.
    pub container_egress_attempts: Vec<String>,
    /// Whether the containers and the network were destroyed.
    pub destroyed: bool,
}

/// Replays a site from its database dump and uploads, crawls it, and destroys it (S3).
///
/// Needs a proven domain (A-I9). The legacy files are never modified or deleted (A-I10).
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::migrate::{SiteSource, SourceContext};
/// use adk_site::migrate::sources::{DockerCliDriver, IsolatedReplay, ReplaySpec};
///
/// # async fn demo(ctx: &SourceContext) -> adk_site::migrate::Result<()> {
/// let spec = ReplaySpec::wordpress("https://old.example.com", "dump.sql", "wp-content/uploads");
/// let replay = IsolatedReplay::new(Arc::new(DockerCliDriver::default()), spec);
/// let inventory = replay.inventory(ctx).await?;
/// assert!(replay.last_report().unwrap().outbound_connections.is_empty());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct IsolatedReplay {
    driver: Arc<dyn ContainerDriver>,
    spec: ReplaySpec,
    signer: Option<Arc<dyn CrawlSigner>>,
    min_interval: Duration,
    report: Mutex<Option<ReplayReport>>,
}

/// Destroys a replay when dropped before [`ReplayGuard::destroy`] ran: on errors and panics.
struct ReplayGuard {
    driver: Arc<dyn ContainerDriver>,
    replay: Option<ReplayContainer>,
}

impl ReplayGuard {
    async fn destroy(&mut self) -> Result<()> {
        match self.replay.take() {
            Some(replay) => self.driver.destroy(&replay).await,
            None => Ok(()),
        }
    }
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        if let Some(replay) = self.replay.take()
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let driver = Arc::clone(&self.driver);
            handle.spawn(async move {
                if let Err(error) = driver.destroy(&replay).await {
                    tracing::error!(replay = %replay.id, error = %error, "replay not destroyed");
                }
            });
        }
    }
}

impl IsolatedReplay {
    /// A replay of `spec` run by `driver`.
    pub fn new(driver: Arc<dyn ContainerDriver>, spec: ReplaySpec) -> Self {
        Self { driver, spec, signer: None, min_interval: Duration::ZERO, report: Mutex::new(None) }
    }

    /// Signs the crawler's requests.
    #[must_use]
    pub fn with_signer(mut self, signer: Arc<dyn CrawlSigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Spaces the crawler's requests.
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// The report of the last run.
    pub fn last_report(&self) -> Option<ReplayReport> {
        self.report.lock().ok().and_then(|r| r.clone())
    }

    /// The client of a run: the loopback origin only, the public origin mapped onto it.
    fn client(&self, loopback: &Url, public: &Url) -> Result<HttpClient> {
        let mut builder = HttpClient::builder()
            .egress(EgressScope::Only(BTreeSet::from([origin_of(loopback)])))
            .map_origin(public, loopback.clone())
            .min_interval(self.min_interval);
        if let Some(signer) = &self.signer {
            builder = builder.signer(Arc::clone(signer));
        }
        builder.build()
    }

    async fn crawl(
        &self,
        ctx: &SourceContext,
        replay: &ReplayContainer,
        report: &mut ReplayReport,
    ) -> Result<SiteInventory> {
        let public = Url::parse(&self.spec.public_origin)
            .map_err(|e| MigrateError::parse("public origin", e))?;
        let loopback =
            Url::parse(&replay.origin).map_err(|e| MigrateError::parse("replay origin", e))?;
        let client = Arc::new(self.client(&loopback, &public)?);
        let replay_ctx = SourceContext { http: Arc::clone(&client), ..ctx.clone() };
        let mut inventory = SiteInventory::new(SourceKind::IsolatedReplay, ctx.clock.now());
        let start = public.join("/").map_err(|e| MigrateError::parse("public origin", e))?;
        Crawl::new(start)
            .reporting_as(SourceKind::IsolatedReplay)
            .crawl_into(&replay_ctx, &mut inventory)
            .await?;
        let origin = origin_of(&public);
        let uploads = self.spec.uploads.clone();
        let files = tokio::task::spawn_blocking(move || list_uploads(&uploads))
            .await
            .map_err(|e| MigrateError::Service(format!("uploads listing failed: {e}")))?;
        for (relative, bytes) in files {
            let path = format!("/wp-content/uploads/{relative}");
            if inventory.media.iter().any(|m| m.path == path) {
                continue;
            }
            inventory.notes.push(format!("{path}: in the uploads directory, linked from no page"));
            inventory.urls.push(InventoryUrl {
                url: format!("{origin}{path}"),
                path: path.clone(),
                status: 200,
                redirect_to: None,
                content_type: None,
                sha256: Some(crate::release::file_digest(&bytes)),
                item: None,
                page: None,
                inbound_links: 0,
                body: None,
            });
            inventory
                .media
                .push(InventoryMedia::new(format!("{origin}{path}"), path).with_bytes(bytes));
        }
        report.connections = client.connections();
        report.outbound_connections = client.outbound_connections();
        Ok(inventory)
    }
}

#[async_trait]
impl SiteSource for IsolatedReplay {
    async fn inventory(&self, ctx: &SourceContext) -> Result<SiteInventory> {
        let public = Url::parse(&self.spec.public_origin)
            .map_err(|e| MigrateError::parse("public origin", e))?;
        ctx.require_proven(public.host_str().unwrap_or_default(), "an isolated replay")?;
        let replay = self.driver.start(&self.spec).await?;
        tracing::info!(replay = %replay.id, origin = %replay.origin, egress_blocked = replay.egress_blocked, "replay started");
        let mut guard =
            ReplayGuard { driver: Arc::clone(&self.driver), replay: Some(replay.clone()) };
        let mut report = ReplayReport {
            origin: replay.origin.clone(),
            egress_blocked: replay.egress_blocked,
            ..ReplayReport::default()
        };
        let crawled = self.crawl(ctx, &replay, &mut report).await;
        report.container_egress_attempts = self
            .driver
            .egress_attempts(&replay)
            .await
            .unwrap_or_else(|e| vec![format!("not observed: {e}")]);
        let destroyed = guard.destroy().await;
        report.destroyed = destroyed.is_ok();
        tracing::info!(
            replay = %replay.id,
            outbound = report.outbound_connections.len(),
            destroyed = report.destroyed,
            "replay finished"
        );
        if let Ok(mut slot) = self.report.lock() {
            *slot = Some(report.clone());
        }
        destroyed?;
        let mut inventory = crawled?;
        if !report.outbound_connections.is_empty() {
            return Err(MigrateError::Egress {
                url: report.outbound_connections[0].url.clone(),
                reason: "the replay crawler connected beyond loopback".into(),
            });
        }
        inventory.notes.push(format!(
            "replayed in isolation: {} requests, all to {}; network egress blocked: {}",
            report.connections.iter().filter(|c| c.allowed).count(),
            report.origin,
            report.egress_blocked
        ));
        Ok(inventory)
    }
}

/// A [`ContainerDriver`] over the `docker` CLI: an `--internal` network for the database and
/// the web server, and a relay published on `127.0.0.1` only.
#[derive(Debug, Clone)]
pub struct DockerCliDriver {
    docker: PathBuf,
    relay_image: String,
    command_timeout: Duration,
}

impl Default for DockerCliDriver {
    fn default() -> Self {
        Self {
            docker: PathBuf::from("docker"),
            relay_image: "alpine/socat:1.8.0.1".into(),
            command_timeout: Duration::from_secs(300),
        }
    }
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    aws_lc_rs::rand::fill(&mut buffer)
        .map_err(|_| MigrateError::Container("no randomness".into()))?;
    Ok(hex::encode(buffer))
}

impl DockerCliDriver {
    /// Uses `docker` at `path` and the relay image `relay_image`.
    pub fn new(path: impl Into<PathBuf>, relay_image: impl Into<String>) -> Self {
        Self { docker: path.into(), relay_image: relay_image.into(), ..Self::default() }
    }

    async fn docker(&self, args: &[&str]) -> Result<String> {
        let mut command = Command::new(&self.docker);
        command.args(args).kill_on_drop(true).stdin(std::process::Stdio::null());
        let output = tokio::time::timeout(self.command_timeout, command.output())
            .await
            .map_err(|_| {
                MigrateError::Container(format!("docker {} timed out", args.first().unwrap_or(&"")))
            })?
            .map_err(|e| MigrateError::Container(format!("docker could not run: {e}")))?;
        if !output.status.success() {
            return Err(MigrateError::Container(format!(
                "docker {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn start_inner(&self, spec: &ReplaySpec, replay: &mut ReplayContainer) -> Result<()> {
        let id = replay.id.clone();
        let label = format!("adk-site.replay={id}");
        let password = random_hex(16)?;
        self.docker(&["network", "create", "--internal", "--label", &label, &replay.network])
            .await?;
        let db = format!("adk-replay-{id}-db");
        let web = format!("adk-replay-{id}-web");
        let relay = format!("adk-replay-{id}-relay");
        let dump = format!("{}:/docker-entrypoint-initdb.d/dump.sql:ro", spec.dump.display());
        let uploads = format!("{}:/var/www/html/wp-content/uploads:ro", spec.uploads.display());
        self.docker(&[
            "run",
            "-d",
            "--name",
            &db,
            "--network",
            &replay.network,
            "--label",
            &label,
            "-e",
            "MARIADB_DATABASE=wordpress",
            "-e",
            "MARIADB_USER=wordpress",
            "-e",
            &format!("MARIADB_PASSWORD={password}"),
            "-e",
            "MARIADB_RANDOM_ROOT_PASSWORD=1",
            "-v",
            &dump,
            &spec.db_image,
        ])
        .await?;
        replay.containers.push(db.clone());
        self.docker(&[
            "run",
            "-d",
            "--name",
            &web,
            "--network",
            &replay.network,
            "--label",
            &label,
            "-e",
            &format!("WORDPRESS_DB_HOST={db}"),
            "-e",
            "WORDPRESS_DB_USER=wordpress",
            "-e",
            &format!("WORDPRESS_DB_PASSWORD={password}"),
            "-e",
            "WORDPRESS_DB_NAME=wordpress",
            "-v",
            &uploads,
            &spec.web_image,
        ])
        .await?;
        replay.containers.push(web.clone());
        self.docker(&[
            "run",
            "-d",
            "--name",
            &relay,
            "--label",
            &label,
            "-p",
            "127.0.0.1::8080",
            &self.relay_image,
            "tcp-listen:8080,fork,reuseaddr",
            &format!("tcp-connect:{web}:80"),
        ])
        .await?;
        replay.containers.push(relay.clone());
        self.docker(&["network", "connect", &replay.network, &relay]).await?;
        let port = self.docker(&["port", &relay, "8080/tcp"]).await?;
        let address = port.lines().find(|l| l.starts_with("127.0.0.1:")).ok_or_else(|| {
            MigrateError::Container(format!("the relay is not published on loopback: {port}"))
        })?;
        replay.origin = format!("http://{address}");
        replay.egress_blocked = self
            .docker(&["network", "inspect", "--format", "{{.Internal}}", &replay.network])
            .await?
            == "true";
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| MigrateError::Container(e.to_string()))?;
        let deadline = tokio::time::Instant::now() + spec.ready_timeout;
        loop {
            if let Ok(response) = client.get(&replay.origin).send().await
                && response.status().as_u16() < 500
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(MigrateError::Container("the replay did not answer in time".into()));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

#[async_trait]
impl ContainerDriver for DockerCliDriver {
    async fn start(&self, spec: &ReplaySpec) -> Result<ReplayContainer> {
        let id = random_hex(6)?;
        let mut replay = ReplayContainer {
            network: format!("adk-replay-{id}"),
            id,
            origin: String::new(),
            containers: Vec::new(),
            egress_blocked: false,
        };
        if let Err(error) = self.start_inner(spec, &mut replay).await {
            let _ = self.destroy(&replay).await;
            return Err(error);
        }
        Ok(replay)
    }

    async fn egress_attempts(&self, replay: &ReplayContainer) -> Result<Vec<String>> {
        let mut attempts = Vec::new();
        for container in replay.containers.iter().filter(|c| !c.ends_with("-relay")) {
            let logs =
                self.docker(&["logs", "--tail", "2000", container]).await.unwrap_or_default();
            attempts.extend(
                logs.lines()
                    .filter(|l| {
                        let l = l.to_ascii_lowercase();
                        l.contains("could not resolve host")
                            || l.contains("php_network_getaddresses")
                            || l.contains("curl error")
                            || l.contains("network is unreachable")
                    })
                    .map(|l| format!("{container}: {}", l.trim())),
            );
        }
        Ok(attempts)
    }

    async fn destroy(&self, replay: &ReplayContainer) -> Result<()> {
        let mut failures = Vec::new();
        for container in replay.containers.iter().rev() {
            if let Err(error) = self.docker(&["rm", "-f", "-v", container]).await {
                failures.push(error.to_string());
            }
        }
        if let Err(error) = self.docker(&["network", "rm", &replay.network]).await
            && !error.to_string().contains("not found")
        {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            tracing::info!(replay = %replay.id, "replay destroyed");
            Ok(())
        } else {
            Err(MigrateError::Container(failures.join("; ")))
        }
    }
}
