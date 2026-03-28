//! Browser session pool for multi-user environments.
//!
//! Provides per-user session isolation by managing a pool of `BrowserSession`
//! instances keyed by user ID. Sessions are created lazily on first access
//! and can be released individually or cleaned up in bulk.

use crate::config::BrowserConfig;
use crate::session::BrowserSession;
use adk_core::{AdkError, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A pool of browser sessions keyed by user ID.
///
/// Use this in multi-user agent platforms where each user needs an isolated
/// browser instance. Sessions are created lazily via [`get_or_create`] and
/// cleaned up via [`release`] or [`cleanup_all`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_browser::{BrowserConfig, BrowserSessionPool};
///
/// let pool = BrowserSessionPool::new(BrowserConfig::default(), 10);
///
/// // In a tool's execute(), resolve session from user context:
/// let session = pool.get_or_create("user_123").await?;
/// session.navigate("https://example.com").await?;
///
/// // On shutdown:
/// pool.cleanup_all().await;
/// ```
pub struct BrowserSessionPool {
    config: BrowserConfig,
    sessions: RwLock<HashMap<String, Arc<BrowserSession>>>,
    max_sessions: usize,
}

impl BrowserSessionPool {
    /// Create a new session pool.
    ///
    /// `max_sessions` limits the number of concurrent browser sessions.
    /// When the limit is reached, `get_or_create` will return an error.
    pub fn new(config: BrowserConfig, max_sessions: usize) -> Self {
        Self { config, sessions: RwLock::new(HashMap::new()), max_sessions }
    }

    /// Get an existing session for the user, or create a new one.
    ///
    /// The session is started automatically if newly created.
    /// If the session exists but is stale, it will be reconnected.
    pub async fn get_or_create(&self, user_id: &str) -> Result<Arc<BrowserSession>> {
        // Fast path: check if session exists and is alive
        {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(user_id) {
                if session.is_active().await {
                    return Ok(session.clone());
                }
            }
        }

        // Slow path: create or replace session
        let mut sessions = self.sessions.write().await;

        // Double-check after acquiring write lock
        if let Some(session) = sessions.get(user_id) {
            if session.is_active().await {
                return Ok(session.clone());
            }
            sessions.remove(user_id);
        }

        // Check capacity
        if sessions.len() >= self.max_sessions {
            return Err(AdkError::tool(format!(
                "Browser session pool full ({} sessions). Release unused sessions or increase max_sessions.",
                self.max_sessions
            )));
        }

        let session = Arc::new(BrowserSession::new(self.config.clone()));
        session.start().await?;
        sessions.insert(user_id.to_string(), session.clone());

        Ok(session)
    }

    /// Release and stop a user's browser session.
    pub async fn release(&self, user_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.remove(user_id) {
            session.stop().await.ok(); // best-effort cleanup
        }
        Ok(())
    }

    /// Stop and remove all sessions. Call during graceful shutdown.
    pub async fn cleanup_all(&self) {
        let mut sessions = self.sessions.write().await;
        for (_, session) in sessions.drain() {
            session.stop().await.ok();
        }
    }

    /// Number of active sessions in the pool.
    pub async fn active_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// List all user IDs with active sessions.
    pub async fn active_users(&self) -> Vec<String> {
        self.sessions.read().await.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = BrowserSessionPool::new(BrowserConfig::default(), 5);
        assert_eq!(pool.max_sessions, 5);
    }

    #[tokio::test]
    async fn test_pool_active_count_starts_zero() {
        let pool = BrowserSessionPool::new(BrowserConfig::default(), 5);
        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_pool_active_users_starts_empty() {
        let pool = BrowserSessionPool::new(BrowserConfig::default(), 5);
        assert!(pool.active_users().await.is_empty());
    }

    #[tokio::test]
    async fn test_pool_release_nonexistent_user() {
        let pool = BrowserSessionPool::new(BrowserConfig::default(), 5);
        let result = pool.release("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_pool_cleanup_all_empty() {
        let pool = BrowserSessionPool::new(BrowserConfig::default(), 5);
        pool.cleanup_all().await;
        assert_eq!(pool.active_count().await, 0);
    }
}
