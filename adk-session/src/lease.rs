//! SQL-backed [`LeaseStore`](adk_core::LeaseStore) implementations.
//!
//! One row per resource in `adk_leases` holds the current owner, epoch, and expiry. The
//! row stays after release, so the next grant continues the epoch count.
//!
//! | Operation | Statement | Why it has one winner |
//! |-----------|-----------|-----------------------|
//! | `acquire` | `INSERT ... ON CONFLICT (resource) DO UPDATE SET epoch = epoch + 1 ... WHERE released OR expires_at_us <= now OR owner = $owner RETURNING epoch` | The upsert is one atomic statement; a concurrent grant makes the row live, so the `WHERE` of every later upsert is false |
//! | `renew` | `UPDATE ... WHERE resource = $r AND owner = $o AND epoch = $e AND NOT released` | The epoch is compared inside the write |
//! | `release` | `UPDATE ... SET released = TRUE WHERE resource = $r AND owner = $o AND epoch = $e` | Same |
//!
//! | Backend | Feature | Serialization |
//! |---------|---------|---------------|
//! | [`SqliteLeaseStore`](crate::SqliteLeaseStore) | `sqlite` | SQLite's single writer |
//! | [`PostgresLeaseStore`](crate::PostgresLeaseStore) | `postgres` | The row lock the upsert takes; a waiting upsert re-checks its `WHERE` against the committed row |

use std::sync::Arc;
use std::time::Duration;

use adk_core::lease::lease_stale_epoch_error;
use adk_core::{Clock, Lease, LeaseStore, Result, SystemClock};

use crate::durable_sql::{deadline, from_micros, now, storage_error, to_u64};

const CODE: &str = "session.lease_store.storage";

const REGISTRY_TABLE: &str = "_adk_lease_migrations";

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create lease table",
    "\
CREATE TABLE IF NOT EXISTS adk_leases (\
    resource TEXT PRIMARY KEY, \
    owner TEXT NOT NULL, \
    epoch BIGINT NOT NULL, \
    expires_at_us BIGINT NOT NULL, \
    released BOOLEAN NOT NULL\
);",
)];

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const ACQUIRE: &str = "\
INSERT INTO adk_leases (resource, owner, epoch, expires_at_us, released) \
VALUES ($1, $2, 1, $3, FALSE) \
ON CONFLICT (resource) DO UPDATE SET \
    owner = excluded.owner, \
    epoch = adk_leases.epoch + 1, \
    expires_at_us = excluded.expires_at_us, \
    released = FALSE \
WHERE adk_leases.released OR adk_leases.expires_at_us <= $4 OR adk_leases.owner = excluded.owner \
RETURNING epoch";

const RENEW: &str = "\
UPDATE adk_leases SET expires_at_us = $1 \
WHERE resource = $2 AND owner = $3 AND epoch = $4 AND NOT released";

const RELEASE: &str = "\
UPDATE adk_leases SET released = TRUE \
WHERE resource = $1 AND owner = $2 AND epoch = $3";

const CURRENT: &str = "\
SELECT owner, epoch, expires_at_us FROM adk_leases \
WHERE resource = $1 AND NOT released AND expires_at_us > $2";

macro_rules! sql_lease_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty, $migrate:expr
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: $pool,
            clock: Arc<dyn Clock>,
        }

        impl $name {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.lease_store.storage` when the database cannot be reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url)
                    .await
                    .map_err(|e| storage_error(CODE, format!("lease store connection failed: {e}")))?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool, clock: Arc::new(SystemClock) }
            }

            /// Sets the clock that decides expiry.
            pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
                self.clock = clock;
                self
            }

            /// Creates or upgrades the lease table.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration statement fails.
            pub async fn migrate(&self) -> Result<()> {
                let migrate = $migrate;
                migrate(&self.pool).await
            }
        }

        #[async_trait::async_trait]
        impl LeaseStore for $name {
            async fn acquire(
                &self,
                resource: &str,
                owner: &str,
                ttl: Duration,
            ) -> Result<Option<Lease>> {
                let (now, now_us) = now(&*self.clock);
                let (expires_at, expires_us) = deadline(now, ttl, "lease.invalid_ttl")?;
                let epoch: Option<i64> = sqlx::query_scalar(ACQUIRE)
                    .bind(resource)
                    .bind(owner)
                    .bind(expires_us)
                    .bind(now_us)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("lease acquire failed: {e}")))?;
                epoch
                    .map(|epoch| {
                        Ok(Lease {
                            resource: resource.to_string(),
                            owner: owner.to_string(),
                            epoch: to_u64(CODE, "epoch", epoch)?,
                            expires_at,
                        })
                    })
                    .transpose()
            }

            async fn renew(&self, lease: &Lease, ttl: Duration) -> Result<Lease> {
                let (now, _) = now(&*self.clock);
                let (expires_at, expires_us) = deadline(now, ttl, "lease.invalid_ttl")?;
                // An epoch past BIGINT was never granted.
                let epoch = i64::try_from(lease.epoch)
                    .map_err(|_| lease_stale_epoch_error(&lease.resource, lease.epoch))?;
                let updated = sqlx::query(RENEW)
                    .bind(expires_us)
                    .bind(&lease.resource)
                    .bind(&lease.owner)
                    .bind(epoch)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("lease renew failed: {e}")))?;
                if updated.rows_affected() == 0 {
                    return Err(lease_stale_epoch_error(&lease.resource, lease.epoch));
                }
                Ok(Lease { expires_at, ..lease.clone() })
            }

            async fn release(&self, lease: &Lease) -> Result<()> {
                // An epoch past BIGINT was never granted, so there is nothing to release.
                let Ok(epoch) = i64::try_from(lease.epoch) else { return Ok(()) };
                sqlx::query(RELEASE)
                    .bind(&lease.resource)
                    .bind(&lease.owner)
                    .bind(epoch)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("lease release failed: {e}")))?;
                Ok(())
            }

            async fn current(&self, resource: &str) -> Result<Option<Lease>> {
                let (_, now_us) = now(&*self.clock);
                let row: Option<(String, i64, i64)> = sqlx::query_as(CURRENT)
                    .bind(resource)
                    .bind(now_us)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("lease read failed: {e}")))?;
                row.map(|(owner, epoch, expires_us)| {
                    Ok(Lease {
                        resource: resource.to_string(),
                        owner,
                        epoch: to_u64(CODE, "epoch", epoch)?,
                        expires_at: from_micros(CODE, "expires_at_us", expires_us)?,
                    })
                })
                .transpose()
            }
        }
    };
}

#[cfg(feature = "sqlite")]
sql_lease_store! {
    /// SQLite-backed [`LeaseStore`].
    ///
    /// Every operation is one statement, and SQLite runs one writer at a time, so two
    /// processes sharing the database file never both hold a resource.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_core::LeaseStore;
    /// use adk_session::SqliteLeaseStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteLeaseStore::new("sqlite://runtime.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// if let Some(lease) = store.acquire("scheduler", "replica-1", Duration::from_secs(30)).await? {
    ///     store.renew(&lease, Duration::from_secs(30)).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    SqliteLeaseStore, sqlx::SqlitePool,
    |pool: &sqlx::SqlitePool| {
        let pool = pool.clone();
        async move {
            crate::migration::sqlite_runner::run_sql_migrations(
                &pool,
                REGISTRY_TABLE,
                MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }
}

#[cfg(feature = "postgres")]
sql_lease_store! {
    /// PostgreSQL-backed [`LeaseStore`]. [`migrate`](Self::migrate) takes an advisory
    /// lock, so concurrent processes can call it at startup.
    ///
    /// `acquire` is a single `INSERT ... ON CONFLICT DO UPDATE ... WHERE` statement: of
    /// any number of concurrent acquirers across processes, one is granted the next
    /// epoch and the rest see the live lease and get `None`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_core::LeaseStore;
    /// use adk_session::PostgresLeaseStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresLeaseStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// if let Some(lease) = store.acquire("scheduler", "replica-1", Duration::from_secs(30)).await? {
    ///     store.release(&lease).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    PostgresLeaseStore, sqlx::PgPool,
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
