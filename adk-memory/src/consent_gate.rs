//! A memory service that remembers nothing about a subject without its consent.

use std::sync::Arc;

use adk_core::Result;
use adk_core::subject::{ConsentGate, ConsentPurpose};
use async_trait::async_trait;

use crate::service::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};

/// A [`MemoryService`] that writes and searches a gated subject's memory only with its
/// consent for a [persistent profile](ConsentPurpose::PersistentProfile).
///
/// For a user the [`ConsentGate`] gates — a visitor id by default — without that consent
/// every add is skipped (the call succeeds and nothing is stored) and a search returns no
/// memories. A subject being erased cannot add (`subject.erasing`). Deletes, listing, and
/// health checks pass through, so erasure always reaches stored memories. Users the gate
/// does not gate pass straight through.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::Content;
/// use adk_core::subject::{ConsentGate, InMemoryConsentStore, VisitorId};
/// use adk_memory::{ConsentGatedMemoryService, InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let memory = ConsentGatedMemoryService::new(
///     Arc::new(InMemoryMemoryService::new()),
///     ConsentGate::new(Arc::new(InMemoryConsentStore::new())),
/// );
/// let visitor = VisitorId::generate().to_string();
/// let entry = MemoryEntry {
///     content: Content::new("user").with_text("Allergic to peanuts."),
///     author: "user".to_string(),
///     timestamp: chrono::Utc::now(),
/// };
/// memory.add_entry("shop", &visitor, entry).await?;
/// let found = memory
///     .search(SearchRequest {
///         query: "peanuts".into(),
///         user_id: visitor,
///         app_name: "shop".into(),
///         limit: None,
///         min_score: None,
///         project_id: None,
///     })
///     .await?;
/// assert!(found.memories.is_empty(), "nothing is remembered without consent");
/// # Ok(())
/// # }
/// ```
pub struct ConsentGatedMemoryService {
    inner: Arc<dyn MemoryService>,
    gate: ConsentGate,
}

impl std::fmt::Debug for ConsentGatedMemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentGatedMemoryService")
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

impl ConsentGatedMemoryService {
    /// Gates `inner` with `gate`.
    pub fn new(inner: Arc<dyn MemoryService>, gate: ConsentGate) -> Self {
        Self { inner, gate }
    }

    /// Whether a memory may be written for `user_id`: always for a user the gate does not
    /// gate; otherwise only with consent, and never while the subject is being erased.
    async fn may_write(&self, user_id: &str) -> Result<bool> {
        let Some(subject) = self.gate.subject_of(user_id) else { return Ok(true) };
        self.gate.ensure_writable(&subject).await?;
        let permitted = self.gate.permits(&subject, &ConsentPurpose::PersistentProfile).await?;
        if !permitted {
            tracing::debug!(subject.kind = %subject.kind, "memory write skipped without consent for a persistent profile");
        }
        Ok(permitted)
    }
}

#[async_trait]
impl MemoryService for ConsentGatedMemoryService {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if !self.may_write(user_id).await? {
            return Ok(());
        }
        self.inner.add_session(app_name, user_id, session_id, entries).await
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        if let Some(subject) = self.gate.subject_of(&req.user_id)
            && !self.gate.permits(&subject, &ConsentPurpose::PersistentProfile).await?
        {
            return Ok(SearchResponse { memories: Vec::new() });
        }
        self.inner.search(req).await
    }

    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        self.inner.delete_user(app_name, user_id).await
    }

    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        self.inner.delete_session(app_name, user_id, session_id).await
    }

    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        if !self.may_write(user_id).await? {
            return Ok(());
        }
        self.inner.add_entry(app_name, user_id, entry).await
    }

    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        self.inner.delete_entries(app_name, user_id, query).await
    }

    async fn list_recent(
        &self,
        app_name: &str,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        self.inner.list_recent(app_name, user_id, limit).await
    }

    async fn health_check(&self) -> Result<()> {
        self.inner.health_check().await
    }

    fn supports_project_scoping(&self) -> bool {
        self.inner.supports_project_scoping()
    }

    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if !self.may_write(user_id).await? {
            return Ok(());
        }
        self.inner.add_session_to_project(app_name, user_id, session_id, project_id, entries).await
    }

    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        if !self.may_write(user_id).await? {
            return Ok(());
        }
        self.inner.add_entry_to_project(app_name, user_id, project_id, entry).await
    }

    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        self.inner.delete_entries_in_project(app_name, user_id, project_id, query).await
    }

    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        self.inner.delete_project(app_name, user_id, project_id).await
    }
}
