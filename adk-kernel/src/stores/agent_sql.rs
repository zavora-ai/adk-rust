//! The SQLite and PostgreSQL [`AgentStore`](crate::agents::AgentStore)s (spec 3.3 §5.7, §10
//! K7).
//!
//! One row per agent in `adk_kernel_agents`: the columns a write compares (`version`) or a
//! delete reads (`state`) beside the record as JSON text, so a record reads back exactly as
//! written. `put_if_version` is one `INSERT … ON CONFLICT DO NOTHING` for a new agent and one
//! `UPDATE … WHERE id = … AND version = …` otherwise, so two writers changing one agent from
//! the same version change it once.
//!
//! Parameters appear in ascending order in every statement, so SQLite, which numbers `$N`
//! parameters by first appearance, binds them as PostgreSQL does.

use crate::agents::AgentRecord;
use crate::error::{KernelError, Result};

/// The migration registry of the agent store, distinct from every other store's.
pub(crate) const AGENT_REGISTRY_TABLE: &str = "_adk_kernel_agent_migrations";

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel agent table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_agents (\
    id TEXT PRIMARY KEY, \
    version BIGINT NOT NULL, \
    state TEXT NOT NULL, \
    record TEXT NOT NULL\
);",
)];

/// The PostgreSQL schema; `id` takes the `C` collation, so `list` orders by the bytes of the
/// UTF-8 text on every server, as the other backends do.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel agent table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_agents (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    version BIGINT NOT NULL, \
    state TEXT NOT NULL, \
    record TEXT NOT NULL\
);",
)];

pub(crate) const INSERT: &str = "\
INSERT INTO adk_kernel_agents (id, version, state, record) VALUES ($1, $2, $3, $4) \
ON CONFLICT (id) DO NOTHING";

pub(crate) const UPDATE: &str = "\
UPDATE adk_kernel_agents SET version = $1, state = $2, record = $3 \
WHERE id = $4 AND version = $5";

pub(crate) const GET: &str = "SELECT record FROM adk_kernel_agents WHERE id = $1";

pub(crate) const LIST: &str = "SELECT record FROM adk_kernel_agents ORDER BY id";

pub(crate) const DELETE: &str =
    "DELETE FROM adk_kernel_agents WHERE id = $1 AND state = 'proposed'";

/// The stored form of a version, a signed 64-bit integer.
pub(crate) fn stored_version(backend: &'static str, version: u64) -> Result<i64> {
    i64::try_from(version).map_err(|_| KernelError::Store {
        backend,
        reason: format!("agent version {version} exceeds a signed 64-bit integer"),
    })
}

/// Reads a stored record back.
pub(crate) fn decode(backend: &'static str, record: &str) -> Result<AgentRecord> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a stored agent is corrupt: {error}"),
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the agent table: {error}") }
}

/// Implements [`AgentStore`](crate::agents::AgentStore) for a store with a `pool` field over
/// the shared statements.
macro_rules! sql_agent_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::agents::AgentStore for $store {
            async fn put_if_version(
                &self,
                record: $crate::agents::AgentRecord,
                expected: u64,
            ) -> $crate::error::Result<bool> {
                let json = serde_json::to_string(&record)?;
                let version = $crate::stores::agent_sql::stored_version($backend, record.version)?;
                let done = if expected == 0 {
                    sqlx::query($crate::stores::agent_sql::INSERT)
                        .bind(&record.id)
                        .bind(version)
                        .bind(record.state.as_str())
                        .bind(json)
                        .execute(&self.pool)
                        .await
                } else {
                    let expected = $crate::stores::agent_sql::stored_version($backend, expected)?;
                    sqlx::query($crate::stores::agent_sql::UPDATE)
                        .bind(version)
                        .bind(record.state.as_str())
                        .bind(json)
                        .bind(&record.id)
                        .bind(expected)
                        .execute(&self.pool)
                        .await
                }
                .map_err(|e| $crate::stores::agent_sql::db_error($backend, "write", &e))?;
                Ok(done.rows_affected() > 0)
            }

            async fn get(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::agents::AgentRecord>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::agent_sql::GET)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agent_sql::db_error($backend, "read", &e))?;
                row.map(|(record,)| $crate::stores::agent_sql::decode($backend, &record))
                    .transpose()
            }

            async fn list(&self) -> $crate::error::Result<Vec<$crate::agents::AgentRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::stores::agent_sql::LIST)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agent_sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(record,)| $crate::stores::agent_sql::decode($backend, record))
                    .collect()
            }

            async fn delete(&self, id: &str) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::agent_sql::DELETE)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::agent_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }
        }
    };
}

/// An [`AgentStore`](crate::agents::AgentStore) in SQLite (spec 3.3 §5.7, §10 K7, feature
/// `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the kernel's other stores, on the same pool:
/// its table is `adk_kernel_agents`, and [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_agent_migrations`, distinct from every other store's. Call `migrate` once
/// before use.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentStore};
/// use adk_kernel::stores::SqliteAgentStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteAgentStore::new(pool);
/// store.migrate().await?;
/// let record = AgentRecord::new("finance", AgentDefinition::new("Finance", "default"),
///     AgentRecordState::Active, AgentOrigin::ByHand, SubjectRef::human("alice"), chrono::Utc::now());
/// assert!(store.put_if_version(record, 0).await?);
/// assert!(store.get("finance").await?.is_some());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteAgentStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteAgentStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's table; a step already recorded in
    /// `_adk_kernel_agent_migrations` is not run again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            AGENT_REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

#[cfg(feature = "sqlite")]
sql_agent_store!(SqliteAgentStore, "sqlite");

/// An [`AgentStore`](crate::agents::AgentStore) in PostgreSQL, shared by every replica
/// (spec 3.3 §5.7, §10 K7, feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its table is `adk_kernel_agents`;
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_agent_migrations` under an
/// advisory lock, so replicas starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::PostgresAgentStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresAgentStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresAgentStore {
    /// A store over `pool`, such as the one the run store uses.
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

    /// Creates or upgrades the store's table under a session-level advisory lock keyed by
    /// the registry table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        super::postgres::migrate_locked(
            self.pool.clone(),
            AGENT_REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
        )
        .await
    }
}

#[cfg(feature = "postgres")]
sql_agent_store!(PostgresAgentStore, "postgres");
