//! A site project on disk and the pipeline over it (spec 3.5 §7.8, §7.8.2).
//!
//! | Path | Holds |
//! |---|---|
//! | `site.toml`, `content/`, `theme/`, `public-terms.json`, `capabilities.json` | The content repository (`adk_site::content::SiteProject::load`) |
//! | `well-known/` | Files released under `/.well-known/`: the root-signed contract and `keys.json` (`awp keys`) |
//! | `static/` | Files released at their own paths, such as a migrated site's uploads |
//! | `data/site-log.db` | The site log's tree and checkpoints (SQLite) |
//! | `data/history/` | Every saved version of every item, for reverts |
//! | `data/keys/` | Delegated keys: `release.jwk` signs the log; `web-bot-auth.jwk` signs crawls; `archive.key` seals legacy exports |
//! | `data/host/` | The `DirectoryHost` releases publish to, unless `--host-dir` names another |
//! | `data/work/` | Builds |
//! | `data/updates/` | The update engine's journal: batches and feeds for `site batches` and `site feeds` |
//! | `data/claims/`, `data/migration/`, `data/archive/` | Domain claims, migration reports, the encrypted legacy archive |

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adk_core::audit_log::AuditLogStore;
use adk_site::content::{FileContentHistory, FsContentStore, SiteConfig, SiteProject};
use adk_site::generator::BuiltinGenerator;
use adk_site::host::{DirectoryHost, SiteHost};
use adk_site::log::SiteLog;
use adk_site::pipeline::{ApprovalGate, PublishingPipeline};
use adk_site::release::{PreviousRelease, manifest_digest, manifest_path};
use anyhow::{Context, Result, bail};
use awp_types::{ActionClass, PublicTerms, ReleaseManifest};
use axum::body::Bytes;

use crate::awp_cli::keyfile::KeyFile;

/// The paths of a site project.
#[derive(Debug, Clone)]
pub(crate) struct SitePaths {
    root: PathBuf,
    host: Option<PathBuf>,
}

impl SitePaths {
    pub(crate) fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), host: None }
    }

    /// Publishes to `host` instead of `data/host`.
    pub(crate) fn with_host(mut self, host: Option<PathBuf>) -> Self {
        self.host = host;
        self
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn site_toml(&self) -> PathBuf {
        self.root.join("site.toml")
    }

    /// `site.toml`, validated.
    pub(crate) fn config(&self) -> Result<SiteConfig> {
        let path = self.site_toml();
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no site project at {}: it needs a site.toml (`adk-website new` writes one); \
                 pass --site DIR",
                self.root.display()
            )
        })?;
        SiteConfig::from_toml(&text).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))
    }

    pub(crate) fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    /// A delegated key's file: `data/keys/<name>.jwk`.
    pub(crate) fn key_file(&self, name: &str) -> PathBuf {
        self.data().join("keys").join(format!("{name}.jwk"))
    }

    pub(crate) fn archive_key(&self) -> PathBuf {
        self.data().join("keys").join("archive.key")
    }

    pub(crate) fn host_dir(&self) -> PathBuf {
        self.host.clone().unwrap_or_else(|| self.data().join("host"))
    }

    pub(crate) fn work(&self) -> PathBuf {
        self.data().join("work")
    }

    pub(crate) fn journal(&self) -> PathBuf {
        self.data().join("updates")
    }

    pub(crate) fn claims(&self) -> PathBuf {
        self.data().join("claims")
    }

    pub(crate) fn migration(&self) -> PathBuf {
        self.data().join("migration")
    }

    fn log_db(&self) -> PathBuf {
        self.data().join("site-log.db")
    }

    pub(crate) fn well_known(&self) -> PathBuf {
        self.root.join("well-known")
    }

    pub(crate) fn static_dir(&self) -> PathBuf {
        self.root.join("static")
    }

    pub(crate) fn contract(&self) -> PathBuf {
        self.well_known().join("awp.json")
    }

    pub(crate) fn keys_json(&self) -> PathBuf {
        self.well_known().join("awp").join("keys.json")
    }

    /// The project file of a `/.well-known/…` URL path.
    pub(crate) fn well_known_file(&self, url_path: &str) -> Result<PathBuf> {
        let rest = url_path
            .strip_prefix("/.well-known/")
            .with_context(|| format!("{url_path} is not under /.well-known/"))?;
        Ok(self.well_known().join(rest))
    }

    /// The charter's public terms, from `public-terms.json`.
    pub(crate) fn public_terms(&self) -> Result<Option<PublicTerms>> {
        read_json(&self.root.join("public-terms.json"))
    }

    /// The site's id: the charter's, else a new one.
    pub(crate) fn site_id(&self) -> uuid::Uuid {
        ["charter.json", "charter.draft.json"]
            .iter()
            .filter_map(|name| read_json::<serde_json::Value>(&self.root.join(name)).ok().flatten())
            .find_map(|charter| charter["siteId"].as_str().and_then(|id| id.parse().ok()))
            .unwrap_or_else(uuid::Uuid::new_v4)
    }

    /// The charter's approval chain for publication, when one names more than one role
    /// (spec 3.5 A-I23).
    pub(crate) fn publish_chain(&self) -> Option<Vec<String>> {
        ["charter.json", "charter.draft.json"]
            .iter()
            .filter_map(|name| {
                read_json::<awp_types::Charter>(&self.root.join(name)).ok().flatten()
            })
            .flat_map(|charter| charter.governance.approval_chains)
            .find(|chain| chain.action == ActionClass::Publish && chain.roles.len() > 1)
            .map(|chain| {
                chain
                    .roles
                    .iter()
                    .filter_map(|role| serde_json::to_value(role).ok())
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
    }

    async fn log_store(&self) -> Result<Arc<dyn AuditLogStore>> {
        std::fs::create_dir_all(self.data())
            .with_context(|| format!("cannot create {}", self.data().display()))?;
        let url = format!("sqlite://{}?mode=rwc", self.log_db().display());
        let store = adk_session::SqliteAuditLogStore::new(&url)
            .await
            .with_context(|| format!("cannot open the site log {}", self.log_db().display()))?;
        store.migrate().await.context("cannot prepare the site log")?;
        Ok(Arc::new(store))
    }

    /// The latest logged release, without opening the log for writing.
    pub(crate) async fn latest_release(&self) -> Result<Option<u64>> {
        if !self.log_db().exists() {
            return Ok(None);
        }
        let domain = self.config()?.domain;
        let size = self.log_store().await?.size(&format!("awp-site-log:{domain}")).await?;
        Ok((size > 0).then_some(size))
    }

    /// Files released beside the generated ones: `well-known/` under `/.well-known/` (the
    /// charter's public terms excepted, which the generator writes), `static/` at their own
    /// paths, and the Web Bot Auth key directory when the site has a
    /// key (spec 3.5 §7.3 "Outbound").
    pub(crate) fn documents(&self) -> Result<BTreeMap<String, Bytes>> {
        let mut documents = BTreeMap::new();
        collect(&self.well_known(), ".well-known", &mut documents)?;
        collect(&self.static_dir(), "", &mut documents)?;
        // The generator publishes the charter's public terms from public-terms.json.
        documents.remove(awp_types::v2::contract::CHARTER_PATH.trim_start_matches('/'));
        let key = self.key_file("web-bot-auth");
        if key.exists() {
            let key = KeyFile::read(&key)?.web_bot_auth()?;
            let directory = adk_auth::web_bot_auth::KeyDirectory::from_keys([&key]).to_json();
            documents.insert(
                ".well-known/http-message-signatures-directory".to_string(),
                Bytes::from(directory),
            );
        }
        Ok(documents)
    }

    /// The release key, generated on first use.
    pub(crate) fn release_key(&self) -> Result<(KeyFile, bool)> {
        let path = self.key_file("release");
        if path.exists() {
            return Ok((KeyFile::read(&path)?, false));
        }
        let key = KeyFile::generate(&format!("release-{}", chrono::Utc::now().format("%Y-%m")))?;
        key.write_new(&path)?;
        Ok((key, true))
    }

    /// The site's Web Bot Auth key, generated on first use: crawls and migration requests
    /// are signed with it, and releases publish its directory.
    pub(crate) fn web_bot_auth_key(&self) -> Result<KeyFile> {
        let path = self.key_file("web-bot-auth");
        if path.exists() {
            return KeyFile::read(&path);
        }
        let key = KeyFile::generate("web-bot-auth")?;
        key.write_new(&path)?;
        Ok(key)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("{} is not valid", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Adds every file under `dir` to `documents` at `<prefix>/<relative path>`.
fn collect(dir: &Path, prefix: &str, documents: &mut BTreeMap<String, Bytes>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)
            .with_context(|| format!("cannot read {}", current.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(dir).unwrap_or(&path);
            if !relative.components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            let relative = relative.to_string_lossy().replace('\\', "/");
            let key = match prefix {
                "" => relative,
                prefix => format!("{prefix}/{relative}"),
            };
            let bytes =
                std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
            documents.insert(key, Bytes::from(bytes));
        }
    }
    Ok(())
}

/// An opened site: its log, host, content and pipeline.
pub(crate) struct Site {
    pub(crate) paths: SitePaths,
    pub(crate) config: SiteConfig,
    pub(crate) host: Arc<DirectoryHost>,
    pub(crate) log: Arc<SiteLog>,
    pub(crate) store: Arc<FsContentStore>,
    pub(crate) history: Arc<FileContentHistory>,
    pub(crate) pipeline: Arc<PublishingPipeline>,
    /// The release the host serves, when there is one.
    pub(crate) previous: Option<PreviousRelease>,
    /// The release key's id, and whether it was generated now.
    pub(crate) release_key: (String, bool),
    /// The project's audit trail, `data/audit.jsonl` (spec 3.5 §9.3).
    pub(crate) trail: adk_core::audit_log::AuditTrail,
}

impl Site {
    /// Opens the site at `paths`, continuing after the release its host serves.
    pub(crate) async fn open(paths: SitePaths, approvals: Arc<dyn ApprovalGate>) -> Result<Self> {
        let config = paths.config()?;
        let (key, generated) = paths.release_key()?;
        let log = Arc::new(
            SiteLog::open(
                paths.log_store().await?,
                &config.domain,
                Arc::new(key.release_signer(&config.domain)?),
            )
            .await?,
        );
        let host = Arc::new(DirectoryHost::new(paths.host_dir()));
        let previous = previous_release(host.as_ref(), log.size().await).await?;
        let mut base = SiteProject::load(paths.root(), chrono::Utc::now()).await?;
        base.documents = paths.documents()?;
        let store = Arc::new(FsContentStore::new(paths.root()));
        let history = Arc::new(FileContentHistory::new(paths.data().join("history")));
        let mut builder = PublishingPipeline::builder(
            base,
            store.clone(),
            Arc::new(BuiltinGenerator),
            host.clone(),
            log.clone(),
            paths.work(),
        )
        .history(history.clone())
        .approvals(approvals);
        // The project's audit trail, which a served site and `awp keys` write too (spec 3.5
        // §9.3): drafted, approved and published content, built, signed and published releases.
        let recorder =
            adk_core::audit_log::FileAuditRecorder::open(paths.data().join("audit.jsonl"))
                .map_err(|error| anyhow::anyhow!("{error}"))?
                .with_log_id(&config.domain);
        let trail = adk_core::audit_log::AuditTrail::new(Arc::new(recorder));
        builder = builder.audit(trail.clone());
        if let Some(previous) = &previous {
            builder = builder.previous(previous.clone());
        }
        Ok(Self {
            paths,
            config,
            host,
            log,
            store,
            history,
            pipeline: Arc::new(builder.build()),
            previous,
            release_key: (key.kid().to_string(), generated),
            trail,
        })
    }

    /// Records a release made outside the pipeline — `site.release.built` and
    /// `site.release.signed`, then `site.release.published` once `uploaded` files are on the
    /// host — as the pipeline records its own (spec 3.5 §9.3).
    pub(crate) async fn record_release(
        &self,
        staged: &adk_site::release::StagedRelease,
        principal: &str,
        uploaded: Option<(usize, usize)>,
    ) {
        use adk_site::pipeline::audit::{
            ReleaseContext, record_release_logged, record_release_published,
        };
        let context = ReleaseContext {
            site: &self.config.domain,
            charter_version: self.config.charter_version,
            principal,
        };
        match uploaded {
            None => {
                let key = self.log.verifier_key().await;
                record_release_logged(Some(&self.trail), context, staged, &key).await;
            }
            Some((files, items)) => {
                record_release_published(Some(&self.trail), context, staged, files, items).await;
            }
        }
    }

    /// Notes for the person about the release key and the AWP documents.
    pub(crate) fn key_notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        let (kid, generated) = &self.release_key;
        if *generated {
            notes.push(format!(
                "generated the release key `{kid}` at {}; delegate it with `adk-rust awp keys \
                 delegate --root-key <root key file> --scope release --key {}`",
                self.paths.key_file("release").display(),
                self.paths.key_file("release").display()
            ));
        }
        let keys: Option<awp_types::KeySet> = read_json(&self.paths.keys_json()).ok().flatten();
        match keys {
            None => notes.push(
                "no AWP key set yet: `awp verify` cannot check this site until its root signs \
                 one (`adk-rust awp keys rotate`)"
                    .to_string(),
            ),
            Some(keys) => {
                let delegated = KeyFile::read(&self.paths.key_file("release")).is_ok_and(|key| {
                    keys.delegations.iter().any(|d| {
                        d.public_key == key.public_key()
                            && d.allows(awp_types::KeyScope::Release)
                            && d.is_valid_at(chrono::Utc::now())
                    })
                });
                if !delegated {
                    notes.push(format!(
                        "the release key `{kid}` has no live delegation in keys.json: \
                         `awp verify` fails at checkpoint.signature until the root delegates it"
                    ));
                }
            }
        }
        notes
    }
}

/// The release the host serves for the log's latest entry, rebuilt from its manifests.
async fn previous_release(host: &dyn SiteHost, size: u64) -> Result<Option<PreviousRelease>> {
    if size == 0 {
        return Ok(None);
    }
    let fetch = |seq: u64| async move {
        let bytes = host.fetch(&manifest_path(seq)).await?.with_context(|| {
            format!(
                "the site log holds release {seq} but the host does not serve its manifest; \
                 publish to the host the log was written for (--host-dir)"
            )
        })?;
        serde_json::from_slice::<ReleaseManifest>(&bytes)
            .with_context(|| format!("release {seq}'s manifest is not valid"))
    };
    let latest = fetch(size).await?;
    let mut chain = vec![latest.clone()];
    while let Some(base) = chain.last().and_then(|manifest| manifest.base) {
        if chain.len() > 10_000 {
            bail!("release {size}'s delta chain does not end in a snapshot");
        }
        chain.push(fetch(base).await?);
    }
    let mut files = BTreeMap::new();
    for manifest in chain.iter().rev() {
        files = manifest.apply_to(&files);
    }
    let snapshot = chain.last().map_or((latest.seq, latest.created_at), |m| (m.seq, m.created_at));
    let (digest, _) = manifest_digest(&latest)?;
    Ok(Some(PreviousRelease { manifest: latest, digest, files, last_snapshot: snapshot }))
}
