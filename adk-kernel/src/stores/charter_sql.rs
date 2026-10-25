//! The SQLite and PostgreSQL charter, KPI, autonomy and signal stores (spec 3.5 §7.10,
//! §7.11).
//!
//! | Table | Rows |
//! |---|---|
//! | `adk_kernel_charters` | One per committed version, keyed by site and version, the [`CharterVersion`] as JSON |
//! | `adk_kernel_kpi_samples` | One per KPI and instant (microseconds since the Unix epoch), value and source as columns |
//! | `adk_kernel_kpi_states` | The tracker's state of each KPI (`kpi:<id>`) and guardrail (`guardrail:<id>`), as JSON |
//! | `adk_kernel_job_autonomy`, `adk_kernel_job_autonomy_history` | Each job's record with its revision, and every change by revision |
//! | `adk_kernel_job_decisions` | Each owner decision on a job's proposals, by id |
//! | `adk_kernel_signals`, `adk_kernel_signal_open` | Each signal record, and the open record of each key |
//! | `adk_kernel_signal_rates` | Each source's count per rate window |
//!
//! Every compare-and-set is one statement or a transaction that writes first: a charter
//! version is inserted only when it is one past the site's latest, a job's record moves only
//! from the revision before, and a signal's count rises in one `UPDATE … RETURNING` on the
//! key's open record, so concurrent replicas agree. The four stores share one migration
//! registry, `_adk_kernel_charter_migrations`.
//!
//! Parameters appear in ascending order in every statement and none is repeated, since
//! SQLite numbers `$N` parameters by first appearance.

use chrono::{DateTime, Utc};

use crate::charter::{CharterVersion, SignalFilter, SignalRecord};
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;

/// The migration registry of the charter stores, distinct from every other store's.
pub(crate) const CHARTER_REGISTRY_TABLE: &str = "_adk_kernel_charter_migrations";

/// The schema; `{c}` is the collation clause of text keys.
macro_rules! charter_schema {
    ($c:literal) => {
        concat!(
            "CREATE TABLE IF NOT EXISTS adk_kernel_charters (",
            "site_id TEXT", $c, " NOT NULL, version BIGINT NOT NULL, committed_at_us BIGINT NOT NULL, ",
            "record TEXT NOT NULL, PRIMARY KEY (site_id, version));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_kpi_samples (",
            "site_id TEXT", $c, " NOT NULL, kpi TEXT", $c, " NOT NULL, at_us BIGINT NOT NULL, ",
            "value DOUBLE PRECISION NOT NULL, source TEXT NOT NULL, PRIMARY KEY (site_id, kpi, at_us));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_kpi_states (",
            "site_id TEXT", $c, " NOT NULL, state_key TEXT", $c, " NOT NULL, record TEXT NOT NULL, ",
            "PRIMARY KEY (site_id, state_key));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_job_autonomy (",
            "site_id TEXT", $c, " NOT NULL, job TEXT", $c, " NOT NULL, revision BIGINT NOT NULL, ",
            "record TEXT NOT NULL, PRIMARY KEY (site_id, job));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_job_autonomy_history (",
            "site_id TEXT", $c, " NOT NULL, job TEXT", $c, " NOT NULL, revision BIGINT NOT NULL, ",
            "change TEXT NOT NULL, PRIMARY KEY (site_id, job, revision));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_job_decisions (",
            "site_id TEXT", $c, " NOT NULL, job TEXT", $c, " NOT NULL, id TEXT", $c, " NOT NULL, ",
            "at_us BIGINT NOT NULL, record TEXT NOT NULL, PRIMARY KEY (site_id, job, id));",
            "CREATE INDEX IF NOT EXISTS adk_kernel_job_decisions_at ON adk_kernel_job_decisions (site_id, job, at_us, id);",
            "CREATE TABLE IF NOT EXISTS adk_kernel_signals (",
            "id TEXT", $c, " PRIMARY KEY, partition_id TEXT", $c, " NOT NULL, kind TEXT", $c, " NOT NULL, ",
            "topic TEXT NOT NULL, count BIGINT NOT NULL, signal TEXT NOT NULL, source TEXT NOT NULL, ",
            "first_at_us BIGINT NOT NULL, last_at_us BIGINT NOT NULL, is_open BIGINT NOT NULL);",
            "CREATE INDEX IF NOT EXISTS adk_kernel_signals_first ON adk_kernel_signals (first_at_us, id);",
            "CREATE TABLE IF NOT EXISTS adk_kernel_signal_open (",
            "partition_id TEXT NOT NULL, kind TEXT NOT NULL, topic TEXT NOT NULL, id TEXT NOT NULL, ",
            "PRIMARY KEY (partition_id, kind, topic));",
            "CREATE TABLE IF NOT EXISTS adk_kernel_signal_rates (",
            "source TEXT NOT NULL, window_us BIGINT NOT NULL, count BIGINT NOT NULL, ",
            "PRIMARY KEY (source, window_us));"
        )
    };
}

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] =
    &[(1, "create the kernel charter, KPI, autonomy and signal tables", charter_schema!(""))];

/// The PostgreSQL schema; text keys take the `C` collation, so they order by the bytes of
/// the UTF-8 text on every server, as the other backends do.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel charter, KPI, autonomy and signal tables",
    charter_schema!(" COLLATE \"C\""),
)];

pub(crate) const CHARTER_APPEND: &str = "\
INSERT INTO adk_kernel_charters (site_id, version, committed_at_us, record) \
SELECT $1, $2, $3, $4 \
WHERE (SELECT COALESCE(MAX(version), 0) FROM adk_kernel_charters WHERE site_id = $5) = $6 \
ON CONFLICT (site_id, version) DO NOTHING";
pub(crate) const CHARTER_CURRENT: &str = "\
SELECT record FROM adk_kernel_charters WHERE site_id = $1 ORDER BY version DESC LIMIT 1";
pub(crate) const CHARTER_HISTORY: &str =
    "SELECT record FROM adk_kernel_charters WHERE site_id = $1 ORDER BY version";
pub(crate) const CHARTER_GET: &str =
    "SELECT record FROM adk_kernel_charters WHERE site_id = $1 AND version = $2";

pub(crate) const KPI_RECORD: &str = "\
INSERT INTO adk_kernel_kpi_samples (site_id, kpi, at_us, value, source) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site_id, kpi, at_us) DO NOTHING";
pub(crate) const KPI_SAMPLES: &str = "\
SELECT at_us, value, source FROM adk_kernel_kpi_samples \
WHERE site_id = $1 AND kpi = $2 AND at_us >= $3 AND at_us < $4 ORDER BY at_us";
pub(crate) const STATE_GET: &str =
    "SELECT record FROM adk_kernel_kpi_states WHERE site_id = $1 AND state_key = $2";
pub(crate) const STATE_PUT: &str = "\
INSERT INTO adk_kernel_kpi_states (site_id, state_key, record) VALUES ($1, $2, $3) \
ON CONFLICT (site_id, state_key) DO UPDATE SET record = excluded.record";

pub(crate) const JOB_GET: &str =
    "SELECT record FROM adk_kernel_job_autonomy WHERE site_id = $1 AND job = $2";
pub(crate) const JOB_LIST: &str =
    "SELECT record FROM adk_kernel_job_autonomy WHERE site_id = $1 ORDER BY job";
pub(crate) const JOB_INSERT: &str = "\
INSERT INTO adk_kernel_job_autonomy (site_id, job, revision, record) VALUES ($1, $2, $3, $4) \
ON CONFLICT (site_id, job) DO NOTHING";
pub(crate) const JOB_UPDATE: &str = "\
UPDATE adk_kernel_job_autonomy SET revision = $1, record = $2 \
WHERE site_id = $3 AND job = $4 AND revision = $5";
pub(crate) const JOB_HISTORY_INSERT: &str = "\
INSERT INTO adk_kernel_job_autonomy_history (site_id, job, revision, change) \
VALUES ($1, $2, $3, $4) ON CONFLICT (site_id, job, revision) DO NOTHING";
pub(crate) const JOB_HISTORY: &str = "\
SELECT change FROM adk_kernel_job_autonomy_history WHERE site_id = $1 AND job = $2 \
ORDER BY revision";
pub(crate) const DECISION_INSERT: &str = "\
INSERT INTO adk_kernel_job_decisions (site_id, job, id, at_us, record) \
VALUES ($1, $2, $3, $4, $5) ON CONFLICT (site_id, job, id) DO NOTHING";
pub(crate) const DECISIONS: &str = "\
SELECT record FROM adk_kernel_job_decisions WHERE site_id = $1 AND job = $2 AND at_us >= $3 \
ORDER BY at_us, id";

/// The columns a signal row reads back in, in [`SignalRow`] order.
macro_rules! signal_columns {
    () => {
        "id, partition_id, kind, topic, count, signal, source, first_at_us, last_at_us, is_open"
    };
}

pub(crate) const SIGNAL_INCREMENT: &str = concat!(
    "UPDATE adk_kernel_signals SET count = count + 1, source = $1, last_at_us = $2, signal = $3 ",
    "WHERE is_open = 1 AND id = (SELECT id FROM adk_kernel_signal_open ",
    "WHERE partition_id = $4 AND kind = $5 AND topic = $6) RETURNING ",
    signal_columns!()
);
pub(crate) const SIGNAL_OPEN_INSERT: &str = "\
INSERT INTO adk_kernel_signal_open (partition_id, kind, topic, id) VALUES ($1, $2, $3, $4) \
ON CONFLICT (partition_id, kind, topic) DO NOTHING";
pub(crate) const SIGNAL_INSERT: &str = concat!(
    "INSERT INTO adk_kernel_signals (",
    signal_columns!(),
    ") VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
);
pub(crate) const SIGNAL_GET: &str =
    concat!("SELECT ", signal_columns!(), " FROM adk_kernel_signals WHERE id = $1");
pub(crate) const SIGNAL_CLOSE: &str =
    "UPDATE adk_kernel_signals SET is_open = 0 WHERE id = $1 AND is_open = 1";
pub(crate) const SIGNAL_OPEN_DELETE: &str = "DELETE FROM adk_kernel_signal_open WHERE id = $1";
pub(crate) const RATE_HIT: &str = "\
INSERT INTO adk_kernel_signal_rates (source, window_us, count) VALUES ($1, $2, 1) \
ON CONFLICT (source, window_us) DO UPDATE SET count = adk_kernel_signal_rates.count + 1 \
RETURNING count";
pub(crate) const RATE_PRUNE: &str =
    "DELETE FROM adk_kernel_signal_rates WHERE source = $1 AND window_us < $2";

/// A signal row: id, partition, kind, topic, count, signal JSON, source, first and last
/// arrival in microseconds, and 1 when open.
pub(crate) type SignalRow = (String, String, String, String, i64, String, String, i64, i64, i64);

/// A value bound to a `signals` statement.
pub(crate) enum Bind {
    Text(String),
    Int(i64),
}

/// The `list` statement of the signal store for `filter` and its parameters, in order.
pub(crate) fn signals(filter: &SignalFilter) -> (String, Vec<Bind>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    if let Some(partition) = &filter.partition {
        binds.push(Bind::Text(partition.as_str().to_string()));
        conditions.push(format!("partition_id = ${}", binds.len()));
    }
    if let Some(kind) = &filter.kind {
        binds.push(Bind::Text(kind.clone()));
        conditions.push(format!("kind = ${}", binds.len()));
    }
    if let Some(open) = filter.open {
        binds.push(Bind::Int(i64::from(open)));
        conditions.push(format!("is_open = ${}", binds.len()));
    }
    let mut sql = concat!("SELECT ", signal_columns!(), " FROM adk_kernel_signals").to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY first_at_us, id");
    if let Some(limit) = filter.limit {
        binds.push(Bind::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" LIMIT ${}", binds.len()));
    }
    (sql, binds)
}

/// The instant `micros` microseconds after the Unix epoch.
pub(crate) fn instant(backend: &'static str, micros: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros).ok_or_else(|| KernelError::Store {
        backend,
        reason: format!("a stored instant is out of range: {micros}"),
    })
}

/// Reads a stored JSON record back.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    backend: &'static str,
    what: &str,
    record: &str,
) -> Result<T> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// Reads a stored charter version back.
pub(crate) fn decode_version(backend: &'static str, record: &str) -> Result<CharterVersion> {
    decode(backend, "charter version", record)
}

/// Reads a signal row back.
pub(crate) fn decode_signal(backend: &'static str, row: SignalRow) -> Result<SignalRecord> {
    let (id, partition, kind, topic, count, signal, source, first, last, open) = row;
    Ok(SignalRecord {
        id,
        partition: PartitionId::new(partition),
        kind,
        topic,
        count: u32::try_from(count).unwrap_or(u32::MAX),
        signal: decode(backend, "signal", &signal)?,
        source,
        first_at: instant(backend, first)?,
        last_at: instant(backend, last)?,
        open: open == 1,
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action}: {error}") }
}

/// Implements the four charter store traits for stores with a `pool` field over the shared
/// statements.
macro_rules! sql_charter_stores {
    ($charter:ty, $kpi:ty, $autonomy:ty, $signal:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::charter::CharterStore for $charter {
            async fn append(
                &self,
                version: $crate::charter::CharterVersion,
            ) -> $crate::error::Result<bool> {
                use $crate::stores::charter_sql as sql;
                let record = serde_json::to_string(&version)?;
                let site = version.site().to_string();
                let number = i64::from(version.version());
                let done = sqlx::query(sql::CHARTER_APPEND)
                    .bind(&site)
                    .bind(number)
                    .bind(version.committed_at.timestamp_micros())
                    .bind(record)
                    .bind(&site)
                    .bind(number - 1)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "append a charter version", &e))?;
                Ok(done.rows_affected() == 1)
            }

            async fn current(
                &self,
                site: uuid::Uuid,
            ) -> $crate::error::Result<Option<$crate::charter::CharterVersion>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<(String,)> = sqlx::query_as(sql::CHARTER_CURRENT)
                    .bind(site.to_string())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read the current charter", &e))?;
                row.map(|(record,)| sql::decode_version($backend, &record)).transpose()
            }

            async fn history(
                &self,
                site: uuid::Uuid,
            ) -> $crate::error::Result<Vec<$crate::charter::CharterVersion>> {
                use $crate::stores::charter_sql as sql;
                let rows: Vec<(String,)> = sqlx::query_as(sql::CHARTER_HISTORY)
                    .bind(site.to_string())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read the charter history", &e))?;
                rows.iter().map(|(record,)| sql::decode_version($backend, record)).collect()
            }

            async fn get(
                &self,
                site: uuid::Uuid,
                version: u32,
            ) -> $crate::error::Result<Option<$crate::charter::CharterVersion>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<(String,)> = sqlx::query_as(sql::CHARTER_GET)
                    .bind(site.to_string())
                    .bind(i64::from(version))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a charter version", &e))?;
                row.map(|(record,)| sql::decode_version($backend, &record)).transpose()
            }
        }

        #[async_trait::async_trait]
        impl $crate::charter::KpiStore for $kpi {
            async fn record(
                &self,
                sample: $crate::charter::KpiSample,
            ) -> $crate::error::Result<bool> {
                use $crate::stores::charter_sql as sql;
                sample.validate()?;
                let done = sqlx::query(sql::KPI_RECORD)
                    .bind(sample.site.to_string())
                    .bind(&sample.kpi)
                    .bind(sample.at.timestamp_micros())
                    .bind(sample.value)
                    .bind(&sample.source)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "record a KPI sample", &e))?;
                Ok(done.rows_affected() == 1)
            }

            async fn samples(
                &self,
                site: uuid::Uuid,
                kpi: &str,
                from: chrono::DateTime<chrono::Utc>,
                to: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<Vec<$crate::charter::KpiSample>> {
                use $crate::stores::charter_sql as sql;
                let rows: Vec<(i64, f64, String)> = sqlx::query_as(sql::KPI_SAMPLES)
                    .bind(site.to_string())
                    .bind(kpi)
                    .bind(from.timestamp_micros())
                    .bind(to.timestamp_micros())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read KPI samples", &e))?;
                rows.into_iter()
                    .map(|(at, value, source)| {
                        Ok($crate::charter::KpiSample::new(
                            site,
                            kpi,
                            sql::instant($backend, at)?,
                            value,
                            source,
                        ))
                    })
                    .collect()
            }

            async fn state(
                &self,
                site: uuid::Uuid,
                kpi: &str,
            ) -> $crate::error::Result<Option<$crate::charter::KpiState>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<(String,)> = sqlx::query_as(sql::STATE_GET)
                    .bind(site.to_string())
                    .bind(format!("kpi:{kpi}"))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a KPI state", &e))?;
                row.map(|(record,)| sql::decode($backend, "KPI state", &record)).transpose()
            }

            async fn put_state(
                &self,
                state: $crate::charter::KpiState,
            ) -> $crate::error::Result<()> {
                use $crate::stores::charter_sql as sql;
                sqlx::query(sql::STATE_PUT)
                    .bind(state.site.to_string())
                    .bind(format!("kpi:{}", state.kpi))
                    .bind(serde_json::to_string(&state)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "write a KPI state", &e))?;
                Ok(())
            }

            async fn guardrail(
                &self,
                site: uuid::Uuid,
                guardrail: &str,
            ) -> $crate::error::Result<Option<$crate::charter::GuardrailState>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<(String,)> = sqlx::query_as(sql::STATE_GET)
                    .bind(site.to_string())
                    .bind(format!("guardrail:{guardrail}"))
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a guardrail state", &e))?;
                row.map(|(record,)| sql::decode($backend, "guardrail state", &record)).transpose()
            }

            async fn put_guardrail(
                &self,
                state: $crate::charter::GuardrailState,
            ) -> $crate::error::Result<()> {
                use $crate::stores::charter_sql as sql;
                sqlx::query(sql::STATE_PUT)
                    .bind(state.site.to_string())
                    .bind(format!("guardrail:{}", state.guardrail))
                    .bind(serde_json::to_string(&state)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "write a guardrail state", &e))?;
                Ok(())
            }
        }

        #[async_trait::async_trait]
        impl $crate::charter::AutonomyStore for $autonomy {
            async fn get(
                &self,
                site: uuid::Uuid,
                job: &str,
            ) -> $crate::error::Result<Option<$crate::charter::JobAutonomy>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<(String,)> = sqlx::query_as(sql::JOB_GET)
                    .bind(site.to_string())
                    .bind(job)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a job's autonomy", &e))?;
                row.map(|(record,)| sql::decode($backend, "job autonomy", &record)).transpose()
            }

            async fn list(
                &self,
                site: uuid::Uuid,
            ) -> $crate::error::Result<Vec<$crate::charter::JobAutonomy>> {
                use $crate::stores::charter_sql as sql;
                let rows: Vec<(String,)> = sqlx::query_as(sql::JOB_LIST)
                    .bind(site.to_string())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "list job autonomy", &e))?;
                rows.iter().map(|(record,)| sql::decode($backend, "job autonomy", record)).collect()
            }

            async fn apply(
                &self,
                record: $crate::charter::JobAutonomy,
                change: $crate::charter::AutonomyChange,
            ) -> $crate::error::Result<bool> {
                use $crate::stores::charter_sql as sql;
                $crate::charter::check_autonomy_apply(&record, &change)?;
                let site = record.site.to_string();
                let revision = i64::try_from(record.revision).unwrap_or(i64::MAX);
                let json = serde_json::to_string(&record)?;
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|e| sql::db_error($backend, "begin an autonomy change", &e))?;
                let moved = if revision == 1 {
                    sqlx::query(sql::JOB_INSERT)
                        .bind(&site)
                        .bind(&record.job)
                        .bind(revision)
                        .bind(json)
                        .execute(&mut *tx)
                        .await
                } else {
                    sqlx::query(sql::JOB_UPDATE)
                        .bind(revision)
                        .bind(json)
                        .bind(&site)
                        .bind(&record.job)
                        .bind(revision - 1)
                        .execute(&mut *tx)
                        .await
                }
                .map_err(|e| sql::db_error($backend, "change a job's autonomy", &e))?;
                if moved.rows_affected() != 1 {
                    return Ok(false);
                }
                let logged = sqlx::query(sql::JOB_HISTORY_INSERT)
                    .bind(&site)
                    .bind(&record.job)
                    .bind(revision)
                    .bind(serde_json::to_string(&change)?)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| sql::db_error($backend, "record an autonomy change", &e))?;
                if logged.rows_affected() != 1 {
                    return Ok(false);
                }
                tx.commit()
                    .await
                    .map_err(|e| sql::db_error($backend, "commit an autonomy change", &e))?;
                Ok(true)
            }

            async fn history(
                &self,
                site: uuid::Uuid,
                job: &str,
            ) -> $crate::error::Result<Vec<$crate::charter::AutonomyChange>> {
                use $crate::stores::charter_sql as sql;
                let rows: Vec<(String,)> = sqlx::query_as(sql::JOB_HISTORY)
                    .bind(site.to_string())
                    .bind(job)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a job's autonomy history", &e))?;
                rows.iter()
                    .map(|(change,)| sql::decode($backend, "autonomy change", change))
                    .collect()
            }

            async fn record_decision(
                &self,
                decision: $crate::charter::JobDecision,
            ) -> $crate::error::Result<bool> {
                use $crate::stores::charter_sql as sql;
                let done = sqlx::query(sql::DECISION_INSERT)
                    .bind(decision.site.to_string())
                    .bind(&decision.job)
                    .bind(&decision.id)
                    .bind(decision.at.timestamp_micros())
                    .bind(serde_json::to_string(&decision)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "record a job decision", &e))?;
                Ok(done.rows_affected() == 1)
            }

            async fn decisions(
                &self,
                site: uuid::Uuid,
                job: &str,
                since: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<Vec<$crate::charter::JobDecision>> {
                use $crate::stores::charter_sql as sql;
                let rows: Vec<(String,)> = sqlx::query_as(sql::DECISIONS)
                    .bind(site.to_string())
                    .bind(job)
                    .bind(since.timestamp_micros())
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read job decisions", &e))?;
                rows.iter().map(|(record,)| sql::decode($backend, "job decision", record)).collect()
            }
        }

        #[async_trait::async_trait]
        impl $crate::charter::SignalStore for $signal {
            async fn record(
                &self,
                key: &$crate::charter::SignalKey,
                signal: &awp_types::Signal,
                source: &str,
                at: chrono::DateTime<chrono::Utc>,
                new_id: &str,
            ) -> $crate::error::Result<($crate::charter::SignalRecord, bool)> {
                use $crate::stores::charter_sql as sql;
                let json = serde_json::to_string(signal)?;
                // A concurrent close or open moves the key; each pass either counts on the
                // open record or opens one, and a lost race tries again.
                for _ in 0..16 {
                    let counted: Option<sql::SignalRow> = sqlx::query_as(sql::SIGNAL_INCREMENT)
                        .bind(source)
                        .bind(at.timestamp_micros())
                        .bind(&json)
                        .bind(key.partition.as_str())
                        .bind(&key.kind)
                        .bind(&key.topic)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(|e| sql::db_error($backend, "count a signal", &e))?;
                    if let Some(row) = counted {
                        return Ok((sql::decode_signal($backend, row)?, false));
                    }
                    let mut tx = self
                        .pool
                        .begin()
                        .await
                        .map_err(|e| sql::db_error($backend, "begin a signal record", &e))?;
                    let opened = sqlx::query(sql::SIGNAL_OPEN_INSERT)
                        .bind(key.partition.as_str())
                        .bind(&key.kind)
                        .bind(&key.topic)
                        .bind(new_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| sql::db_error($backend, "open a signal record", &e))?;
                    if opened.rows_affected() != 1 {
                        continue;
                    }
                    sqlx::query(sql::SIGNAL_INSERT)
                        .bind(new_id)
                        .bind(key.partition.as_str())
                        .bind(&key.kind)
                        .bind(&key.topic)
                        .bind(1_i64)
                        .bind(&json)
                        .bind(source)
                        .bind(at.timestamp_micros())
                        .bind(at.timestamp_micros())
                        .bind(1_i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| sql::db_error($backend, "write a signal record", &e))?;
                    tx.commit()
                        .await
                        .map_err(|e| sql::db_error($backend, "commit a signal record", &e))?;
                    let record = $crate::charter::SignalRecord {
                        id: new_id.to_string(),
                        partition: key.partition.clone(),
                        kind: key.kind.clone(),
                        topic: key.topic.clone(),
                        count: 1,
                        signal: signal.clone(),
                        source: source.to_string(),
                        first_at: at,
                        last_at: at,
                        open: true,
                    };
                    return Ok((record, true));
                }
                Err($crate::error::KernelError::Store {
                    backend: $backend,
                    reason: "a signal's key kept changing under 16 attempts; try again".into(),
                })
            }

            async fn get(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::charter::SignalRecord>> {
                use $crate::stores::charter_sql as sql;
                let row: Option<sql::SignalRow> = sqlx::query_as(sql::SIGNAL_GET)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "read a signal record", &e))?;
                row.map(|row| sql::decode_signal($backend, row)).transpose()
            }

            async fn list(
                &self,
                filter: &$crate::charter::SignalFilter,
            ) -> $crate::error::Result<Vec<$crate::charter::SignalRecord>> {
                use $crate::stores::charter_sql as sql;
                let (statement, binds) = sql::signals(filter);
                let mut query = sqlx::query_as::<_, sql::SignalRow>(sqlx::AssertSqlSafe(statement));
                for bind in binds {
                    query = match bind {
                        sql::Bind::Text(text) => query.bind(text),
                        sql::Bind::Int(int) => query.bind(int),
                    };
                }
                let rows = query
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "list signal records", &e))?;
                rows.into_iter().map(|row| sql::decode_signal($backend, row)).collect()
            }

            async fn close(&self, id: &str) -> $crate::error::Result<bool> {
                use $crate::stores::charter_sql as sql;
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|e| sql::db_error($backend, "begin a signal close", &e))?;
                let closed = sqlx::query(sql::SIGNAL_CLOSE)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| sql::db_error($backend, "close a signal record", &e))?;
                if closed.rows_affected() != 1 {
                    return Ok(false);
                }
                sqlx::query(sql::SIGNAL_OPEN_DELETE)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| sql::db_error($backend, "close a signal key", &e))?;
                tx.commit()
                    .await
                    .map_err(|e| sql::db_error($backend, "commit a signal close", &e))?;
                Ok(true)
            }

            async fn hit(
                &self,
                source: &str,
                window_start: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<u64> {
                use $crate::stores::charter_sql as sql;
                let start = window_start.timestamp_micros();
                let (count,): (i64,) = sqlx::query_as(sql::RATE_HIT)
                    .bind(source)
                    .bind(start)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "count a signal source", &e))?;
                sqlx::query(sql::RATE_PRUNE)
                    .bind(source)
                    .bind(start - $crate::charter::RATE_HORIZON_MICROS)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| sql::db_error($backend, "prune signal rate windows", &e))?;
                Ok(u64::try_from(count).unwrap_or(0))
            }
        }
    };
}

/// Declares one store type of a backend over `$pool`, with `new`, `pool` and `migrate`.
macro_rules! sql_charter_store_type {
    ($(#[$doc:meta])* $name:ident, sqlite) => {
        $(#[$doc])*
        #[cfg(feature = "sqlite")]
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: sqlx::SqlitePool,
        }

        #[cfg(feature = "sqlite")]
        impl $name {
            /// A store over `pool`, such as the pool of `harness.db`.
            pub fn new(pool: sqlx::SqlitePool) -> Self {
                Self { pool }
            }

            /// The pool the store writes to.
            pub fn pool(&self) -> &sqlx::SqlitePool {
                &self.pool
            }

            /// Creates or upgrades the charter tables, which the four charter stores
            /// share; idempotent, recorded in `_adk_kernel_charter_migrations`.
            ///
            /// # Errors
            ///
            /// Returns [`KernelError::Store`] when a statement fails, or when the database
            /// was migrated by a newer release.
            pub async fn migrate(&self) -> Result<()> {
                adk_session::migration::sqlite_runner::run_sql_migrations(
                    &self.pool,
                    CHARTER_REGISTRY_TABLE,
                    SQLITE_MIGRATIONS,
                    || async { Ok(false) },
                )
                .await
                .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
            }
        }
    };
    ($(#[$doc:meta])* $name:ident, postgres) => {
        $(#[$doc])*
        #[cfg(feature = "postgres")]
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: sqlx::PgPool,
        }

        #[cfg(feature = "postgres")]
        impl $name {
            /// A store over `pool`, such as the one the session service and the run store
            /// use.
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
                let pool = sqlx::PgPool::connect(database_url).await.map_err(|error| {
                    KernelError::Store {
                        backend: "postgres",
                        reason: format!("could not connect: {error}"),
                    }
                })?;
                Ok(Self::new(pool))
            }

            /// Creates or upgrades the charter tables, which the four charter stores share,
            /// under a session-level advisory lock keyed by the registry table, so replicas
            /// starting together migrate one at a time.
            ///
            /// # Errors
            ///
            /// Returns [`KernelError::Store`] when a statement fails, or when the database
            /// was migrated by a newer release.
            pub async fn migrate(&self) -> Result<()> {
                migrate_locked(self.pool.clone()).await
            }
        }
    };
}

sql_charter_store_type!(
    /// A [`CharterStore`](crate::charter::CharterStore) in SQLite (spec 3.5 §7.11, features
    /// `charter` and `sqlite`), on a pool such as `harness.db`'s. Call `migrate` once
    /// before use.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::charter::{CharterStore, SqliteCharterStore};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_kernel::Result<()> {
    /// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    /// let store = SqliteCharterStore::new(pool);
    /// store.migrate().await?;
    /// assert!(store.current(uuid::Uuid::nil()).await?.is_none());
    /// # Ok(())
    /// # }
    /// ```
    SqliteCharterStore,
    sqlite
);
sql_charter_store_type!(
    /// A [`KpiStore`](crate::charter::KpiStore) in SQLite (spec 3.5 §7.11, features
    /// `charter` and `sqlite`). Call `migrate` once before use.
    SqliteKpiStore,
    sqlite
);
sql_charter_store_type!(
    /// An [`AutonomyStore`](crate::charter::AutonomyStore) in SQLite (spec 3.5 §7.11,
    /// features `charter` and `sqlite`). Call `migrate` once before use.
    SqliteAutonomyStore,
    sqlite
);
sql_charter_store_type!(
    /// A [`SignalStore`](crate::charter::SignalStore) in SQLite (spec 3.5 §7.10, features
    /// `charter` and `sqlite`). Call `migrate` once before use.
    SqliteSignalStore,
    sqlite
);
sql_charter_store_type!(
    /// A [`CharterStore`](crate::charter::CharterStore) in PostgreSQL, shared by every
    /// replica (spec 3.5 §7.11, features `charter` and `postgres`).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> adk_kernel::Result<()> {
    /// let store = adk_kernel::charter::PostgresCharterStore::connect("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    PostgresCharterStore,
    postgres
);
sql_charter_store_type!(
    /// A [`KpiStore`](crate::charter::KpiStore) in PostgreSQL (spec 3.5 §7.11, features
    /// `charter` and `postgres`).
    PostgresKpiStore,
    postgres
);
sql_charter_store_type!(
    /// An [`AutonomyStore`](crate::charter::AutonomyStore) in PostgreSQL (spec 3.5 §7.11,
    /// features `charter` and `postgres`).
    PostgresAutonomyStore,
    postgres
);
sql_charter_store_type!(
    /// A [`SignalStore`](crate::charter::SignalStore) in PostgreSQL (spec 3.5 §7.10,
    /// features `charter` and `postgres`).
    PostgresSignalStore,
    postgres
);

#[cfg(feature = "sqlite")]
sql_charter_stores!(
    SqliteCharterStore,
    SqliteKpiStore,
    SqliteAutonomyStore,
    SqliteSignalStore,
    "sqlite"
);
#[cfg(feature = "postgres")]
sql_charter_stores!(
    PostgresCharterStore,
    PostgresKpiStore,
    PostgresAutonomyStore,
    PostgresSignalStore,
    "postgres"
);

/// Runs the migrations under the advisory lock. Boxed so the future is checked for `Send`
/// here, with concrete lifetimes, rather than inside callers' `async_trait` methods.
#[cfg(feature = "postgres")]
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = CHARTER_REGISTRY_TABLE
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .map_err(|error| store_error("connect for the migration lock", &error))?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(|error| store_error("take the migration lock", &error))?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            CHARTER_REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_numbers_parameters_in_order() {
        let (sql, binds) = signals(&SignalFilter::default());
        assert_eq!(
            sql,
            concat!(
                "SELECT ",
                signal_columns!(),
                " FROM adk_kernel_signals ORDER BY first_at_us, id"
            )
        );
        assert!(binds.is_empty());
        let filter =
            SignalFilter::default().partition("back").kind("content_gap").open(true).limit(5);
        let (sql, binds) = signals(&filter);
        assert!(sql.ends_with(
            "WHERE partition_id = $1 AND kind = $2 AND is_open = $3 ORDER BY first_at_us, id LIMIT $4"
        ));
        assert_eq!(binds.len(), 4);
    }

    #[test]
    fn statements_number_parameters_by_first_appearance() {
        for statement in [
            CHARTER_APPEND,
            KPI_RECORD,
            KPI_SAMPLES,
            STATE_PUT,
            JOB_UPDATE,
            JOB_HISTORY_INSERT,
            DECISION_INSERT,
            DECISIONS,
            SIGNAL_INCREMENT,
            SIGNAL_INSERT,
            RATE_HIT,
            RATE_PRUNE,
        ] {
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
            let expected: Vec<usize> = (1..=seen.len()).collect();
            assert_eq!(seen, expected, "{statement}");
        }
    }
}
