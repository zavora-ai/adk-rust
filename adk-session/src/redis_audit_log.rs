//! Redis-backed [`AuditLogStore`](adk_core::audit_log::AuditLogStore).
//!
//! # Key layout
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `adk:audit:{<log>}:size` | String | Number of records in the log, the value every append compares against |
//! | `adk:audit:{<log>}:leaves` | Hash | `seq` → leaf hash |
//! | `adk:audit:{<log>}:records` | Hash | `seq` → canonical record JSON; the field is deleted when pruned |
//! | `adk:audit:{<log>}:nodes` | Hash | `<level>:<index>` → subtree hash |
//! | `adk:audit:{<log>}:checkpoints` | Hash | `size` → JSON [`AuditCheckpoint`](adk_core::audit_log::AuditCheckpoint) |
//! | `adk:audit:logs` | Set | Every log id written to |
//!
//! The log id is percent-encoded (`%`, `:`, `{`, `}`) and used as the hash tag, so a
//! log's keys share one slot and the store runs on Redis Cluster.
//!
//! # Atomicity
//!
//! | Operation | Mechanism |
//! |-----------|-----------|
//! | `append` | One script compares the first `seq` with `size`, then writes leaves, records, nodes, and the new size; a mismatch writes nothing |
//! | `put_checkpoint` | One script checks the size and any existing root at that size, then writes the checkpoint |
//! | `prune` | One script checks the size and deletes record fields below the position |
//!
//! The batch's shape (contiguity, records present, nodes inside the batch) is checked
//! before the script runs, so a malformed batch never reaches the server. The `logs`
//! registry lives in its own slot and is written before the first append script, so a
//! listed log can hold no records when its first append was refused.

use std::collections::HashMap;
use std::ops::Range;

use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore,
    checkpoint_conflict_error, out_of_range_error, seq_conflict_error, validate_append,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use fred::prelude::*;

const REGISTRY_KEY: &str = "adk:audit:logs";

/// KEYS: size, leaves, records, nodes. ARGV: first seq, entry count, then a leaf hash
/// and record per entry, then a field and hash per node.
/// Returns `ok`, or `conflict:<size>` when the first seq is not the log's size.
const APPEND_SCRIPT: &str = r#"
local size = tonumber(redis.call('GET', KEYS[1]) or '0')
local first = tonumber(ARGV[1])
if first ~= size then return 'conflict:' .. size end
local count = tonumber(ARGV[2])
for i = 0, count - 1 do
  local seq = tostring(first + i)
  redis.call('HSET', KEYS[2], seq, ARGV[3 + 2 * i])
  redis.call('HSET', KEYS[3], seq, ARGV[4 + 2 * i])
end
for i = 3 + 2 * count, #ARGV, 2 do
  redis.call('HSET', KEYS[4], ARGV[i], ARGV[i + 1])
end
redis.call('SET', KEYS[1], tostring(first + count))
return 'ok'
"#;

/// KEYS: size, checkpoints. ARGV: checkpoint size, root hash, checkpoint JSON.
/// Returns `ok`, `range:<size>` when the checkpoint passes the log's end, or
/// `conflict` when a checkpoint at that size has a different root.
const CHECKPOINT_SCRIPT: &str = r#"
local size = tonumber(redis.call('GET', KEYS[1]) or '0')
if tonumber(ARGV[1]) > size then return 'range:' .. size end
local existing = redis.call('HGET', KEYS[2], ARGV[1])
if existing and cjson.decode(existing).rootHash ~= ARGV[2] then return 'conflict' end
redis.call('HSET', KEYS[2], ARGV[1], ARGV[3])
return 'ok'
"#;

/// KEYS: size, records. ARGV: position. Returns the number of bodies deleted, or
/// `range:<size>` when the position passes the log's end.
const PRUNE_SCRIPT: &str = r#"
local size = tonumber(redis.call('GET', KEYS[1]) or '0')
local before = tonumber(ARGV[1])
if before > size then return 'range:' .. size end
local pruned = 0
for seq = 0, before - 1 do
  pruned = pruned + redis.call('HDEL', KEYS[2], tostring(seq))
end
return tostring(pruned)
"#;

fn store_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.audit_log.storage",
        message,
    )
}

fn redis_error(operation: &str) -> impl FnOnce(fred::error::Error) -> AdkError + '_ {
    move |e| store_error(format!("audit log {operation} failed: {e}"))
}

fn corrupt(what: &str, error: impl std::fmt::Display) -> AdkError {
    store_error(format!("stored audit log {what} is corrupt: {error}"))
}

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

struct LogKeys {
    size: String,
    leaves: String,
    records: String,
    nodes: String,
    checkpoints: String,
}

impl LogKeys {
    fn new(log_id: &str) -> Self {
        let prefix = format!("adk:audit:{{{}}}", encode_segment(log_id));
        Self {
            size: format!("{prefix}:size"),
            leaves: format!("{prefix}:leaves"),
            records: format!("{prefix}:records"),
            nodes: format!("{prefix}:nodes"),
            checkpoints: format!("{prefix}:checkpoints"),
        }
    }
}

/// Parses a script reply of the form `<tag>:<size>`.
fn tagged_size(reply: &str, tag: &str) -> Option<u64> {
    reply.strip_prefix(tag)?.strip_prefix(':')?.parse().ok()
}

/// Redis-backed [`AuditLogStore`].
///
/// Each append, checkpoint, and prune is one Lua script, so two processes writing one
/// `log_id` cannot fork it: the second append is refused with `audit_log.seq_conflict`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::RedisAuditLogStore;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = RedisAuditLogStore::new("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisAuditLogStore {
    client: Client,
}

impl RedisAuditLogStore {
    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when the server cannot be reached.
    pub async fn new(url: &str) -> Result<Self> {
        let config = Config::from_url(url)
            .map_err(|e| store_error(format!("redis connection failed: {e}")))?;
        let client = Builder::from_config(config)
            .build()
            .map_err(|e| store_error(format!("redis connection failed: {e}")))?;
        client.init().await.map_err(|e| store_error(format!("redis connection failed: {e}")))?;
        Ok(Self::from_client(client))
    }

    /// Uses an initialized client, such as one shared with other Redis stores.
    pub fn from_client(client: Client) -> Self {
        Self { client }
    }

    /// Loads the store's scripts into the server's script cache.
    ///
    /// Redis needs no schema; this exists so every durable store is set up the same way.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when the server rejects a script.
    pub async fn migrate(&self) -> Result<()> {
        for script in [APPEND_SCRIPT, CHECKPOINT_SCRIPT, PRUNE_SCRIPT] {
            let _: String =
                self.client.script_load(script).await.map_err(redis_error("script load"))?;
        }
        Ok(())
    }
}

#[async_trait]
impl AuditLogStore for RedisAuditLogStore {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        // Checks the batch's shape against its own start; the script checks the start.
        let first = batch.entries.first().map_or(0, |entry| entry.seq);
        let range = validate_append(&batch, first)?;
        let keys = LogKeys::new(&batch.log_id);
        let _: i64 = self
            .client
            .sadd(REGISTRY_KEY, batch.log_id.as_str())
            .await
            .map_err(redis_error("append"))?;
        let mut args = vec![range.start.to_string(), batch.entries.len().to_string()];
        for entry in &batch.entries {
            args.push(entry.leaf_hash.to_string());
            args.push(entry.record.clone().unwrap_or_default());
        }
        for node in &batch.nodes {
            args.push(format!("{}:{}", node.level, node.index));
            args.push(node.hash.to_string());
        }
        let reply: String = self
            .client
            .eval(APPEND_SCRIPT, vec![keys.size, keys.leaves, keys.records, keys.nodes], args)
            .await
            .map_err(redis_error("append"))?;
        if reply == "ok" {
            return Ok(range);
        }
        match tagged_size(&reply, "conflict") {
            Some(size) => Err(seq_conflict_error(&batch.log_id, size, range.start)),
            None => Err(store_error(format!("unexpected append script reply '{reply}'"))),
        }
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        let size: Option<String> =
            self.client.get(LogKeys::new(log_id).size).await.map_err(redis_error("read"))?;
        size.map_or(Ok(0), |size| size.parse().map_err(|e| corrupt("size", e)))
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let size = self.size(log_id).await?;
        if range.end > size {
            return Err(out_of_range_error(log_id, size, range.end));
        }
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        let keys = LogKeys::new(log_id);
        let fields: Vec<String> = range.clone().map(|seq| seq.to_string()).collect();
        let leaves: Vec<Option<String>> =
            self.client.hmget(keys.leaves, fields.clone()).await.map_err(redis_error("read"))?;
        let records: Vec<Option<String>> =
            self.client.hmget(keys.records, fields).await.map_err(redis_error("read"))?;
        range
            .zip(leaves)
            .zip(records)
            .map(|((seq, leaf), record)| {
                let leaf =
                    leaf.ok_or_else(|| corrupt("leaf hash", format!("seq {seq} missing")))?;
                Ok(AuditLogEntry {
                    seq,
                    leaf_hash: leaf.parse().map_err(|e| corrupt("leaf hash", e))?,
                    record,
                })
            })
            .collect()
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        let keys = LogKeys::new(log_id);
        let hash: Option<String> = if level == 0 {
            self.client.hget(keys.leaves, index.to_string()).await
        } else {
            self.client.hget(keys.nodes, format!("{level}:{index}")).await
        }
        .map_err(redis_error("read"))?;
        hash.map(|hash| hash.parse().map_err(|e| corrupt("hash", e))).transpose()
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        let keys = LogKeys::new(&checkpoint.log_id);
        let json = serde_json::to_string(&checkpoint)
            .map_err(|e| store_error(format!("audit checkpoint serialize failed: {e}")))?;
        let reply: String = self
            .client
            .eval(
                CHECKPOINT_SCRIPT,
                vec![keys.size, keys.checkpoints],
                vec![checkpoint.size.to_string(), checkpoint.root_hash.to_string(), json],
            )
            .await
            .map_err(redis_error("checkpoint write"))?;
        if reply == "ok" {
            return Ok(());
        }
        if reply == "conflict" {
            return Err(checkpoint_conflict_error(&checkpoint.log_id, checkpoint.size));
        }
        match tagged_size(&reply, "range") {
            Some(size) => Err(out_of_range_error(&checkpoint.log_id, size, checkpoint.size)),
            None => Err(store_error(format!("unexpected checkpoint script reply '{reply}'"))),
        }
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        Ok(self.checkpoints(log_id).await?.pop())
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let stored: HashMap<String, String> = self
            .client
            .hgetall(LogKeys::new(log_id).checkpoints)
            .await
            .map_err(redis_error("checkpoint read"))?;
        let mut checkpoints = stored
            .values()
            .map(|json| {
                serde_json::from_str::<AuditCheckpoint>(json).map_err(|e| corrupt("checkpoint", e))
            })
            .collect::<Result<Vec<_>>>()?;
        checkpoints.sort_by_key(|checkpoint| checkpoint.size);
        Ok(checkpoints)
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let keys = LogKeys::new(log_id);
        let reply: String = self
            .client
            .eval(PRUNE_SCRIPT, vec![keys.size, keys.records], vec![before_seq.to_string()])
            .await
            .map_err(redis_error("prune"))?;
        if let Some(size) = tagged_size(&reply, "range") {
            return Err(out_of_range_error(log_id, size, before_seq));
        }
        reply.parse().map_err(|_| store_error(format!("unexpected prune script reply '{reply}'")))
    }

    async fn logs(&self) -> Result<Vec<String>> {
        let mut ids: Vec<String> =
            self.client.smembers(REGISTRY_KEY).await.map_err(redis_error("read"))?;
        ids.sort();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_share_one_hash_tag_per_log() {
        let keys = LogKeys::new("replica:{1}");
        assert_eq!(keys.size, "adk:audit:{replica%3A%7B1%7D}:size");
        assert!(keys.checkpoints.starts_with("adk:audit:{replica%3A%7B1%7D}:"));
    }

    #[test]
    fn tagged_size_parses_script_replies() {
        assert_eq!(tagged_size("conflict:7", "conflict"), Some(7));
        assert_eq!(tagged_size("range:0", "range"), Some(0));
        assert_eq!(tagged_size("ok", "range"), None);
    }
}
