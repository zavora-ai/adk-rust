//! SQL-backed [`SpendLedger`] implementations.
//!
//! Every reservation, commit, and release is one row in `adk_spend_entries`, so spend
//! survives restarts and is shared by every process that points at the same database.
//! Limits are configuration, not data: give every process the same [`SpendLimits`].
//!
//! | Backend | Feature | How reservations serialize |
//! |---------|---------|----------------------------|
//! | [`SqliteSpendLedger`] | `sqlite` | `BEGIN IMMEDIATE` takes the write lock before the limit check |
//! | [`PostgresSpendLedger`] | `postgres` | `SELECT ... FOR UPDATE` on the organization's row in `adk_spend_orgs` |
//!
//! A reservation that is neither committed nor released stops counting once its
//! `expires_at` passes; its row stays for audit.

use std::time::Duration;

use adk_core::{
    DEFAULT_RESERVATION_TTL, ReservationId, Result, SpendError, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod,
};
use chrono::{DateTime, Utc};

const STATE_RESERVED: &str = "reserved";
const STATE_COMMITTED: &str = "committed";
const STATE_RELEASED: &str = "released";

fn unavailable(error: impl std::fmt::Display) -> adk_core::AdkError {
    SpendError::Unavailable(error.to_string()).into()
}

fn stored_amount(amount_micro_usd: u64) -> Result<i64> {
    i64::try_from(amount_micro_usd).map_err(|_| SpendError::AmountTooLarge(amount_micro_usd).into())
}

/// Microseconds since the epoch at the start of `period`'s current window.
fn window_start_micros(period: SpendPeriod, now: DateTime<Utc>) -> i64 {
    period.window_start(now).map_or(i64::MIN, |start| start.timestamp_micros())
}

fn expires_at_micros(now: DateTime<Utc>, ttl: Duration) -> i64 {
    let ttl = i64::try_from(ttl.as_micros()).unwrap_or(i64::MAX);
    now.timestamp_micros().saturating_add(ttl)
}

fn limit_exceeded(
    limit: &adk_core::SpendLimit,
    consumed: i64,
    requested: u64,
) -> Option<adk_core::AdkError> {
    let consumed = u64::try_from(consumed).unwrap_or(0);
    (consumed.saturating_add(requested) > limit.max_micro_usd).then(|| {
        SpendError::LimitExceeded {
            limit: limit.key.clone(),
            max_micro_usd: limit.max_micro_usd,
            consumed_micro_usd: consumed,
            requested_micro_usd: requested,
        }
        .into()
    })
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use super::*;
    use sqlx::{Row, sqlite::SqlitePool};

    const REGISTRY_TABLE: &str = "_adk_spend_migrations";

    const MIGRATIONS: &[(i64, &str, &str)] = &[(
        1,
        "create spend ledger entries",
        "\
CREATE TABLE IF NOT EXISTS adk_spend_entries (\
    id TEXT PRIMARY KEY, \
    org TEXT NOT NULL, \
    agent TEXT, \
    vendor TEXT, \
    state TEXT NOT NULL, \
    reserved_micro_usd INTEGER NOT NULL, \
    committed_micro_usd INTEGER, \
    created_at_us INTEGER NOT NULL, \
    expires_at_us INTEGER NOT NULL, \
    settled_at_us INTEGER\
);\
CREATE INDEX IF NOT EXISTS adk_spend_entries_org_state ON adk_spend_entries (org, state);",
    )];

    /// Committed spend in the window plus live holds, for one limit's scope.
    const CONSUMED: &str = "\
SELECT COALESCE(SUM(CASE \
    WHEN state = 'committed' AND settled_at_us >= ?1 THEN committed_micro_usd \
    WHEN state = 'reserved' AND expires_at_us > ?2 THEN reserved_micro_usd \
    ELSE 0 END), 0) AS consumed \
FROM adk_spend_entries \
WHERE org = ?3 AND (?4 IS NULL OR agent = ?4) AND (?5 IS NULL OR vendor = ?5)";

    const SPENT: &str = "\
SELECT COALESCE(SUM(committed_micro_usd), 0) AS spent \
FROM adk_spend_entries \
WHERE state = 'committed' AND settled_at_us >= ?1 \
    AND org = ?2 AND (?3 IS NULL OR agent = ?3) AND (?4 IS NULL OR vendor = ?4)";

    /// SQLite-backed [`SpendLedger`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
    /// use adk_session::SqliteSpendLedger;
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let ledger = SqliteSpendLedger::new("sqlite://spend.db?mode=rwc")
    ///     .await?
    ///     .with_limits(
    ///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
    ///     );
    /// ledger.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[derive(Debug)]
    pub struct SqliteSpendLedger {
        pool: SqlitePool,
        limits: SpendLimits,
        ttl: Duration,
    }

    impl SqliteSpendLedger {
        /// Connects to `database_url` with no limits and [`DEFAULT_RESERVATION_TTL`].
        ///
        /// # Errors
        ///
        /// Returns `spend.ledger_unavailable` when the database cannot be opened.
        pub async fn new(database_url: &str) -> Result<Self> {
            let pool = SqlitePool::connect(database_url).await.map_err(unavailable)?;
            Ok(Self::from_pool(pool))
        }

        /// Uses an existing pool, such as the one a `SqliteSessionService` holds.
        pub fn from_pool(pool: SqlitePool) -> Self {
            Self { pool, limits: SpendLimits::new(), ttl: DEFAULT_RESERVATION_TTL }
        }

        /// Sets the limits every reservation is checked against.
        pub fn with_limits(mut self, limits: SpendLimits) -> Self {
            self.limits = limits;
            self
        }

        /// Sets how long an unsettled reservation holds budget.
        pub fn with_reservation_ttl(mut self, ttl: Duration) -> Self {
            self.ttl = ttl;
            self
        }

        /// Creates or upgrades the ledger table.
        ///
        /// # Errors
        ///
        /// Returns an error when a migration statement fails.
        pub async fn migrate(&self) -> Result<()> {
            crate::migration::sqlite_runner::run_sql_migrations(
                &self.pool,
                REGISTRY_TABLE,
                MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }

    #[async_trait::async_trait]
    impl SpendLedger for SqliteSpendLedger {
        async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
            let amount = stored_amount(amount_micro_usd)?;
            // The write lock is taken before the limit check, so a concurrent reservation
            // waits for this one to commit instead of reading the same total.
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.map_err(unavailable)?;
            let now = Utc::now();
            let now_us = now.timestamp_micros();
            for limit in self.limits.applicable(key) {
                let consumed: i64 = sqlx::query(CONSUMED)
                    .bind(window_start_micros(limit.key.period, now))
                    .bind(now_us)
                    .bind(&limit.key.org)
                    .bind(limit.key.agent.as_deref())
                    .bind(limit.key.vendor.as_deref())
                    .fetch_one(&mut *tx)
                    .await
                    .and_then(|row| row.try_get("consumed"))
                    .map_err(unavailable)?;
                if let Some(error) = limit_exceeded(limit, consumed, amount_micro_usd) {
                    return Err(error);
                }
            }
            let id = ReservationId::generate();
            sqlx::query(
                "INSERT INTO adk_spend_entries \
                 (id, org, agent, vendor, state, reserved_micro_usd, created_at_us, expires_at_us) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(id.as_str())
            .bind(&key.org)
            .bind(key.agent.as_deref())
            .bind(key.vendor.as_deref())
            .bind(STATE_RESERVED)
            .bind(amount)
            .bind(now_us)
            .bind(expires_at_micros(now, self.ttl))
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
            tx.commit().await.map_err(unavailable)?;
            Ok(id)
        }

        async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
            let amount = stored_amount(actual_micro_usd)?;
            let updated = sqlx::query(
                "UPDATE adk_spend_entries \
                 SET state = ?1, committed_micro_usd = ?2, settled_at_us = ?3 \
                 WHERE id = ?4 AND state = ?5",
            )
            .bind(STATE_COMMITTED)
            .bind(amount)
            .bind(Utc::now().timestamp_micros())
            .bind(id.as_str())
            .bind(STATE_RESERVED)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
            if updated.rows_affected() == 0 {
                return Err(SpendError::UnknownReservation(id).into());
            }
            Ok(())
        }

        async fn release(&self, id: ReservationId) -> Result<()> {
            let updated = sqlx::query(
                "UPDATE adk_spend_entries SET state = ?1, settled_at_us = ?2 \
                 WHERE id = ?3 AND state = ?4",
            )
            .bind(STATE_RELEASED)
            .bind(Utc::now().timestamp_micros())
            .bind(id.as_str())
            .bind(STATE_RESERVED)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
            if updated.rows_affected() == 0 {
                return Err(SpendError::UnknownReservation(id).into());
            }
            Ok(())
        }

        async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
            let spent: i64 = sqlx::query(SPENT)
                .bind(window_start_micros(period, Utc::now()))
                .bind(&key.org)
                .bind(key.agent.as_deref())
                .bind(key.vendor.as_deref())
                .fetch_one(&self.pool)
                .await
                .and_then(|row| row.try_get("spent"))
                .map_err(unavailable)?;
            Ok(u64::try_from(spent).unwrap_or(0))
        }
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteSpendLedger;

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;
    use sqlx::{Row, postgres::PgPool};

    const REGISTRY_TABLE: &str = "_adk_spend_migrations";

    const MIGRATIONS: &[(i64, &str, &str)] = &[(
        1,
        "create spend ledger entries",
        "\
CREATE TABLE IF NOT EXISTS adk_spend_orgs (org TEXT PRIMARY KEY);\
CREATE TABLE IF NOT EXISTS adk_spend_entries (\
    id TEXT PRIMARY KEY, \
    org TEXT NOT NULL, \
    agent TEXT, \
    vendor TEXT, \
    state TEXT NOT NULL, \
    reserved_micro_usd BIGINT NOT NULL, \
    committed_micro_usd BIGINT, \
    created_at_us BIGINT NOT NULL, \
    expires_at_us BIGINT NOT NULL, \
    settled_at_us BIGINT\
);\
CREATE INDEX IF NOT EXISTS adk_spend_entries_org_state ON adk_spend_entries (org, state);",
    )];

    const CONSUMED: &str = "\
SELECT COALESCE(SUM(CASE \
    WHEN state = 'committed' AND settled_at_us >= $1 THEN committed_micro_usd \
    WHEN state = 'reserved' AND expires_at_us > $2 THEN reserved_micro_usd \
    ELSE 0 END), 0)::BIGINT AS consumed \
FROM adk_spend_entries \
WHERE org = $3 AND ($4::TEXT IS NULL OR agent = $4) AND ($5::TEXT IS NULL OR vendor = $5)";

    const SPENT: &str = "\
SELECT COALESCE(SUM(committed_micro_usd), 0)::BIGINT AS spent \
FROM adk_spend_entries \
WHERE state = 'committed' AND settled_at_us >= $1 \
    AND org = $2 AND ($3::TEXT IS NULL OR agent = $3) AND ($4::TEXT IS NULL OR vendor = $4)";

    /// PostgreSQL-backed [`SpendLedger`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
    /// use adk_session::PostgresSpendLedger;
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let ledger = PostgresSpendLedger::new("postgres://localhost/app")
    ///     .await?
    ///     .with_limits(
    ///         SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
    ///     );
    /// ledger.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    #[derive(Debug)]
    pub struct PostgresSpendLedger {
        pool: PgPool,
        limits: SpendLimits,
        ttl: Duration,
    }

    impl PostgresSpendLedger {
        /// Connects to `database_url` with no limits and [`DEFAULT_RESERVATION_TTL`].
        ///
        /// # Errors
        ///
        /// Returns `spend.ledger_unavailable` when the database cannot be reached.
        pub async fn new(database_url: &str) -> Result<Self> {
            let pool = PgPool::connect(database_url).await.map_err(unavailable)?;
            Ok(Self::from_pool(pool))
        }

        /// Uses an existing pool, such as the one a `PostgresSessionService` holds.
        pub fn from_pool(pool: PgPool) -> Self {
            Self { pool, limits: SpendLimits::new(), ttl: DEFAULT_RESERVATION_TTL }
        }

        /// Sets the limits every reservation is checked against.
        pub fn with_limits(mut self, limits: SpendLimits) -> Self {
            self.limits = limits;
            self
        }

        /// Sets how long an unsettled reservation holds budget.
        pub fn with_reservation_ttl(mut self, ttl: Duration) -> Self {
            self.ttl = ttl;
            self
        }

        /// Creates or upgrades the ledger tables.
        ///
        /// # Errors
        ///
        /// Returns an error when a migration statement fails.
        pub async fn migrate(&self) -> Result<()> {
            crate::migration::pg_runner::run_sql_migrations(
                &self.pool,
                REGISTRY_TABLE,
                MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }

    #[async_trait::async_trait]
    impl SpendLedger for PostgresSpendLedger {
        async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
            let amount = stored_amount(amount_micro_usd)?;
            let mut tx = self.pool.begin().await.map_err(unavailable)?;
            // Every limit that covers this key shares its organization, so locking the
            // organization's row serializes every reservation that could interact.
            sqlx::query("INSERT INTO adk_spend_orgs (org) VALUES ($1) ON CONFLICT DO NOTHING")
                .bind(&key.org)
                .execute(&mut *tx)
                .await
                .map_err(unavailable)?;
            sqlx::query("SELECT org FROM adk_spend_orgs WHERE org = $1 FOR UPDATE")
                .bind(&key.org)
                .fetch_one(&mut *tx)
                .await
                .map_err(unavailable)?;
            let now = Utc::now();
            let now_us = now.timestamp_micros();
            for limit in self.limits.applicable(key) {
                let consumed: i64 = sqlx::query(CONSUMED)
                    .bind(window_start_micros(limit.key.period, now))
                    .bind(now_us)
                    .bind(&limit.key.org)
                    .bind(limit.key.agent.as_deref())
                    .bind(limit.key.vendor.as_deref())
                    .fetch_one(&mut *tx)
                    .await
                    .and_then(|row| row.try_get("consumed"))
                    .map_err(unavailable)?;
                if let Some(error) = limit_exceeded(limit, consumed, amount_micro_usd) {
                    return Err(error);
                }
            }
            let id = ReservationId::generate();
            sqlx::query(
                "INSERT INTO adk_spend_entries \
                 (id, org, agent, vendor, state, reserved_micro_usd, created_at_us, expires_at_us) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id.as_str())
            .bind(&key.org)
            .bind(key.agent.as_deref())
            .bind(key.vendor.as_deref())
            .bind(STATE_RESERVED)
            .bind(amount)
            .bind(now_us)
            .bind(expires_at_micros(now, self.ttl))
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
            tx.commit().await.map_err(unavailable)?;
            Ok(id)
        }

        async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
            let amount = stored_amount(actual_micro_usd)?;
            let updated = sqlx::query(
                "UPDATE adk_spend_entries \
                 SET state = $1, committed_micro_usd = $2, settled_at_us = $3 \
                 WHERE id = $4 AND state = $5",
            )
            .bind(STATE_COMMITTED)
            .bind(amount)
            .bind(Utc::now().timestamp_micros())
            .bind(id.as_str())
            .bind(STATE_RESERVED)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
            if updated.rows_affected() == 0 {
                return Err(SpendError::UnknownReservation(id).into());
            }
            Ok(())
        }

        async fn release(&self, id: ReservationId) -> Result<()> {
            let updated = sqlx::query(
                "UPDATE adk_spend_entries SET state = $1, settled_at_us = $2 \
                 WHERE id = $3 AND state = $4",
            )
            .bind(STATE_RELEASED)
            .bind(Utc::now().timestamp_micros())
            .bind(id.as_str())
            .bind(STATE_RESERVED)
            .execute(&self.pool)
            .await
            .map_err(unavailable)?;
            if updated.rows_affected() == 0 {
                return Err(SpendError::UnknownReservation(id).into());
            }
            Ok(())
        }

        async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
            let spent: i64 = sqlx::query(SPENT)
                .bind(window_start_micros(period, Utc::now()))
                .bind(&key.org)
                .bind(key.agent.as_deref())
                .bind(key.vendor.as_deref())
                .fetch_one(&self.pool)
                .await
                .and_then(|row| row.try_get("spent"))
                .map_err(unavailable)?;
            Ok(u64::try_from(spent).unwrap_or(0))
        }
    }
}

#[cfg(feature = "postgres")]
pub use postgres::PostgresSpendLedger;
