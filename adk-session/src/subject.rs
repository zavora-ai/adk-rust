//! The session family's [`SubjectDataStore`] adapter: a subject's sessions, events, and
//! `user:` state, exported and erased through any [`SessionService`].

use std::sync::Arc;

use adk_core::Result;
use adk_core::subject::{StoreErasure, StoreSupport, SubjectDataStore, SubjectScope};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::service::{GetRequest, ListRequest, SessionService};
use crate::session::KEY_PREFIX_APP;

/// Exports and erases a subject's sessions through a [`SessionService`], for
/// [`SubjectRights`](adk_core::subject::SubjectRights).
///
/// | Operation | Per application and user id of the scope |
/// |-----------|-------------------------------------------|
/// | `export` | Every session with its events and state — the `user:` tier included, the app-wide `app:` tier left out |
/// | `erase` | [`SessionService::delete_all_sessions`]: every session, its events, and the `user:` tier |
///
/// The store is named `sessions` and erases [`StoreSupport::Hard`]; a service whose deletes
/// complete asynchronously, such as Vertex AI's, declares it with
/// [`with_support`](Self::with_support).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::principal::SubjectRef;
/// use adk_core::subject::{SubjectDataStore, SubjectScope};
/// use adk_session::{CreateRequest, InMemorySessionService, SessionService, SessionSubjectData};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = Arc::new(InMemorySessionService::new());
/// service
///     .create(CreateRequest {
///         app_name: "shop".into(),
///         user_id: "alice".into(),
///         session_id: None,
///         state: Default::default(),
///     })
///     .await?;
///
/// let sessions = SessionSubjectData::new(service.clone());
/// let scope = SubjectScope::new(["shop"], SubjectRef::human("alice"));
/// assert_eq!(sessions.erase(&scope).await?.deleted, 1);
/// assert_eq!(sessions.erase(&scope).await?.deleted, 0);
/// # Ok(())
/// # }
/// ```
pub struct SessionSubjectData {
    service: Arc<dyn SessionService>,
    support: StoreSupport,
}

impl SessionSubjectData {
    /// The adapter over `service`, erasing [`StoreSupport::Hard`].
    pub fn new(service: Arc<dyn SessionService>) -> Self {
        Self { service, support: StoreSupport::Hard }
    }

    /// Declares how the service erases, such as
    /// [`StoreSupport::Eventual`] for a service whose deletes complete asynchronously.
    #[must_use]
    pub fn with_support(mut self, support: StoreSupport) -> Self {
        self.support = support;
        self
    }

    async fn list(&self, app_name: &str, user_id: &str) -> Result<Vec<String>> {
        let sessions = self
            .service
            .list(ListRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                limit: None,
                offset: None,
            })
            .await?;
        Ok(sessions.iter().map(|session| session.id().to_owned()).collect())
    }
}

impl std::fmt::Debug for SessionSubjectData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSubjectData").field("support", &self.support).finish_non_exhaustive()
    }
}

#[async_trait]
impl SubjectDataStore for SessionSubjectData {
    fn name(&self) -> &str {
        "sessions"
    }

    fn support(&self) -> StoreSupport {
        self.support
    }

    async fn export(&self, scope: &SubjectScope) -> Result<Value> {
        let mut exported = Vec::new();
        for app_name in &scope.app_names {
            for user_id in scope.user_ids() {
                for session_id in self.list(app_name, user_id).await? {
                    let session = self
                        .service
                        .get(GetRequest {
                            app_name: app_name.clone(),
                            user_id: user_id.to_owned(),
                            session_id,
                            num_recent_events: None,
                            after: None,
                        })
                        .await?;
                    let state: serde_json::Map<String, Value> = session
                        .state()
                        .all()
                        .into_iter()
                        .filter(|(key, _)| !key.starts_with(KEY_PREFIX_APP))
                        .collect();
                    exported.push(json!({
                        "appName": session.app_name(),
                        "userId": session.user_id(),
                        "id": session.id(),
                        "state": state,
                        "events": session.events().all(),
                        "lastUpdateTime": session.last_update_time(),
                    }));
                }
            }
        }
        Ok(json!({ "sessions": exported }))
    }

    async fn erase(&self, scope: &SubjectScope) -> Result<StoreErasure> {
        let mut deleted = 0;
        for app_name in &scope.app_names {
            for user_id in scope.user_ids() {
                deleted += self.list(app_name, user_id).await?.len() as u64;
                self.service.delete_all_sessions(app_name, user_id).await?;
            }
        }
        Ok(StoreErasure::deleted(deleted))
    }
}
