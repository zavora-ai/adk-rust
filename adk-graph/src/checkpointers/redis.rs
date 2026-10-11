//! Redis [`Checkpointer`].
//!
//! # Key layout
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `adk:graph:{<thread>}:versions` | Sorted set | Checkpoint ID, scored by version |
//! | `adk:graph:{<thread>}:data` | Hash | Checkpoint ID → checkpoint JSON |
//! | `adk:graph:id:{<id>}` | String | The thread that holds the checkpoint ID |
//!
//! Thread and checkpoint IDs are percent-encoded (`%`, `:`, `{`, `}`), so an ID cannot
//! spill into the next segment or move a key's hash slot. A thread's two keys share a
//! hash tag, so the save script runs on Redis Cluster. Versions are sorted-set scores,
//! exact up to 2^53.

use async_trait::async_trait;
use fred::prelude::*;

use super::{decode, encode, storage_error};
use crate::checkpoint::{Checkpointer, RetentionPolicy, conflict, duplicate_id, ensure_newer};
use crate::error::Result;
use crate::state::Checkpoint;

const BACKEND: &str = "redis";

/// KEYS: versions, data. ARGV: version, checkpoint ID, checkpoint JSON.
/// Returns -1 when stored, or the thread's head version when it refuses.
const SAVE_SCRIPT: &str = r#"
local top = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')
if top[2] and tonumber(top[2]) >= tonumber(ARGV[1]) then
  return tonumber(top[2])
end
redis.call('ZADD', KEYS[1], ARGV[1], ARGV[2])
redis.call('HSET', KEYS[2], ARGV[2], ARGV[3])
return -1
"#;

/// KEYS: versions, data. ARGV: ZREVRANGEBYSCORE max, count (-1 for all), order.
/// Returns checkpoint JSON in the requested order.
const READ_SCRIPT: &str = r#"
local ids
if ARGV[3] == 'asc' then
  ids = redis.call('ZRANGE', KEYS[1], 0, -1)
else
  ids = redis.call('ZREVRANGEBYSCORE', KEYS[1], ARGV[1], '-inf', 'LIMIT', 0, ARGV[2])
end
if #ids == 0 then return {} end
return redis.call('HMGET', KEYS[2], unpack(ids))
"#;

/// KEYS: versions, data. ARGV: checkpoint IDs to remove.
const REMOVE_SCRIPT: &str = r#"
local removed = 0
for _, id in ipairs(ARGV) do
  removed = removed + redis.call('ZREM', KEYS[1], id)
  redis.call('HDEL', KEYS[2], id)
end
return removed
"#;

/// Percent-encodes the characters that delimit key segments or hash tags.
fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            '{' => encoded.push_str("%7B"),
            '}' => encoded.push_str("%7D"),
            other => encoded.push(other),
        }
    }
    encoded
}

fn thread_keys(thread_id: &str) -> Vec<String> {
    let prefix = format!("adk:graph:{{{}}}", encode_segment(thread_id));
    vec![format!("{prefix}:versions"), format!("{prefix}:data")]
}

fn id_key(checkpoint_id: &str) -> String {
    format!("adk:graph:id:{{{}}}", encode_segment(checkpoint_id))
}

/// Redis-backed [`Checkpointer`].
///
/// A save first claims the checkpoint ID with `SET NX`, then one Lua script compares
/// the version with the thread's highest and stores the checkpoint; a refused save
/// releases the ID. See the [module documentation](self) for the key layout.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::RedisCheckpointer;
///
/// # async fn example() -> adk_graph::Result<()> {
/// let checkpointer = RedisCheckpointer::new("redis://localhost:6379").await?;
/// checkpointer.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct RedisCheckpointer {
    client: Client,
}

impl std::fmt::Debug for RedisCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCheckpointer").finish_non_exhaustive()
    }
}

impl RedisCheckpointer {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        let fail =
            |e: fred::error::Error| storage_error(BACKEND, format!("connection failed: {e}"));
        let config = Config::from_url(url).map_err(fail)?;
        let client = Builder::from_config(config).build().map_err(fail)?;
        client.init().await.map_err(fail)?;
        Ok(Self::from_client(client))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the checkpointer's scripts into the server's script cache.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        for script in [SAVE_SCRIPT, READ_SCRIPT, REMOVE_SCRIPT] {
            let _: String =
                self.client.script_load(script).await.map_err(|e| storage_error(BACKEND, e))?;
        }
        Ok(())
    }

    async fn read(
        &self,
        thread_id: &str,
        max: &str,
        count: i64,
        order: &str,
    ) -> Result<Vec<Checkpoint>> {
        let documents: Vec<Option<String>> = self
            .client
            .eval(
                READ_SCRIPT,
                thread_keys(thread_id),
                vec![max.to_string(), count.to_string(), order.to_string()],
            )
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        documents.into_iter().flatten().map(|data| decode(BACKEND, &data)).collect()
    }
}

#[async_trait]
impl Checkpointer for RedisCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        let claimed: Option<String> = self
            .client
            .set(
                id_key(&checkpoint.checkpoint_id),
                checkpoint.thread_id.as_str(),
                None,
                Some(SetOptions::NX),
                false,
            )
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        if claimed.is_none() {
            return Err(duplicate_id(&checkpoint.checkpoint_id));
        }
        let stored: Result<i64> = self
            .client
            .eval(
                SAVE_SCRIPT,
                thread_keys(&checkpoint.thread_id),
                vec![
                    checkpoint.version.to_string(),
                    checkpoint.checkpoint_id.clone(),
                    encode(checkpoint)?,
                ],
            )
            .await
            .map_err(|e| storage_error(BACKEND, e));
        match stored {
            Ok(-1) => Ok(checkpoint.checkpoint_id.clone()),
            refused => {
                let _: i64 = self
                    .client
                    .del(id_key(&checkpoint.checkpoint_id))
                    .await
                    .map_err(|e| storage_error(BACKEND, e))?;
                Err(conflict(checkpoint, refused? as u64))
            }
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self.read(thread_id, "+inf", 1, "desc").await?.pop())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let thread: Option<String> =
            self.client.get(id_key(checkpoint_id)).await.map_err(|e| storage_error(BACKEND, e))?;
        let Some(thread) = thread else { return Ok(None) };
        let data: Option<String> = self
            .client
            .hget(&thread_keys(&thread)[1], checkpoint_id)
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        data.map(|data| decode(BACKEND, &data)).transpose()
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        self.read(thread_id, "+inf", -1, "asc").await
    }

    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let max = before.map_or_else(|| "+inf".to_string(), |before| format!("({before}"));
        self.read(thread_id, &max, i64::try_from(limit).unwrap_or(i64::MAX), "desc").await
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        let ids: Vec<String> = self
            .list(thread_id)
            .await?
            .into_iter()
            .map(|checkpoint| id_key(&checkpoint.checkpoint_id))
            .collect();
        let _: i64 =
            self.client.del(thread_keys(thread_id)).await.map_err(|e| storage_error(BACKEND, e))?;
        // ID keys live in their own hash slots, so they are removed one at a time.
        for key in ids {
            let _: i64 = self.client.del(key).await.map_err(|e| storage_error(BACKEND, e))?;
        }
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let expired = policy.expired(&self.list(thread_id).await?);
        if expired.is_empty() {
            return Ok(0);
        }
        let removed: i64 = self
            .client
            .eval(REMOVE_SCRIPT, thread_keys(thread_id), expired.clone())
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        for checkpoint_id in &expired {
            let _: i64 = self
                .client
                .del(id_key(checkpoint_id))
                .await
                .map_err(|e| storage_error(BACKEND, e))?;
        }
        Ok(removed as usize)
    }
}
