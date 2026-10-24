//! Statements and the [`DeviceStore`](super::DeviceStore) implementation shared by the
//! SQLite and PostgreSQL backends (spec 3.3 §10 K2).
//!
//! One row per device in `adk_kernel_devices` and one per waiting code in
//! `adk_kernel_pairing_codes`, each holding its record as JSON text beside the columns the
//! store orders and prunes by. A code is taken with one `DELETE … RETURNING`, so two claims
//! of one code take it once.

use crate::error::KernelError;

/// The migration registry of the device store, distinct from every other store's.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_device_migrations";

pub(crate) const PUT_CODE: &str = "\
INSERT INTO adk_kernel_pairing_codes (code_hash, expires_us, record) VALUES ($1, $2, $3) \
ON CONFLICT (code_hash) DO UPDATE SET expires_us = excluded.expires_us, record = excluded.record";

pub(crate) const TAKE_CODE: &str =
    "DELETE FROM adk_kernel_pairing_codes WHERE code_hash = $1 RETURNING record";

pub(crate) const PRUNE_CODES: &str = "DELETE FROM adk_kernel_pairing_codes WHERE expires_us <= $1";

pub(crate) const CLEAR_CODES: &str = "DELETE FROM adk_kernel_pairing_codes";

pub(crate) const PUT_DEVICE: &str = "\
INSERT INTO adk_kernel_devices (id, created_us, record) VALUES ($1, $2, $3) \
ON CONFLICT (id) DO UPDATE SET created_us = excluded.created_us, record = excluded.record";

pub(crate) const GET_DEVICE: &str = "SELECT record FROM adk_kernel_devices WHERE id = $1";

pub(crate) const LIST_DEVICES: &str =
    "SELECT record FROM adk_kernel_devices ORDER BY created_us, id";

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the device table: {error}") }
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

/// Implements [`DeviceStore`](crate::pairing::DeviceStore) for a store with a `pool` field
/// over the shared statements.
macro_rules! sql_device_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::pairing::DeviceStore for $store {
            async fn put_code(
                &self,
                code: $crate::pairing::PairingCode,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::pairing::sql::PUT_CODE)
                    .bind(&code.code_hash)
                    .bind(code.expires_at.timestamp_micros())
                    .bind(serde_json::to_string(&code)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn take_code(
                &self,
                code_hash: &str,
            ) -> $crate::error::Result<Option<$crate::pairing::PairingCode>> {
                let row: Option<(String,)> = sqlx::query_as($crate::pairing::sql::TAKE_CODE)
                    .bind(code_hash)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "take from", &e))?;
                row.map(|(text,)| $crate::pairing::sql::decode($backend, "pairing code", &text))
                    .transpose()
            }

            async fn prune_codes(
                &self,
                before: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<usize> {
                let done = sqlx::query($crate::pairing::sql::PRUNE_CODES)
                    .bind(before.timestamp_micros())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "prune", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }

            async fn clear_codes(&self) -> $crate::error::Result<usize> {
                let done = sqlx::query($crate::pairing::sql::CLEAR_CODES)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "clear", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }

            async fn put_device(
                &self,
                device: $crate::pairing::DeviceRecord,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::pairing::sql::PUT_DEVICE)
                    .bind(&device.id)
                    .bind(device.created_at.timestamp_micros())
                    .bind(serde_json::to_string(&device)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get_device(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::pairing::DeviceRecord>> {
                let row: Option<(String,)> = sqlx::query_as($crate::pairing::sql::GET_DEVICE)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| $crate::pairing::sql::decode($backend, "device", &text))
                    .transpose()
            }

            async fn list_devices(
                &self,
            ) -> $crate::error::Result<Vec<$crate::pairing::DeviceRecord>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::pairing::sql::LIST_DEVICES)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::pairing::sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| $crate::pairing::sql::decode($backend, "device", text))
                    .collect()
            }
        }
    };
}

pub(crate) use sql_device_store;
