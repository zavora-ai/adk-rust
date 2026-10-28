//! A served site's project on disk (spec 3.5 §7.8, §7.17): the layout `adk-rust site` uses,
//! so the CLI and a running site share one content repository, log, host and journal.
//!
//! | Path | Holds |
//! |---|---|
//! | `site.toml`, `content/`, `theme/`, `public-terms.json`, `capabilities.json` | The content repository |
//! | `well-known/`, `static/` | Files released under `/.well-known/` (the root-signed contract and `keys.json`) and at their own paths |
//! | `feeds.json`, `calendar.json`, `payments.json` | The owner's feeds with their bounds, bookable slots, and payment links |
//! | `caldav.json` | The owner's CalDAV calendar of open slots, in place of `calendar.json` ([`CalDavConfig`]) |
//! | `approvals.json` | The owner's amount thresholds: orders, bookings and payments above them wait for the owner (spec 3.5 §7.4) |
//! | `data/site-log.db`, `data/host/`, `data/history/`, `data/work/` | The site log, the `DirectoryHost`, every saved version, builds |
//! | `data/keys/` | Delegated keys as private JWK files: `release.jwk` signs the log, `edge.jwk` the card and receipts, `web-bot-auth.jwk` crawls; `subject.key` keys consent sessions |
//! | `data/kernel.db`, `data/edge.db` | The kernel's durable stores and the edge's |
//! | `data/updates/` | The update engine's journal: `batches.jsonl` and `feeds.json`, read by `site batches` and `site feeds` |
//! | `data/experiments/` | Experiment records |
//!
//! The root key never lives here: the owner keeps it and delegates the keys above with
//! `adk-rust awp keys delegate` (A-I5).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use adk_core::{AdkError, Result};
use adk_site::feed::FeedMapping;
use adk_site::host::SiteHost;
use adk_site::release::{PreviousRelease, manifest_digest, manifest_path};
use awp_types::{FeedBounds, ReleaseManifest};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::edge::handlers::Slot;

fn io(path: &Path, error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("{}: {error}", path.display()))
}

/// The paths of a site project.
///
/// # Example
///
/// ```rust
/// use adk_website::deploy::project::ProjectPaths;
///
/// let paths = ProjectPaths::new("/srv/bakery");
/// assert!(paths.journal().ends_with("data/updates"));
/// assert!(paths.key_file("release").ends_with("data/keys/release.jwk"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    root: PathBuf,
}

impl ProjectPaths {
    /// The project at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The project's root: the content repository.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `data/`.
    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    /// The private key file `data/keys/{name}.jwk`.
    pub fn key_file(&self, name: &str) -> PathBuf {
        self.data().join("keys").join(format!("{name}.jwk"))
    }

    /// The `DirectoryHost`'s directory.
    pub fn host_dir(&self) -> PathBuf {
        self.data().join("host")
    }

    /// Builds.
    pub fn work(&self) -> PathBuf {
        self.data().join("work")
    }

    /// Every saved version of every item.
    pub fn history(&self) -> PathBuf {
        self.data().join("history")
    }

    /// The update engine's journal.
    pub fn journal(&self) -> PathBuf {
        self.data().join("updates")
    }

    /// Experiment records.
    pub fn experiments(&self) -> PathBuf {
        self.data().join("experiments")
    }

    /// The SQLite database `data/{name}.db`.
    pub fn database(&self, name: &str) -> String {
        format!("sqlite://{}?mode=rwc", self.data().join(format!("{name}.db")).display())
    }

    /// The kernel database the site runs on (spec 3.5 §7.22, A-I22): the agency's
    /// `data/kernel.db` when an `agency.toml` beside the project lists it, as
    /// `serve_agency` serves it, else the project's own.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::deploy::ProjectPaths;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("agency.toml"), "[[site]]\ndomain = \"bakery.example\"\ndir = \"bakery\"\n").unwrap();
    /// let site = ProjectPaths::new(dir.path().join("bakery"));
    /// assert_eq!(site.kernel_database(), ProjectPaths::new(dir.path()).database("kernel"));
    /// let alone = ProjectPaths::new(dir.path().join("notes"));
    /// assert_eq!(alone.kernel_database(), alone.database("kernel"));
    /// ```
    pub fn kernel_database(&self) -> String {
        let listed = self.root.parent().and_then(|agency| {
            let text = std::fs::read_to_string(agency.join("agency.toml")).ok()?;
            let file: super::serve::AgencyFile = toml::from_str(&text).ok()?;
            let name = self.root.file_name()?;
            file.sites
                .iter()
                .any(|site| site.dir.as_os_str() == name || agency.join(&site.dir) == self.root)
                .then(|| ProjectPaths::new(agency))
        });
        listed.map_or_else(|| self.database("kernel"), |agency| agency.database("kernel"))
    }

    /// Files released under `/.well-known/`.
    pub fn well_known(&self) -> PathBuf {
        self.root.join("well-known")
    }

    /// Files released at their own paths.
    pub fn static_dir(&self) -> PathBuf {
        self.root.join("static")
    }
}

/// An Ed25519 key from a private JWK file `{"kty":"OKP","crv":"Ed25519","kid","x","d"}`,
/// the format `adk-rust awp keys` writes.
pub struct KeyFile {
    /// The key id.
    pub kid: String,
    seed: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyFile").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl KeyFile {
    /// The key at `path`, made with `kid` on first use, readable by its owner only.
    ///
    /// # Errors
    ///
    /// A file that is not an Ed25519 private JWK, or the file system's error.
    pub fn load_or_create(path: &Path, kid: &str) -> Result<Self> {
        if path.exists() {
            return Self::read(path);
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::fill(seed.as_mut()).map_err(|e| io(path, e))?;
        let key = Self { kid: kid.to_string(), seed };
        key.write_new(path)?;
        Ok(key)
    }

    /// Reads the key at `path`.
    ///
    /// # Errors
    ///
    /// A file that is not an Ed25519 private JWK carrying `kid` and `d`.
    pub fn read(path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct Jwk {
            kty: String,
            crv: String,
            kid: Option<String>,
            d: Option<String>,
        }
        let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| io(path, e))?);
        let jwk: Jwk = serde_json::from_str(&text)
            .map_err(|_| io(path, "not a JSON Web Key; a key file is an Ed25519 private JWK"))?;
        if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
            return Err(io(path, "not an Ed25519 key (kty OKP, crv Ed25519)"));
        }
        let d = Zeroizing::new(jwk.d.unwrap_or_default());
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD.decode(d.as_bytes()).map_err(|_| io(path, "`d` is not base64url"))?,
        );
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| io(path, "`d` is not a 32-byte Ed25519 seed"))?;
        let kid = jwk.kid.filter(|kid| !kid.is_empty()).ok_or_else(|| io(path, "no `kid`"))?;
        Ok(Self { kid, seed: Zeroizing::new(seed) })
    }

    /// The key's seed.
    pub fn seed(&self) -> [u8; 32] {
        *self.seed
    }

    fn write_new(&self, path: &Path) -> Result<()> {
        use std::io::Write as _;
        let key = adk_auth::web_bot_auth::Ed25519SigningKey::from_seed(*self.seed)
            .map_err(|e| io(path, e))?;
        let text = Zeroizing::new(format!(
            "{{\"kty\":\"OKP\",\"crv\":\"Ed25519\",\"kid\":{},\"x\":\"{}\",\"d\":\"{}\"}}\n",
            serde_json::Value::String(self.kid.clone()),
            URL_SAFE_NO_PAD.encode(key.public_key()),
            Zeroizing::new(URL_SAFE_NO_PAD.encode(self.seed.as_ref())).as_str()
        ));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(path).map_err(|e| io(path, e))?;
        file.write_all(text.as_bytes()).map_err(|e| io(path, e))
    }
}

/// The files released beside the generated ones (spec 3.5 §7.2): `well-known/` under
/// `/.well-known/` (the public terms excepted, which the generator writes) and `static/` at
/// their own paths.
///
/// # Errors
///
/// An unreadable file.
pub fn documents(paths: &ProjectPaths) -> Result<BTreeMap<String, Bytes>> {
    let mut documents = BTreeMap::new();
    collect(&paths.well_known(), ".well-known", &mut documents)?;
    collect(&paths.static_dir(), "", &mut documents)?;
    documents.remove(awp_types::v2::contract::CHARTER_PATH.trim_start_matches('/'));
    Ok(documents)
}

fn collect(dir: &Path, prefix: &str, documents: &mut BTreeMap<String, Bytes>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).map_err(|e| io(&current, e))? {
            let path = entry.map_err(|e| io(&current, e))?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path.strip_prefix(dir).unwrap_or(&path);
            if !relative.components().all(|c| matches!(c, Component::Normal(_))) {
                continue;
            }
            let relative = relative.to_string_lossy().replace('\\', "/");
            let key = if prefix.is_empty() { relative } else { format!("{prefix}/{relative}") };
            let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
            documents.insert(key, Bytes::from(bytes));
        }
    }
    Ok(())
}

/// The release `host` serves for a log of `size` entries, so a restarted site continues
/// after it.
///
/// # Errors
///
/// A logged release whose manifest the host does not serve.
pub async fn previous_release(host: &dyn SiteHost, size: u64) -> Result<Option<PreviousRelease>> {
    if size == 0 {
        return Ok(None);
    }
    let fetch = |seq: u64| async move {
        let bytes = host
            .fetch(&manifest_path(seq))
            .await
            .map_err(|e| AdkError::config(e.to_string()))?
            .ok_or_else(|| {
                AdkError::config(format!(
                    "the site log holds release {seq} but the host does not serve its manifest"
                ))
            })?;
        serde_json::from_slice::<ReleaseManifest>(&bytes)
            .map_err(|e| AdkError::config(format!("release {seq}'s manifest: {e}")))
    };
    let latest = fetch(size).await?;
    let mut chain = vec![latest.clone()];
    while let Some(base) = chain.last().and_then(|manifest| manifest.base) {
        if chain.len() > 10_000 {
            return Err(AdkError::config("the delta chain does not end in a snapshot"));
        }
        chain.push(fetch(base).await?);
    }
    let mut files = BTreeMap::new();
    for manifest in chain.iter().rev() {
        files = manifest.apply_to(&files);
    }
    let snapshot = chain.last().map_or((latest.seq, latest.created_at), |m| (m.seq, m.created_at));
    let (digest, _) = manifest_digest(&latest).map_err(|e| AdkError::config(e.to_string()))?;
    Ok(Some(PreviousRelease { manifest: latest, digest, files, last_snapshot: snapshot }))
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| io(path, e)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(path, error)),
    }
}

/// One feed the owner configured in `feeds.json` (A-I24, A-I27).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfiguredFeed {
    /// The mapping.
    pub mapping: FeedMapping,
    /// Its bounds.
    pub bounds: FeedBounds,
}

/// The feeds in `feeds.json`; none without the file.
///
/// # Errors
///
/// A file that is not a list of `{mapping, bounds}`.
pub fn feeds(paths: &ProjectPaths) -> Result<Vec<ConfiguredFeed>> {
    Ok(read_json(&paths.root().join("feeds.json"))?.unwrap_or_default())
}

/// The bookable slots in `calendar.json`; `None` without the file.
///
/// # Errors
///
/// A file that is not a list of slots.
pub fn slots(paths: &ProjectPaths) -> Result<Option<Vec<Slot>>> {
    read_json(&paths.root().join("calendar.json"))
}

/// The owner's CalDAV calendar in `caldav.json` (spec 3.5 §7.5 "Commerce"): where the open
/// slots are, where bookings go, and how to sign in. Secrets are references, `env:VAR` or
/// `file:PATH` (relative to the project), never the secret itself (A-I19).
///
/// ```json
/// {
///   "slots": "https://dav.example.com/calendars/juniper/open-slots/",
///   "bookings": "https://dav.example.com/calendars/juniper/personal/",
///   "username": "juniper",
///   "password": "env:JUNIPER_CALDAV_PASSWORD",
///   "price": 1500,
///   "timeZone": "Africa/Nairobi"
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CalDavConfig {
    /// The collection of open slots.
    pub slots: String,
    /// The collection bookings are written to; the slots' collection when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookings: Option<String>,
    /// The account, for HTTP Basic with `password`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The app password, as `env:VAR` or `file:PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// An OAuth bearer token in place of a password, as `env:VAR` or `file:PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The price of a slot with none of its own, in minor units of the site's currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<u64>,
    /// The IANA zone times without one are read in; UTC when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

/// A secret from `env:VAR` or `file:PATH`, a relative path read from the project root;
/// `file` names the configuration file for the error.
pub(crate) fn secret_in(
    paths: &ProjectPaths,
    file: &str,
    source: &str,
) -> Result<adk_core::CredentialSecret> {
    let text = if let Some(var) = source.strip_prefix("env:") {
        std::env::var(var)
            .map_err(|_| AdkError::config(format!("{file}: the environment holds no `{var}`")))?
    } else if let Some(path) = source.strip_prefix("file:") {
        let path = paths.root().join(path);
        std::fs::read_to_string(&path).map_err(|e| io(&path, e))?
    } else {
        return Err(AdkError::config(format!(
            "{file}: write each secret as `env:VAR` or `file:PATH`, never in the file itself"
        )));
    };
    Ok(adk_core::CredentialSecret::new(text.trim()))
}

fn secret_reference(paths: &ProjectPaths, source: &str) -> Result<adk_core::CredentialSecret> {
    secret_in(paths, "caldav.json", source)
}

/// The owner's CalDAV calendar from `caldav.json`; `None` without the file.
///
/// # Errors
///
/// A file that is not a [`CalDavConfig`], a secret written in it or not found, an unknown
/// time zone, or an address that is not `https` (`http` on loopback only).
pub fn caldav(paths: &ProjectPaths) -> Result<Option<crate::edge::caldav::CalDavCalendar>> {
    use crate::edge::caldav::{CalDavAuth, CalDavCalendar};

    let Some(config) = read_json::<CalDavConfig>(&paths.root().join("caldav.json"))? else {
        return Ok(None);
    };
    let auth = match (&config.username, &config.password, &config.token) {
        (Some(username), Some(password), None) => CalDavAuth::Basic {
            username: username.clone(),
            password: secret_reference(paths, password)?,
        },
        (None, None, Some(token)) => CalDavAuth::Bearer(secret_reference(paths, token)?),
        (None, None, None) => CalDavAuth::None,
        _ => {
            return Err(AdkError::config(
                "caldav.json: sign in with `username` and `password`, or with `token`",
            ));
        }
    };
    let mut calendar = CalDavCalendar::new(&config.slots, auth)?;
    if let Some(bookings) = &config.bookings {
        calendar = calendar.with_bookings(bookings)?;
    }
    if let Some(price) = config.price {
        calendar = calendar.with_price(price);
    }
    if let Some(zone) = &config.time_zone {
        let zone = zone.parse::<chrono_tz::Tz>().map_err(|_| {
            AdkError::config(format!("caldav.json: `{zone}` is not an IANA time zone"))
        })?;
        calendar = calendar.with_time_zone(zone);
    }
    Ok(Some(calendar))
}

/// One way to pay the owner, from `payments.json`: a rail's name, what the buyer is told,
/// and the owner's own payment page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentLink {
    /// The rail, as `pay_order`'s `rail` names it, such as `mpesa` or `card`.
    pub rail: String,
    /// What the buyer is told, such as "Pay till 123456 with the order id as reference".
    pub instructions: String,
    /// The owner's payment page, when the rail has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The payment links in `payments.json`; `None` without the file.
///
/// # Errors
///
/// A file that is not a list of payment links.
pub fn payment_links(paths: &ProjectPaths) -> Result<Option<Vec<PaymentLink>>> {
    read_json(&paths.root().join("payments.json"))
}

/// The owner's amount thresholds in `approvals.json`, such as
/// `{"place_order": 20000, "book": 15000}` in minor units of the site's currency: calls above
/// them answer `202` and wait for the owner (spec 3.5 §7.4, S9). None without the file.
///
/// # Errors
///
/// A file that is not a map of capability names to amounts, or a threshold on a capability
/// whose calls carry no amount.
pub fn approval_thresholds(paths: &ProjectPaths) -> Result<crate::packs::ApprovalThresholds> {
    let thresholds: crate::packs::ApprovalThresholds =
        read_json(&paths.root().join("approvals.json"))?.unwrap_or_default();
    thresholds.validate()?;
    Ok(thresholds)
}
