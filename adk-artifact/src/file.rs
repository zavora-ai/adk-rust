use crate::service::*;
use adk_core::{Part, Result};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

const USER_SCOPED_DIR: &str = "_user_scoped_";

/// Persist artifacts on the local filesystem.
pub struct FileArtifactService {
    base_dir: PathBuf,
}

impl FileArtifactService {
    /// Create a new filesystem-backed artifact service rooted at `base_dir`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into() }
    }

    fn validate_file_name(file_name: &str) -> Result<()> {
        if file_name.is_empty() {
            return Err(adk_core::AdkError::artifact("invalid artifact file name: empty name"));
        }

        if file_name.contains('/')
            || file_name.contains('\\')
            || file_name == "."
            || file_name == ".."
            || file_name.contains("..")
        {
            return Err(adk_core::AdkError::artifact(format!(
                "invalid artifact file name '{}': path separators and traversal patterns are not allowed",
                file_name
            )));
        }

        Ok(())
    }

    /// Validates a path component (app_name, user_id, session_id) used to build artifact paths.
    ///
    /// Rejects empty values, directory separators, and traversal patterns.
    fn validate_path_component(component: &str, field: &str) -> Result<()> {
        if component.is_empty() {
            return Err(adk_core::AdkError::artifact(format!(
                "invalid artifact {field}: empty value"
            )));
        }

        if component.contains('/')
            || component.contains('\\')
            || component == "."
            || component == ".."
            || component.contains("..")
        {
            return Err(adk_core::AdkError::artifact(format!(
                "invalid artifact {field} '{component}': path separators and traversal patterns are not allowed"
            )));
        }

        Ok(())
    }

    /// Ensures the given path stays within the configured base directory.
    fn ensure_within_base_dir(&self, path: &Path) -> Result<()> {
        let canonical_base = self.base_dir.canonicalize().map_err(|e| {
            adk_core::AdkError::artifact(format!("canonicalize base dir failed: {e}"))
        })?;

        // For paths that may not exist yet, resolve relative to canonical base
        let canonical_path = match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(_) => {
                let relative = path.strip_prefix(&self.base_dir).unwrap_or(path);
                canonical_base.join(relative)
            }
        };

        if !canonical_path.starts_with(&canonical_base) {
            return Err(adk_core::AdkError::artifact(
                "artifact path escapes configured base directory",
            ));
        }

        Ok(())
    }

    fn is_user_scoped(file_name: &str) -> bool {
        file_name.starts_with("user:")
    }

    /// Build a safe artifact directory path from validated components.
    ///
    /// All components must pass `validate_path_component` before calling this.
    /// The returned path is guaranteed to be under `self.base_dir`.
    fn safe_artifact_dir(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<PathBuf> {
        Self::validate_path_component(app_name, "app name")?;
        Self::validate_path_component(user_id, "user id")?;
        Self::validate_path_component(session_id, "session id")?;
        Self::validate_file_name(file_name)?;

        let dir = if Self::is_user_scoped(file_name) {
            self.base_dir.join(app_name).join(user_id).join(USER_SCOPED_DIR).join(file_name)
        } else {
            self.base_dir.join(app_name).join(user_id).join(session_id).join(file_name)
        };

        // Verify the constructed path hasn't escaped base_dir
        self.ensure_within_base_dir(&dir)?;
        Ok(dir)
    }

    /// Build a safe version file path from validated components.
    fn safe_version_path(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
        version: i64,
    ) -> Result<PathBuf> {
        let dir = self.safe_artifact_dir(app_name, user_id, session_id, file_name)?;
        let path = dir.join(format!("v{version}.json"));
        Ok(path)
    }

    async fn read_versions(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<Vec<i64>> {
        let dir = self.safe_artifact_dir(app_name, user_id, session_id, file_name)?;
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(adk_core::AdkError::artifact("artifact not found"));
            }
            Err(error) => {
                return Err(adk_core::AdkError::artifact(format!("read dir failed: {error}")));
            }
        };

        let mut versions = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("read dir entry failed: {e}")))?
        {
            let Some(file_name) = entry.file_name().to_str().map(ToString::to_string) else {
                continue;
            };
            let Some(raw) =
                file_name.strip_prefix('v').and_then(|value| value.strip_suffix(".json"))
            else {
                continue;
            };
            if let Ok(version) = raw.parse::<i64>() {
                versions.push(version);
            }
        }

        if versions.is_empty() {
            return Err(adk_core::AdkError::artifact("artifact not found"));
        }

        versions.sort_by(|left, right| right.cmp(left));
        Ok(versions)
    }

    async fn list_scope_dir(path: &Path) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let mut entries = match fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
            Err(error) => {
                return Err(adk_core::AdkError::artifact(format!("read dir failed: {error}")));
            }
        };

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("read dir entry failed: {e}")))?
        {
            if entry
                .file_type()
                .await
                .map_err(|e| adk_core::AdkError::artifact(format!("file type check failed: {e}")))?
                .is_dir()
            {
                if let Some(name) = entry.file_name().to_str() {
                    names.insert(name.to_string());
                }
            }
        }

        Ok(names)
    }
}

#[async_trait]
impl ArtifactService for FileArtifactService {
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse> {
        let version = match req.version {
            Some(version) => version,
            None => self
                .read_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
                .await
                .map(|versions| versions[0] + 1)
                .unwrap_or(1),
        };

        // Validate all components reject traversal patterns
        Self::validate_path_component(&req.app_name, "app name")?;
        Self::validate_path_component(&req.user_id, "user id")?;
        Self::validate_path_component(&req.session_id, "session id")?;
        Self::validate_file_name(&req.file_name)?;

        // Bootstrap base_dir synchronously so we can canonicalize it.
        // base_dir is configuration-controlled, not direct user input per-request.
        std::fs::create_dir_all(&self.base_dir)
            .map_err(|e| adk_core::AdkError::artifact(format!("create base dir failed: {e}")))?;
        let canonical_base = self.base_dir.canonicalize().map_err(|e| {
            adk_core::AdkError::artifact(format!("canonicalize base dir failed: {e}"))
        })?;

        // Build path from canonical base + validated segments (no user data in base)
        let canonical_dir = if Self::is_user_scoped(&req.file_name) {
            canonical_base
                .join(&req.app_name)
                .join(&req.user_id)
                .join(USER_SCOPED_DIR)
                .join(&req.file_name)
        } else {
            canonical_base
                .join(&req.app_name)
                .join(&req.user_id)
                .join(&req.session_id)
                .join(&req.file_name)
        };

        fs::create_dir_all(&canonical_dir)
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("create dir failed: {e}")))?;

        // Final canonicalization check after directory exists
        let verified_dir = canonical_dir.canonicalize().map_err(|e| {
            adk_core::AdkError::artifact(format!("canonicalize artifact dir failed: {e}"))
        })?;
        if !verified_dir.starts_with(&canonical_base) {
            return Err(adk_core::AdkError::artifact(
                "artifact path escapes configured base directory",
            ));
        }

        let write_path = verified_dir.join(format!("v{version}.json"));
        let payload = serde_json::to_vec(&req.part)
            .map_err(|error| adk_core::AdkError::artifact(error.to_string()))?;
        fs::write(write_path, payload)
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("write failed: {e}")))?;

        Ok(SaveResponse { version })
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        let version = match req.version {
            Some(version) => version,
            None => {
                self.read_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
                    .await?[0]
            }
        };

        let path = self.safe_version_path(
            &req.app_name,
            &req.user_id,
            &req.session_id,
            &req.file_name,
            version,
        )?;
        let payload = fs::read(&path).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                adk_core::AdkError::artifact("artifact not found")
            } else {
                adk_core::AdkError::artifact(format!("read failed: {error}"))
            }
        })?;

        let part = serde_json::from_slice::<Part>(&payload)
            .map_err(|error| adk_core::AdkError::artifact(error.to_string()))?;

        Ok(LoadResponse { part })
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        if let Some(version) = req.version {
            let path = self.safe_version_path(
                &req.app_name,
                &req.user_id,
                &req.session_id,
                &req.file_name,
                version,
            )?;
            match fs::remove_file(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(adk_core::AdkError::artifact(format!(
                        "remove file failed: {error}"
                    )));
                }
            }
        } else {
            let dir = self.safe_artifact_dir(
                &req.app_name,
                &req.user_id,
                &req.session_id,
                &req.file_name,
            )?;
            match fs::remove_dir_all(dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(adk_core::AdkError::artifact(format!(
                        "remove dir failed: {error}"
                    )));
                }
            }
        }

        Ok(())
    }

    async fn list(&self, req: ListRequest) -> Result<ListResponse> {
        Self::validate_path_component(&req.app_name, "app name")?;
        Self::validate_path_component(&req.user_id, "user id")?;
        Self::validate_path_component(&req.session_id, "session id")?;

        // Build paths from validated components only
        let app = req.app_name.clone();
        let user = req.user_id.clone();
        let session = req.session_id.clone();
        let session_dir = self.base_dir.join(&app).join(&user).join(&session);
        let user_dir = self.base_dir.join(&app).join(&user).join(USER_SCOPED_DIR);

        self.ensure_within_base_dir(&session_dir)?;
        self.ensure_within_base_dir(&user_dir)?;

        let mut names = Self::list_scope_dir(&session_dir).await?;
        names.extend(Self::list_scope_dir(&user_dir).await?);

        Ok(ListResponse { file_names: names.into_iter().collect() })
    }

    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse> {
        // Validation happens inside read_versions → safe_artifact_dir
        let versions = self
            .read_versions(&req.app_name, &req.user_id, &req.session_id, &req.file_name)
            .await?;
        Ok(VersionsResponse { versions })
    }

    async fn health_check(&self) -> Result<()> {
        fs::create_dir_all(&self.base_dir)
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("health check failed: {e}")))?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = self.base_dir.join(format!(".healthcheck-{nonce}"));
        fs::write(&path, b"ok")
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("health check failed: {e}")))?;
        fs::remove_file(path)
            .await
            .map_err(|e| adk_core::AdkError::artifact(format!("health check failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn user_scoped_artifacts_are_visible_across_sessions() {
        let tempdir = tempfile::tempdir().unwrap();
        let service = FileArtifactService::new(tempdir.path());

        service
            .save(SaveRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "s1".into(),
                file_name: "user:shared.txt".into(),
                part: Part::Text { text: "hello".into() },
                version: None,
            })
            .await
            .unwrap();

        let list = service
            .list(ListRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: "s2".into(),
            })
            .await
            .unwrap();

        assert_eq!(list.file_names, vec!["user:shared.txt".to_string()]);
    }
}
