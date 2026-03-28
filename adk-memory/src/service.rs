use adk_core::{Content, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub content: Content,
    pub author: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub user_id: String,
    pub app_name: String,
    /// Maximum number of results to return. `None` defaults to 10.
    pub limit: Option<usize>,
    /// Minimum similarity score threshold (0.0–1.0). Results below this
    /// score are excluded. `None` means no threshold.
    pub min_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub memories: Vec<MemoryEntry>,
}

#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;

    /// Delete all memory entries for a specific user.
    ///
    /// Required for GDPR right-to-erasure compliance. Removes all stored
    /// memories (including embeddings) for the given app and user.
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let _ = (app_name, user_id);
        Err(adk_core::AdkError::memory("delete_user not implemented"))
    }

    /// Delete all memory entries for a specific session.
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        let _ = (app_name, user_id, session_id);
        Err(adk_core::AdkError::memory("delete_session not implemented"))
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
