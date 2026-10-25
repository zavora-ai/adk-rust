//! Consent capture, check, and revocation framework.
//!
//! Provides two implementations:
//! - [`InMemoryConsentService`] — ephemeral, for development and testing
//! - [`FileConsentService`] — durable local JSON storage

use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use awp_types::AwpError;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// Trait for managing user consent records.
#[async_trait]
pub trait ConsentService: Send + Sync {
    /// Record consent for a subject and purpose.
    async fn capture_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError>;

    /// Check whether consent is currently active for a subject and purpose.
    async fn check_consent(&self, subject: &str, purpose: &str) -> Result<bool, AwpError>;

    /// Revoke previously captured consent.
    async fn revoke_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

/// A consent record with timestamp and revocation state.
#[derive(Debug, Clone)]
struct ConsentRecord {
    _captured_at: DateTime<Utc>,
    revoked: bool,
}

/// In-memory consent service backed by [`DashMap`].
///
/// Keys are `(subject, purpose)` tuples. Capturing consent on an already-
/// captured pair re-activates it (clears the revoked flag).
///
/// Records are lost on process restart. Use [`FileConsentService`] for
/// durable storage.
pub struct InMemoryConsentService {
    records: DashMap<(String, String), ConsentRecord>,
}

impl InMemoryConsentService {
    /// Create a new empty consent service.
    pub fn new() -> Self {
        Self { records: DashMap::new() }
    }
}

impl Default for InMemoryConsentService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConsentService for InMemoryConsentService {
    async fn capture_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        self.records.insert(key, ConsentRecord { _captured_at: Utc::now(), revoked: false });
        Ok(())
    }

    async fn check_consent(&self, subject: &str, purpose: &str) -> Result<bool, AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        Ok(self.records.get(&key).is_some_and(|r| !r.revoked))
    }

    async fn revoke_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        if let Some(mut entry) = self.records.get_mut(&key) {
            entry.revoked = true;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// File-backed implementation
// ---------------------------------------------------------------------------

/// Serializable consent record for file persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileConsentRecord {
    subject: String,
    purpose: String,
    captured_at: DateTime<Utc>,
    revoked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    revoked_at: Option<DateTime<Utc>>,
}

/// Serializable consent store for JSON file persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileConsentStore {
    records: Vec<FileConsentRecord>,
}

/// JSON file-backed consent service for durable storage.
///
/// Persists consent records to a JSON file on every mutation (capture/revoke).
/// Loads existing records from the file on construction. Safe for single-process
/// deployments; for multi-process, use a database-backed implementation.
///
/// Each write goes to a temporary file in the same directory, is synced, and is renamed
/// over the target on a blocking thread, so a reader or a crash never sees a partial file
/// and the async executor never blocks on disk (spec 3.5 §10 X3). Writes are serialised, so
/// the file always ends with the latest state.
///
/// # Example
///
/// ```rust,ignore
/// use adk_awp::FileConsentService;
///
/// let consent = FileConsentService::new("data/consent.json")?;
/// consent.capture_consent("visitor-123", "analytics").await?;
/// assert!(consent.check_consent("visitor-123", "analytics").await?);
/// ```
pub struct FileConsentService {
    path: PathBuf,
    records: DashMap<(String, String), FileConsentRecord>,
    write_lock: tokio::sync::Mutex<()>,
}

impl FileConsentService {
    /// Create a new file-backed consent service.
    ///
    /// If the file exists, records are loaded from it. If it doesn't exist,
    /// an empty store is created and the file is written on first mutation.
    ///
    /// # Errors
    ///
    /// Returns [`AwpError::InternalError`] if the file exists but cannot be
    /// read or parsed.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, AwpError> {
        let path = path.as_ref().to_path_buf();
        let records = DashMap::new();

        if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|e| {
                AwpError::InternalError(format!(
                    "failed to read consent file {}: {e}",
                    path.display()
                ))
            })?;
            let store: FileConsentStore = serde_json::from_str(&content).map_err(|e| {
                AwpError::InternalError(format!(
                    "failed to parse consent file {}: {e}",
                    path.display()
                ))
            })?;
            for record in store.records {
                let key = (record.subject.clone(), record.purpose.clone());
                records.insert(key, record);
            }
        }

        Ok(Self { path, records, write_lock: tokio::sync::Mutex::new(()) })
    }

    /// Persist all records to the JSON file.
    async fn flush(&self) -> Result<(), AwpError> {
        // The snapshot is taken under the lock, so writes land in order and the last one
        // holds every mutation made before it.
        let _guard = self.write_lock.lock().await;
        let mut records: Vec<FileConsentRecord> =
            self.records.iter().map(|entry| entry.value().clone()).collect();
        records.sort_by(|a, b| (&a.subject, &a.purpose).cmp(&(&b.subject, &b.purpose)));
        let json = serde_json::to_vec_pretty(&FileConsentStore { records }).map_err(|e| {
            AwpError::InternalError(format!("failed to serialize consent records: {e}"))
        })?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || write_atomically(&path, &json))
            .await
            .map_err(|e| AwpError::InternalError(format!("consent file write task failed: {e}")))?
    }
}

/// Writes `bytes` to a synced temporary file beside `path` and renames it over `path`.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AwpError> {
    let io_error = |action: &str, target: &Path, e: std::io::Error| {
        AwpError::InternalError(format!("failed to {action} {}: {e}", target.display()))
    };
    let Some(file_name) = path.file_name() else {
        return Err(AwpError::InternalError(format!(
            "consent file path {} has no file name",
            path.display()
        )));
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_error("create consent directory", parent, e))?;
    }
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        uuid::Uuid::now_v7().simple()
    ));
    let written = std::fs::File::create(&temp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(|e| io_error("write consent file", &temp, e))
        .and_then(|()| {
            std::fs::rename(&temp, path).map_err(|e| io_error("replace consent file", path, e))
        });
    if written.is_err() {
        // Best effort: the temporary file is never read, only left behind.
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[async_trait]
impl ConsentService for FileConsentService {
    async fn capture_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        self.records.insert(
            key,
            FileConsentRecord {
                subject: subject.to_string(),
                purpose: purpose.to_string(),
                captured_at: Utc::now(),
                revoked: false,
                revoked_at: None,
            },
        );
        self.flush().await
    }

    async fn check_consent(&self, subject: &str, purpose: &str) -> Result<bool, AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        Ok(self.records.get(&key).is_some_and(|r| !r.revoked))
    }

    async fn revoke_consent(&self, subject: &str, purpose: &str) -> Result<(), AwpError> {
        let key = (subject.to_string(), purpose.to_string());
        if let Some(mut entry) = self.records.get_mut(&key) {
            entry.revoked = true;
            entry.revoked_at = Some(Utc::now());
        }
        self.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- InMemoryConsentService tests ---

    #[tokio::test]
    async fn test_capture_and_check() {
        let svc = InMemoryConsentService::new();
        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_check_without_capture_returns_false() {
        let svc = InMemoryConsentService::new();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_revoke_consent() {
        let svc = InMemoryConsentService::new();
        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(svc.check_consent("user1", "analytics").await.unwrap());

        svc.revoke_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_recapture_after_revoke() {
        let svc = InMemoryConsentService::new();
        svc.capture_consent("user1", "analytics").await.unwrap();
        svc.revoke_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());

        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_different_subjects_independent() {
        let svc = InMemoryConsentService::new();
        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user2", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_different_purposes_independent() {
        let svc = InMemoryConsentService::new();
        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "marketing").await.unwrap());
    }

    #[tokio::test]
    async fn test_revoke_nonexistent_is_noop() {
        let svc = InMemoryConsentService::new();
        svc.revoke_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }

    // --- FileConsentService tests ---

    #[tokio::test]
    async fn test_file_capture_and_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();

        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(svc.check_consent("user1", "analytics").await.unwrap());

        // File should exist
        assert!(path.exists());
    }

    #[tokio::test]
    async fn test_file_persistence_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");

        // First instance: capture consent
        {
            let svc = FileConsentService::new(&path).unwrap();
            svc.capture_consent("user1", "analytics").await.unwrap();
            svc.capture_consent("user2", "marketing").await.unwrap();
        }

        // Second instance: records should be loaded from file
        {
            let svc = FileConsentService::new(&path).unwrap();
            assert!(svc.check_consent("user1", "analytics").await.unwrap());
            assert!(svc.check_consent("user2", "marketing").await.unwrap());
            assert!(!svc.check_consent("user3", "analytics").await.unwrap());
        }
    }

    #[tokio::test]
    async fn test_file_revoke_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");

        {
            let svc = FileConsentService::new(&path).unwrap();
            svc.capture_consent("user1", "analytics").await.unwrap();
            svc.revoke_consent("user1", "analytics").await.unwrap();
        }

        {
            let svc = FileConsentService::new(&path).unwrap();
            assert!(!svc.check_consent("user1", "analytics").await.unwrap());
        }
    }

    #[tokio::test]
    async fn test_file_recapture_after_revoke_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");

        {
            let svc = FileConsentService::new(&path).unwrap();
            svc.capture_consent("user1", "analytics").await.unwrap();
            svc.revoke_consent("user1", "analytics").await.unwrap();
            svc.capture_consent("user1", "analytics").await.unwrap();
        }

        {
            let svc = FileConsentService::new(&path).unwrap();
            assert!(svc.check_consent("user1", "analytics").await.unwrap());
        }
    }

    #[tokio::test]
    async fn test_file_nonexistent_path_creates_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("consent.json");
        assert!(!path.exists());

        let svc = FileConsentService::new(&path).unwrap();
        svc.capture_consent("user1", "analytics").await.unwrap();
        assert!(path.exists());
    }

    /// Writes go through a temporary file that is renamed away (spec 3.5 §10 X3).
    #[tokio::test]
    async fn test_file_writes_leave_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();
        for i in 0..10 {
            svc.capture_consent(&format!("user{i}"), "analytics").await.unwrap();
        }
        svc.revoke_consent("user3", "analytics").await.unwrap();
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["consent.json"]);
    }

    /// Concurrent mutations all reach the file, and the last write holds every one of them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_file_concurrent_captures_all_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = std::sync::Arc::new(FileConsentService::new(&path).unwrap());
        let tasks: Vec<_> = (0..64)
            .map(|i| {
                let svc = std::sync::Arc::clone(&svc);
                tokio::spawn(
                    async move { svc.capture_consent(&format!("user{i}"), "outreach").await },
                )
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let reloaded = FileConsentService::new(&path).unwrap();
        for i in 0..64 {
            assert!(
                reloaded.check_consent(&format!("user{i}"), "outreach").await.unwrap(),
                "user{i}"
            );
        }
    }

    /// A reader never sees a partially written file (spec 3.5 §10 X3).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_file_readers_never_see_a_partial_file() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();
        svc.capture_consent("seed", "analytics").await.unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = {
            let (done, reads, path) = (Arc::clone(&done), Arc::clone(&reads), path.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let content = std::fs::read_to_string(&path).unwrap();
                    serde_json::from_str::<FileConsentStore>(&content)
                        .unwrap_or_else(|e| panic!("partial consent file ({e}): {content:?}"));
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        for i in 0..200 {
            svc.capture_consent(&format!("visitor-{i:04}"), "persistent_profile").await.unwrap();
        }
        done.store(true, Ordering::Relaxed);
        reader.join().expect("every read parsed");
        assert!(reads.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn test_file_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        std::fs::write(&path, "not valid json").unwrap();

        let result = FileConsentService::new(&path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_check_without_capture_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_file_revoke_nonexistent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();
        svc.revoke_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }

    #[tokio::test]
    async fn test_file_consent_json_structure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let svc = FileConsentService::new(&path).unwrap();
        svc.capture_consent("user1", "analytics").await.unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["records"].is_array());
        let record = &json["records"][0];
        assert_eq!(record["subject"], "user1");
        assert_eq!(record["purpose"], "analytics");
        assert_eq!(record["revoked"], false);
    }
}
