use crate::service::*;
use adk_core::{Part, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const USER_SCOPED_KEY: &str = "user";

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ArtifactKey {
    app_name: String,
    user_id: String,
    session_id: String,
    file_name: String,
    version: i64,
}

pub struct InMemoryArtifactService {
    artifacts: Arc<RwLock<HashMap<ArtifactKey, Part>>>,
}

impl InMemoryArtifactService {
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
            return Err(adk_core::AdkError::Artifact(
                "invalid artifact file name: empty name".to_string(),
            ));
        }

        // Prevent path traversal and path-like names; artifacts are logical keys, not paths.
        if file_name.contains('/')
            || file_name.contains('\\')
            || file_name == "."
            || file_name == ".."
            || file_name.contains("..")
        {
            return Err(adk_core::AdkError::Artifact(format!(
                "invalid artifact file name '{}': path separators and traversal patterns are not allowed",
                file_name
            )));
        }

        Ok(())
    }

    fn find_latest_version(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Option<(i64, Part)> {
        let artifacts = self.artifacts.read().unwrap();
        let mut versions: Vec<_> = artifacts
            .iter()
            .filter(|(k, _)| {
                k.app_name == app_name
                    && k.user_id == user_id
                    && k.session_id == session_id
                    && k.file_name == file_name
            })
            .collect();

        versions.sort_by(|a, b| b.0.version.cmp(&a.0.version));
        versions.first().map(|(k, v)| (k.version, (*v).clone()))
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
        let session_id = Self::get_session_id(&req.session_id, &req.file_name);

        let version = if let Some(v) = req.version {
            v
        } else {
            let latest =
                self.find_latest_version(&req.app_name, &req.user_id, &session_id, &req.file_name);
            latest.map(|(v, _)| v + 1).unwrap_or(1)
        };

        let key = ArtifactKey {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            file_name: req.file_name,
            version,
        };

        let mut artifacts = self.artifacts.write().unwrap();
        artifacts.insert(key, req.part);

        Ok(SaveResponse { version })
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        Self::validate_file_name(&req.file_name)?;
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
                .ok_or_else(|| adk_core::AdkError::Artifact("artifact not found".into()))?;

            Ok(LoadResponse { part: part.clone() })
        } else {
            let (_, part) = self
                .find_latest_version(&req.app_name, &req.user_id, &session_id, &req.file_name)
                .ok_or_else(|| adk_core::AdkError::Artifact("artifact not found".into()))?;

            Ok(LoadResponse { part })
        }
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        Self::validate_file_name(&req.file_name)?;
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
            return Err(adk_core::AdkError::Artifact("artifact not found".into()));
        }

        versions.sort_by(|a, b| b.cmp(a));

        Ok(VersionsResponse { versions })
    }
}
