//! The packs a site loads (spec 3.5 §7.23): the built-in bundles, and the bundles installed
//! in the project's `packs/` directory, each verified against the site's trust list whenever
//! it is installed or loaded.
//!
//! | Path | Holds |
//! |---|---|
//! | `packs/trust.json` | The [`TrustList`] of publisher keys |
//! | `packs/{name}.pack.json` | Each installed bundle, byte for byte as its publisher signed it |
//! | `packs/drivers/{name}/{id}.toml` | The installed packs' driver manifests, which the owner mounts with `adk-rust drivers mount` |
//!
//! A pack takes effect when the site next starts: its capabilities on the edge, its playbooks
//! in its agents' workspaces under `.skills/packs/{name}/`. A file that no longer verifies —
//! tampered, or its publisher no longer trusted — is not loaded and is reported in
//! [`PackSet::refused`].

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
use async_trait::async_trait;
use awp_types::CapabilityV2;
use serde_json::Value;

use super::Pack;
use super::builtin::is_builtin_test_key;
use super::bundle::{HandlerConfig, PackBundle, PackError, VerifiedPack};
use super::trust::{TrustList, TrustedKey};
use crate::kinds::KindTemplate;
use crate::team::AgentRole;

/// The extension of an installed bundle's file.
pub const BUNDLE_EXTENSION: &str = ".pack.json";

fn io(path: &Path, error: impl std::fmt::Display) -> PackError {
    PackError::Io(format!("{}: {error}", path.display()))
}

/// Writes `bytes` to `path` through a temporary file beside it, so a reader sees the old
/// file or the new one.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PackError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = parent.join(format!(".{name}.tmp"));
    std::fs::write(&temporary, bytes).map_err(|error| io(&temporary, error))?;
    std::fs::rename(&temporary, path).map_err(|error| io(path, error))
}

/// Where a loaded pack comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackOrigin {
    /// Compiled into adk-website.
    BuiltIn,
    /// Installed in the project.
    Installed {
        /// Its bundle file.
        file: PathBuf,
    },
}

/// A pack the site loads: its verified bundle and where it comes from.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPack {
    /// The verified bundle.
    pub pack: VerifiedPack,
    /// Where it comes from.
    pub origin: PackOrigin,
}

impl LoadedPack {
    /// The pack's name.
    pub fn name(&self) -> &str {
        &self.pack.bundle.name
    }

    /// The bundle.
    pub fn bundle(&self) -> &PackBundle {
        &self.pack.bundle
    }

    /// Whether it carries a built-in pack's name: the built-in pack, or an upgrade of it.
    pub fn is_builtin_name(&self) -> bool {
        Pack::ALL.iter().any(|pack| pack.as_str() == self.name())
    }
}

/// What installing a pack does to a site's packs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// A pack the site does not have.
    New,
    /// A newer version of a pack the site has.
    Upgrade {
        /// The version it replaces.
        from: String,
    },
    /// The very version and content the site has.
    Unchanged,
}

/// A bundle file the site does not load, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedPack {
    /// The file.
    pub file: PathBuf,
    /// Why.
    pub error: PackError,
}

/// The packs a site loads: built-in first, then the installed ones by name (spec 3.5
/// §7.23). Names, capabilities, playbooks and drivers are unique across the set.
///
/// # Example
///
/// ```rust
/// use adk_website::kinds::template;
/// use adk_website::packs::installed::PackSet;
/// use awp_types::SiteKind;
///
/// let packs = PackSet::builtin().unwrap();
/// assert_eq!(packs.packs().len(), 6);
/// let served: Vec<String> =
///     packs.served(&template(SiteKind::Business)).into_iter().map(|(c, _)| c.name).collect();
/// assert!(served.contains(&"get_quote".to_string()));
/// ```
#[derive(Debug, Clone, Default)]
pub struct PackSet {
    packs: Vec<Arc<LoadedPack>>,
    refused: Vec<RefusedPack>,
}

impl PackSet {
    /// The built-in packs, each verified against [`builtin_trust`](super::builtin::builtin_trust).
    ///
    /// # Errors
    ///
    /// The [`PackError`] of a shipped bundle that does not verify, which the crate's tests
    /// rule out.
    pub fn builtin() -> Result<Self, PackError> {
        static BUILTIN: OnceLock<Result<PackSet, PackError>> = OnceLock::new();
        BUILTIN
            .get_or_init(|| {
                let mut set = Self::default();
                for pack in Pack::ALL {
                    set.packs.push(Arc::new(LoadedPack {
                        pack: pack.bundle()?,
                        origin: PackOrigin::BuiltIn,
                    }));
                }
                Ok(set)
            })
            .clone()
    }

    /// The packs, built-in first.
    pub fn packs(&self) -> &[Arc<LoadedPack>] {
        &self.packs
    }

    /// The pack named `name`.
    pub fn get(&self, name: &str) -> Option<&LoadedPack> {
        self.packs.iter().find(|pack| pack.name() == name).map(AsRef::as_ref)
    }

    /// The installed bundle files the site does not load, and why.
    pub fn refused(&self) -> &[RefusedPack] {
        &self.refused
    }

    /// What installing `candidate` would do (spec 3.5 §7.23): a new pack, an upgrade of the
    /// pack of its name, or nothing for the very version and content the set holds.
    ///
    /// # Errors
    ///
    /// [`PackError::Downgrade`] for a version older than the one held;
    /// [`PackError::Republished`] for the held version with other content;
    /// [`PackError::Conflict`] for a name another publisher's pack holds, or a capability,
    /// playbook or driver of another pack.
    pub fn admit(&self, candidate: &VerifiedPack) -> Result<Admission, PackError> {
        let bundle = &candidate.bundle;
        let mut admission = Admission::New;
        if let Some(held) = self.get(&bundle.name) {
            if held.bundle().publisher.id != bundle.publisher.id {
                return Err(PackError::Conflict(format!(
                    "pack `{}` is published by `{}`, and a pack of `{}` cannot take its name",
                    bundle.name,
                    held.bundle().publisher.id,
                    bundle.publisher.id
                )));
            }
            admission = match candidate.version().cmp(&held.pack.version()) {
                Ordering::Less => {
                    return Err(PackError::Downgrade {
                        name: bundle.name.clone(),
                        installed: held.bundle().version.clone(),
                        offered: bundle.version.clone(),
                    });
                }
                Ordering::Equal if held.pack.digest == candidate.digest => Admission::Unchanged,
                Ordering::Equal => {
                    return Err(PackError::Republished {
                        name: bundle.name.clone(),
                        version: bundle.version.clone(),
                    });
                }
                Ordering::Greater => Admission::Upgrade { from: held.bundle().version.clone() },
            };
        }
        for other in self.packs.iter().filter(|pack| pack.name() != bundle.name) {
            let held = other.bundle();
            let taken = |what: &str, name: &str| {
                Err(PackError::Conflict(format!(
                    "{what} `{name}` belongs to pack `{}`",
                    other.name()
                )))
            };
            if let Some(capability) = bundle
                .capabilities
                .iter()
                .find(|c| held.capabilities.iter().any(|h| h.name == c.name))
            {
                return taken("capability", &capability.name);
            }
            if let Some(playbook) =
                bundle.playbooks.iter().find(|p| held.playbooks.iter().any(|h| h.id == p.id))
            {
                return taken("playbook", &playbook.id);
            }
            if let Some(driver) =
                bundle.drivers.iter().find(|d| held.drivers.iter().any(|h| h.id == d.id))
            {
                return taken("driver", &driver.id);
            }
        }
        Ok(admission)
    }

    /// The set with `pack` admitted: added, or replacing the pack of its name.
    ///
    /// # Errors
    ///
    /// As [`admit`](Self::admit).
    pub fn with(mut self, pack: LoadedPack) -> Result<(Self, Admission), PackError> {
        let admission = self.admit(&pack.pack)?;
        match self.packs.iter().position(|held| held.name() == pack.name()) {
            Some(index) => self.packs[index] = Arc::new(pack),
            None => self.packs.push(Arc::new(pack)),
        }
        Ok((self, admission))
    }

    /// The declaration of capability `name` and its handler, from the pack that declares it.
    pub fn declaration(&self, name: &str) -> Option<(CapabilityV2, HandlerConfig)> {
        self.packs.iter().find_map(|pack| {
            let bundle = pack.bundle();
            let declaration = bundle.capabilities.iter().find(|c| c.name == name)?;
            Some((declaration.clone(), *bundle.handlers.get(name)?))
        })
    }

    /// The capabilities a site of `template`'s kind serves, each with its handler (spec 3.5
    /// §7.16.1, §7.23): the kind's capabilities, from the packs that declare them, then every
    /// capability of each installed pack that is not a built-in pack's. A kind's capability
    /// no loaded pack declares is logged and not served.
    pub fn served(&self, template: &KindTemplate) -> Vec<(CapabilityV2, HandlerConfig)> {
        let mut names = BTreeSet::new();
        let mut served = Vec::new();
        for name in &template.capabilities {
            match self.declaration(name) {
                Some(found) => {
                    names.insert(name.to_string());
                    served.push(found);
                }
                None => tracing::warn!(
                    capability = name,
                    "no loaded pack declares this capability; it is not served"
                ),
            }
        }
        for pack in self.packs.iter().filter(|pack| !pack.is_builtin_name()) {
            let bundle = pack.bundle();
            for declaration in &bundle.capabilities {
                if let Some(handler) = bundle.handlers.get(&declaration.name)
                    && names.insert(declaration.name.clone())
                {
                    served.push((declaration.clone(), *handler));
                }
            }
        }
        served
    }

    /// Writes the packs' playbooks into each agent's workspace under `root`, as
    /// `{root}/{agent}/.skills/packs/{pack}/{id}.md`, first removing what an earlier start
    /// wrote there, so a removed pack's playbooks go with it (spec 3.5 §7.15, §7.23). Returns
    /// the files written.
    ///
    /// # Errors
    ///
    /// The file system's error.
    pub async fn install_playbooks(&self, root: &Path) -> adk_core::Result<Vec<PathBuf>> {
        let failed = |path: &Path, error: std::io::Error| {
            adk_core::AdkError::config(format!("{}: {error}", path.display()))
        };
        let mut written = Vec::new();
        for role in AgentRole::ALL {
            let dir = root.join(role.as_str()).join(".skills").join("packs");
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(failed(&dir, error)),
            }
            for pack in &self.packs {
                for playbook in pack.bundle().playbooks.iter().filter(|p| p.agents.contains(&role))
                {
                    let folder = dir.join(pack.name());
                    tokio::fs::create_dir_all(&folder).await.map_err(|e| failed(&folder, e))?;
                    let path = folder.join(format!("{}.md", playbook.id));
                    tokio::fs::write(&path, &playbook.text).await.map_err(|e| failed(&path, e))?;
                    written.push(path);
                }
            }
        }
        Ok(written)
    }
}

/// A pack's capability served by a built-in handler that dispatches on a built-in
/// capability's name, such as the orders handler on `place_order` (spec 3.5 §7.23): each call
/// reaches the handler as that capability, and amounts are read as the handler reads them.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
/// use adk_website::packs::installed::Dispatched;
/// use serde_json::{Value, json};
///
/// struct Named;
///
/// #[async_trait::async_trait]
/// impl CapabilityHandler for Named {
///     async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
///         Ok(json!(call.capability))
///     }
/// }
///
/// let handler = Dispatched::new("place_order", Arc::new(Named));
/// # let _ = handler;
/// ```
pub struct Dispatched {
    capability: &'static str,
    inner: Arc<dyn CapabilityHandler>,
}

impl Dispatched {
    /// `inner`, reached as the built-in `capability`.
    pub fn new(capability: &'static str, inner: Arc<dyn CapabilityHandler>) -> Self {
        Self { capability, inner }
    }
}

#[async_trait]
impl CapabilityHandler for Dispatched {
    async fn handle(&self, mut call: CapabilityCall) -> Result<Value, CapabilityError> {
        call.capability = self.capability.to_string();
        self.inner.handle(call).await
    }

    fn call_amount(&self, input: &Value) -> Option<u64> {
        self.inner.call_amount(input)
    }

    async fn amount(&self, input: &Value) -> Option<u64> {
        self.inner.amount(input).await
    }
}

/// What installing a pack did, or would do.
#[derive(Debug, Clone, PartialEq)]
pub struct Installation {
    /// The verified pack.
    pub pack: VerifiedPack,
    /// New, upgrade or unchanged.
    pub admission: Admission,
    /// Its driver manifests' files, for `adk-rust drivers mount`.
    pub drivers: Vec<PathBuf>,
}

/// A project's `packs/` directory (spec 3.5 §7.23): its trust list, installed bundles and
/// their driver manifests.
///
/// # Example
///
/// ```rust
/// use adk_website::packs::installed::PackDir;
///
/// let project = tempfile::tempdir().unwrap();
/// let packs = PackDir::new(project.path());
/// assert!(packs.trust().unwrap().keys.is_empty(), "nothing is trusted until the owner says so");
/// let set = packs.load().unwrap();
/// assert_eq!(set.packs().len(), 6, "the built-in packs");
/// assert!(packs.remove("booking").is_err(), "a built-in pack is not removed");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackDir {
    dir: PathBuf,
}

impl PackDir {
    /// The `packs/` directory of the project at `project`.
    pub fn new(project: impl AsRef<Path>) -> Self {
        Self { dir: project.as_ref().join("packs") }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `packs/trust.json`.
    pub fn trust_file(&self) -> PathBuf {
        self.dir.join("trust.json")
    }

    /// `packs/{name}.pack.json`.
    pub fn bundle_file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}{BUNDLE_EXTENSION}"))
    }

    /// `packs/drivers/{name}/`.
    pub fn drivers_dir(&self, name: &str) -> PathBuf {
        self.dir.join("drivers").join(name)
    }

    /// The trust list; empty without `packs/trust.json`.
    ///
    /// # Errors
    ///
    /// [`PackError::Io`] for a file that cannot be read or is not a trust list.
    pub fn trust(&self) -> Result<TrustList, PackError> {
        TrustList::read(&self.trust_file())
    }

    /// Trusts `key` for installing its publisher's packs; `false` when it was trusted
    /// already.
    ///
    /// # Errors
    ///
    /// [`PackError::Invalid`] for the built-in packs' test key, whose seed is public, or a
    /// publisher id that is not valid; [`PackError::Io`] when the list cannot be written.
    pub fn trust_key(&self, key: TrustedKey) -> Result<bool, PackError> {
        if is_builtin_test_key(&key.public_key) {
            return Err(PackError::Invalid(
                "that is the built-in packs' test key, whose seed is public in adk-website's \
                 source; anyone can sign with it, so no site trusts it"
                    .into(),
            ));
        }
        if !super::bundle::is_valid_publisher_id(&key.publisher) || key.kid.trim().is_empty() {
            return Err(PackError::Invalid(format!(
                "publisher `{}` with key id `{}`: a publisher id is lowercase letters, digits, \
                 `.` and `-`, and a key needs an id",
                key.publisher, key.kid
            )));
        }
        adk_awp::signing::jws::public_key_bytes(&key.public_key).map_err(|error| {
            PackError::Invalid(format!("the key is not an Ed25519 public key: {error}"))
        })?;
        let mut trust = self.trust()?;
        let added = trust.add(key);
        if added {
            trust.write(&self.trust_file())?;
        }
        Ok(added)
    }

    /// The packs the site loads: the built-in ones, then each installed bundle that verifies
    /// against the trust list and fits beside the others, in name order; the rest are in
    /// [`PackSet::refused`].
    ///
    /// # Errors
    ///
    /// [`PackError::Io`] when the directory cannot be read; the built-in packs' error when
    /// one does not verify.
    pub fn load(&self) -> Result<PackSet, PackError> {
        let mut set = PackSet::builtin()?;
        let files = self.bundle_files()?;
        if files.is_empty() {
            return Ok(set);
        }
        let trust = match self.trust() {
            Ok(trust) => trust,
            Err(error) => {
                set.refused.extend(
                    files.into_iter().map(|file| RefusedPack { file, error: error.clone() }),
                );
                return Ok(set);
            }
        };
        for file in files {
            match self.load_file(&file, &trust) {
                Ok(pack) => {
                    let pack =
                        LoadedPack { pack, origin: PackOrigin::Installed { file: file.clone() } };
                    match set.clone().with(pack) {
                        Ok((next, _)) => set = next,
                        Err(error) => set.refused.push(RefusedPack { file, error }),
                    }
                }
                Err(error) => set.refused.push(RefusedPack { file, error }),
            }
        }
        for refused in &set.refused {
            tracing::error!(
                file = %refused.file.display(),
                error = %refused.error,
                "an installed capability pack is not loaded"
            );
        }
        Ok(set)
    }

    fn bundle_files(&self) -> Result<Vec<PathBuf>, PackError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io(&self.dir, error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.map_err(|error| io(&self.dir, error))?.path();
            let name =
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if path.is_file() && !name.starts_with('.') && name.ends_with(BUNDLE_EXTENSION) {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn load_file(&self, file: &Path, trust: &TrustList) -> Result<VerifiedPack, PackError> {
        let bytes = std::fs::read(file).map_err(|error| io(file, error))?;
        let pack = PackBundle::verify(&bytes, trust)?;
        if *file != self.bundle_file(&pack.bundle.name) {
            return Err(PackError::Invalid(format!(
                "{} holds pack `{}`; an installed pack's file is named after it",
                file.display(),
                pack.bundle.name
            )));
        }
        Ok(pack)
    }

    /// What installing the bundle `bytes` would do, without writing anything.
    ///
    /// # Errors
    ///
    /// The [`PackError`] installation would refuse it with: unsigned, tampered, from an
    /// unknown publisher, invalid, conflicting, a downgrade or a republished version.
    pub fn plan(&self, bytes: &[u8]) -> Result<Installation, PackError> {
        let pack = PackBundle::verify(bytes, &self.trust()?)?;
        let admission = self.load()?.admit(&pack)?;
        let drivers = pack
            .bundle
            .drivers
            .iter()
            .map(|driver| self.drivers_dir(&pack.bundle.name).join(format!("{}.toml", driver.id)))
            .collect();
        Ok(Installation { pack, admission, drivers })
    }

    /// Installs the bundle `bytes` (spec 3.5 §7.23): verifies it against the trust list and
    /// beside the site's packs, then keeps the file as signed and writes its driver
    /// manifests. The site loads it when it next starts.
    ///
    /// # Errors
    ///
    /// As [`plan`](Self::plan), or [`PackError::Io`] when a file cannot be written.
    pub fn install(&self, bytes: &[u8]) -> Result<Installation, PackError> {
        let installation = self.plan(bytes)?;
        if installation.admission == Admission::Unchanged {
            return Ok(installation);
        }
        let name = &installation.pack.bundle.name;
        let drivers = self.drivers_dir(name);
        match std::fs::remove_dir_all(&drivers) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(&drivers, error)),
        }
        for (driver, file) in installation.pack.bundle.drivers.iter().zip(&installation.drivers) {
            write_atomically(file, driver.manifest.as_bytes())?;
        }
        write_atomically(&self.bundle_file(name), bytes)?;
        tracing::info!(
            pack = %name,
            version = %installation.pack.bundle.version,
            publisher = %installation.pack.bundle.publisher.id,
            digest = %installation.pack.digest,
            "capability pack installed"
        );
        Ok(installation)
    }

    /// Removes the installed pack `name` and its driver manifests; a removed upgrade of a
    /// built-in pack leaves the built-in one. The site drops it when it next starts.
    ///
    /// # Errors
    ///
    /// [`PackError::BuiltIn`] for a built-in pack; [`PackError::NotInstalled`] for a name
    /// with no bundle file; [`PackError::Io`] when a file cannot be removed.
    pub fn remove(&self, name: &str) -> Result<(), PackError> {
        let file = self.bundle_file(name);
        if !super::bundle::is_valid_pack_name(name) || !file.is_file() {
            if Pack::ALL.iter().any(|pack| pack.as_str() == name) {
                return Err(PackError::BuiltIn(name.to_string()));
            }
            return Err(PackError::NotInstalled(name.to_string()));
        }
        std::fs::remove_file(&file).map_err(|error| io(&file, error))?;
        let drivers = self.drivers_dir(name);
        match std::fs::remove_dir_all(&drivers) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(&drivers, error)),
        }
        tracing::info!(pack = %name, "capability pack removed");
        Ok(())
    }
}
