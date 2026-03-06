use crate::service::*;
use adk_core::types::{SessionId, UserId};
use adk_core::{Part, Result};
use async_trait::async_trait;
use moka::future::Cache;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const USER_SCOPED_KEY: &str = "user";

/// The base identity of an artifact, ignoring its version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ArtifactBaseKey {
    app_name: String,
    user_id: UserId,
    session_id: SessionId,
    file_name: String,
}

pub struct InMemoryArtifactService {
    /// Moka handles TTL, TTI, and capacity limits asynchronously.
    /// The Arc<RwLock<BTreeMap>> allows hyper-fast, deadlock-free version mutations.
    artifacts: Cache<ArtifactBaseKey, Arc<RwLock<BTreeMap<i64, Part>>>>,
    user_scoped_session: SessionId,
}

impl InMemoryArtifactService {
    pub fn new() -> Self {
        // Configure the cache limits to prevent OOM crashes in production.
        // Adjust these heuristics based on your expected server RAM.
        let cache = Cache::builder()
            // Max 100,000 active artifact streams in memory at once
            .max_capacity(100_000)
            // Automatically evict streams that haven't been read/written in 2 hours
            .time_to_idle(Duration::from_secs(2 * 60 * 60))
            .build();

        Self {
            artifacts: cache,
            user_scoped_session: SessionId::new(USER_SCOPED_KEY.to_string()).unwrap(),
        }
    }

    fn is_user_scoped(file_name: &str) -> bool {
        file_name.starts_with("user:")
    }

    fn get_session_id(&self, session_id: &SessionId, file_name: &str) -> SessionId {
        if Self::is_user_scoped(file_name) {
            self.user_scoped_session.clone()
        } else {
            session_id.clone()
        }
    }

    fn validate_file_name(file_name: &str) -> Result<()> {
        if file_name.is_empty() {
            return Err(adk_core::AdkError::Artifact(
                "invalid artifact file name: empty name".to_string(),
            ));
        }

        if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
            return Err(adk_core::AdkError::Artifact(format!(
                "invalid artifact file name '{}': path separators and traversal patterns are not allowed",
                file_name
            )));
        }

        Ok(())
    }

    fn build_base_key(
        &self,
        app_name: &str,
        user_id: &UserId,
        session_id: &SessionId,
        file_name: &str,
    ) -> ArtifactBaseKey {
        ArtifactBaseKey {
            app_name: app_name.to_string(),
            user_id: user_id.clone(),
            session_id: self.get_session_id(session_id, file_name),
            file_name: file_name.to_string(),
        }
    }
}

impl Default for InMemoryArtifactService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ArtifactService for InMemoryArtifactService {
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse> {
        Self::validate_file_name(&req.file_name)?;

        let base_key =
            self.build_base_key(&req.app_name, &req.user_id, &req.session_id, &req.file_name);

        // Atomically fetch the existing tree or initialize a new one.
        // This is safe and lock-free at the cache level.
        let tree_arc = self
            .artifacts
            .get_with(base_key, async { Arc::new(RwLock::new(BTreeMap::new())) })
            .await;

        // Acquire a synchronous lock strictly for the nanoseconds it takes to insert.
        // Never hold this across an await point.
        let mut versions = tree_arc.write().unwrap();

        let version = req
            .version
            .unwrap_or_else(|| versions.last_key_value().map(|(&v, _)| v + 1).unwrap_or(1));

        versions.insert(version, req.part);

        Ok(SaveResponse { version })
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        Self::validate_file_name(&req.file_name)?;

        let base_key =
            self.build_base_key(&req.app_name, &req.user_id, &req.session_id, &req.file_name);

        let tree_arc = self
            .artifacts
            .get(&base_key)
            .await // Moka uses .await for get() in future cache
            .ok_or_else(|| adk_core::AdkError::Artifact("artifact not found".into()))?;

        let versions = tree_arc.read().unwrap();

        let part = if let Some(version) = req.version {
            versions
                .get(&version)
                .ok_or_else(|| {
                    adk_core::AdkError::Artifact(format!("version {} not found", version))
                })?
                .clone()
        } else {
            versions
                .last_key_value()
                .ok_or_else(|| adk_core::AdkError::Artifact("artifact has no versions".into()))?
                .1
                .clone()
        };

        Ok(LoadResponse { part })
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        Self::validate_file_name(&req.file_name)?;

        let base_key =
            self.build_base_key(&req.app_name, &req.user_id, &req.session_id, &req.file_name);

        if let Some(version) = req.version {
            let should_remove_parent = {
                // Scope the read lock
                if let Some(tree_arc) = self.artifacts.get(&base_key).await {
                    let mut versions = tree_arc.write().unwrap();
                    versions.remove(&version);
                    versions.is_empty() // Return true if the map is now empty
                } else {
                    false
                }
            }; // Lock drops here

            if should_remove_parent {
                self.artifacts.invalidate(&base_key).await;
            }
        } else {
            // Invalidate triggers eviction instantly
            self.artifacts.invalidate(&base_key).await;
        }

        Ok(())
    }

    async fn list(&self, req: ListRequest) -> Result<ListResponse> {
        let mut file_names = Vec::new();

        // Moka allows safe iteration over the snapshot of keys
        for (k, _) in self.artifacts.iter() {
            if k.app_name == req.app_name
                && k.user_id == req.user_id
                && (k.session_id == req.session_id || k.session_id == self.user_scoped_session)
            {
                file_names.push(k.file_name.clone());
            }
        }

        file_names.sort();
        file_names.dedup();

        Ok(ListResponse { file_names })
    }

    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse> {
        Self::validate_file_name(&req.file_name)?;

        let base_key =
            self.build_base_key(&req.app_name, &req.user_id, &req.session_id, &req.file_name);

        let tree_arc = self
            .artifacts
            .get(&base_key)
            .await
            .ok_or_else(|| adk_core::AdkError::Artifact("artifact not found".into()))?;

        let versions_map = tree_arc.read().unwrap();

        let mut versions: Vec<i64> = versions_map.keys().copied().collect();
        versions.reverse();

        Ok(VersionsResponse { versions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{ListRequest, LoadRequest, SaveRequest, VersionsRequest};
    use adk_core::Part;
    use adk_core::types::{SessionId, UserId};

    #[tokio::test]
    async fn test_in_memory_artifact_service_user_scope() {
        let service = InMemoryArtifactService::new();
        let app_name = "test_app".to_string();
        let user_id = UserId::new("user_1").unwrap();
        let session_1 = SessionId::new("session_1").unwrap();
        let session_2 = SessionId::new("session_2").unwrap();

        // Save user-scoped artifact in session 1
        service
            .save(SaveRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_1.clone(),
                file_name: "user:prefs.json".to_string(),
                version: None,
                part: Part::text("dark mode"),
            })
            .await
            .unwrap();

        // Save session-scoped artifact in session 1
        service
            .save(SaveRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_1.clone(),
                file_name: "chat_history.txt".to_string(),
                version: None,
                part: Part::text("hello"),
            })
            .await
            .unwrap();

        // Both should be listed in session 1
        let list1 = service
            .list(ListRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_1.clone(),
            })
            .await
            .unwrap();
        assert_eq!(list1.file_names.len(), 2);
        assert!(list1.file_names.contains(&"user:prefs.json".to_string()));
        assert!(list1.file_names.contains(&"chat_history.txt".to_string()));

        // Only user-scoped should be listed in session 2
        let list2 = service
            .list(ListRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_2.clone(),
            })
            .await
            .unwrap();
        assert_eq!(list2.file_names.len(), 1);
        assert!(list2.file_names.contains(&"user:prefs.json".to_string()));

        // Retrieving from session 2 works for user-scoped
        let load2 = service
            .load(LoadRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_2.clone(),
                file_name: "user:prefs.json".to_string(),
                version: None,
            })
            .await
            .unwrap();
        assert_eq!(load2.part, Part::text("dark mode"));

        // Retrieving from session 2 fails for session 1 scoped
        let res = service
            .load(LoadRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_2.clone(),
                file_name: "chat_history.txt".to_string(),
                version: None,
            })
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_in_memory_artifact_service_versions() {
        let service = InMemoryArtifactService::new();
        let app_name = "test_app".to_string();
        let user_id = UserId::new("user_1").unwrap();
        let session_id = SessionId::new("session_1").unwrap();

        for i in 1..=3 {
            service
                .save(SaveRequest {
                    app_name: app_name.clone(),
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    file_name: "doc.txt".to_string(),
                    version: None,
                    part: Part::text(format!("v{}", i)),
                })
                .await
                .unwrap();
        }

        // Implicitly loads latest (v3)
        let load_latest = service
            .load(LoadRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                file_name: "doc.txt".to_string(),
                version: None,
            })
            .await
            .unwrap();
        assert_eq!(load_latest.part, Part::text("v3"));

        // Explicitly load v1
        let load_v1 = service
            .load(LoadRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                file_name: "doc.txt".to_string(),
                version: Some(1),
            })
            .await
            .unwrap();
        assert_eq!(load_v1.part, Part::text("v1"));

        // Versions list (descending)
        let vers = service
            .versions(VersionsRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                file_name: "doc.txt".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(vers.versions, vec![3, 2, 1]);
    }
}
