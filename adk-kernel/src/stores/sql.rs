//! Statements, encoding and the [`ProcessStore`](super::ProcessStore) implementation shared
//! by the SQLite and PostgreSQL backends (spec 3.2 §7.3, §13 Q8).
//!
//! One row per process in `adk_kernel_processes`. Fields a filter selects on (`partition_id`,
//! `queue`, `parent`, `suspend_requested`) are plain columns with an index each; structured
//! fields (`kind`, `wake`, `suspended_by`, `labels`) are JSON text, so a value reads back
//! exactly as written. Every change is one statement, so concurrent writers on different
//! fields of one row never lose an update.
//!
//! Spend is one row per process, partition and UTC day in `adk_kernel_spend`, the day as its
//! number of days from the common era, so a range compares as integers; an addition is one
//! upsert that adds to the stored total, so concurrent additions all count. Model calls,
//! total tokens and units are one row per counter beside it in `adk_kernel_usage`, added in
//! the same transaction. Deleting a
//! process's row keeps its spend, so a partition's totals for past days do not change when
//! the run store prunes a finished run.
//!
//! Parameters appear in ascending order in every statement, so SQLite, which numbers `$N`
//! parameters by first appearance, binds them as PostgreSQL does.

use std::collections::BTreeMap;

use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta, Suspension};
use crate::sched::QueueClass;
use crate::stores::ProcessFilter;
use crate::wake::WakeSet;

/// The migration registry of the process store, distinct from every other store's.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_process_migrations";

pub(crate) const PUT: &str = "\
INSERT INTO adk_kernel_processes \
    (pid, parent, partition_id, queue, kind, wake, suspended_by, suspend_requested, \
     agenda_item, labels) \
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
ON CONFLICT (pid) DO UPDATE SET \
    parent = excluded.parent, partition_id = excluded.partition_id, queue = excluded.queue, \
    kind = excluded.kind, wake = excluded.wake, suspended_by = excluded.suspended_by, \
    suspend_requested = excluded.suspend_requested, agenda_item = excluded.agenda_item, \
    labels = excluded.labels";

const COLUMNS: &str = "pid, parent, partition_id, queue, kind, wake, suspended_by, \
                       suspend_requested, agenda_item, labels";

pub(crate) const GET: &str = "\
SELECT pid, parent, partition_id, queue, kind, wake, suspended_by, suspend_requested, \
    agenda_item, labels \
FROM adk_kernel_processes WHERE pid = $1";

pub(crate) const SET_WAKE: &str = "UPDATE adk_kernel_processes SET wake = $1 WHERE pid = $2";

pub(crate) const SET_QUEUE: &str = "UPDATE adk_kernel_processes SET queue = $1 WHERE pid = $2";

pub(crate) const SET_SUSPEND_REQUESTED: &str =
    "UPDATE adk_kernel_processes SET suspend_requested = $1 WHERE pid = $2";

pub(crate) const DELETE: &str = "DELETE FROM adk_kernel_processes WHERE pid = $1";

pub(crate) const ADD_SPEND: &str = "\
INSERT INTO adk_kernel_spend (pid, partition_id, day, micro_usd) VALUES ($1, $2, $3, $4) \
ON CONFLICT (pid, partition_id, day) DO UPDATE SET \
    micro_usd = adk_kernel_spend.micro_usd + excluded.micro_usd";

pub(crate) const ADD_USAGE: &str = "\
INSERT INTO adk_kernel_usage (pid, partition_id, day, counter, amount) VALUES ($1, $2, $3, $4, $5) \
ON CONFLICT (pid, partition_id, day, counter) DO UPDATE SET \
    amount = adk_kernel_usage.amount + excluded.amount";

pub(crate) const PROCESS_USAGE: &str = "\
SELECT counter, CAST(SUM(amount) AS BIGINT) FROM adk_kernel_usage \
WHERE pid = $1 AND day >= $2 AND day <= $3 GROUP BY counter";

pub(crate) const PARTITION_USAGE: &str = "\
SELECT counter, CAST(SUM(amount) AS BIGINT) FROM adk_kernel_usage \
WHERE partition_id = $1 AND day >= $2 AND day <= $3 GROUP BY counter";

pub(crate) const PROCESS_SPEND: &str = "\
SELECT CAST(COALESCE(SUM(micro_usd), 0) AS BIGINT) FROM adk_kernel_spend \
WHERE pid = $1 AND day >= $2 AND day <= $3";

pub(crate) const PARTITION_SPEND: &str = "\
SELECT CAST(COALESCE(SUM(micro_usd), 0) AS BIGINT) FROM adk_kernel_spend \
WHERE partition_id = $1 AND day >= $2 AND day <= $3";

/// The stored form of a UTC day: its number of days from the common era.
pub(crate) fn day(day: chrono::NaiveDate) -> i64 {
    i64::from(chrono::Datelike::num_days_from_ce(&day))
}

/// A spend delta as stored.
pub(crate) fn micro_usd(backend: &'static str, delta: &crate::bus::SpendDelta) -> Result<i64> {
    i64::try_from(delta.micro_usd).map_err(|_| KernelError::Store {
        backend,
        reason: format!("a spend delta of {} micro-USD exceeds a BIGINT column", delta.micro_usd),
    })
}

/// A summed total as returned.
pub(crate) fn total(backend: &'static str, total: i64) -> Result<u64> {
    u64::try_from(total).map_err(|_| KernelError::Store {
        backend,
        reason: format!("the stored spend total {total} is negative"),
    })
}

/// A stored row, in [`COLUMNS`] order.
pub(crate) type Row = (
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    String,
);

/// A value bound to a `list` statement.
pub(crate) enum Bind {
    Text(String),
    Bool(bool),
    Int(i64),
}

/// The `list` statement for `filter` and its parameters, in order.
pub(crate) fn list(filter: &ProcessFilter) -> (String, Vec<Bind>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    let mut condition = |column: &str, value: String| {
        binds.push(Bind::Text(value));
        conditions.push(format!("{column} = ${}", binds.len()));
    };
    if let Some(partition) = &filter.partition {
        condition("partition_id", partition.as_str().to_string());
    }
    if let Some(queue) = &filter.queue {
        condition("queue", queue.as_str().to_string());
    }
    if let Some(parent) = &filter.parent {
        condition("parent", parent.clone());
    }
    if let Some(requested) = filter.suspend_requested {
        binds.push(Bind::Bool(requested));
        conditions.push(format!("suspend_requested = ${}", binds.len()));
    }
    let mut sql = format!("SELECT {COLUMNS} FROM adk_kernel_processes");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY pid");
    if let Some(limit) = filter.limit {
        binds.push(Bind::Int(i64::try_from(limit).unwrap_or(i64::MAX)));
        sql.push_str(&format!(" LIMIT ${}", binds.len()));
    }
    (sql, binds)
}

/// The columns of `meta` other than `pid`, encoded for storage.
pub(crate) struct Encoded {
    pub(crate) parent: Option<String>,
    pub(crate) partition: String,
    pub(crate) queue: String,
    pub(crate) kind: String,
    pub(crate) wake: Option<String>,
    pub(crate) suspended_by: Option<String>,
    pub(crate) suspend_requested: bool,
    pub(crate) agenda_item: Option<String>,
    pub(crate) labels: String,
}

impl Encoded {
    pub(crate) fn new(meta: &ProcessMeta) -> Result<Self> {
        Ok(Self {
            parent: meta.parent.clone(),
            partition: meta.partition.as_str().to_string(),
            queue: meta.queue.as_str().to_string(),
            kind: serde_json::to_string(&meta.kind)?,
            wake: encode_wake(meta.wake.as_ref())?,
            suspended_by: meta.suspended_by.as_ref().map(serde_json::to_string).transpose()?,
            suspend_requested: meta.suspend_requested,
            agenda_item: meta.agenda_item.clone(),
            labels: serde_json::to_string(&meta.labels)?,
        })
    }
}

/// A wake set as stored, `None` for no wake set.
pub(crate) fn encode_wake(wake: Option<&WakeSet>) -> Result<Option<String>> {
    Ok(wake.map(serde_json::to_string).transpose()?)
}

/// Reads a stored row back into the [`ProcessMeta`] that was put.
pub(crate) fn decode(backend: &'static str, row: Row) -> Result<ProcessMeta> {
    let (
        pid,
        parent,
        partition,
        queue,
        kind,
        wake,
        suspended_by,
        suspend_requested,
        agenda_item,
        labels,
    ) = row;
    let corrupt = |column: &str, error: serde_json::Error| KernelError::Store {
        backend,
        reason: format!("column `{column}` of process {pid} is corrupt: {error}"),
    };
    let kind: ProcessKind = serde_json::from_str(&kind).map_err(|e| corrupt("kind", e))?;
    let wake: Option<WakeSet> =
        wake.map(|wake| serde_json::from_str(&wake)).transpose().map_err(|e| corrupt("wake", e))?;
    let suspended_by: Option<Suspension> = suspended_by
        .map(|suspension| serde_json::from_str(&suspension))
        .transpose()
        .map_err(|e| corrupt("suspended_by", e))?;
    let labels: BTreeMap<String, String> =
        serde_json::from_str(&labels).map_err(|e| corrupt("labels", e))?;
    Ok(ProcessMeta {
        pid,
        parent,
        partition: PartitionId::new(partition),
        queue: QueueClass::from_name(&queue),
        kind,
        wake,
        suspended_by,
        suspend_requested,
        agenda_item,
        labels,
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the process table: {error}") }
}

/// Maps a database error of the spend table.
pub(crate) fn spend_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the spend totals: {error}") }
}

/// Implements [`ProcessStore`](crate::ProcessStore) for a store with a `pool` field over the
/// shared statements.
macro_rules! sql_process_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::stores::ProcessStore for $store {
            async fn put(&self, meta: $crate::process::ProcessMeta) -> $crate::error::Result<()> {
                let row = $crate::stores::sql::Encoded::new(&meta)?;
                sqlx::query($crate::stores::sql::PUT)
                    .bind(&meta.pid)
                    .bind(row.parent)
                    .bind(row.partition)
                    .bind(row.queue)
                    .bind(row.kind)
                    .bind(row.wake)
                    .bind(row.suspended_by)
                    .bind(row.suspend_requested)
                    .bind(row.agenda_item)
                    .bind(row.labels)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get(
                &self,
                pid: &str,
            ) -> $crate::error::Result<Option<$crate::process::ProcessMeta>> {
                let row: Option<$crate::stores::sql::Row> =
                    sqlx::query_as($crate::stores::sql::GET)
                        .bind(pid)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(|e| $crate::stores::sql::db_error($backend, "read", &e))?;
                row.map(|row| $crate::stores::sql::decode($backend, row)).transpose()
            }

            async fn list(
                &self,
                filter: &$crate::stores::ProcessFilter,
            ) -> $crate::error::Result<Vec<$crate::process::ProcessMeta>> {
                let (sql, binds) = $crate::stores::sql::list(filter);
                let mut query =
                    sqlx::query_as::<_, $crate::stores::sql::Row>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    query = match bind {
                        $crate::stores::sql::Bind::Text(text) => query.bind(text),
                        $crate::stores::sql::Bind::Bool(flag) => query.bind(flag),
                        $crate::stores::sql::Bind::Int(int) => query.bind(int),
                    };
                }
                let rows = query
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "list", &e))?;
                rows.into_iter().map(|row| $crate::stores::sql::decode($backend, row)).collect()
            }

            async fn set_wake(
                &self,
                pid: &str,
                wake: Option<$crate::wake::WakeSet>,
            ) -> $crate::error::Result<()> {
                let wake = $crate::stores::sql::encode_wake(wake.as_ref())?;
                let done = sqlx::query($crate::stores::sql::SET_WAKE)
                    .bind(wake)
                    .bind(pid)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "update", &e))?;
                if done.rows_affected() == 0 {
                    return Err($crate::error::KernelError::NotFound { pid: pid.into() });
                }
                Ok(())
            }

            async fn set_queue(
                &self,
                pid: &str,
                queue: $crate::sched::QueueClass,
            ) -> $crate::error::Result<()> {
                let done = sqlx::query($crate::stores::sql::SET_QUEUE)
                    .bind(queue.as_str())
                    .bind(pid)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "update", &e))?;
                if done.rows_affected() == 0 {
                    return Err($crate::error::KernelError::NotFound { pid: pid.into() });
                }
                Ok(())
            }

            async fn delete(&self, pid: &str) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::sql::DELETE)
                    .bind(pid)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "delete from", &e))?;
                Ok(done.rows_affected() > 0)
            }

            async fn set_suspend_requested(
                &self,
                pid: &str,
                requested: bool,
            ) -> $crate::error::Result<()> {
                let done = sqlx::query($crate::stores::sql::SET_SUSPEND_REQUESTED)
                    .bind(requested)
                    .bind(pid)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::db_error($backend, "update", &e))?;
                if done.rows_affected() == 0 {
                    return Err($crate::error::KernelError::NotFound { pid: pid.into() });
                }
                Ok(())
            }

            async fn add_spend(
                &self,
                pid: &str,
                partition: &$crate::partition::PartitionId,
                day: chrono::NaiveDate,
                delta: $crate::bus::SpendDelta,
            ) -> $crate::error::Result<()> {
                let failed =
                    |e: sqlx::Error| $crate::stores::sql::spend_error($backend, "add to", &e);
                let day = $crate::stores::sql::day(day);
                let mut tx = self.pool.begin().await.map_err(failed)?;
                sqlx::query($crate::stores::sql::ADD_SPEND)
                    .bind(pid)
                    .bind(partition.as_str())
                    .bind(day)
                    .bind($crate::stores::sql::micro_usd($backend, &delta)?)
                    .execute(&mut *tx)
                    .await
                    .map_err(failed)?;
                for (name, amount) in $crate::stores::counters(&delta) {
                    sqlx::query($crate::stores::sql::ADD_USAGE)
                        .bind(pid)
                        .bind(partition.as_str())
                        .bind(day)
                        .bind(name)
                        .bind($crate::stores::stored_amount($backend, name, amount)?)
                        .execute(&mut *tx)
                        .await
                        .map_err(failed)?;
                }
                tx.commit().await.map_err(failed)?;
                Ok(())
            }

            async fn spend(
                &self,
                scope: &$crate::stores::SpendScope,
                from: chrono::NaiveDate,
                to_inclusive: chrono::NaiveDate,
            ) -> $crate::error::Result<u64> {
                let (sql, key) = match scope {
                    $crate::stores::SpendScope::Process(pid) => {
                        ($crate::stores::sql::PROCESS_SPEND, pid.as_str())
                    }
                    $crate::stores::SpendScope::Partition(partition) => {
                        ($crate::stores::sql::PARTITION_SPEND, partition.as_str())
                    }
                };
                let (total,): (i64,) = sqlx::query_as(sql)
                    .bind(key)
                    .bind($crate::stores::sql::day(from))
                    .bind($crate::stores::sql::day(to_inclusive))
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::spend_error($backend, "sum", &e))?;
                $crate::stores::sql::total($backend, total)
            }

            async fn usage(
                &self,
                scope: &$crate::stores::SpendScope,
                from: chrono::NaiveDate,
                to_inclusive: chrono::NaiveDate,
            ) -> $crate::error::Result<$crate::stores::UsageTotals> {
                let (sql, key) = match scope {
                    $crate::stores::SpendScope::Process(pid) => {
                        ($crate::stores::sql::PROCESS_USAGE, pid.as_str())
                    }
                    $crate::stores::SpendScope::Partition(partition) => {
                        ($crate::stores::sql::PARTITION_USAGE, partition.as_str())
                    }
                };
                let mut totals = $crate::stores::UsageTotals {
                    micro_usd: $crate::stores::ProcessStore::spend(self, scope, from, to_inclusive)
                        .await?,
                    ..Default::default()
                };
                let counters: Vec<(String, i64)> = sqlx::query_as(sql)
                    .bind(key)
                    .bind($crate::stores::sql::day(from))
                    .bind($crate::stores::sql::day(to_inclusive))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::sql::spend_error($backend, "sum", &e))?;
                for (name, amount) in counters {
                    totals.add_counter(&name, $crate::stores::sql::total($backend, amount)?);
                }
                Ok(totals)
            }
        }
    };
}

pub(crate) use sql_process_store;

#[cfg(test)]
mod tests {
    use super::*;

    fn sql(filter: &ProcessFilter) -> String {
        list(filter).0
    }

    #[test]
    fn list_numbers_parameters_in_order() {
        assert_eq!(
            sql(&ProcessFilter::default()),
            format!("SELECT {COLUMNS} FROM adk_kernel_processes ORDER BY pid")
        );
        let filter = ProcessFilter::default()
            .partition("work")
            .queue(QueueClass::Background)
            .parent("run-1")
            .suspend_requested(true)
            .limit(3);
        assert_eq!(
            sql(&filter),
            format!(
                "SELECT {COLUMNS} FROM adk_kernel_processes WHERE partition_id = $1 AND \
                 queue = $2 AND parent = $3 AND suspend_requested = $4 ORDER BY pid LIMIT $5"
            )
        );
        let filter = ProcessFilter::default().parent("run-1").limit(3);
        assert_eq!(
            sql(&filter),
            format!(
                "SELECT {COLUMNS} FROM adk_kernel_processes WHERE parent = $1 ORDER BY pid LIMIT $2"
            )
        );
    }

    #[test]
    fn list_ignores_states() {
        let filter = ProcessFilter::default().states([adk_core::RunState::Waiting]);
        assert_eq!(sql(&filter), sql(&ProcessFilter::default()));
    }
}
