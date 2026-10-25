//! The SQLite and PostgreSQL wallet stores (spec 3.5 §7.14, features `sqlite` and
//! `postgres`).
//!
//! | Table | Rows |
//! |-------|------|
//! | `adk_wallet_payments` | One per payment, unique by wallet and id and by wallet and idempotency key: the [`PaymentRecord`] as JSON, its category, state and instant (settlement, else creation, in microseconds since the Unix epoch) as columns, and `seq`, the insertion order listings follow |
//! | `adk_wallet_pauses` | One per paused category of a wallet, the [`CategoryPause`] as JSON |
//!
//! Inserting a payment is one `INSERT … ON CONFLICT (wallet, idempotency_key) DO NOTHING`, so
//! instances racing on one key store one payment; lifting a pause is one
//! `DELETE … RETURNING`, so one pause is lifted once. Both backends record their steps in
//! `_adk_wallet_migrations` with adk-session's migration runners; every step is additive.
//!
//! Parameters appear in ascending order in every statement and none is repeated, since
//! SQLite numbers `$N` parameters by first appearance.

use adk_core::Result;

use super::super::policy::category_name;
use super::super::store::{CategoryPause, PaymentQuery, PaymentRecord, WalletStore};
use super::{encode, instant_micros, micros, state_name, unavailable};

/// The migration registry of the wallet stores, distinct from every other store's.
const REGISTRY_TABLE: &str = "_adk_wallet_migrations";

/// The schema; `$seq` is the backend's auto-numbered key column.
macro_rules! wallet_schema {
    ($seq:literal) => {
        concat!(
            "CREATE TABLE IF NOT EXISTS adk_wallet_payments (",
            $seq,
            ", wallet TEXT NOT NULL, id TEXT NOT NULL, idempotency_key TEXT NOT NULL, ",
            "category TEXT NOT NULL, state TEXT NOT NULL, at_us BIGINT NOT NULL, ",
            "record TEXT NOT NULL, UNIQUE (wallet, id), UNIQUE (wallet, idempotency_key));",
            "CREATE INDEX IF NOT EXISTS adk_wallet_payments_at ON adk_wallet_payments (wallet, at_us);",
            "CREATE INDEX IF NOT EXISTS adk_wallet_payments_state ON adk_wallet_payments (wallet, state, seq);",
            "CREATE TABLE IF NOT EXISTS adk_wallet_pauses (",
            "wallet TEXT NOT NULL, category TEXT NOT NULL, record TEXT NOT NULL, ",
            "PRIMARY KEY (wallet, category));"
        )
    };
}

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the wallet payment and pause tables",
    wallet_schema!("seq INTEGER PRIMARY KEY AUTOINCREMENT"),
)];

/// The PostgreSQL schema.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the wallet payment and pause tables",
    wallet_schema!("seq BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY"),
)];

const INSERT: &str = "\
INSERT INTO adk_wallet_payments (wallet, id, idempotency_key, category, state, at_us, record) \
VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (wallet, idempotency_key) DO NOTHING";
const UPDATE: &str = "\
UPDATE adk_wallet_payments SET category = $1, state = $2, at_us = $3, record = $4 \
WHERE wallet = $5 AND id = $6";
const GET: &str = "SELECT record FROM adk_wallet_payments WHERE wallet = $1 AND id = $2";
const BY_KEY: &str =
    "SELECT record FROM adk_wallet_payments WHERE wallet = $1 AND idempotency_key = $2";
const PAUSE: &str = "\
INSERT INTO adk_wallet_pauses (wallet, category, record) VALUES ($1, $2, $3) \
ON CONFLICT (wallet, category) DO NOTHING";
const RESUME: &str =
    "DELETE FROM adk_wallet_pauses WHERE wallet = $1 AND category = $2 RETURNING record";
const PAUSES: &str = "SELECT record FROM adk_wallet_pauses WHERE wallet = $1";

/// A value bound to the `list` statement.
enum Bind {
    Text(String),
    Int(i64),
}

/// The `list` statement for `query` and its parameters, in order. Its window is never
/// narrower than the query's, which the caller applies exactly.
fn list(query: &PaymentQuery) -> (String, Vec<Bind>) {
    let mut binds = vec![Bind::Text(query.wallet.clone())];
    let mut sql = "SELECT record FROM adk_wallet_payments WHERE wallet = $1".to_string();
    let mut condition = |clause: &str, bind: Bind| {
        binds.push(bind);
        sql.push_str(&format!(" AND {clause} ${}", binds.len()));
    };
    if let Some(category) = query.category {
        condition("category =", Bind::Text(category_name(category)));
    }
    if let Some(state) = query.state {
        condition("state =", Bind::Text(state_name(state).to_string()));
    }
    if let Some(from) = query.from {
        condition("at_us >=", Bind::Int(micros(from)));
    }
    if let Some(to) = query.to {
        condition("at_us <=", Bind::Int(micros(to)));
    }
    sql.push_str(" ORDER BY seq");
    (sql, binds)
}

/// The columns of `record` beside its JSON.
struct Columns {
    category: String,
    state: &'static str,
    at_us: i64,
    record: String,
}

impl Columns {
    fn new(backend: &str, record: &PaymentRecord) -> Result<Self> {
        Ok(Self {
            category: category_name(record.category),
            state: state_name(record.state),
            at_us: instant_micros(record),
            record: encode(backend, record)?,
        })
    }
}

/// Implements [`WalletStore`](super::super::WalletStore) for a store with a `pool` field over
/// the shared statements.
macro_rules! sql_wallet_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl WalletStore for $store {
            async fn insert(&self, record: PaymentRecord) -> Result<(PaymentRecord, bool)> {
                let columns = Columns::new($backend, &record)?;
                let done = sqlx::query(INSERT)
                    .bind(&record.wallet)
                    .bind(&record.id)
                    .bind(&record.key)
                    .bind(columns.category)
                    .bind(columns.state)
                    .bind(columns.at_us)
                    .bind(columns.record)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "store a payment", &e))?;
                if done.rows_affected() == 1 {
                    return Ok((record, true));
                }
                let stored = self.by_key(&record.wallet, &record.key).await?.ok_or_else(|| {
                    unavailable(
                        $backend,
                        "store a payment",
                        &format!("key {} is taken but its payment cannot be read", record.key),
                    )
                })?;
                Ok((stored, false))
            }

            async fn update(&self, record: &PaymentRecord) -> Result<()> {
                let columns = Columns::new($backend, record)?;
                let done = sqlx::query(UPDATE)
                    .bind(columns.category)
                    .bind(columns.state)
                    .bind(columns.at_us)
                    .bind(columns.record)
                    .bind(&record.wallet)
                    .bind(&record.id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "update a payment", &e))?;
                if done.rows_affected() == 0 {
                    return Err(super::payment_not_found(record));
                }
                Ok(())
            }

            async fn get(&self, wallet: &str, id: &str) -> Result<Option<PaymentRecord>> {
                let row: Option<(String,)> = sqlx::query_as(GET)
                    .bind(wallet)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "read a payment", &e))?;
                row.map(|(record,)| super::decode($backend, "payment", &record)).transpose()
            }

            async fn by_key(&self, wallet: &str, key: &str) -> Result<Option<PaymentRecord>> {
                let row: Option<(String,)> = sqlx::query_as(BY_KEY)
                    .bind(wallet)
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "read a payment", &e))?;
                row.map(|(record,)| super::decode($backend, "payment", &record)).transpose()
            }

            async fn list(&self, query: &PaymentQuery) -> Result<Vec<PaymentRecord>> {
                let (sql, binds) = list(query);
                let mut statement = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    statement = match bind {
                        Bind::Text(text) => statement.bind(text),
                        Bind::Int(int) => statement.bind(int),
                    };
                }
                let rows = statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "list payments", &e))?;
                let mut records = Vec::with_capacity(rows.len());
                for (record,) in rows {
                    let record: PaymentRecord = super::decode($backend, "payment", &record)?;
                    if query.matches(&record) {
                        records.push(record);
                    }
                }
                Ok(records)
            }

            async fn pause(&self, pause: CategoryPause) -> Result<()> {
                sqlx::query(PAUSE)
                    .bind(&pause.wallet)
                    .bind(category_name(pause.category))
                    .bind(encode($backend, &pause)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "pause a category", &e))?;
                Ok(())
            }

            async fn resume(
                &self,
                wallet: &str,
                category: $crate::wallet::SpendCategory,
            ) -> Result<Option<CategoryPause>> {
                let row: Option<(String,)> = sqlx::query_as(RESUME)
                    .bind(wallet)
                    .bind(category_name(category))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "resume a category", &e))?;
                row.map(|(record,)| super::decode($backend, "pause", &record)).transpose()
            }

            async fn pauses(&self, wallet: &str) -> Result<Vec<CategoryPause>> {
                let rows: Vec<(String,)> = sqlx::query_as(PAUSES)
                    .bind(wallet)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| unavailable($backend, "list paused categories", &e))?;
                let pauses = rows
                    .iter()
                    .map(|(record,)| super::decode($backend, "pause", record))
                    .collect::<Result<Vec<CategoryPause>>>()?;
                Ok(super::in_category_order(pauses))
            }
        }
    };
}

/// A [`WalletStore`](super::super::WalletStore) in SQLite (spec 3.5 §7.14, feature `sqlite`).
///
/// Its tables are `adk_wallet_payments` and `adk_wallet_pauses`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_wallet_migrations`, so it shares a
/// database, such as `harness.db`, with other stores. Every change is one statement, so
/// writers on the pool, or another process on the same file, queue on SQLite's write lock for
/// the pool's busy timeout. Call `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_payments::wallet::{PaymentQuery, SqliteWalletStore, WalletStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteWalletStore::new(pool);
/// store.migrate().await?;
/// assert!(store.list(&PaymentQuery::new("site-1")).await?.is_empty());
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[cfg_attr(docsrs, doc(cfg(feature = "sqlite")))]
#[derive(Debug, Clone)]
pub struct SqliteWalletStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteWalletStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's tables.
    ///
    /// Idempotent: a step already recorded in `_adk_wallet_migrations` is not run again.
    ///
    /// # Errors
    ///
    /// Returns `wallet.store_unavailable` when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| unavailable("sqlite", "migrate", &error.message))
    }
}

#[cfg(feature = "sqlite")]
sql_wallet_store!(SqliteWalletStore, "sqlite");

/// A [`WalletStore`](super::super::WalletStore) in PostgreSQL, shared by every instance of a
/// site (spec 3.5 §7.14, feature `postgres`).
///
/// Its tables are `adk_wallet_payments` and `adk_wallet_pauses`; [`migrate`](Self::migrate)
/// records its steps in `_adk_wallet_migrations` under an advisory lock, so instances
/// starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// use adk_payments::wallet::PostgresWalletStore;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = PostgresWalletStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[cfg_attr(docsrs, doc(cfg(feature = "postgres")))]
#[derive(Debug, Clone)]
pub struct PostgresWalletStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresWalletStore {
    /// A store over `pool`, such as the one the session service and spend ledger use.
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
    /// Returns `wallet.store_unavailable` when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = sqlx::PgPool::connect(database_url)
            .await
            .map_err(|error| unavailable("postgres", "connect", &error))?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's tables.
    ///
    /// The steps run under a session-level advisory lock keyed by the registry table, held on
    /// a connection opened beside the pool, so a one-connection pool still runs the
    /// statements and instances starting together migrate one at a time.
    ///
    /// # Errors
    ///
    /// Returns `wallet.store_unavailable` when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        migrate_locked(self.pool.clone()).await
    }
}

#[cfg(feature = "postgres")]
sql_wallet_store!(PostgresWalletStore, "postgres");

/// Runs the migrations under the advisory lock. Boxed so the future is checked for `Send`
/// here, with concrete lifetimes, rather than inside callers' `async_trait` methods.
#[cfg(feature = "postgres")]
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    Box::pin(async move {
        let key = REGISTRY_TABLE
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .map_err(|error| unavailable("postgres", "connect for the migration lock", &error))?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(|error| unavailable("postgres", "take the migration lock", &error))?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| unavailable("postgres", "migrate", &error.message));
        let _ = lock.close().await;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::{PaymentState, SpendCategory};
    use chrono::{TimeZone, Utc};

    fn parameters(statement: &str) -> Vec<usize> {
        let mut seen = Vec::new();
        let bytes = statement.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'$' {
                let digits: String =
                    statement[index + 1..].chars().take_while(char::is_ascii_digit).collect();
                seen.push(digits.parse::<usize>().unwrap());
                index += digits.len();
            }
            index += 1;
        }
        seen
    }

    #[test]
    fn statements_number_parameters_by_first_appearance() {
        for statement in [INSERT, UPDATE, GET, BY_KEY, PAUSE, RESUME, PAUSES] {
            let seen = parameters(statement);
            assert_eq!(seen, (1..=seen.len()).collect::<Vec<_>>(), "{statement}");
        }
    }

    #[test]
    fn list_numbers_parameters_in_order() {
        let (sql, binds) = list(&PaymentQuery::new("site-1"));
        assert_eq!(sql, "SELECT record FROM adk_wallet_payments WHERE wallet = $1 ORDER BY seq");
        assert_eq!(binds.len(), 1);

        let from = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let query = PaymentQuery::new("site-1")
            .category(SpendCategory::Models)
            .state(PaymentState::Settled)
            .between(from, to);
        let (sql, binds) = list(&query);
        assert_eq!(
            sql,
            "SELECT record FROM adk_wallet_payments WHERE wallet = $1 AND category = $2 \
             AND state = $3 AND at_us >= $4 AND at_us <= $5 ORDER BY seq"
        );
        assert_eq!(parameters(&sql), vec![1, 2, 3, 4, 5]);
        let texts: Vec<_> = binds
            .iter()
            .map(|bind| match bind {
                Bind::Text(text) => text.clone(),
                Bind::Int(int) => int.to_string(),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "site-1".to_string(),
                "models".to_string(),
                "settled".to_string(),
                from.timestamp_micros().to_string(),
                to.timestamp_micros().to_string(),
            ]
        );
    }
}
