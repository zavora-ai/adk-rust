//! Durable read records for the harness's file tools (spec 3.1 §7.26, §7.33 fix 5,
//! §10 D7).
//!
//! adk-devtools records the sha256 of every file a session's agent reads or writes in a
//! [`ReadLedger`](adk_devtools::ReadLedger), and `edit_file` uses it as the edit's
//! precondition. Kept in process memory, a held edit approved from a new process found no
//! read and failed; these stores keep the records with the session's other harness state,
//! keyed by (app, user, session, agent, path):
//!
//! | Store | Where | Shared by replicas |
//! |---|---|---|
//! | [`SqliteReadRecords`] | `harness.db`, sealed under the store key when one is set | no |
//! | [`PostgresReadRecords`] | the served harness's PostgreSQL store | yes |
//! | [`RedisReadRecords`] | the served harness's Redis store | yes |

use adk_core::Result;
use adk_devtools::{ReadLedger, ReadScope};
use async_trait::async_trait;

/// Most paths a scope keeps in [`SqliteReadRecords`]; the least recently recorded go first.
#[cfg(feature = "sqlite")]
const SQLITE_PATHS_PER_SCOPE: usize = 4096;

/// Read records in SQLite — `harness.db` in adk-cli — one row per session and agent
/// holding its paths and digests (spec 3.1 §10 D7; feature `sqlite`).
///
/// A row is sealed under the store key with
/// [`with_keyring`](Self::with_keyring), so file names stay out of the database file as the
/// sessions' content does (spec 3.1 §7.12, I57). A scope keeps its 4,096 most recently
/// recorded paths.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_devtools::Workspace;
/// use adk_harness::SqliteReadRecords;
///
/// # async fn example() -> adk_core::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let records = SqliteReadRecords::new(pool);
/// records.migrate().await?;
/// let workspace = Workspace::new("./repo").with_read_ledger(Arc::new(records));
/// # let _ = workspace;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteReadRecords {
    pool: sqlx::SqlitePool,
    /// Serializes read-modify-write of a row within this process.
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteReadRecords {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            pool,
            lock: std::sync::Arc::default(),
            #[cfg(feature = "encrypted")]
            envelope: None,
        }
    }

    /// Seals every row under `keyring` (spec 3.1 §7.12).
    #[cfg(feature = "encrypted")]
    #[must_use]
    pub fn with_keyring(mut self, keyring: crate::encrypted::StoreKeyring) -> Self {
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "read-records"));
        self
    }

    /// Re-seals every row a previous key sealed with the current key, seals rows written
    /// before a key was configured, and returns how many rows changed (spec 3.1 §7.12).
    /// Without a keyring, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.decrypt_failed` when a row opens under no key the keyring
    /// holds, or the database error.
    #[cfg(feature = "encrypted")]
    pub async fn rewrap(&self) -> Result<u64> {
        let Some(envelope) = &self.envelope else {
            return Ok(0);
        };
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT app_name, user_id, session_id, agent_name, records FROM adk_harness_read_records",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (app, user, session, agent, text) in rows {
            if let Some(resealed) =
                envelope.reseal_or_seal(&[&app, &user, &session, &agent], &text)?
            {
                sqlx::query(
                    "UPDATE adk_harness_read_records SET records = ? WHERE app_name = ? AND \
                     user_id = ? AND session_id = ? AND agent_name = ?",
                )
                .bind(resealed)
                .bind(&app)
                .bind(&user)
                .bind(&session)
                .bind(&agent)
                .execute(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    #[cfg(feature = "encrypted")]
    fn encode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::encode_row(self.envelope.as_ref(), address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::decode_row(self.envelope.as_ref(), "read-records", address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    /// Creates the store's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_read_records (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                agent_name TEXT NOT NULL, records TEXT NOT NULL, \
                PRIMARY KEY (app_name, user_id, session_id, agent_name))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    /// The scope's (path, digest) pairs, least recently recorded first.
    async fn load(&self, scope: &ReadScope) -> Result<Vec<(String, String)>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT records FROM adk_harness_read_records WHERE app_name = ? AND user_id = ? \
             AND session_id = ? AND agent_name = ?",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.session)
        .bind(&scope.agent)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let Some((text,)) = row else {
            return Ok(Vec::new());
        };
        let text = self.decode(&[&scope.app, &scope.user, &scope.session, &scope.agent], text)?;
        // An unreadable row reads as no record: the next edit asks for a read.
        Ok(serde_json::from_str(&text).unwrap_or_default())
    }

    async fn store(&self, scope: &ReadScope, records: &[(String, String)]) -> Result<()> {
        let text = serde_json::to_string(records)
            .map_err(|error| crate::fsutil::serialize_error("read records", &error))?;
        let text = self.encode(&[&scope.app, &scope.user, &scope.session, &scope.agent], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_read_records \
             (app_name, user_id, session_id, agent_name, records) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (app_name, user_id, session_id, agent_name) \
             DO UPDATE SET records = excluded.records",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.session)
        .bind(&scope.agent)
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl ReadLedger for SqliteReadRecords {
    async fn record(&self, scope: &ReadScope, path: &str, sha256: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut records = self.load(scope).await?;
        records.retain(|(recorded, _)| recorded != path);
        records.push((path.to_string(), sha256.to_string()));
        if records.len() > SQLITE_PATHS_PER_SCOPE {
            records.drain(..records.len() - SQLITE_PATHS_PER_SCOPE);
        }
        self.store(scope, &records).await
    }

    async fn digest(&self, scope: &ReadScope, path: &str) -> Result<Option<String>> {
        let records = self.load(scope).await?;
        Ok(records.into_iter().rev().find(|(recorded, _)| recorded == path).map(|(_, d)| d))
    }

    async fn forget(&self, scope: &ReadScope, path: &str) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut records = self.load(scope).await?;
        let before = records.len();
        records.retain(|(recorded, _)| recorded != path);
        if records.len() == before {
            return Ok(());
        }
        self.store(scope, &records).await
    }
}

/// Migrations of [`PostgresReadRecords`], recorded in `_adk_harness_read_record_migrations`.
#[cfg(feature = "postgres")]
const POSTGRES_READ_RECORD_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create harness read record table",
    "CREATE TABLE IF NOT EXISTS adk_harness_read_records (\
        app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
        agent_name TEXT NOT NULL, path TEXT NOT NULL, sha256 TEXT NOT NULL, \
        PRIMARY KEY (app_name, user_id, session_id, agent_name, path))",
)];

/// Read records in PostgreSQL, shared by every replica of a served harness, so a held
/// edit approved on another replica finds the read (spec 3.1 §10 D7; feature `postgres`).
///
/// One row per (`app_name`, `user_id`, `session_id`, `agent_name`, `path`) in
/// `adk_harness_read_records`. [`migrate`](Self::migrate) records its steps in
/// `_adk_harness_read_record_migrations` under an advisory lock.
///
/// # Example
///
/// ```rust,no_run
/// use adk_devtools::{ReadLedger, ReadScope};
/// use adk_harness::PostgresReadRecords;
///
/// # async fn example() -> adk_core::Result<()> {
/// let records = PostgresReadRecords::connect("postgres://localhost/app").await?;
/// records.migrate().await?;
/// let scope = ReadScope::new("app", "alice", "s1", "coder");
/// records.record(&scope, "src/lib.rs", "ab12").await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresReadRecords {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresReadRecords {
    /// A store over `pool`, such as the one the session service uses.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.database` when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = sqlx::PgPool::connect(database_url).await.map_err(crate::fsutil::db_error)?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's table.
    ///
    /// # Errors
    ///
    /// Returns the database error when a statement fails, or a version mismatch when the
    /// database was migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        crate::fsutil::migrate_postgres(
            &self.pool,
            "_adk_harness_read_record_migrations",
            POSTGRES_READ_RECORD_MIGRATIONS,
        )
        .await
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl ReadLedger for PostgresReadRecords {
    async fn record(&self, scope: &ReadScope, path: &str, sha256: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO adk_harness_read_records \
             (app_name, user_id, session_id, agent_name, path, sha256) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (app_name, user_id, session_id, agent_name, path) \
             DO UPDATE SET sha256 = excluded.sha256",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.session)
        .bind(&scope.agent)
        .bind(path)
        .bind(sha256)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn digest(&self, scope: &ReadScope, path: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT sha256 FROM adk_harness_read_records WHERE app_name = $1 AND user_id = $2 \
             AND session_id = $3 AND agent_name = $4 AND path = $5",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.session)
        .bind(&scope.agent)
        .bind(path)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(row.map(|(digest,)| digest))
    }

    async fn forget(&self, scope: &ReadScope, path: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM adk_harness_read_records WHERE app_name = $1 AND user_id = $2 \
             AND session_id = $3 AND agent_name = $4 AND path = $5",
        )
        .bind(&scope.app)
        .bind(&scope.user)
        .bind(&scope.session)
        .bind(&scope.agent)
        .bind(path)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

/// Read records in Redis, shared by every replica of a served harness (spec 3.1 §10 D7;
/// feature `redis`).
///
/// One hash per session and agent at
/// `<namespace>:{<app>}:reads:<user>:<session>:<agent>`, mapping each path to its digest.
/// The namespace is `adk:harness` unless [`with_namespace`](Self::with_namespace) sets
/// another; the app segment is the key's hash tag, so every key of an app sits in one Redis
/// Cluster slot.
///
/// # Example
///
/// ```rust,no_run
/// use adk_devtools::{ReadLedger, ReadScope};
/// use adk_harness::RedisReadRecords;
///
/// # async fn example() -> adk_core::Result<()> {
/// let records = RedisReadRecords::connect("redis://localhost:6379").await?;
/// let scope = ReadScope::new("app", "alice", "s1", "coder");
/// records.record(&scope, "src/lib.rs", "ab12").await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "redis")]
#[derive(Debug, Clone)]
pub struct RedisReadRecords {
    client: fred::prelude::Client,
    namespace: String,
}

#[cfg(feature = "redis")]
impl RedisReadRecords {
    /// A store over an initialized client, such as one shared with the mode store.
    pub fn from_client(client: fred::prelude::Client) -> Self {
        Self { client, namespace: crate::fsutil::REDIS_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.database` when the server cannot be reached.
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::from_client(crate::fsutil::redis_connect(url).await?))
    }

    /// Keeps the store's keys under `namespace` instead of `adk:harness`.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = crate::fsutil::redis_namespace(&namespace.into());
        self
    }

    fn key(&self, scope: &ReadScope) -> String {
        use crate::fsutil::redis_segment;
        format!(
            "{}:{{{}}}:reads:{}:{}:{}",
            self.namespace,
            redis_segment(&scope.app),
            redis_segment(&scope.user),
            redis_segment(&scope.session),
            redis_segment(&scope.agent)
        )
    }
}

#[cfg(feature = "redis")]
#[async_trait]
impl ReadLedger for RedisReadRecords {
    async fn record(&self, scope: &ReadScope, path: &str, sha256: &str) -> Result<()> {
        use fred::prelude::HashesInterface as _;
        let _: i64 = self
            .client
            .hset(self.key(scope), (path, sha256))
            .await
            .map_err(|error| crate::fsutil::redis_error("write a read record", &error))?;
        Ok(())
    }

    async fn digest(&self, scope: &ReadScope, path: &str) -> Result<Option<String>> {
        use fred::prelude::HashesInterface as _;
        self.client
            .hget(self.key(scope), path)
            .await
            .map_err(|error| crate::fsutil::redis_error("read a read record", &error))
    }

    async fn forget(&self, scope: &ReadScope, path: &str) -> Result<()> {
        use fred::prelude::HashesInterface as _;
        let _: i64 = self
            .client
            .hdel(self.key(scope), path)
            .await
            .map_err(|error| crate::fsutil::redis_error("remove a read record", &error))?;
        Ok(())
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;

    async fn store() -> SqliteReadRecords {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = SqliteReadRecords::new(pool);
        store.migrate().await.unwrap();
        store
    }

    #[tokio::test]
    async fn sqlite_read_records_conform() {
        crate::conformance::read_records::round_trip(&store().await).await;
        crate::conformance::read_records::scopes_are_isolated(&store().await).await;
        crate::conformance::read_records::forget_removes_one_path(&store().await).await;
    }

    #[tokio::test]
    async fn a_scope_keeps_its_most_recent_paths() {
        let store = store().await;
        let scope = ReadScope::new("app", "alice", "s1", "coder");
        for index in 0..=SQLITE_PATHS_PER_SCOPE {
            store.record(&scope, &format!("f{index}"), "d").await.unwrap();
        }
        assert_eq!(store.digest(&scope, "f0").await.unwrap(), None);
        assert_eq!(store.digest(&scope, "f1").await.unwrap().as_deref(), Some("d"));
        let last = format!("f{SQLITE_PATHS_PER_SCOPE}");
        assert_eq!(store.digest(&scope, &last).await.unwrap().as_deref(), Some("d"));
    }
}
