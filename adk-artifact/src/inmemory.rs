use crate::service::*;
use adk_core::{Part, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const USER_SCOPED_KEY: &str = USER_SCOPE_SESSION_ID;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ArtifactKey {
    app_name: String,
    user_id: String,
    session_id: String,
    file_name: String,
    version: i64,
}

/// In-memory artifact storage for development and testing.
///
/// Artifacts are stored in a `HashMap` behind an `RwLock`. Data is lost
/// when the process exits. For persistent storage, use [`FileArtifactService`](crate::FileArtifactService).
pub struct InMemoryArtifactService {
    artifacts: Arc<RwLock<HashMap<ArtifactKey, Part>>>,
}

impl InMemoryArtifactService {
    /// Create a new empty in-memory artifact store.
    pub fn new() -> Self {
        Self { artifacts: Arc::new(RwLock::new(HashMap::new())) }
    }

    fn is_user_scoped(file_name: &str) -> bool {
        file_name.starts_with("user:")
    }

    fn get_session_id(session_id: &str, file_name: &str) -> String {
        if Self::is_user_scoped(file_name) {
            USER_SCOPED_KEY.to_string()
        } else {
            session_id.to_string()
        }
    }

    fn validate_file_name(file_name: &str) -> Result<()> {
        if file_name.is_empty() {
            return Err(invalid_file_name(file_name, "the name is empty"));
        }

        // Prevent path traversal and path-like names; artifacts are logical keys, not paths.
        if file_name.contains('/')
            || file_name.contains('\\')
            || file_name == "."
            || file_name == ".."
            || file_name.contains("..")
        {
            return Err(invalid_file_name(
                file_name,
                "path separators and traversal patterns are not allowed",
            ));
        }

        Ok(())
    }

    fn find_latest_version(
        artifacts: &HashMap<ArtifactKey, Part>,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Option<(i64, Part)> {
        artifacts
            .iter()
            .filter(|(k, _)| {
                k.app_name == app_name
                    && k.user_id == user_id
                    && k.session_id == session_id
                    && k.file_name == file_name
            })
            .max_by_key(|(k, _)| k.version)
            .map(|(k, v)| (k.version, v.clone()))
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
        check_session_id(&req.session_id)?;
        let session_id = Self::get_session_id(&req.session_id, &req.file_name);

        // One write lock spans choosing the version and inserting it, so concurrent saves
        // never pick the same version.
        let mut artifacts = self.artifacts.write().unwrap();
        let mut key = ArtifactKey {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            file_name: req.file_name,
            version: 0,
        };
        key.version = match req.version {
            Some(version) => version,
            None => artifacts
                .keys()
                .filter(|k| {
                    k.app_name == key.app_name
                        && k.user_id == key.user_id
                        && k.session_id == key.session_id
                        && k.file_name == key.file_name
                })
                .map(|k| k.version)
                .max()
                .map_or(1, |latest| latest + 1),
        };
        if artifacts.contains_key(&key) {
            return Err(version_exists(&key.file_name, key.version));
        }
        let version = key.version;
        artifacts.insert(key, req.part);

        Ok(SaveResponse { version })
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        Self::validate_file_name(&req.file_name)?;
        check_session_id(&req.session_id)?;
        let session_id = Self::get_session_id(&req.session_id, &req.file_name);

        if let Some(version) = req.version {
            let key = ArtifactKey {
                app_name: req.app_name,
                user_id: req.user_id,
                session_id,
                file_name: req.file_name,
                version,
            };

            let artifacts = self.artifacts.read().unwrap();
            let part = artifacts
                .get(&key)
                .ok_or_else(|| artifact_not_found(&key.file_name, Some(version)))?;

            Ok(LoadResponse { part: part.clone() })
        } else {
            let artifacts = self.artifacts.read().unwrap();
            let (_, part) = Self::find_latest_version(
                &artifacts,
                &req.app_name,
                &req.user_id,
                &session_id,
                &req.file_name,
            )
            .ok_or_else(|| artifact_not_found(&req.file_name, None))?;

            Ok(LoadResponse { part })
        }
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        Self::validate_file_name(&req.file_name)?;
        check_session_id(&req.session_id)?;
        let session_id = Self::get_session_id(&req.session_id, &req.file_name);

        let mut artifacts = self.artifacts.write().unwrap();

        if let Some(version) = req.version {
            let key = ArtifactKey {
                app_name: req.app_name,
                user_id: req.user_id,
                session_id,
                file_name: req.file_name,
                version,
            };
            artifacts.remove(&key);
        } else {
            artifacts.retain(|k, _| {
                !(k.app_name == req.app_name
                    && k.user_id == req.user_id
                    && k.session_id == session_id
                    && k.file_name == req.file_name)
            });
        }

        Ok(())
    }

    async fn list(&self, req: ListRequest) -> Result<ListResponse> {
        check_session_id(&req.session_id)?;
        let artifacts = self.artifacts.read().unwrap();
        let mut file_names = std::collections::HashSet::new();

        for key in artifacts.keys() {
            if key.app_name == req.app_name
                && key.user_id == req.user_id
                && (key.session_id == req.session_id || key.session_id == USER_SCOPED_KEY)
            {
                file_names.insert(key.file_name.clone());
            }
        }

        let mut sorted: Vec<_> = file_names.into_iter().collect();
        sorted.sort();

        Ok(ListResponse { file_names: sorted })
    }

    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse> {
        Self::validate_file_name(&req.file_name)?;
        check_session_id(&req.session_id)?;
        let session_id = Self::get_session_id(&req.session_id, &req.file_name);
        let artifacts = self.artifacts.read().unwrap();

        let mut versions: Vec<i64> = artifacts
            .keys()
            .filter(|k| {
                k.app_name == req.app_name
                    && k.user_id == req.user_id
                    && k.session_id == session_id
                    && k.file_name == req.file_name
            })
            .map(|k| k.version)
            .collect();

        if versions.is_empty() {
            return Err(artifact_not_found(&req.file_name, None));
        }

        versions.sort_by(|a, b| b.cmp(a));

        Ok(VersionsResponse { versions })
    }
}
