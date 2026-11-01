use crate::service::*;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Part, Result};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Directory of user-scoped artifacts. It shares the reserved session id's name, so no
/// session directory can collide with it.
const USER_SCOPE_DIR: &str = USER_SCOPE_SESSION_ID;

/// Directory earlier releases kept user-scoped artifacts in, still read so they stay loadable.
const LEGACY_USER_SCOPE_DIR: &str = "_user_scoped_";

/// Prefix of a save's temporary file, which never parses as a version file.
const TEMP_PREFIX: &str = ".save-";

/// How many versions taken by concurrent saves one save steps past before it gives up.
const MAX_SAVE_ATTEMPTS: usize = 1024;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The error for an artifact whose resolved path leaves the base directory.
fn path_escapes() -> AdkError {
    AdkError::new(
        ErrorComponent::Artifact,
        ErrorCategory::InvalidInput,
        "artifact.path_escapes",
        "the artifact path resolves outside the configured base directory",
    )
}

/// A filesystem failure under the base directory; `operation` names what failed.
fn io_error(operation: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::internal(
        ErrorComponent::Artifact,
        "artifact.file.io",
        format!("{operation}: {error}"),
    )
}

/// Encodes an artifact name as one directory name.
///
/// `%`, and the characters Windows forbids in file names (`<>:"|?*` and ASCII control
/// characters), become `%XX` escapes. So does the second `_` of every `__`, so no encoded
/// name contains `__`, the marker of the encoding earlier releases used (`:` as `__`).
/// Distinct names therefore always get distinct directories.
fn encode_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    let mut after_underscore = false;
    for c in name.chars() {
        let escape = match c {
            '%' | '<' | '>' | ':' | '"' | '|' | '?' | '*' => true,
            '_' => after_underscore,
            other => other.is_ascii_control(),
        };
        if escape {
            // Every escaped character is ASCII, so it fits in two hex digits.
            let _ = write!(encoded, "%{:02X}", u32::from(c));
            after_underscore = false;
        } else {
            encoded.push(c);
            after_underscore = c == '_';
        }
    }
    encoded
}

/// The directory name earlier releases stored `name` under.
fn legacy_encode_name(name: &str) -> String {
    name.replace(':', "__")
}

/// Recovers the artifact name from a directory written by this or an earlier release.
fn decode_name(dir_name: &str) -> String {
    if dir_name.contains("__") {
        // Only the earlier encoding produces `__`; it stored `:` that way.
        return dir_name.replace("__", ":");
    }
    percent_decode(dir_name).unwrap_or_else(|| dir_name.to_string())
}

fn percent_decode(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            decoded.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn version_file_name(version: i64) -> String {
    format!("v{version}.json")
}

/// The directories one artifact's versions live in.
struct ArtifactDirs {
    /// Where saves write.
    primary: PathBuf,
    /// Where an earlier release stored the same artifact, when that differs.
    legacy: Option<PathBuf>,
}

impl ArtifactDirs {
    fn all(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.primary).chain(self.legacy.as_ref())
    }
}

/// Persist artifacts on the local filesystem.
///
/// The base directory is created and canonicalized at construction time. Each version is
/// one JSON file:
///
/// - Session-scoped: `{app_name}/{user_id}/{session_id}/{name}/v{version}.json`
/// - User-scoped (`user:` names): `{app_name}/{user_id}/user/{name}/v{version}.json`
///
/// `{name}` is the artifact name with `%`, `:`, the other characters Windows forbids in file
/// names, and the second `_` of every `__` written as `%XX` escapes, so distinct names never
/// share a directory.
///
/// A save writes the version to a temporary file, flushes it to disk, and links it into
/// place without replacing an existing file. A crash mid-save leaves the earlier versions
/// intact, and concurrent saves each claim their own version. On filesystems without hard
/// links (FAT, exFAT) the version file is created exclusively and written in place instead.
///
/// Artifacts stored by earlier releases, which wrote `:` as `__` and kept user-scoped
/// artifacts under `_user_scoped_`, are still loaded, listed, versioned, and deleted; new
/// versions of them are written to the current layout.
///
/// # Example
///
/// ```rust,no_run
/// use adk_artifact::{ArtifactService, FileArtifactService, SaveRequest};
/// use adk_core::Part;
///
/// # async fn example() -> adk_core::Result<()> {
/// let service = FileArtifactService::new("./artifacts")?;
/// let saved = service
///     .save(SaveRequest {
///         app_name: "my_app".to_string(),
///         user_id: "user_123".to_string(),
///         session_id: "session_456".to_string(),
///         file_name: "report:q1".to_string(),
///         part: Part::Text { text: "quarterly numbers".to_string() },
///         version: None,
///     })
///     .await?;
/// assert_eq!(saved.version, 1);
/// # Ok(())
/// # }
/// ```
pub struct FileArtifactService {
    /// Canonical (absolute, resolved) base directory. Set once at construction.
    base_dir: PathBuf,
}

impl FileArtifactService {
    /// Create a new filesystem-backed artifact service rooted at `base_dir`.
    ///
    /// Creates the directory if it doesn't exist and stores the canonical path.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or canonicalized.
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self> {
        let raw = base_dir.into();
        std::fs::create_dir_all(&raw).map_err(|e| io_error("create base dir failed", e))?;
        let canonical =
            raw.canonicalize().map_err(|e| io_error("canonicalize base dir failed", e))?;
        Ok(Self { base_dir: canonical })
    }

    fn validate_file_name(file_name: &str) -> Result<()> {
        if file_name.is_empty() {
            return Err(invalid_file_name(file_name, "the name is empty"));
        }

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

    /// Validates a path component (app_name, user_id, session_id) used to build artifact paths.
    ///
    /// Rejects empty values, directory separators, and traversal patterns.
    fn validate_path_component(component: &str, field: &str) -> Result<()> {
        if component.is_empty() {
            return Err(invalid_identifier(field, component, "the value is empty"));
        }

        if component.contains('/')
            || component.contains('\\')
            || component == "."
            || component == ".."
            || component.contains("..")
        {
            return Err(invalid_identifier(
                field,
                component,
                "path separators and traversal patterns are not allowed",
            ));
        }

        Ok(())
    }

    /// Validates the app, user, and session components and returns the user's directory.
    fn user_root(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<PathBuf> {
        Self::validate_path_component(app_name, "app name")?;
        Self::validate_path_component(user_id, "user id")?;
        Self::validate_path_component(session_id, "session id")?;
        check_session_id(session_id)?;
        Ok(self.base_dir.join(app_name).join(user_id))
    }

    /// Ensures the given path stays within the configured base directory.
    fn ensure_within_base_dir(&self, path: &Path) -> Result<()> {
        let canonical_base = self
            .base_dir
            .canonicalize()
            .map_err(|e| io_error("canonicalize base dir failed", e))?;

        // For paths that may not exist yet, resolve relative to canonical base
        let canonical_path = match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(_) => {
                let relative = path.strip_prefix(&self.base_dir).unwrap_or(path);
                canonical_base.join(relative)
            }
        };

        if !canonical_path.starts_with(&canonical_base) {
            return Err(path_escapes());
        }

        Ok(())
    }

    fn is_user_scoped(file_name: &str) -> bool {
        file_name.starts_with("user:")
    }

    /// Builds the directories of one artifact from validated components.
    ///
    /// Both returned paths are verified to be under `self.base_dir`.
    fn artifact_dirs(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        file_name: &str,
    ) -> Result<ArtifactDirs> {
        let user_root = self.user_root(app_name, user_id, session_id)?;
        Self::validate_file_name(file_name)?;

        let (scope, legacy_scope) = if Self::is_user_scoped(file_name) {
            (USER_SCOPE_DIR, LEGACY_USER_SCOPE_DIR)
        } else {
            (session_id, session_id)
        };
        let primary = user_root.join(scope).join(encode_name(file_name));
        let legacy = user_root.join(legacy_scope).join(legacy_encode_name(file_name));
        let dirs = ArtifactDirs { legacy: (legacy != primary).then_some(legacy), primary };
        for dir in dirs.all() {
            self.ensure_within_base_dir(dir)?;
        }
        Ok(dirs)
    }

    async fn versions_in(dir: &Path) -> Result<Vec<i64>> {
        let mut entries = match fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(io_error("read dir failed", error));
            }
        };

        let mut versions = Vec::new();
        while let Some(entry) =
            entries.next_entry().await.map_err(|e| io_error("read dir entry failed", e))?
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
        Ok(versions)
    }

    /// Every stored version of an artifact, across the current and the legacy layout.
    async fn stored_versions(dirs: &ArtifactDirs) -> Result<BTreeSet<i64>> {
        let mut versions = BTreeSet::new();
        for dir in dirs.all() {
            versions.extend(Self::versions_in(dir).await?);
        }
        Ok(versions)
    }

    /// The file holding `version`, preferring the current layout.
    async fn version_path(dirs: &ArtifactDirs, version: i64) -> Option<PathBuf> {
        for dir in dirs.all() {
            let path = dir.join(version_file_name(version));
            if fs::try_exists(&path).await.unwrap_or(false) {
                return Some(path);
            }
        }
        None
    }

    /// Lists the artifact names stored in a scope directory that satisfy `keep`.
    async fn list_scope_dir(path: &Path, keep: fn(&str) -> bool) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let mut entries = match fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(names),
            Err(error) => {
                return Err(io_error("read dir failed", error));
            }
        };

        while let Some(entry) =
            entries.next_entry().await.map_err(|e| io_error("read dir entry failed", e))?
        {
            if entry.file_type().await.map_err(|e| io_error("file type check failed", e))?.is_dir()
                && let Some(dir_name) = entry.file_name().to_str()
            {
                let name = decode_name(dir_name);
                if keep(&name) {
                    names.insert(name);
                }
            }
        }

        Ok(names)
    }

    /// Every version file of a user: its session (`None` for the `user:` scope), artifact
    /// name, version, and path. An empty list when the user has no directory.
    async fn user_version_files(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> Result<Vec<(Option<String>, String, i64, PathBuf)>> {
        Self::validate_path_component(app_name, "app name")?;
        Self::validate_path_component(user_id, "user id")?;
        let user_root = self.base_dir.join(app_name).join(user_id);
        self.ensure_within_base_dir(&user_root)?;
        let mut found = Vec::new();
        for scope in Self::subdirectories(&user_root).await? {
            let scope_dir = user_root.join(&scope);
            for dir_name in Self::subdirectories(&scope_dir).await? {
                let name = decode_name(&dir_name);
                let session = match scope.as_str() {
                    USER_SCOPE_DIR => None,
                    LEGACY_USER_SCOPE_DIR if Self::is_user_scoped(&name) => None,
                    _ => Some(scope.clone()),
                };
                let artifact_dir = scope_dir.join(&dir_name);
                let mut versions = Self::versions_in(&artifact_dir).await?;
                versions.sort_unstable();
                for version in versions {
                    let path = artifact_dir.join(version_file_name(version));
                    found.push((session.clone(), name.clone(), version, path));
                }
            }
        }
        found.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
        Ok(found)
    }

    /// The names of the directories in `dir`; empty when `dir` does not exist.
    async fn subdirectories(dir: &Path) -> Result<Vec<String>> {
        let mut entries = match fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error("read dir failed", error)),
        };
        let mut names = Vec::new();
        while let Some(entry) =
            entries.next_entry().await.map_err(|e| io_error("read dir entry failed", e))?
        {
            if entry.file_type().await.map_err(|e| io_error("file type check failed", e))?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    /// Writes `payload` to a new file and flushes it to disk.
    async fn write_synced(path: &Path, payload: &[u8]) -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).await?;
        file.write_all(payload).await?;
        file.sync_all().await
    }

    /// Publishes the fully written `temp` file as `target` unless `target` already exists.
    ///
    /// Returns `false` when `target` exists.
    async fn publish(temp: &Path, target: &Path, payload: &[u8]) -> std::io::Result<bool> {
        match fs::hard_link(temp, target).await {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            // Filesystems without hard links: claim the name exclusively, then write in place.
            Err(_) => match Self::write_synced(target, payload).await {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            },
        }
    }

    /// Links the temporary file to the first free version, starting at the requested one.
    async fn claim_version(
        dirs: &ArtifactDirs,
        dir: &Path,
        temp: &Path,
        payload: &[u8],
        req: &SaveRequest,
    ) -> Result<i64> {
        let mut version = match req.version {
            Some(version) => {
                if let Some(legacy) = &dirs.legacy
                    && fs::try_exists(legacy.join(version_file_name(version)))
                        .await
                        .unwrap_or(false)
                {
                    return Err(version_exists(&req.file_name, version));
                }
                version
            }
            None => Self::stored_versions(dirs).await?.last().map_or(1, |latest| latest + 1),
        };
        for _ in 0..MAX_SAVE_ATTEMPTS {
            let target = dir.join(version_file_name(version));
            let published = Self::publish(temp, &target, payload)
                .await
                .map_err(|e| io_error("write failed", e))?;
            if published {
                return Ok(version);
            }
            if req.version.is_some() {
                return Err(version_exists(&req.file_name, version));
            }
            version += 1;
        }
        Err(AdkError::new(
            ErrorComponent::Artifact,
            ErrorCategory::Conflict,
            "artifact.save_contended",
            format!(
                "artifact '{}' gained {MAX_SAVE_ATTEMPTS} versions while this save ran; retry the save",
                req.file_name
            ),
        ))
    }
}

/// Flushes a directory entry change to disk where the platform supports it.
async fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir).await {
        // Best effort: the version is already visible, so a failed flush does not fail the save.
        let _ = handle.sync_all().await;
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[async_trait]
impl ArtifactService for FileArtifactService {
    async fn list_user(&self, app_name: &str, user_id: &str) -> Result<Vec<UserArtifact>> {
        let mut artifacts = Vec::new();
        for (session_id, file_name, version, path) in
            self.user_version_files(app_name, user_id).await?
        {
            let bytes = fs::read(&path).await.map_err(|e| io_error("read failed", e))?;
            let part: Part = serde_json::from_slice(&bytes)
                .map_err(|error| io_error("decode artifact failed", error))?;
            let (mime_type, size) = part_summary(&part);
            artifacts.push(UserArtifact { session_id, file_name, version, mime_type, size });
        }
        Ok(artifacts)
    }

    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<u64> {
        let versions = self.user_version_files(app_name, user_id).await?.len() as u64;
        match fs::remove_dir_all(self.base_dir.join(app_name).join(user_id)).await {
            Ok(()) => Ok(versions),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
            Err(error) => Err(io_error("delete user dir failed", error)),
        }
    }

    async fn save(&self, req: SaveRequest) -> Result<SaveResponse> {
        let dirs =
            self.artifact_dirs(&req.app_name, &req.user_id, &req.session_id, &req.file_name)?;
        let payload = serde_json::to_vec(&req.part)
            .map_err(|error| io_error("encode artifact failed", error))?;

        fs::create_dir_all(&dirs.primary).await.map_err(|e| io_error("create dir failed", e))?;

        // Final canonicalization check after directory exists
        let verified_dir = dirs
            .primary
            .canonicalize()
            .map_err(|e| io_error("canonicalize artifact dir failed", e))?;
        if !verified_dir.starts_with(&self.base_dir) {
            return Err(path_escapes());
        }

        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let temp = verified_dir.join(format!(
            "{TEMP_PREFIX}{}-{nonce}-{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(error) = Self::write_synced(&temp, &payload).await {
            let _ = fs::remove_file(&temp).await;
            return Err(io_error("write failed", error));
        }
        let claimed = Self::claim_version(&dirs, &verified_dir, &temp, &payload, &req).await;
        let _ = fs::remove_file(&temp).await;
        let version = claimed?;
        sync_dir(&verified_dir).await;

        Ok(SaveResponse { version })
    }

    async fn load(&self, req: LoadRequest) -> Result<LoadResponse> {
        let dirs =
            self.artifact_dirs(&req.app_name, &req.user_id, &req.session_id, &req.file_name)?;
        let version = match req.version {
            Some(version) => version,
            None => *Self::stored_versions(&dirs)
                .await?
                .last()
                .ok_or_else(|| artifact_not_found(&req.file_name, None))?,
        };

        let path = Self::version_path(&dirs, version)
            .await
            .ok_or_else(|| artifact_not_found(&req.file_name, req.version))?;
        let payload = fs::read(&path).await.map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                artifact_not_found(&req.file_name, req.version)
            } else {
                io_error("read failed", error)
            }
        })?;

        let part = serde_json::from_slice::<Part>(&payload).map_err(|error| {
            AdkError::internal(
                ErrorComponent::Artifact,
                "artifact.file.corrupt",
                format!(
                    "artifact '{}' version {version} is not a stored artifact: {error}",
                    req.file_name
                ),
            )
        })?;

        Ok(LoadResponse { part })
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let dirs =
            self.artifact_dirs(&req.app_name, &req.user_id, &req.session_id, &req.file_name)?;
        for dir in dirs.all() {
            let removed = match req.version {
                Some(version) => fs::remove_file(dir.join(version_file_name(version))).await,
                None => fs::remove_dir_all(dir).await,
            };
            match removed {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(io_error("delete failed", error));
                }
            }
        }

        Ok(())
    }

    async fn list(&self, req: ListRequest) -> Result<ListResponse> {
        let user_root = self.user_root(&req.app_name, &req.user_id, &req.session_id)?;
        let session_dir = user_root.join(&req.session_id);
        self.ensure_within_base_dir(&session_dir)?;

        // A session directory holds only session-scoped names; the user scope directories
        // only `user:` names.
        let mut names =
            Self::list_scope_dir(&session_dir, |name| !Self::is_user_scoped(name)).await?;
        for scope in [USER_SCOPE_DIR, LEGACY_USER_SCOPE_DIR] {
            let user_dir = user_root.join(scope);
            self.ensure_within_base_dir(&user_dir)?;
            names.extend(Self::list_scope_dir(&user_dir, Self::is_user_scoped).await?);
        }

        Ok(ListResponse { file_names: names.into_iter().collect() })
    }

    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse> {
        let dirs =
            self.artifact_dirs(&req.app_name, &req.user_id, &req.session_id, &req.file_name)?;
        let versions: Vec<i64> = Self::stored_versions(&dirs).await?.into_iter().rev().collect();
        if versions.is_empty() {
            return Err(artifact_not_found(&req.file_name, None));
        }
        Ok(VersionsResponse { versions })
    }

    async fn health_check(&self) -> Result<()> {
        fs::create_dir_all(&self.base_dir).await.map_err(|e| io_error("health check failed", e))?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = self.base_dir.join(format!(".healthcheck-{nonce}"));
        fs::write(&path, b"ok").await.map_err(|e| io_error("health check failed", e))?;
        fs::remove_file(path).await.map_err(|e| io_error("health check failed", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn user_scoped_artifacts_are_visible_across_sessions() {
        let tempdir = tempfile::tempdir().unwrap();
        let service = FileArtifactService::new(tempdir.path()).unwrap();

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

    #[test]
    fn name_encoding_is_injective_and_round_trips() {
        let names = [
            "a:b",
            "a__b",
            "a_b",
            "a%3Ab",
            "a___b",
            "_",
            "__",
            "___",
            "a_:b",
            "50%.txt",
            "what?.txt",
            "q<1>|\"*\"",
            "tab\tname",
            "user:prefs",
            "plain.txt",
            "日本:語",
        ];
        let encoded: BTreeSet<String> = names.iter().map(|name| encode_name(name)).collect();
        assert_eq!(encoded.len(), names.len(), "{encoded:?}");
        for name in names {
            let dir = encode_name(name);
            assert!(!dir.contains("__"), "{name} encoded as {dir}");
            assert!(!dir.contains(['<', '>', ':', '"', '|', '?', '*', '\t']), "{dir}");
            assert_eq!(decode_name(&dir), name);
        }
    }

    #[test]
    fn legacy_directory_names_decode_as_before() {
        assert_eq!(decode_name("user__prefs"), "user:prefs");
        assert_eq!(decode_name("report__q1"), "report:q1");
        assert_eq!(decode_name("plain.txt"), "plain.txt");
        // A literal `%` that is not an escape stays as written.
        assert_eq!(decode_name("50%off"), "50%off");
    }
}
