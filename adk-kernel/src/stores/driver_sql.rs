//! Statements and the [`DriverStore`](crate::drivers::DriverStore) implementation shared by
//! the SQLite and PostgreSQL backends (spec 3.2 §7.7, §13 Q8).
//!
//! One row per driver in `adk_kernel_drivers` and one per parked call in
//! `adk_kernel_driver_parks`, each holding its record as JSON text, so a record reads back
//! exactly as written. Every change is one statement.

use crate::error::KernelError;

/// The migration registry of the driver store, distinct from every other store's.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_driver_migrations";

pub(crate) const PUT: &str = "\
INSERT INTO adk_kernel_drivers (id, record) VALUES ($1, $2) \
ON CONFLICT (id) DO UPDATE SET record = excluded.record";

pub(crate) const GET: &str = "SELECT record FROM adk_kernel_drivers WHERE id = $1";

pub(crate) const LIST: &str = "SELECT record FROM adk_kernel_drivers ORDER BY id";

pub(crate) const DELETE: &str = "DELETE FROM adk_kernel_drivers WHERE id = $1";

pub(crate) const PUT_PARK: &str = "\
INSERT INTO adk_kernel_driver_parks (app_name, user_id, session_id, call_id, driver_id, park) \
VALUES ($1, $2, $3, $4, $5, $6) \
ON CONFLICT (app_name, user_id, session_id, call_id) DO UPDATE SET \
    driver_id = excluded.driver_id, park = excluded.park";

pub(crate) const GET_PARK: &str = "\
SELECT park FROM adk_kernel_driver_parks \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 AND call_id = $4";

pub(crate) const LIST_PARKS: &str = "\
SELECT park FROM adk_kernel_driver_parks ORDER BY app_name, user_id, session_id, call_id";

pub(crate) const LIST_DRIVER_PARKS: &str = "\
SELECT park FROM adk_kernel_driver_parks WHERE driver_id = $1 \
ORDER BY app_name, user_id, session_id, call_id";

pub(crate) const DELETE_PARK: &str = "\
DELETE FROM adk_kernel_driver_parks \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 AND call_id = $4";

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the driver table: {error}") }
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

/// Implements [`DriverStore`](crate::drivers::DriverStore) for a store with a `pool` field
/// over the shared statements.
macro_rules! sql_driver_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::drivers::DriverStore for $store {
            async fn put(
                &self,
                record: $crate::drivers::DriverRecord,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::stores::driver_sql::PUT)
                    .bind(record.id())
                    .bind(serde_json::to_string(&record)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::drivers::DriverRecord>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::driver_sql::GET)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| $crate::stores::driver_sql::decode($backend, "driver", &text))
                    .transpose()
            }

            async fn list(&self) -> $crate::error::Result<Vec<$crate::drivers::DriverRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::stores::driver_sql::LIST)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| $crate::stores::driver_sql::decode($backend, "driver", text))
                    .collect()
            }

            async fn delete(&self, id: &str) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::driver_sql::DELETE)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::driver_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }

            async fn put_park(
                &self,
                park: $crate::drivers::DriverPark,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::stores::driver_sql::PUT_PARK)
                    .bind(&park.scope.app_name)
                    .bind(&park.scope.user_id)
                    .bind(&park.scope.session_id)
                    .bind(&park.call_id)
                    .bind(&park.driver)
                    .bind(serde_json::to_string(&park)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get_park(
                &self,
                scope: &adk_core::ApprovalScope,
                call_id: &str,
            ) -> $crate::error::Result<Option<$crate::drivers::DriverPark>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::driver_sql::GET_PARK)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(call_id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| {
                    $crate::stores::driver_sql::decode($backend, "parked driver call", &text)
                })
                .transpose()
            }

            async fn list_parks(
                &self,
                driver: Option<&str>,
            ) -> $crate::error::Result<Vec<$crate::drivers::DriverPark>> {
                let query = match driver {
                    Some(driver) => sqlx::query_as::<_, (String,)>(
                        $crate::stores::driver_sql::LIST_DRIVER_PARKS,
                    )
                    .bind(driver.to_string()),
                    None => sqlx::query_as::<_, (String,)>($crate::stores::driver_sql::LIST_PARKS),
                };
                let rows = query
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::driver_sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| {
                        $crate::stores::driver_sql::decode($backend, "parked driver call", text)
                    })
                    .collect()
            }

            async fn delete_park(
                &self,
                scope: &adk_core::ApprovalScope,
                call_id: &str,
            ) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::driver_sql::DELETE_PARK)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(call_id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::driver_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }
        }
    };
}

pub(crate) use sql_driver_store;
