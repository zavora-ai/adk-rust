//! The SQLite [`ChannelBindingStore`](super::ChannelBindingStore) (spec 3.3 §10 K1; features
//! `channels`, `sqlite`).

use super::sql::{REGISTRY_TABLE, sql_channel_store};
use crate::error::{KernelError, Result};

/// Migrations of [`SqliteChannelBindingStore`], recorded in `_adk_kernel_channel_migrations`.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel channel binding and pairing-request tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_channel_bindings (\
    id TEXT PRIMARY KEY, \
    paired_us INTEGER NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_channel_requests (\
    channel TEXT NOT NULL, \
    account TEXT NOT NULL, \
    code_hash TEXT NOT NULL UNIQUE, \
    requested_us INTEGER NOT NULL, \
    expires_us INTEGER NOT NULL, \
    record TEXT NOT NULL, \
    PRIMARY KEY (channel, account)\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_channel_requests_expiry ON adk_kernel_channel_requests (expires_us);",
)];

/// A [`ChannelBindingStore`](super::ChannelBindingStore) in SQLite (spec 3.3 §10 K1,
/// features `channels`, `sqlite`).
///
/// In the CLI it lives in `harness.db` on the same pool as the harness and process stores:
/// its tables are `adk_kernel_channel_bindings` and `adk_kernel_channel_requests`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_channel_migrations`. Call
/// `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::{ChannelBinding, ChannelBindingStore, SqliteChannelBindingStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteChannelBindingStore::new(pool);
/// store.migrate().await?;
/// let binding = ChannelBinding::new("telegram", "4217", "4217", "ana", chrono::Utc::now());
/// store.put_binding(binding.clone()).await?;
/// assert_eq!(store.list_bindings().await?, [binding]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteChannelBindingStore {
    pool: sqlx::SqlitePool,
}

impl SqliteChannelBindingStore {
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

sql_channel_store!(SqliteChannelBindingStore, "sqlite");
