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
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) fn db_error(error: sqlx::Error) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "harness.store.database",
        format!("harness store database error: {error}"),
    )
}

/// Runs a PostgreSQL store's migrations under a session-level advisory lock keyed by
/// `registry_table`, so replicas starting together migrate one at a time (spec 3.1 §13 Q2).
///
/// The lock is held on a connection opened outside `pool`, so a one-connection pool can
/// still run the statements; closing that connection releases the lock.
#[cfg(feature = "postgres")]
pub(crate) fn migrate_postgres(
    pool: &sqlx::PgPool,
    registry_table: &'static str,
    steps: &'static [(i64, &'static str, &'static str)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    // Boxed so the future is checked for `Send` here, with concrete lifetimes.
    let pool = pool.clone();
    Box::pin(async move {
        let key = registry_table
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock =
            sqlx::PgConnection::connect_with(&pool.connect_options()).await.map_err(db_error)?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(db_error)?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            registry_table,
            steps,
            || async { Ok(false) },
        )
        .await;
        let _ = lock.close().await;
        result
    })
}

/// The key namespace of the Redis stores unless one is set.
#[cfg(feature = "redis")]
pub(crate) const REDIS_NAMESPACE: &str = "adk:harness";

/// Connects to the Redis server at `url`.
#[cfg(feature = "redis")]
pub(crate) async fn redis_connect(url: &str) -> Result<fred::prelude::Client> {
    use fred::prelude::ClientLike as _;
    let config = fred::prelude::Config::from_url(url)
        .map_err(|error| redis_error("parse the Redis URL", &error))?;
    let client = fred::prelude::Builder::from_config(config)
        .build()
        .map_err(|error| redis_error("build the Redis client", &error))?;
    client.init().await.map_err(|error| redis_error("connect to Redis", &error))?;
    Ok(client)
}

/// Maps a Redis error to a retryable store error.
#[cfg(feature = "redis")]
pub(crate) fn redis_error(action: &str, error: &dyn std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Unavailable,
        "harness.store.database",
        format!("harness store could not {action}: {error}"),
    )
}

/// Percent-encodes the characters that delimit Redis key segments and hash tags, so an
/// identifier cannot spill into the next segment or move a key's hash slot.
#[cfg(feature = "redis")]
pub(crate) fn redis_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            '{' => encoded.push_str("%7B"),
            '}' => encoded.push_str("%7D"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// A key namespace with its hash-tag braces encoded; colons stay, as separators.
#[cfg(feature = "redis")]
pub(crate) fn redis_namespace(namespace: &str) -> String {
    namespace.replace('{', "%7B").replace('}', "%7D")
}

/// `url` with the password of its userinfo replaced by `***`, for banners, logs and
/// errors (spec 3.1 §13 Q5).
///
/// # Example
///
/// ```rust
/// use adk_harness::redact_store_url;
///
/// assert_eq!(
///     redact_store_url("postgres://app:s3cret@db:5432/harness"),
///     "postgres://app:***@db:5432/harness"
/// );
/// assert_eq!(redact_store_url("redis://localhost:6379"), "redis://localhost:6379");
/// ```
pub fn redact_store_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => {
            let user = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
            if userinfo.contains(':') {
                format!("{scheme}://{user}:***@{host}{tail}")
            } else {
                format!("{scheme}://{user}@{host}{tail}")
            }
        }
        None => url.to_string(),
    }
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
