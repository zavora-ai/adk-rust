//! The PostgreSQL [`ChannelBindingStore`](super::ChannelBindingStore) (spec 3.3 §10 K1;
//! features `channels`, `postgres`).

use super::sql::{REGISTRY_TABLE, sql_channel_store};
use crate::error::{KernelError, Result};

/// Migrations of [`PostgresChannelBindingStore`], recorded in
/// `_adk_kernel_channel_migrations`.
///
/// The key columns take the `C` collation, so lists order by the bytes of the UTF-8 text on
/// every server, as the other backends do.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel channel binding and pairing-request tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_channel_bindings (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    paired_us BIGINT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_channel_requests (\
    channel TEXT COLLATE \"C\" NOT NULL, \
    account TEXT COLLATE \"C\" NOT NULL, \
    code_hash TEXT NOT NULL UNIQUE, \
    requested_us BIGINT NOT NULL, \
    expires_us BIGINT NOT NULL, \
    record TEXT NOT NULL, \
    PRIMARY KEY (channel, account)\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_channel_requests_expiry ON adk_kernel_channel_requests (expires_us);",
)];

/// A [`ChannelBindingStore`](super::ChannelBindingStore) in PostgreSQL, shared by every
/// replica (spec 3.3 §10 K1, features `channels`, `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its tables are
/// `adk_kernel_channel_bindings` and `adk_kernel_channel_requests`;
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_channel_migrations` under an
/// advisory lock, so replicas starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::channels::PostgresChannelBindingStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresChannelBindingStore {
    pool: sqlx::PgPool,
}

impl PostgresChannelBindingStore {
    /// A store over `pool`, such as the one the session service and the run store use.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool =
            sqlx::PgPool::connect(database_url).await.map_err(|error| KernelError::Store {
                backend: "postgres",
                reason: format!("could not connect: {error}"),
            })?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's tables, under a session-level advisory lock keyed
    /// by the registry table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        crate::pairing::postgres::migrate_locked(self.pool.clone(), REGISTRY_TABLE, MIGRATIONS)
            .await
    }
}

sql_channel_store!(PostgresChannelBindingStore, "postgres");
