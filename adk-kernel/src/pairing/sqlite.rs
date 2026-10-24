//! The SQLite [`DeviceStore`](super::DeviceStore) (spec 3.3 §10 K2; feature `sqlite`).

use super::sql::{REGISTRY_TABLE, sql_device_store};
use crate::error::{KernelError, Result};

/// Migrations of [`SqliteDeviceStore`], recorded in `_adk_kernel_device_migrations`.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel device and pairing-code tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_devices (\
    id TEXT PRIMARY KEY, \
    created_us INTEGER NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_pairing_codes (\
    code_hash TEXT PRIMARY KEY, \
    expires_us INTEGER NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_pairing_codes_expiry ON adk_kernel_pairing_codes (expires_us);",
)];

/// A [`DeviceStore`](super::DeviceStore) in SQLite (spec 3.3 §10 K2, feature `sqlite`).
///
/// In the CLI it lives in `harness.db` on the same pool as the harness and process stores:
/// its tables are `adk_kernel_devices` and `adk_kernel_pairing_codes`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_device_migrations`. Call
/// `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::pairing::{IssueCode, Pairing, SqliteDeviceStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteDeviceStore::new(pool);
/// store.migrate().await?;
/// let pairing = Pairing::new(Arc::new(store));
/// let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await?;
/// let claimed = pairing.claim(&issued.code, None).await?;
/// assert_eq!(pairing.authenticate(&claimed.token).await?.owner, "ana");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteDeviceStore {
    pool: sqlx::SqlitePool,
}

impl SqliteDeviceStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's tables. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

sql_device_store!(SqliteDeviceStore, "sqlite");
