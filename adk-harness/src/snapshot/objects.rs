//! The content-addressed object store of snapshots (spec 3.1 §7.27, §9.2 I68).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};

/// One stored object, as [`SnapshotObjects::list`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    /// The object's id: the lowercase hex SHA-256 of its plaintext.
    pub id: String,
    /// Bytes it takes in the store (sealed, when sealed).
    pub bytes: u64,
    /// When it was written, where the store knows; garbage collection spares recent
    /// objects, which another process may be about to reference.
    pub modified: Option<SystemTime>,
}

/// Where snapshot contents live: objects addressed by the SHA-256 of their plaintext, so a
/// file that did not change between snapshots is stored once (spec 3.1 §7.27).
///
/// The methods are synchronous: the snapshot walk calls them from a blocking task.
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::{InMemorySnapshotObjects, SnapshotObjects};
///
/// let objects = InMemorySnapshotObjects::new();
/// let id = adk_harness::snapshot::object_id(b"hello");
/// assert_eq!(objects.put(&id, b"hello").unwrap(), 5);
/// assert_eq!(objects.put(&id, b"hello").unwrap(), 0, "stored once");
/// assert_eq!(objects.get(&id).unwrap(), b"hello");
/// ```
pub trait SnapshotObjects: Send + Sync + std::fmt::Debug {
    /// The directory the objects are kept in, when they are on disk: a snapshot store is
    /// refused inside the workspace it snapshots (I68).
    fn location(&self) -> Option<&Path>;

    /// Whether `id` is stored.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.io` when the store cannot be read.
    fn contains(&self, id: &str) -> Result<bool>;

    /// Stores `bytes` under `id` unless it is stored; returns the bytes written, `0` when
    /// it was already there.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.io` when the object cannot be written.
    fn put(&self, id: &str, bytes: &[u8]) -> Result<u64>;

    /// The plaintext of `id`.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.missing_object` when `id` is not stored, or the store's
    /// key error when it cannot be opened.
    fn get(&self, id: &str) -> Result<Vec<u8>>;

    /// Every stored object.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.io` when the store cannot be read.
    fn list(&self) -> Result<Vec<ObjectInfo>>;

    /// Removes `id`; removing an object that is not stored succeeds.
    ///
    /// # Errors
    ///
    /// Returns `harness.snapshot.io` when the object cannot be removed.
    fn remove(&self, id: &str) -> Result<()>;
}

/// The object id of `bytes`: the lowercase hex SHA-256.
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_harness::snapshot::object_id(b""),
///     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
/// );
/// ```
pub fn object_id(bytes: &[u8]) -> String {
    crate::fsutil::hex_digest(bytes)
}

/// The directory name of a workspace's snapshots: the SHA-256 of its canonical root.
///
/// # Example
///
/// ```rust
/// let a = adk_harness::snapshot::root_digest(std::path::Path::new("/work/a"));
/// assert_eq!(a.len(), 64);
/// assert_ne!(a, adk_harness::snapshot::root_digest(std::path::Path::new("/work/b")));
/// ```
pub fn root_digest(root: &Path) -> String {
    crate::fsutil::sha256_hex(&root.to_string_lossy())
}

pub(crate) fn snapshot_error(
    code: &'static str,
    category: ErrorCategory,
    message: impl Into<String>,
) -> AdkError {
    AdkError::new(ErrorComponent::Agent, category, code, message)
}

fn io_error(path: &Path, action: &str, error: &std::io::Error) -> AdkError {
    snapshot_error(
        "harness.snapshot.io",
        ErrorCategory::Unavailable,
        format!("{action} {}: {error}", path.display()),
    )
}

fn missing(id: &str) -> AdkError {
    snapshot_error(
        "harness.snapshot.missing_object",
        ErrorCategory::NotFound,
        format!(
            "snapshot object {id} is not in the store: it was collected or the store was moved; \
             this snapshot cannot be restored"
        ),
    )
}

fn valid_id(id: &str) -> Result<()> {
    if id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(())
    } else {
        Err(snapshot_error(
            "harness.snapshot.invalid_object",
            ErrorCategory::InvalidInput,
            format!("'{id}' is not a snapshot object id (64 lowercase hex characters)"),
        ))
    }
}

/// Snapshot objects in memory, for tests and `--ephemeral` sessions.
#[derive(Debug, Default)]
pub struct InMemorySnapshotObjects {
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemorySnapshotObjects {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SnapshotObjects for InMemorySnapshotObjects {
    fn location(&self) -> Option<&Path> {
        None
    }

    fn contains(&self, id: &str) -> Result<bool> {
        Ok(self.objects.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id))
    }

    fn put(&self, id: &str, bytes: &[u8]) -> Result<u64> {
        valid_id(id)?;
        let mut objects = self.objects.lock().unwrap_or_else(|e| e.into_inner());
        if objects.contains_key(id) {
            return Ok(0);
        }
        objects.insert(id.to_string(), bytes.to_vec());
        Ok(bytes.len() as u64)
    }

    fn get(&self, id: &str) -> Result<Vec<u8>> {
        self.objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| missing(id))
    }

    fn list(&self) -> Result<Vec<ObjectInfo>> {
        Ok(self
            .objects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, bytes)| ObjectInfo { id: id.clone(), bytes: bytes.len() as u64, modified: None })
            .collect())
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.objects.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        Ok(())
    }
}

/// Snapshot objects on disk: `<dir>/objects/<first two hex digits>/<the rest>`, each
/// readable by its owner only, and sealed with the §7.12 envelope when a store key is
/// configured (spec 3.1 §7.27, I68).
///
/// [`for_workspace`](Self::for_workspace) puts a workspace's objects under
/// `<snapshots dir>/<sha256 of the canonical root>/`; adk-cli's snapshots dir is
/// `<config dir>/adk-rust/snapshots`, beside `harness.db`.
///
/// # Example
///
/// ```rust
/// use adk_harness::snapshot::{FileSnapshotObjects, SnapshotObjects};
///
/// let home = tempfile::tempdir().unwrap();
/// let objects = FileSnapshotObjects::for_workspace(home.path(), std::path::Path::new("/work/app"));
/// let id = adk_harness::snapshot::object_id(b"fn main() {}");
/// objects.put(&id, b"fn main() {}").unwrap();
/// assert_eq!(objects.get(&id).unwrap(), b"fn main() {}");
/// assert!(objects.location().unwrap().starts_with(home.path()));
/// ```
#[derive(Debug, Clone)]
pub struct FileSnapshotObjects {
    dir: PathBuf,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

impl FileSnapshotObjects {
    /// Objects under `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            #[cfg(feature = "encrypted")]
            envelope: None,
        }
    }

    /// The objects of the workspace at `root` under `snapshots_dir`:
    /// `<snapshots_dir>/<sha256 of root>/`. Pass the canonical root.
    pub fn for_workspace(snapshots_dir: &Path, root: &Path) -> Self {
        Self::new(snapshots_dir.join(root_digest(root)))
    }

    /// Seals every object under `keyring` (spec 3.1 §7.12): an object written in
    /// plaintext then fails to open (I57) until `rewrap` seals it.
    #[cfg(feature = "encrypted")]
    #[must_use]
    pub fn with_keyring(mut self, keyring: crate::encrypted::StoreKeyring) -> Self {
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "snapshot-objects"));
        self
    }

    /// Seals every object a previous key or no key sealed under the current key, and
    /// returns how many changed. Without a keyring, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.decrypt_failed` when an object opens under no key the
    /// keyring holds, or `harness.snapshot.io`.
    #[cfg(feature = "encrypted")]
    pub fn rewrap(&self) -> Result<u64> {
        let Some(envelope) = &self.envelope else {
            return Ok(0);
        };
        let mut changed = 0;
        for info in self.list()? {
            let path = self.path(&info.id);
            let bytes = std::fs::read(&path).map_err(|error| io_error(&path, "read", &error))?;
            let address = [info.id.as_str()];
            let text = std::str::from_utf8(&bytes).ok().filter(|t| adk_session::envelope::is_envelope(t));
            let resealed = match text {
                Some(text) => envelope.reseal(&address, text)?,
                None => Some(envelope.seal(&address, &bytes)?),
            };
            if let Some(resealed) = resealed {
                crate::fsutil::write_atomic_private_sync(&path, resealed.as_bytes())?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    fn objects_dir(&self) -> PathBuf {
        self.dir.join("objects")
    }

    fn path(&self, id: &str) -> PathBuf {
        let (fan, rest) = id.split_at(2.min(id.len()));
        self.objects_dir().join(fan).join(rest)
    }

    #[cfg(feature = "encrypted")]
    fn encode(&self, id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        match &self.envelope {
            Some(envelope) => Ok(envelope.seal(&[id], bytes)?.into_bytes()),
            None => Ok(bytes.to_vec()),
        }
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _id: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(bytes.to_vec())
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, id: &str, stored: Vec<u8>) -> Result<Vec<u8>> {
        let text = std::str::from_utf8(&stored).ok().filter(|t| adk_session::envelope::is_envelope(t));
        match (&self.envelope, text) {
            (Some(envelope), Some(text)) => Ok(envelope.open(&[id], text)?.0),
            (Some(envelope), None) => Err(snapshot_error(
                "harness.store.unencrypted",
                ErrorCategory::InvalidInput,
                format!(
                    "snapshot object {id} is in plaintext while a store key is configured; seal \
                     existing objects with `adk-rust store rotate-key` ({})",
                    envelope.store()
                ),
            )),
            (None, Some(_)) => Err(snapshot_error(
                "harness.store.key_required",
                ErrorCategory::Unauthorized,
                "the snapshot objects were written under a store key; pass --store-key (or set \
                 [store] key in the user config) — they never open in plaintext",
            )),
            (None, None) => Ok(stored),
        }
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _id: &str, stored: Vec<u8>) -> Result<Vec<u8>> {
        Ok(stored)
    }
}

impl SnapshotObjects for FileSnapshotObjects {
    fn location(&self) -> Option<&Path> {
        Some(&self.dir)
    }

    fn contains(&self, id: &str) -> Result<bool> {
        valid_id(id)?;
        let path = self.path(id);
        match path.symlink_metadata() {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_error(&path, "read", &error)),
        }
    }

    fn put(&self, id: &str, bytes: &[u8]) -> Result<u64> {
        if self.contains(id)? {
            return Ok(0);
        }
        let encoded = self.encode(id, bytes)?;
        crate::fsutil::write_atomic_private_sync(&self.path(id), &encoded)?;
        Ok(encoded.len() as u64)
    }

    fn get(&self, id: &str) -> Result<Vec<u8>> {
        valid_id(id)?;
        let path = self.path(id);
        let stored = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(missing(id)),
            Err(error) => return Err(io_error(&path, "read", &error)),
        };
        let plaintext = self.decode(id, stored)?;
        if object_id(&plaintext) != id {
            return Err(snapshot_error(
                "harness.snapshot.corrupt_object",
                ErrorCategory::Internal,
                format!("snapshot object {id} does not hash to its id; it was changed on disk"),
            ));
        }
        Ok(plaintext)
    }

    fn list(&self) -> Result<Vec<ObjectInfo>> {
        let root = self.objects_dir();
        let fans = match std::fs::read_dir(&root) {
            Ok(fans) => fans,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&root, "list", &error)),
        };
        let mut out = Vec::new();
        for fan in fans.flatten() {
            let fan_name = fan.file_name().to_string_lossy().into_owned();
            let Ok(entries) = std::fs::read_dir(fan.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = format!("{fan_name}{}", entry.file_name().to_string_lossy());
                if valid_id(&id).is_err() {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                out.push(ObjectInfo { id, bytes: metadata.len(), modified: metadata.modified().ok() });
            }
        }
        Ok(out)
    }

    fn remove(&self, id: &str) -> Result<()> {
        valid_id(id)?;
        let path = self.path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(&path, "remove", &error)),
        }
    }
}

/// Seals every workspace's objects under `snapshots_dir` with `keyring`'s current key
/// (`adk-rust store rotate-key`); returns how many objects changed.
///
/// # Errors
///
/// As [`FileSnapshotObjects::rewrap`].
#[cfg(feature = "encrypted")]
pub fn rewrap_all(snapshots_dir: &Path, keyring: &crate::encrypted::StoreKeyring) -> Result<u64> {
    let Ok(roots) = std::fs::read_dir(snapshots_dir) else {
        return Ok(0);
    };
    let mut changed = 0;
    for root in roots.flatten() {
        if root.path().is_dir() {
            changed += FileSnapshotObjects::new(root.path()).with_keyring(keyring.clone()).rewrap()?;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_is_stored_once_and_listed() {
        let dir = tempfile::tempdir().unwrap();
        let objects = FileSnapshotObjects::new(dir.path());
        let id = object_id(b"content");
        assert!(!objects.contains(&id).unwrap());
        assert_eq!(objects.put(&id, b"content").unwrap(), 7);
        assert_eq!(objects.put(&id, b"content").unwrap(), 0);
        assert_eq!(objects.list().unwrap().len(), 1);
        objects.remove(&id).unwrap();
        assert!(objects.list().unwrap().is_empty());
        assert_eq!(objects.get(&id).unwrap_err().code, "harness.snapshot.missing_object");
        assert!(objects.put("../escape", b"x").is_err(), "an id is never a path");
    }

    #[cfg(unix)]
    #[test]
    fn objects_are_readable_by_their_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let objects = FileSnapshotObjects::new(dir.path());
        let id = object_id(b"secret");
        objects.put(&id, b"secret").unwrap();
        let mode = std::fs::metadata(objects.path(&id)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(feature = "encrypted")]
    #[test]
    fn sealed_objects_hold_no_plaintext_and_need_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let keyring = crate::encrypted::StoreKeyring::generate();
        let sealed = FileSnapshotObjects::new(dir.path()).with_keyring(keyring.clone());
        let id = object_id(b"password = hunter2");
        sealed.put(&id, b"password = hunter2").unwrap();
        let on_disk = std::fs::read(sealed.path(&id)).unwrap();
        assert!(!String::from_utf8_lossy(&on_disk).contains("hunter2"));
        assert_eq!(sealed.get(&id).unwrap(), b"password = hunter2");
        let plain = FileSnapshotObjects::new(dir.path());
        assert_eq!(plain.get(&id).unwrap_err().code, "harness.store.key_required");
        let other = FileSnapshotObjects::new(dir.path())
            .with_keyring(crate::encrypted::StoreKeyring::generate());
        assert!(other.get(&id).is_err(), "another key opens nothing");

        // A plaintext object written before the key fails closed until it is rewrapped.
        let plain_id = object_id(b"older");
        plain.put(&plain_id, b"older").unwrap();
        assert_eq!(sealed.get(&plain_id).unwrap_err().code, "harness.store.unencrypted");
        assert_eq!(sealed.rewrap().unwrap(), 1);
        assert_eq!(sealed.get(&plain_id).unwrap(), b"older");
    }
}
