//! The Redis [`PartitionStore`] (spec 3.2 §7.8, §13 Q8, feature `redis`).

use async_trait::async_trait;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, KeysInterface as _, LuaInterface as _,
    SetsInterface as _,
};
use fred::types::Value;

use crate::error::{KernelError, Result};
use crate::partition::{CrossingFilter, CrossingRecord, Partition, PartitionId, PartitionStore};

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

/// KEYS: row, index. ARGV: id, record. Writes the record and indexes the id.
const PUT_SCRIPT: &str = r"
redis.call('SET', KEYS[1], ARGV[2])
redis.call('ZADD', KEYS[2], 0, ARGV[1])
return 1
";

/// KEYS: row, index. ARGV: id. Returns 0 when there was no record.
const DELETE_SCRIPT: &str = r"
local existed = redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return existed
";

/// KEYS: index. ARGV: row key prefix. Returns every indexed record, in id order.
const LIST_SCRIPT: &str = r"
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local record = redis.call('GET', ARGV[1] .. id)
  if record then table.insert(out, record) end
end
return out
";

/// KEYS: row, every-crossing index, run index, source index. ARGV: id, score, record.
const RECORD_SCRIPT: &str = r"
redis.call('SET', KEYS[1], ARGV[3])
for i = 2, 4 do redis.call('ZADD', KEYS[i], ARGV[2], ARGV[1]) end
return 1
";

/// KEYS: the index to walk. ARGV: row key prefix, least score (`-inf` for any). Returns the
/// records in (time, id) order.
const CROSSINGS_SCRIPT: &str = r"
local out = {}
for _, id in ipairs(redis.call('ZRANGEBYSCORE', KEYS[1], ARGV[2], '+inf')) do
  local record = redis.call('GET', ARGV[1] .. id)
  if record then table.insert(out, record) end
end
return out
";

/// KEYS: the run's grant set. Returns how many grants it held, then drops it.
const RELEASE_SCRIPT: &str = r"
local held = redis.call('SCARD', KEYS[1])
redis.call('DEL', KEYS[1])
return held
";

/// A [`PartitionStore`] in Redis, shared by every replica (spec 3.2 §7.1, §7.8, feature
/// `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot; braces in the
/// namespace are percent-encoded. The process store's keys under the same namespace do not
/// collide with these.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:partition:<id>` | String | One partition, as JSON |
/// | `{<ns>}:partitions` | Sorted set | Every partition id, scored 0, so it ranges in byte order |
/// | `{<ns>}:crossing:<id>` | String | One crossing record, as JSON |
/// | `{<ns>}:crossings`, `:crossings:run:<run>`, `:crossings:from:<partition>` | Sorted set | Crossing ids scored by time in microseconds: every crossing, one run's, one source partition's |
/// | `{<ns>}:crossing-grants:<run>` | Set | The fingerprint digests of a durable run's live grants, dropped when the run is released |
/// | `{<ns>}:partitions:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// Every write is one Lua script, which Redis runs atomically; crossing records are only
/// ever added.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::RedisPartitionStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisPartitionStore {
    client: Client,
    namespace: String,
}

impl RedisPartitionStore {
    /// The namespace keys sit under by default, shared with the process store.
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

    /// Keeps the store's keys under `namespace`.
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
            .eval(MIGRATE_SCRIPT, vec![self.key("partitions:layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the partition keys under {} have layout v{stored}, but this release \
                     knows up to v{LAYOUT}; upgrade ADK-Rust",
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

    async fn strings(
        &self,
        script: &str,
        keys: Vec<String>,
        args: Vec<String>,
    ) -> Result<Vec<String>> {
        let reply: Value = self
            .client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("read the partition table", &error))?;
        reply
            .into_array()
            .into_iter()
            .map(|value| {
                value
                    .into_string()
                    .ok_or_else(|| store_error("read the partition table", &"a record is not text"))
            })
            .collect()
    }
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

fn corrupt(what: &str, error: &serde_json::Error) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("a stored {what} is corrupt: {error}") }
}

#[async_trait]
impl PartitionStore for RedisPartitionStore {
    async fn put(&self, partition: Partition) -> Result<()> {
        let record = serde_json::to_string(&partition)?;
        let id = partition.id.as_str().to_string();
        let keys = vec![self.key(&format!("partition:{id}")), self.key("partitions")];
        self.client
            .eval::<i64, _, _, _>(PUT_SCRIPT, keys, vec![id, record])
            .await
            .map_err(|error| store_error("write the partition table", &error))?;
        Ok(())
    }

    async fn get(&self, id: &PartitionId) -> Result<Option<Partition>> {
        let record: Option<String> = self
            .client
            .get(self.key(&format!("partition:{id}")))
            .await
            .map_err(|error| store_error("read the partition table", &error))?;
        record
            .map(|record| serde_json::from_str(&record).map_err(|e| corrupt("partition", &e)))
            .transpose()
    }

    async fn list(&self) -> Result<Vec<Partition>> {
        let records = self
            .strings(LIST_SCRIPT, vec![self.key("partitions")], vec![self.key("partition:")])
            .await?;
        records
            .iter()
            .map(|record| serde_json::from_str(record).map_err(|e| corrupt("partition", &e)))
            .collect()
    }

    async fn delete(&self, id: &PartitionId) -> Result<bool> {
        let keys = vec![self.key(&format!("partition:{id}")), self.key("partitions")];
        let existed: i64 = self
            .client
            .eval(DELETE_SCRIPT, keys, vec![id.as_str().to_string()])
            .await
            .map_err(|error| store_error("delete from the partition table", &error))?;
        Ok(existed > 0)
    }

    async fn record_crossing(&self, record: CrossingRecord) -> Result<()> {
        let json = serde_json::to_string(&record)?;
        let keys = vec![
            self.key(&format!("crossing:{}", record.id)),
            self.key("crossings"),
            self.key(&format!("crossings:run:{}", record.run)),
            self.key(&format!("crossings:from:{}", record.from)),
        ];
        let args = vec![record.id.clone(), record.at.timestamp_micros().to_string(), json];
        self.client
            .eval::<i64, _, _, _>(RECORD_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("record a crossing", &error))?;
        Ok(())
    }

    async fn crossings(&self, filter: &CrossingFilter) -> Result<Vec<CrossingRecord>> {
        // Walk the narrowest index the filter names; the rest is checked here.
        let walk = match (&filter.run, &filter.from) {
            (Some(run), _) => self.key(&format!("crossings:run:{run}")),
            (None, Some(from)) => self.key(&format!("crossings:from:{from}")),
            (None, None) => self.key("crossings"),
        };
        let least = filter
            .since
            .map_or_else(|| "-inf".to_string(), |since| since.timestamp_micros().to_string());
        let records =
            self.strings(CROSSINGS_SCRIPT, vec![walk], vec![self.key("crossing:"), least]).await?;
        let mut out = Vec::new();
        for record in records {
            let record: CrossingRecord =
                serde_json::from_str(&record).map_err(|e| corrupt("crossing record", &e))?;
            if filter.matches(&record) {
                out.push(record);
            }
            if filter.limit.is_some_and(|limit| out.len() >= limit) {
                break;
            }
        }
        Ok(out)
    }

    async fn grant(&self, run: &str, fingerprint: &str) -> Result<()> {
        self.client
            .sadd::<i64, _, _>(self.key(&format!("crossing-grants:{run}")), fingerprint)
            .await
            .map_err(|error| store_error("record a crossing grant", &error))?;
        Ok(())
    }

    async fn grants(&self, run: &str) -> Result<Vec<String>> {
        let mut grants: Vec<String> = self
            .client
            .smembers(self.key(&format!("crossing-grants:{run}")))
            .await
            .map_err(|error| store_error("read the crossing grants", &error))?;
        grants.sort();
        Ok(grants)
    }

    async fn release_run(&self, run: &str) -> Result<u64> {
        let held: i64 = self
            .client
            .eval(
                RELEASE_SCRIPT,
                vec![self.key(&format!("crossing-grants:{run}"))],
                Vec::<String>::new(),
            )
            .await
            .map_err(|error| store_error("release the crossing grants", &error))?;
        Ok(u64::try_from(held).unwrap_or(0))
    }
}
