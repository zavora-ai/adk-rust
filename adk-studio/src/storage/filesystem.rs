use crate::schema::{ProjectMeta, ProjectSchema};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

/// File-based project storage
pub struct FileStorage {
    base_dir: PathBuf,
}

impl FileStorage {
    pub async fn new(base_dir: PathBuf) -> Result<Self> {
        let base_dir = base_dir.canonicalize().unwrap_or(base_dir.clone());
        fs::create_dir_all(&base_dir).await?;
        let base_dir = base_dir.canonicalize().unwrap_or(base_dir);
        Ok(Self { base_dir })
    }

    /// Build a safe project path from a UUID, ensuring it stays within base_dir.
    fn project_path(&self, id: Uuid) -> Result<PathBuf> {
        // UUID is guaranteed to be alphanumeric + hyphens, but we validate the
        // resulting path stays within base_dir to satisfy path-injection checks.
        let filename = format!("{}.json", id);
        let path = self.base_dir.join(&filename);
        let canonical = path.canonicalize().unwrap_or_else(|_| self.base_dir.join(&filename));
        if !canonical.starts_with(&self.base_dir) {
            bail!("Path traversal detected for project id {}", id);
        }
        Ok(canonical)
    }

    pub async fn list(&self) -> Result<Vec<ProjectMeta>> {
        let mut projects = Vec::new();
        let mut entries = fs::read_dir(&self.base_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            // Only read files directly inside base_dir (no subdirectories)
            if !path.is_file() {
                continue;
            }
            if let Ok(canonical) = path.canonicalize() {
                if !canonical.starts_with(&self.base_dir) {
                    continue;
                }
            }
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(project) = serde_json::from_str::<ProjectSchema>(&content) {
                        projects.push(ProjectMeta::from(&project));
                    }
                }
            }
        }

        projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(projects)
    }

    pub async fn get(&self, id: Uuid) -> Result<ProjectSchema> {
        let path = self.project_path(id)?;
        let content =
            fs::read_to_string(&path).await.with_context(|| format!("Project {} not found", id))?;
        serde_json::from_str(&content).context("Invalid project format")
    }

    pub async fn save(&self, project: &ProjectSchema) -> Result<()> {
        let path = self.project_path(project.id)?;
        let content = serde_json::to_string_pretty(project)?;
        // Atomic write: write to temp file then rename to avoid corruption on crash
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, content).await?;
        fs::rename(&tmp_path, &path)
            .await
            .with_context(|| format!("Failed to rename temp file to {}", path.display()))?;
        Ok(())
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.project_path(id)?;
        fs::remove_file(&path).await.with_context(|| format!("Project {} not found", id))
    }

    pub async fn exists(&self, id: Uuid) -> bool {
        self.project_path(id).map(|p| p.exists()).unwrap_or(false)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}
