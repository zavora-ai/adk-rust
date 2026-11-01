//! Artifacts as a [`SubjectDataStore`] for `adk_core::subject::SubjectRights`.

use std::sync::Arc;

use adk_core::Result;
use adk_core::subject::{StoreErasure, StoreSupport, SubjectDataStore, SubjectScope};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::service::ArtifactService;

/// Exports and erases a data subject's artifacts through any [`ArtifactService`].
///
/// The export lists every stored version of every artifact of each of the scope's user
/// ids in each of its applications — session-scoped and `user:` artifacts alike — with
/// its session, name, version, MIME type, and size, as
/// [`ArtifactService::list_user`] returns them. Content is not inlined: artifacts can be
/// large, and each listed version stays loadable through [`ArtifactService::load`] until
/// the erasure. The erasure calls [`ArtifactService::delete_user`] for each user id.
///
/// A bucket with object versioning or soft delete keeps deleted objects for its
/// retention period; set [`with_support`](Self::with_support) to
/// [`StoreSupport::Eventual`] with that period so reports say when the copies are gone.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_artifact::{ArtifactService, ArtifactSubjectData, InMemoryArtifactService, SaveRequest};
/// use adk_core::Part;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{SubjectDataStore, SubjectScope};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = Arc::new(InMemoryArtifactService::new());
/// service
///     .save(SaveRequest {
///         app_name: "shop".into(),
///         user_id: "alice".into(),
///         session_id: "s1".into(),
///         file_name: "receipt.txt".into(),
///         part: Part::Text { text: "paid".into() },
///         version: None,
///     })
///     .await?;
///
/// let artifacts = ArtifactSubjectData::new(service);
/// let scope = SubjectScope::new(["shop"], SubjectRef::human("alice"));
/// assert_eq!(artifacts.erase(&scope).await?.deleted, 1);
/// assert_eq!(artifacts.erase(&scope).await?.deleted, 0);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ArtifactSubjectData {
    service: Arc<dyn ArtifactService>,
    name: String,
    support: StoreSupport,
}

impl ArtifactSubjectData {
    /// Adapts `service`, named `artifacts` in reports, erasing with
    /// [`StoreSupport::Hard`].
    pub fn new(service: Arc<dyn ArtifactService>) -> Self {
        Self { service, name: "artifacts".to_string(), support: StoreSupport::Hard }
    }

    /// Sets the name reports use, for a deployment with more than one artifact service.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets how the service erases, such as [`StoreSupport::Eventual`] for a bucket that
    /// keeps deleted objects for a retention period.
    #[must_use]
    pub fn with_support(mut self, support: StoreSupport) -> Self {
        self.support = support;
        self
    }
}

#[async_trait]
impl SubjectDataStore for ArtifactSubjectData {
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
                let artifacts: Vec<Value> = self
                    .service
                    .list_user(app_name, user_id)
                    .await?
                    .into_iter()
                    .map(|artifact| {
                        json!({
                            "sessionId": artifact.session_id,
                            "fileName": artifact.file_name,
                            "version": artifact.version,
                            "mimeType": artifact.mime_type,
                            "size": artifact.size,
                        })
                    })
                    .collect();
                if !artifacts.is_empty() {
                    groups.push(
                        json!({ "appName": app_name, "userId": user_id, "artifacts": artifacts }),
                    );
                }
            }
        }
        Ok(json!({ "artifacts": groups }))
    }

    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
        let mut deleted = 0;
        for app_name in &scope.app_names {
            for user_id in scope.user_ids() {
                deleted += self.service.delete_user(app_name, user_id).await?;
            }
        }
        Ok(StoreErasure::deleted(deleted))
    }
}
