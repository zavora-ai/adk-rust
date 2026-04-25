//! Consent capture, check, and revocation framework.
//!
//! Provides two implementations:
//! - [`InMemoryConsentService`] — ephemeral, for development and testing
//! - [`FileConsentService`] — JSON file-backed, for production (GDPR/KPA compliance)

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

        Ok(Self { path, records })
    }

    /// Persist all records to the JSON file.
    fn flush(&self) -> Result<(), AwpError> {
        let records: Vec<FileConsentRecord> =
            self.records.iter().map(|entry| entry.value().clone()).collect();
        let store = FileConsentStore { records };
        let json = serde_json::to_string_pretty(&store).map_err(|e| {
            AwpError::InternalError(format!("failed to serialize consent records: {e}"))
        })?;

        // Create parent directories if needed
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AwpError::InternalError(format!(
                        "failed to create consent directory {}: {e}",
                        parent.display()
                    ))
                })?;
            }
        }

        std::fs::write(&self.path, json).map_err(|e| {
            AwpError::InternalError(format!(
                "failed to write consent file {}: {e}",
                self.path.display()
            ))
        })?;
        Ok(())
    }
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
        self.flush()
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
        self.flush()
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
