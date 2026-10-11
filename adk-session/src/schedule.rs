//! SQL-backed [`ScheduleStore`](adk_core::ScheduleStore) implementations.
//!
//! One row per schedule in `adk_schedules`; the spec, run template, and missed-fire
//! policy are JSON text, and `next_fire_at_us` is indexed with `paused` for
//! [`due`](adk_core::ScheduleStore::due).
//!
//! | Operation | Statement | Why it is safe under concurrency |
//! |-----------|-----------|----------------------------------|
//! | `advance` | `UPDATE ... SET revision = revision + 1 WHERE id = $id AND revision = $expected` | Compare-and-swap on the revision inside the write |
//! | `upsert` | Read, compare, then insert or update in one transaction (`BEGIN IMMEDIATE` on SQLite, `SELECT ... FOR UPDATE` on PostgreSQL) | An identical definition leaves the revision alone, so replicas registering at startup do not break an advance in flight |
//! | `set_paused` | `UPDATE ... WHERE id = $id AND paused <> $paused` | Moves the revision only on a change |
//! | `due` | `WHERE NOT paused AND next_fire_at_us <= $now ORDER BY next_fire_at_us, id LIMIT $n` | Served by the `(paused, next_fire_at_us)` index |

use adk_core::schedule::schedule_not_found_error;
use adk_core::{Result, Schedule, ScheduleStore};
use chrono::{DateTime, Utc};

use crate::durable_sql::{from_json, from_micros, micros, storage_error, to_json, to_u64};

const CODE: &str = "session.schedule_store.storage";

const REGISTRY_TABLE: &str = "_adk_schedule_migrations";

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create schedule table",
    "\
CREATE TABLE IF NOT EXISTS adk_schedules (\
    id TEXT PRIMARY KEY, \
    spec TEXT NOT NULL, \
    run TEXT NOT NULL, \
    missed TEXT NOT NULL, \
    paused BOOLEAN NOT NULL, \
    next_fire_at_us BIGINT, \
    revision BIGINT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_schedules_due ON adk_schedules (paused, next_fire_at_us);",
)];

macro_rules! schedule_columns {
    () => {
        "id, spec, run, missed, paused, next_fire_at_us, revision"
    };
}

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const INSERT: &str = "\
INSERT INTO adk_schedules (id, spec, run, missed, paused, next_fire_at_us, revision) \
VALUES ($1, $2, $3, $4, $5, $6, 1) \
ON CONFLICT (id) DO NOTHING";

const UPDATE: &str = "\
UPDATE adk_schedules \
SET spec = $1, run = $2, missed = $3, paused = $4, next_fire_at_us = $5, revision = revision + 1 \
WHERE id = $6 \
RETURNING revision";

const GET: &str = concat!("SELECT ", schedule_columns!(), " FROM adk_schedules WHERE id = $1");

const SET_PAUSED: &str = concat!(
    "UPDATE adk_schedules SET paused = $1, revision = revision + 1 ",
    "WHERE id = $2 AND paused <> $1"
);

const ADVANCE: &str = "\
UPDATE adk_schedules SET next_fire_at_us = $1, revision = revision + 1 \
WHERE id = $2 AND revision = $3";

const EXISTS: &str = "SELECT 1 FROM adk_schedules WHERE id = $1";

const DELETE: &str = "DELETE FROM adk_schedules WHERE id = $1";

type ScheduleRow = (String, String, String, String, bool, Option<i64>, i64);

fn decode(
    (id, spec, run, missed, paused, next_fire_at_us, revision): ScheduleRow,
) -> Result<Schedule> {
    let mut schedule = Schedule::new(
        id,
        from_json(CODE, "spec", &spec)?,
        from_json(CODE, "run", &run)?,
        from_json(CODE, "missed", &missed)?,
    )
    .with_paused(paused);
    schedule.next_fire_at =
        next_fire_at_us.map(|us| from_micros(CODE, "next_fire_at_us", us)).transpose()?;
    schedule.revision = to_u64(CODE, "revision", revision)?;
    Ok(schedule)
}

/// Whether two schedules have the same definition, ignoring the revision.
fn same_definition(a: &Schedule, b: &Schedule) -> bool {
    a.spec == b.spec
        && a.run == b.run
        && a.missed == b.missed
        && a.paused == b.paused
        && a.next_fire_at == b.next_fire_at
}

fn stored_next_fire_at(at: Option<DateTime<Utc>>) -> (Option<DateTime<Utc>>, Option<i64>) {
    at.map(micros).map_or((None, None), |(at, us)| (Some(at), Some(us)))
}

fn failed(e: sqlx::Error) -> adk_core::AdkError {
    storage_error(CODE, format!("schedule store query failed: {e}"))
}

macro_rules! sql_schedule_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty,
        begin = $begin:literal,
        lock = $lock:literal,
        collate = $collate:literal,
        $migrate:expr
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: $pool,
        }

        impl $name {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.schedule_store.storage` when the database cannot be
            /// reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    storage_error(CODE, format!("schedule store connection failed: {e}"))
                })?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool }
            }

            /// Creates or upgrades the schedule table.
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
        impl ScheduleStore for $name {
            async fn upsert(&self, mut schedule: Schedule) -> Result<Schedule> {
                schedule.validate()?;
                let (next_fire_at, next_fire_at_us) = stored_next_fire_at(schedule.next_fire_at);
                schedule.next_fire_at = next_fire_at;
                let spec = to_json(CODE, &schedule.spec)?;
                let run = to_json(CODE, &schedule.run)?;
                let missed = to_json(CODE, &schedule.missed)?;
                // A concurrent first insert of the same id makes ours conflict; the retry
                // then reads the committed row.
                for _ in 0..3 {
                    let mut tx = self.pool.begin_with($begin).await.map_err(failed)?;
                    let existing: Option<ScheduleRow> =
                        sqlx::query_as(concat!("SELECT ", schedule_columns!(),
                            " FROM adk_schedules WHERE id = $1", $lock))
                            .bind(&schedule.id)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(failed)?;
                    match existing.map(decode).transpose()? {
                        Some(existing) if same_definition(&existing, &schedule) => {
                            tx.rollback().await.map_err(failed)?;
                            return Ok(existing);
                        }
                        Some(_) => {
                            let revision: i64 = sqlx::query_scalar(UPDATE)
                                .bind(&spec)
                                .bind(&run)
                                .bind(&missed)
                                .bind(schedule.paused)
                                .bind(next_fire_at_us)
                                .bind(&schedule.id)
                                .fetch_one(&mut *tx)
                                .await
                                .map_err(failed)?;
                            tx.commit().await.map_err(failed)?;
                            schedule.revision = to_u64(CODE, "revision", revision)?;
                            return Ok(schedule);
                        }
                        None => {
                            let inserted = sqlx::query(INSERT)
                                .bind(&schedule.id)
                                .bind(&spec)
                                .bind(&run)
                                .bind(&missed)
                                .bind(schedule.paused)
                                .bind(next_fire_at_us)
                                .execute(&mut *tx)
                                .await
                                .map_err(failed)?;
                            if inserted.rows_affected() == 1 {
                                tx.commit().await.map_err(failed)?;
                                schedule.revision = 1;
                                return Ok(schedule);
                            }
                            tx.rollback().await.map_err(failed)?;
                        }
                    }
                }
                Err(storage_error(
                    CODE,
                    format!(
                        "schedule '{}' was inserted and deleted concurrently three times; retry \
                         the upsert",
                        schedule.id
                    ),
                ))
            }

            async fn get(&self, id: &str) -> Result<Option<Schedule>> {
                let row: Option<ScheduleRow> =
                    sqlx::query_as(GET).bind(id).fetch_optional(&self.pool).await.map_err(failed)?;
                row.map(decode).transpose()
            }

            async fn list(&self) -> Result<Vec<Schedule>> {
                let rows: Vec<ScheduleRow> = sqlx::query_as(concat!(
                    "SELECT ", schedule_columns!(), " FROM adk_schedules ORDER BY id", $collate
                ))
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
                rows.into_iter().map(decode).collect()
            }

            async fn delete(&self, id: &str) -> Result<()> {
                let deleted = sqlx::query(DELETE).bind(id).execute(&self.pool).await.map_err(failed)?;
                if deleted.rows_affected() == 0 {
                    return Err(schedule_not_found_error(id));
                }
                Ok(())
            }

            async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
                sqlx::query(SET_PAUSED)
                    .bind(paused)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                self.get(id).await?.ok_or_else(|| schedule_not_found_error(id))
            }

            async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
                let rows: Vec<ScheduleRow> = sqlx::query_as(concat!(
                    "SELECT ", schedule_columns!(), " FROM adk_schedules ",
                    "WHERE paused = FALSE AND next_fire_at_us IS NOT NULL AND next_fire_at_us <= $1 ",
                    "ORDER BY next_fire_at_us, id", $collate, " LIMIT $2"
                ))
                .bind(micros(now).1)
                .bind(i64::try_from(limit).unwrap_or(i64::MAX))
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
                rows.into_iter().map(decode).collect()
            }

            async fn advance(
                &self,
                id: &str,
                expected_revision: u64,
                next_fire_at: Option<DateTime<Utc>>,
            ) -> Result<bool> {
                let advanced = match i64::try_from(expected_revision) {
                    Ok(expected) => sqlx::query(ADVANCE)
                        .bind(stored_next_fire_at(next_fire_at).1)
                        .bind(id)
                        .bind(expected)
                        .execute(&self.pool)
                        .await
                        .map_err(failed)?
                        .rows_affected(),
                    // A revision past BIGINT was never assigned.
                    Err(_) => 0,
                };
                if advanced == 1 {
                    return Ok(true);
                }
                let exists: Option<i32> = sqlx::query_scalar(EXISTS)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(failed)?;
                match exists {
                    Some(_) => Ok(false),
                    None => Err(schedule_not_found_error(id)),
                }
            }
        }
    };
}

#[cfg(feature = "sqlite")]
sql_schedule_store! {
    /// SQLite-backed [`ScheduleStore`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use adk_core::{MissedFirePolicy, NewRun, Schedule, ScheduleSpec, ScheduleStore};
    /// use adk_session::SqliteScheduleStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteScheduleStore::new("sqlite://runtime.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// let spec = ScheduleSpec::Every(Duration::from_secs(3600));
    /// let first = spec.next_after(chrono::Utc::now())?.expect("an interval always fires");
    /// let run = NewRun::new("default", "reports", "system", "report_agent", serde_json::json!({}));
    /// let schedule = Schedule::new("hourly-report", spec, run, MissedFirePolicy::FireOnce)
    ///     .with_next_fire_at(first);
    /// store.upsert(schedule).await?;
    /// # Ok(())
    /// # }
    /// ```
    SqliteScheduleStore, sqlx::SqlitePool,
    begin = "BEGIN IMMEDIATE",
    lock = "",
    collate = "",
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
sql_schedule_store! {
    /// PostgreSQL-backed [`ScheduleStore`]. [`migrate`](Self::migrate) takes an advisory
    /// lock, so concurrent processes can call it at startup.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::ScheduleStore;
    /// use adk_session::PostgresScheduleStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresScheduleStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// for schedule in store.due(chrono::Utc::now(), 100).await? {
    ///     println!("{} is due", schedule.id);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    PostgresScheduleStore, sqlx::PgPool,
    begin = "BEGIN",
    lock = " FOR UPDATE",
    // Byte order, as schedule ids compare in Rust.
    collate = " COLLATE \"C\"",
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
