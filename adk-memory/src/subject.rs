//! Memory as a [`SubjectDataStore`] for `adk_core::subject::SubjectRights`.

use std::sync::Arc;

use adk_core::Result;
use adk_core::subject::{
    SUBJECT_UNSUPPORTED_CODE, StoreErasure, StoreSupport, SubjectDataStore, SubjectScope,
};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::service::MemoryService;

/// Exports and erases a data subject's long-term memory through any [`MemoryService`].
///
/// The export lists every entry of each of the scope's user ids in each of its
/// applications, as [`MemoryService::export_user`] returns them; the erasure calls
/// [`MemoryService::delete_user`] for each, and counts what the export listed.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::Content;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{SubjectDataStore, SubjectScope};
/// use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, MemorySubjectData};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = Arc::new(InMemoryMemoryService::new());
/// let entry = MemoryEntry {
///     content: Content::new("user").with_text("Hometown: Nairobi."),
///     author: "user".to_string(),
///     timestamp: chrono::Utc::now(),
/// };
/// service.add_session("shop", "alice", "s1", vec![entry]).await?;
///
/// let memory = MemorySubjectData::new(service);
/// let scope = SubjectScope::new(["shop"], SubjectRef::human("alice"));
/// assert_eq!(memory.erase(&scope).await?.deleted, 1);
/// assert_eq!(memory.erase(&scope).await?.deleted, 0);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct MemorySubjectData {
    service: Arc<dyn MemoryService>,
    name: String,
    support: StoreSupport,
}

impl MemorySubjectData {
    /// Adapts `service`, named `memory` in reports, erasing with
    /// [`StoreSupport::Hard`].
    pub fn new(service: Arc<dyn MemoryService>) -> Self {
        Self { service, name: "memory".to_string(), support: StoreSupport::Hard }
    }

    /// Sets the name reports use, for a deployment with more than one memory service.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets how the service erases, such as [`StoreSupport::Eventual`] for a managed
    /// service whose deletions complete after `delete_user` returns.
    #[must_use]
    pub fn with_support(mut self, support: StoreSupport) -> Self {
        self.support = support;
        self
    }
}

#[async_trait]
impl SubjectDataStore for MemorySubjectData {
    fn name(&self) -> &str {
        &self.name
    }

    fn support(&self) -> StoreSupport {
        self.support
    }

    async fn export(&self, scope: &SubjectScope) -> Result<Value> {
        let mut groups = Vec::new();
        for app_name in &scope.app_names {
            for user_id in scope.user_ids() {
                let entries = self.service.export_user(app_name, user_id).await?;
                if !entries.is_empty() {
                    groups.push(
                        json!({ "appName": app_name, "userId": user_id, "entries": entries }),
                    );
                }
            }
        }
        Ok(json!({ "memories": groups }))
    }

    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
        let mut deleted = 0;
        for app_name in &scope.app_names {
            for user_id in scope.user_ids() {
                // A backend that cannot list still deletes; its count is unknown.
                deleted += match self.service.export_user(app_name, user_id).await {
                    Ok(entries) => entries.len() as u64,
                    Err(error) if error.code == SUBJECT_UNSUPPORTED_CODE => 0,
                    Err(error) => return Err(error),
                };
                self.service.delete_user(app_name, user_id).await?;
            }
        }
        Ok(StoreErasure::deleted(deleted))
    }
}
