//! Adapter bridging [`MemoryService`] to [`adk_core::Memory`].
//!
//! The runner expects `Arc<dyn adk_core::Memory>`, which has a simple
//! `search(&str)` signature. [`MemoryService`] requires a [`SearchRequest`]
//! with `app_name` and `user_id`. This adapter binds those fields at
//! construction time so any `MemoryService` can be used as `adk_core::Memory`.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
//! use std::sync::Arc;
//!
//! let service = Arc::new(InMemoryMemoryService::new());
//! let memory = Arc::new(MemoryServiceAdapter::new(service, "my-app", "user-1"));
//! // memory implements adk_core::Memory and can be passed to RunnerConfig
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;

use crate::{MemoryService, SearchRequest};

/// Adapts any [`MemoryService`] into an [`adk_core::Memory`] implementation.
///
/// Binds `app_name` and `user_id` at construction so the runner's
/// `search(query: &str)` calls are forwarded with full context.
pub struct MemoryServiceAdapter {
    inner: Arc<dyn MemoryService>,
    app_name: String,
    user_id: String,
    project_id: Option<String>,
}

impl MemoryServiceAdapter {
    /// Create a new adapter binding a memory service to a specific app and user.
    pub fn new(
        inner: Arc<dyn MemoryService>,
        app_name: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        Self { inner, app_name: app_name.into(), user_id: user_id.into(), project_id: None }
    }

    /// Bind this adapter to a specific project scope.
    ///
    /// When set, `search` includes project entries, and `add`/`delete`
    /// operate within the project scope.
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }
}

#[async_trait]
impl adk_core::Memory for MemoryServiceAdapter {
    async fn search(&self, query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        let inner = self.inner.clone();
        let resp = inner
            .search(SearchRequest {
                query: query.to_string(),
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                limit: None,
                min_score: None,
                project_id: self.project_id.clone(),
            })
            .await?;

        Ok(resp
            .memories
            .into_iter()
            .map(|m| adk_core::MemoryEntry { content: m.content, author: m.author })
            .collect())
    }

    async fn add(&self, entry: adk_core::MemoryEntry) -> adk_core::Result<()> {
        let inner = self.inner.clone();
        let mem_entry = crate::MemoryEntry {
            content: entry.content,
            author: entry.author,
            timestamp: Utc::now(),
        };
        if let Some(ref pid) = self.project_id {
            inner.add_entry_to_project(&self.app_name, &self.user_id, pid, mem_entry).await
        } else {
            inner.add_entry(&self.app_name, &self.user_id, mem_entry).await
        }
    }

    async fn delete(&self, query: &str) -> adk_core::Result<u64> {
        let inner = self.inner.clone();
        if let Some(ref pid) = self.project_id {
            inner.delete_entries_in_project(&self.app_name, &self.user_id, pid, query).await
        } else {
            inner.delete_entries(&self.app_name, &self.user_id, query).await
        }
    }

    async fn health_check(&self) -> adk_core::Result<()> {
        let inner = self.inner.clone();
        inner.health_check().await
    }

    async fn search_in_project(
        &self,
        query: &str,
        project_id: &str,
    ) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        let inner = self.inner.clone();
        let resp = inner
            .search(SearchRequest {
                query: query.to_string(),
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                limit: None,
                min_score: None,
                project_id: Some(project_id.to_string()),
            })
            .await?;

        Ok(resp
            .memories
            .into_iter()
            .map(|m| adk_core::MemoryEntry { content: m.content, author: m.author })
            .collect())
    }

    async fn add_to_project(
        &self,
        entry: adk_core::MemoryEntry,
        project_id: &str,
    ) -> adk_core::Result<()> {
        let inner = self.inner.clone();
        let mem_entry = crate::MemoryEntry {
            content: entry.content,
            author: entry.author,
            timestamp: Utc::now(),
        };
        inner.add_entry_to_project(&self.app_name, &self.user_id, project_id, mem_entry).await
    }
}
