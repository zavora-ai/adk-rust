//! Statements and the [`ChannelBindingStore`](super::ChannelBindingStore) implementation
//! shared by the SQLite and PostgreSQL backends (spec 3.3 §10 K1).
//!
//! One row per binding in `adk_kernel_channel_bindings` and one per pairing request in
//! `adk_kernel_channel_requests`, keyed by channel and account with a unique code hash, each
//! holding its record as JSON text beside the columns the store orders and prunes by. A
//! request is taken with one `DELETE … RETURNING`, so two acceptances of one code take it
//! once.

use crate::error::KernelError;

/// The migration registry of the channel binding store, distinct from every other store's.
pub(crate) const REGISTRY_TABLE: &str = "_adk_kernel_channel_migrations";

pub(crate) const PUT_BINDING: &str = "\
INSERT INTO adk_kernel_channel_bindings (id, paired_us, record) VALUES ($1, $2, $3) \
ON CONFLICT (id) DO UPDATE SET paired_us = excluded.paired_us, record = excluded.record";

pub(crate) const GET_BINDING: &str = "SELECT record FROM adk_kernel_channel_bindings WHERE id = $1";

pub(crate) const LIST_BINDINGS: &str =
    "SELECT record FROM adk_kernel_channel_bindings ORDER BY paired_us, id";

pub(crate) const PUT_REQUEST: &str = "\
INSERT INTO adk_kernel_channel_requests \
    (channel, account, code_hash, requested_us, expires_us, record) \
VALUES ($1, $2, $3, $4, $5, $6) \
ON CONFLICT (channel, account) DO UPDATE SET code_hash = excluded.code_hash, \
    requested_us = excluded.requested_us, expires_us = excluded.expires_us, \
    record = excluded.record";

pub(crate) const GET_REQUEST: &str =
    "SELECT record FROM adk_kernel_channel_requests WHERE channel = $1 AND account = $2";

pub(crate) const TAKE_REQUEST: &str =
    "DELETE FROM adk_kernel_channel_requests WHERE code_hash = $1 RETURNING record";

pub(crate) const LIST_REQUESTS: &str =
    "SELECT record FROM adk_kernel_channel_requests ORDER BY requested_us, channel, account";

pub(crate) const PRUNE_REQUESTS: &str =
    "DELETE FROM adk_kernel_channel_requests WHERE expires_us <= $1";

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store {
        backend,
        reason: format!("could not {action} the channel binding table: {error}"),
    }
}

/// Implements [`ChannelBindingStore`](crate::channels::ChannelBindingStore) for a store with
/// a `pool` field over the shared statements.
macro_rules! sql_channel_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::channels::ChannelBindingStore for $store {
            async fn put_binding(
                &self,
                binding: $crate::channels::ChannelBinding,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::channels::sql::PUT_BINDING)
                    .bind(&binding.id)
                    .bind(binding.paired_at.timestamp_micros())
                    .bind(serde_json::to_string(&binding)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get_binding(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::channels::ChannelBinding>> {
                let row: Option<(String,)> = sqlx::query_as($crate::channels::sql::GET_BINDING)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| $crate::pairing::sql::decode($backend, "channel binding", &text))
                    .transpose()
            }

            async fn list_bindings(
                &self,
            ) -> $crate::error::Result<Vec<$crate::channels::ChannelBinding>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::channels::sql::LIST_BINDINGS)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| $crate::pairing::sql::decode($backend, "channel binding", text))
                    .collect()
            }

            async fn put_request(
                &self,
                request: $crate::channels::ChannelPairingRequest,
            ) -> $crate::error::Result<()> {
                sqlx::query($crate::channels::sql::PUT_REQUEST)
                    .bind(&request.channel)
                    .bind(&request.account)
                    .bind(&request.code_hash)
                    .bind(request.requested_at.timestamp_micros())
                    .bind(request.expires_at.timestamp_micros())
                    .bind(serde_json::to_string(&request)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get_request(
                &self,
                channel: &str,
                account: &str,
            ) -> $crate::error::Result<Option<$crate::channels::ChannelPairingRequest>> {
                let row: Option<(String,)> = sqlx::query_as($crate::channels::sql::GET_REQUEST)
                    .bind(channel)
                    .bind(account)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "read", &e))?;
                row.map(|(text,)| {
                    $crate::pairing::sql::decode($backend, "channel pairing request", &text)
                })
                .transpose()
            }

            async fn take_request(
                &self,
                code_hash: &str,
            ) -> $crate::error::Result<Option<$crate::channels::ChannelPairingRequest>> {
                let row: Option<(String,)> = sqlx::query_as($crate::channels::sql::TAKE_REQUEST)
                    .bind(code_hash)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "take from", &e))?;
                row.map(|(text,)| {
                    $crate::pairing::sql::decode($backend, "channel pairing request", &text)
                })
                .transpose()
            }

            async fn list_requests(
                &self,
            ) -> $crate::error::Result<Vec<$crate::channels::ChannelPairingRequest>> {
                let rows: Vec<(String,)> = sqlx::query_as($crate::channels::sql::LIST_REQUESTS)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(text,)| {
                        $crate::pairing::sql::decode($backend, "channel pairing request", text)
                    })
                    .collect()
            }

            async fn prune_requests(
                &self,
                before: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<usize> {
                let done = sqlx::query($crate::channels::sql::PRUNE_REQUESTS)
                    .bind(before.timestamp_micros())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::channels::sql::db_error($backend, "prune", &e))?;
                Ok(usize::try_from(done.rows_affected()).unwrap_or(usize::MAX))
            }
        }
    };
}

pub(crate) use sql_channel_store;
