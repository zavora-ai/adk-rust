//! File and database helpers shared by the harness stores.

use std::path::{Path, PathBuf};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use sha2::{Digest, Sha256};

/// `~/.adk`, the root of the per-user harness files.
pub(crate) fn adk_home() -> Result<PathBuf> {
    dirs::home_dir().map(|home| home.join(".adk")).ok_or_else(|| {
        AdkError::config(
            "cannot determine the home directory for ~/.adk; pass an explicit store root",
        )
    })
}

/// Lowercase hex SHA-256 of `text`.
pub(crate) fn sha256_hex(text: &str) -> String {
    hex_digest(text.as_bytes())
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn io_error(path: &Path, action: &str, error: &std::io::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "harness.store.io",
        format!("{action} {}: {error}", path.display()),
    )
}

/// Reads `path`, or `None` when it does not exist.
pub(crate) async fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, "read", &error)),
    }
}

/// Writes `bytes` to a private temporary sibling of `path` and renames it into place, so
/// a reader never sees a partial file. The file is `0600` on Unix.
pub(crate) async fn write_atomic_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| AdkError::config("store path has no parent"))?;
    tokio::fs::create_dir_all(parent).await.map_err(|error| io_error(parent, "create", &error))?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("store");
    let temp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let result = async {
        use tokio::io::AsyncWriteExt as _;
        let mut file = options.open(&temp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(io_error(path, "write", &error));
    }
    Ok(())
}

/// Maps a database error to a retryable store error.
#[cfg(feature = "sqlite")]
pub(crate) fn db_error(error: sqlx::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "harness.store.database",
        format!("harness store database error: {error}"),
    )
}

/// Maps a serialization failure of a store record.
pub(crate) fn serialize_error(what: &str, error: &serde_json::Error) -> AdkError {
    AdkError::internal(
        ErrorComponent::Agent,
        "harness.store.serialize",
        format!("serialize {what}: {error}"),
    )
}

/// The synchronous form of [`write_atomic_private`], for stores read from sync code.
pub(crate) fn write_atomic_private_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| AdkError::config("store path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| io_error(parent, "create", &error))?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("store");
    let temp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(io_error(path, "write", &error));
    }
    Ok(())
}
