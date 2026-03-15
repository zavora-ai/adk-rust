use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use tar::Builder;
use tracing::info;

use crate::{DeployError, DeployResult, DeploymentManifest};

#[derive(Debug, Clone)]
pub struct BundleArtifact {
    pub bundle_path: PathBuf,
    pub checksum_sha256: String,
    pub binary_path: PathBuf,
}

pub struct BundleBuilder {
    manifest_path: PathBuf,
    manifest: DeploymentManifest,
}

impl BundleBuilder {
    pub fn new(manifest_path: impl Into<PathBuf>, manifest: DeploymentManifest) -> Self {
        Self { manifest_path: manifest_path.into(), manifest }
    }

    pub fn build(&self) -> DeployResult<BundleArtifact> {
        self.manifest.validate()?;
        let project_dir = self.manifest_path.parent().ok_or_else(|| DeployError::BundleBuild {
            message: "manifest path has no parent directory".to_string(),
        })?;
        let canonical_project_dir = project_dir.canonicalize()?;

        info!(agent.name = %self.manifest.agent.name, "building deployment bundle");

        let mut build = Command::new("cargo");
        build.current_dir(project_dir).arg("build");
        match self.manifest.build.profile.as_str() {
            "release" => {
                build.arg("--release");
            }
            profile => {
                build.arg("--profile").arg(profile);
            }
        }
        build.arg("--bin").arg(&self.manifest.agent.binary);
        if let Some(target) = &self.manifest.build.target {
            build.arg("--target").arg(target);
        }
        if !self.manifest.build.features.is_empty() {
            build.arg("--features").arg(self.manifest.build.features.join(","));
        }

        let output = build.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DeployError::BundleBuild { message: stderr });
        }

        let binary_path = self.resolve_binary_path(project_dir)?;
        if !binary_path.exists() {
            return Err(DeployError::BundleBuild {
                message: format!("expected compiled binary at {}", binary_path.display()),
            });
        }

        let dist_dir = project_dir.join(".adk-deploy").join("dist");
        fs::create_dir_all(&dist_dir)?;
        let timestamp = Utc::now().format("%Y%m%d%H%M%S");
        let archive_name = format!("{}-{}.tar.gz", self.manifest.agent.name, timestamp);
        let bundle_path = dist_dir.join(archive_name);

        let file = fs::File::create(&bundle_path)?;
        let encoder = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(encoder);
        tar.append_path_with_name(&binary_path, format!("bin/{}", self.manifest.agent.binary))?;
        tar.append_path_with_name(&self.manifest_path, "adk-deploy.toml")?;

        for asset in &self.manifest.build.assets {
            if let Some((source, archive_name)) = resolve_asset_path(&canonical_project_dir, asset)?
            {
                tar.append_path_with_name(&source, Path::new("assets").join(archive_name))?;
            }
        }

        tar.finish()?;
        let encoder = tar.into_inner()?;
        let _file = encoder.finish()?;

        let checksum_sha256 = checksum_file(&bundle_path)?;
        let sums_path =
            dist_dir.join(format!("{}.sha256", bundle_path.file_name().unwrap().to_string_lossy()));
        fs::write(&sums_path, format!("{checksum_sha256}  {}\n", bundle_path.display()))?;

        Ok(BundleArtifact { bundle_path, checksum_sha256, binary_path })
    }

    fn resolve_binary_path(&self, project_dir: &Path) -> DeployResult<PathBuf> {
        let profile = if self.manifest.build.profile == "release" {
            "release".to_string()
        } else {
            self.manifest.build.profile.clone()
        };

        let mut path = project_dir.join("target");
        if let Some(target) = &self.manifest.build.target {
            path = path.join(target);
        }
        path = path.join(profile).join(binary_name(&self.manifest.agent.binary));
        Ok(path)
    }
}

fn binary_name(binary: &str) -> String {
    if cfg!(windows) { format!("{binary}.exe") } else { binary.to_string() }
}

fn checksum_file(path: &Path) -> DeployResult<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn resolve_asset_path(project_dir: &Path, asset: &str) -> DeployResult<Option<(PathBuf, PathBuf)>> {
    let source = project_dir.join(asset);
    if !source.exists() {
        return Ok(None);
    }
    let canonical_source = source.canonicalize()?;
    let relative = canonical_source
        .strip_prefix(project_dir)
        .map_err(|_| DeployError::BundleBuild {
            message: format!("asset path escapes project directory: {asset}"),
        })?
        .to_path_buf();
    Ok(Some((canonical_source, relative)))
}

#[cfg(test)]
mod tests {
    use super::resolve_asset_path;
    use tempfile::tempdir;

    #[test]
    fn rejects_asset_paths_outside_project_root() {
        let project = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let escaped = outside.path().join("secret.txt");
        std::fs::write(&escaped, "secret").unwrap();

        let result = resolve_asset_path(project.path(), escaped.to_str().unwrap());
        assert!(result.is_err());
    }
}
