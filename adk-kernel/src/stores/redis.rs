//! The Redis [`ProcessStore`] (spec 3.2 §7.1, §13 Q8, feature `redis`).

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{Datelike as _, NaiveDate};
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;

use super::{ProcessFilter, ProcessStore, SpendScope};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: layout. ARGV: this release's layout version. Records it unless the stored one is
/// newer, and returns the stored one.
const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// KEYS: row, all-pids index. ARGV: pid, then the row's field-value pairs. Replaces the row
/// and moves its index entries.
const PUT_SCRIPT: &str = r"
local row, pid = KEYS[1], ARGV[1]
local old = redis.call('HMGET', row, 'idx_partition', 'idx_queue', 'idx_parent', 'idx_suspend')
for i = 1, 4 do if old[i] then redis.call('ZREM', old[i], pid) end end
redis.call('DEL', row)
redis.call('HSET', row, unpack(ARGV, 2))
local new = redis.call('HMGET', row, 'idx_partition', 'idx_queue', 'idx_parent', 'idx_suspend')
for i = 1, 4 do if new[i] then redis.call('ZADD', new[i], 0, pid) end end
redis.call('ZADD', KEYS[2], 0, pid)
return 1
";

/// KEYS: row. ARGV: `1` to set or `0` to clear, the wake set's JSON. Returns 0 when the row
/// does not exist.
const SET_WAKE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
if ARGV[1] == '1' then
  redis.call('HSET', KEYS[1], 'wake', ARGV[2])
else
  redis.call('HDEL', KEYS[1], 'wake')
end
return 1
";

/// KEYS: row. ARGV: pid, queue name, the queue's index key. Returns 0 when the row does not
/// exist.
const SET_QUEUE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
local old = redis.call('HGET', KEYS[1], 'idx_queue')
if old then redis.call('ZREM', old, ARGV[1]) end
redis.call('HSET', KEYS[1], 'queue', ARGV[2], 'idx_queue', ARGV[3])
redis.call('ZADD', ARGV[3], 0, ARGV[1])
return 1
";

/// KEYS: row, the pending-suspension index. ARGV: pid, `1` to request or `0` to clear.
/// Returns 0 when the row does not exist.
const SET_SUSPEND_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
redis.call('HSET', KEYS[1], 'suspend_requested', ARGV[2])
if ARGV[2] == '1' then
  redis.call('HSET', KEYS[1], 'idx_suspend', KEYS[2])
  redis.call('ZADD', KEYS[2], 0, ARGV[1])
else
  redis.call('HDEL', KEYS[1], 'idx_suspend')
  redis.call('ZREM', KEYS[2], ARGV[1])
end
return 1
";

/// KEYS: row, all-pids index. ARGV: pid. Returns 0 when the row does not exist.
const DELETE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
local old = redis.call('HMGET', KEYS[1], 'idx_partition', 'idx_queue', 'idx_parent', 'idx_suspend')
for i = 1, 4 do if old[i] then redis.call('ZREM', old[i], ARGV[1]) end end
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return 1
";

/// KEYS: the index to walk. ARGV: row key prefix, then the partition, queue and parent index
/// keys a row must be in (empty for any), then the pending suspension request it must have
/// (`1`, `0`, or empty for any), then the limit (negative for none). Returns the matching
/// rows' fields in pid order.
const LIST_SCRIPT: &str = r"
local limit = tonumber(ARGV[6])
local out = {}
for _, pid in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  if limit >= 0 and #out >= limit then break end
  local row = ARGV[1] .. pid
  local idx = redis.call('HMGET', row, 'idx_partition', 'idx_queue', 'idx_parent', 'suspend_requested')
  if (ARGV[2] == '' or idx[1] == ARGV[2])
    and (ARGV[3] == '' or idx[2] == ARGV[3])
    and (ARGV[4] == '' or idx[3] == ARGV[4])
    and (ARGV[5] == '' or (idx[4] or '0') == ARGV[5]) then
    local fields = redis.call('HGETALL', row)
    if #fields > 0 then table.insert(out, fields) end
  end
end
return out
";

/// KEYS: the process's spend hash, the partition's spend hash. ARGV: day, micro-USD.
const ADD_SPEND_SCRIPT: &str = r"
redis.call('HINCRBY', KEYS[1], ARGV[1], ARGV[2])
redis.call('HINCRBY', KEYS[2], ARGV[1], ARGV[2])
return 1
";

/// A [`ProcessStore`] in Redis, shared by every replica (spec 3.2 §7.1, feature `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot; braces in the
/// namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:process:<pid>` | Hash | One process: `pid`, `partition`, `queue`, `kind`, `labels`, `suspend_requested` (`1` or `0`), and `parent`, `wake`, `suspended_by`, `agenda_item` when set (structured fields as JSON), plus `idx_partition`, `idx_queue`, `idx_parent`, `idx_suspend`, the index keys the row is in |
/// | `{<ns>}:processes` | Sorted set | Every pid, all scored 0, so it ranges in byte order |
/// | `{<ns>}:processes:partition:<partition>`, `:queue:<queue>`, `:parent:<pid>` | Sorted set | The pids of one partition, queue class or parent, scored 0 |
/// | `{<ns>}:processes:suspend_requested` | Sorted set | The pids with a pending suspension request, scored 0 |
/// | `{<ns>}:spend:process:<pid>`, `{<ns>}:spend:partition:<partition>` | Hash | Micro-USD per UTC day (days from the common era) of one process, and of every process of one partition |
/// | `{<ns>}:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// Identifiers follow a fixed prefix and end the key, so no identifier can spill into
/// another key or move a key's hash slot.
///
/// Every change runs as one Lua script, which Redis executes atomically: `put` replaces
/// the row and moves its index entries, `set_wake` and `set_queue` change their field
/// only when the row exists, `delete` removes the row with its index entries, and `list`
/// walks one index and filters on the others in a single consistent read. `add_spend`
/// increments the process's and the partition's day totals in one script, so concurrent
/// additions all count; deleting a process keeps its spend, so a partition's totals for
/// past days do not change when the run store prunes a finished run.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::RedisProcessStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisProcessStore {
    client: Client,
    namespace: String,
}

impl RedisProcessStore {
    /// The namespace keys sit under by default.
    pub const DEFAULT_NAMESPACE: &'static str = "adk:kernel";

    /// A store over an initialized client, such as one shared with the run store.
    pub fn from_client(client: Client) -> Self {
        Self { client, namespace: Self::DEFAULT_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the URL is invalid or the server cannot be
    /// reached.
    pub async fn connect(url: &str) -> Result<Self> {
        let config =
            Config::from_url(url).map_err(|error| store_error("parse the Redis URL", &error))?;
        let client = Builder::from_config(config)
            .build()
            .map_err(|error| store_error("build the Redis client", &error))?;
        client.init().await.map_err(|error| store_error("connect to Redis", &error))?;
        Ok(Self::from_client(client))
    }

    /// Keeps the store's keys under `namespace`, for example to separate deployments that
    /// share a server.
    #[must_use]
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// The client the store uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The namespace the store's keys sit under.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Records the store's layout version, refusing a database a newer release wrote.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server fails or holds a newer layout.
    pub async fn migrate(&self) -> Result<()> {
        let stored: i64 = self
            .client
            .eval(MIGRATE_SCRIPT, vec![self.key("layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the keys under {} have layout v{stored}, but this release knows up to \
                     v{LAYOUT}; upgrade ADK-Rust",
                    self.namespace
                ),
            });
        }
        Ok(())
    }

    /// `{<namespace>}:<suffix>`.
    fn key(&self, suffix: &str) -> String {
        let namespace = self.namespace.replace('{', "%7B").replace('}', "%7D");
        format!("{{{namespace}}}:{suffix}")
    }

    fn row_key(&self, pid: &str) -> String {
        self.key(&format!("process:{pid}"))
    }

    fn index_key(&self, index: &str, value: &str) -> String {
        self.key(&format!("processes:{index}:{value}"))
    }

    async fn eval(&self, script: &str, keys: Vec<String>, args: Vec<String>) -> Result<i64> {
        self.client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("write the process table", &error))
    }
}

/// The suffix of the index of pids with a pending suspension request.
const SUSPEND_INDEX: &str = "processes:suspend_requested";

/// A flag as stored.
fn flag(set: bool) -> &'static str {
    if set { "1" } else { "0" }
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

/// Reads a row's fields back into the [`ProcessMeta`] that was put.
fn decode(mut fields: HashMap<String, String>) -> Result<ProcessMeta> {
    let pid = fields
        .remove("pid")
        .ok_or_else(|| store_error("read a process row", &"the row has no `pid` field"))?;
    let mut required = |field: &str| {
        fields.remove(field).ok_or_else(|| KernelError::Store {
            backend: "redis",
            reason: format!("the row of process {pid} has no `{field}` field"),
        })
    };
    let partition = required("partition")?;
    let queue = required("queue")?;
    let kind = required("kind")?;
    let labels = required("labels")?;
    let corrupt = |field: &str, error: serde_json::Error| KernelError::Store {
        backend: "redis",
        reason: format!("field `{field}` of process {pid} is corrupt: {error}"),
    };
    let kind = serde_json::from_str(&kind).map_err(|e| corrupt("kind", e))?;
    let labels = serde_json::from_str(&labels).map_err(|e| corrupt("labels", e))?;
    let wake = fields
        .remove("wake")
        .map(|wake| serde_json::from_str(&wake))
        .transpose()
        .map_err(|e| corrupt("wake", e))?;
    let suspended_by = fields
        .remove("suspended_by")
        .map(|suspension| serde_json::from_str(&suspension))
        .transpose()
        .map_err(|e| corrupt("suspended_by", e))?;
    let suspend_requested = fields.remove("suspend_requested").as_deref() == Some("1");
    Ok(ProcessMeta {
        parent: fields.remove("parent"),
        partition: PartitionId::new(partition),
        queue: QueueClass::from_name(&queue),
        kind,
        wake,
        suspended_by,
        suspend_requested,
        agenda_item: fields.remove("agenda_item"),
        labels,
        pid,
    })
}

/// The field-value pairs of a list-script row reply.
fn reply_fields(row: Value) -> Result<HashMap<String, String>> {
    let items = row.into_array();
    let mut fields = HashMap::with_capacity(items.len() / 2);
    let mut items = items.into_iter();
    while let (Some(field), Some(value)) = (items.next(), items.next()) {
        match (field.into_string(), value.into_string()) {
            (Some(field), Some(value)) => {
                fields.insert(field, value);
            }
            _ => return Err(store_error("list processes", &"a row field is not text")),
        }
    }
    Ok(fields)
}

#[async_trait]
impl ProcessStore for RedisProcessStore {
    async fn put(&self, meta: ProcessMeta) -> Result<()> {
        let mut args = vec![meta.pid.clone()];
        let mut field = |name: &str, value: String| {
            args.push(name.to_string());
            args.push(value);
        };
        field("pid", meta.pid.clone());
        field("partition", meta.partition.as_str().to_string());
        field("idx_partition", self.index_key("partition", meta.partition.as_str()));
        field("queue", meta.queue.as_str().to_string());
        field("idx_queue", self.index_key("queue", meta.queue.as_str()));
        field("kind", serde_json::to_string(&meta.kind)?);
        field("labels", serde_json::to_string(&meta.labels)?);
        field("suspend_requested", flag(meta.suspend_requested).to_string());
        if meta.suspend_requested {
            field("idx_suspend", self.key(SUSPEND_INDEX));
        }
        if let Some(parent) = &meta.parent {
            field("parent", parent.clone());
            field("idx_parent", self.index_key("parent", parent));
        }
        if let Some(wake) = &meta.wake {
            field("wake", serde_json::to_string(wake)?);
        }
        if let Some(suspension) = &meta.suspended_by {
            field("suspended_by", serde_json::to_string(suspension)?);
        }
        if let Some(item) = &meta.agenda_item {
            field("agenda_item", item.clone());
        }
        self.eval(PUT_SCRIPT, vec![self.row_key(&meta.pid), self.key("processes")], args).await?;
        Ok(())
    }

    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>> {
        let fields: HashMap<String, String> = self
            .client
            .hgetall(self.row_key(pid))
            .await
            .map_err(|error| store_error("read the process table", &error))?;
        if fields.is_empty() {
            return Ok(None);
        }
        decode(fields).map(Some)
    }

    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        let partition = filter
            .partition
            .as_ref()
            .map(|partition| self.index_key("partition", partition.as_str()));
        let queue = filter.queue.as_ref().map(|queue| self.index_key("queue", queue.as_str()));
        let parent = filter.parent.as_ref().map(|parent| self.index_key("parent", parent));
        // Walk the narrowest index the filter names; the script checks the others.
        let walk = (filter.suspend_requested == Some(true))
            .then(|| self.key(SUSPEND_INDEX))
            .or_else(|| parent.clone())
            .or_else(|| partition.clone())
            .or_else(|| queue.clone())
            .unwrap_or_else(|| self.key("processes"));
        let limit = filter.limit.map_or(-1, |limit| i64::try_from(limit).unwrap_or(i64::MAX));
        let reply: Value = self
            .client
            .eval(
                LIST_SCRIPT,
                vec![walk],
                vec![
                    self.row_key(""),
                    partition.unwrap_or_default(),
                    queue.unwrap_or_default(),
                    parent.unwrap_or_default(),
                    filter.suspend_requested.map(flag).unwrap_or_default().to_string(),
                    limit.to_string(),
                ],
            )
            .await
            .map_err(|error| store_error("list processes", &error))?;
        reply.into_array().into_iter().map(|row| decode(reply_fields(row)?)).collect()
    }

    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()> {
        let args = match &wake {
            Some(wake) => vec!["1".to_string(), serde_json::to_string(wake)?],
            None => vec!["0".to_string(), String::new()],
        };
        if self.eval(SET_WAKE_SCRIPT, vec![self.row_key(pid)], args).await? == 0 {
            return Err(KernelError::NotFound { pid: pid.into() });
        }
        Ok(())
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()> {
        let args = vec![
            pid.to_string(),
            queue.as_str().to_string(),
            self.index_key("queue", queue.as_str()),
        ];
        if self.eval(SET_QUEUE_SCRIPT, vec![self.row_key(pid)], args).await? == 0 {
            return Err(KernelError::NotFound { pid: pid.into() });
        }
        Ok(())
    }

    async fn delete(&self, pid: &str) -> Result<bool> {
        let keys = vec![self.row_key(pid), self.key("processes")];
        Ok(self.eval(DELETE_SCRIPT, keys, vec![pid.to_string()]).await? == 1)
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()> {
        let keys = vec![self.row_key(pid), self.key(SUSPEND_INDEX)];
        let args = vec![pid.to_string(), flag(requested).to_string()];
        if self.eval(SET_SUSPEND_SCRIPT, keys, args).await? == 0 {
            return Err(KernelError::NotFound { pid: pid.into() });
        }
        Ok(())
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()> {
        let micro_usd = i64::try_from(delta.micro_usd).map_err(|_| KernelError::Store {
            backend: "redis",
            reason: format!(
                "a spend delta of {} micro-USD exceeds a Redis integer",
                delta.micro_usd
            ),
        })?;
        let keys = vec![
            self.key(&format!("spend:process:{pid}")),
            self.key(&format!("spend:partition:{partition}")),
        ];
        let args = vec![day.num_days_from_ce().to_string(), micro_usd.to_string()];
        self.client
            .eval::<i64, _, _, _>(ADD_SPEND_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("add to the spend totals", &error))?;
        Ok(())
    }

    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64> {
        let key = match scope {
            SpendScope::Process(pid) => self.key(&format!("spend:process:{pid}")),
            SpendScope::Partition(partition) => self.key(&format!("spend:partition:{partition}")),
        };
        let days: HashMap<String, String> = self
            .client
            .hgetall(key)
            .await
            .map_err(|error| store_error("read the spend totals", &error))?;
        let range = from.num_days_from_ce()..=to_inclusive.num_days_from_ce();
        let mut total = 0_u64;
        for (day, micro_usd) in days {
            let (Ok(number), Ok(amount)) = (day.parse::<i32>(), micro_usd.parse::<u64>()) else {
                return Err(store_error(
                    "read the spend totals",
                    &format!("the day total {day} = {micro_usd} is corrupt"),
                ));
            };
            if range.contains(&number) {
                total = total.saturating_add(amount);
            }
        }
        Ok(total)
    }
}
