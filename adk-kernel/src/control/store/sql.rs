//! Statements and the [`ControlStore`](super::ControlStore) implementation shared by the
//! SQLite and PostgreSQL backends (spec 3.3 §10 K3, K4).
//!
//! One row per area in `adk_kernel_area_settings` and one per viewer in
//! `adk_kernel_away_cursors`, each holding its record as JSON text beside the columns the
//! store looks up and compares by. A cursor advances with one `INSERT … ON CONFLICT DO
//! UPDATE … WHERE`, so two concurrent advances keep the higher.

use crate::error::KernelError;

/// The migration registry of the control store, distinct from every other store's.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_control_migrations";

pub(crate) const GET_SETTINGS: &str =
    "SELECT record FROM adk_kernel_area_settings WHERE partition_id = $1";

pub(crate) const LIST_SETTINGS: &str =
    "SELECT record FROM adk_kernel_area_settings ORDER BY partition_id";

pub(crate) const PUT_SETTINGS: &str = "\
INSERT INTO adk_kernel_area_settings (partition_id, record) VALUES ($1, $2) \
ON CONFLICT (partition_id) DO UPDATE SET record = excluded.record";

pub(crate) const GET_CURSOR: &str =
    "SELECT record FROM adk_kernel_away_cursors WHERE user_id = $1 AND viewer = $2";

pub(crate) const ADVANCE_CURSOR: &str = "\
INSERT INTO adk_kernel_away_cursors (user_id, viewer, seq, record) VALUES ($1, $2, $3, $4) \
ON CONFLICT (user_id, viewer) DO UPDATE SET seq = excluded.seq, record = excluded.record \
WHERE adk_kernel_away_cursors.seq <= excluded.seq";

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store {
        backend,
        reason: format!("could not {action} the control tables: {error}"),
    }
}

/// Reads a stored JSON column back.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    backend: &'static str,
    what: &str,
    text: &str,
) -> crate::error::Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend,
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// A sequence number as the `BIGINT` column holds it.
pub(crate) fn seq_column(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

/// Implements [`ControlStore`](crate::control::ControlStore) for a store with a `pool`
/// field over the shared statements.
macro_rules! sql_control_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::control::ControlStore for $store {
            async fn area_settings(
                &self,
                partition: &$crate::partition::PartitionId,
            ) -> $crate::error::Result<Option<$crate::control::AreaSettings>> {
                let row: Option<(String,)> =
                    sqlx::query_as($crate::control::store::sql::GET_SETTINGS)
                        .bind(partition.as_str())
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(|e| $crate::control::store::sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| {
                    $crate::control::store::sql::decode($backend, "area's settings", &text)
                })
                .transpose()
            }

            async fn list_area_settings(
                &self,
            ) -> $crate::error::Result<Vec<$crate::control::AreaSettings>> {
                let rows: Vec<(String,)> =
                    sqlx::query_as($crate::control::store::sql::LIST_SETTINGS)
                        .fetch_all(&self.pool)
                        .await
                        .map_err(|e| $crate::control::store::sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| {
                        $crate::control::store::sql::decode($backend, "area's settings", text)
                    })
                    .collect()
            }

            async fn put_area_settings(
                &self,
                settings: $crate::control::AreaSettings,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::control::store::sql::PUT_SETTINGS)
                    .bind(settings.partition.as_str())
                    .bind(serde_json::to_string(&settings)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::control::store::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn cursor(
                &self,
                user: &str,
                viewer: &str,
            ) -> $crate::error::Result<Option<$crate::control::AwayCursor>> {
                let row: Option<(String,)> =
                    sqlx::query_as($crate::control::store::sql::GET_CURSOR)
                        .bind(user)
                        .bind(viewer)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(|e| $crate::control::store::sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| $crate::control::store::sql::decode($backend, "cursor", &text))
                    .transpose()
            }

            async fn advance_cursor(
                &self,
                cursor: $crate::control::AwayCursor,
            ) -> $crate::error::Result<$crate::control::AwayCursor> {
                sqlx::query($crate::control::store::sql::ADVANCE_CURSOR)
                    .bind(&cursor.user)
                    .bind(&cursor.viewer)
                    .bind($crate::control::store::sql::seq_column(cursor.seq))
                    .bind(serde_json::to_string(&cursor)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::control::store::sql::db_error($backend, "write", &e))?;
                let stored = self.cursor(&cursor.user, &cursor.viewer).await?;
                stored.ok_or_else(|| $crate::error::KernelError::Store {
                    backend: $backend,
                    reason: "a cursor just written could not be read back".into(),
                })
            }
        }
    };
}

pub(crate) use sql_control_store;
