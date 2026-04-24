//! Consent capture, check, and revocation framework.

use async_trait::async_trait;
use awp_types::AwpError;
use chrono::{DateTime, Utc};
use dashmap::DashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Should not error
        svc.revoke_consent("user1", "analytics").await.unwrap();
        assert!(!svc.check_consent("user1", "analytics").await.unwrap());
    }
}
