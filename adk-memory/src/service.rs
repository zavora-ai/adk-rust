use adk_core::{Content, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub content: Content,
    pub author: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub user_id: String,
    pub app_name: String,
    /// Maximum number of results to return. `None` defaults to 10.
    pub limit: Option<usize>,
    /// Minimum similarity score threshold (0.0–1.0). Results below this
    /// score are excluded. `None` means no threshold.
    pub min_score: Option<f32>,
    /// Optional project scope. `None` returns only global entries.
    /// `Some(id)` returns global entries + entries for that project.
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub memories: Vec<MemoryEntry>,
}

/// Validate a project identifier.
///
/// Returns `Ok(())` if the project_id is non-empty and at most 256 characters.
/// Returns a descriptive error otherwise.
pub fn validate_project_id(project_id: &str) -> Result<()> {
    if project_id.is_empty() {
        return Err(adk_core::AdkError::memory("project_id must not be empty"));
    }
    if project_id.len() > 256 {
        return Err(adk_core::AdkError::memory(format!(
            "project_id exceeds maximum length of 256 characters (got {})",
            project_id.len()
        )));
    }
    Ok(())
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

    /// Add a single memory entry directly (not tied to a session).
    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        let _ = (app_name, user_id, entry);
        Err(adk_core::AdkError::memory("add_entry not implemented"))
    }

    /// Delete entries matching a query. Returns count of deleted entries.
    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        let _ = (app_name, user_id, query);
        Err(adk_core::AdkError::memory("delete_entries not implemented"))
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Add session entries scoped to a project.
    /// Default delegates to `add_session` (global).
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        let _ = project_id;
        self.add_session(app_name, user_id, session_id, entries).await
    }

    /// Add a single entry scoped to a project.
    /// Default delegates to `add_entry` (global).
    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        let _ = project_id;
        self.add_entry(app_name, user_id, entry).await
    }

    /// Delete entries matching a query within a specific project.
    /// Default delegates to `delete_entries` (global).
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        let _ = project_id;
        self.delete_entries(app_name, user_id, query).await
    }

    /// Delete all entries for a specific project.
    /// Default returns "not implemented" error.
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        let _ = (app_name, user_id, project_id);
        Err(adk_core::AdkError::memory("delete_project not implemented"))
    }
}
