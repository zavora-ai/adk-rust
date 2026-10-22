//! The Redis [`AgendaStore`] (spec 3.2 §7.1, §7.5, §13 Q8, feature `redis`).

use async_trait::async_trait;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;

use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind, AgendaStore};
use crate::error::{KernelError, Result};

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

/// KEYS: row, the items index. ARGV: id, partition, state, score, item JSON. Replaces the
/// row and its index entry.
const PUT_SCRIPT: &str = r"
redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[1], 'partition', ARGV[2], 'state', ARGV[3], 'item', ARGV[5])
redis.call('ZADD', KEYS[2], ARGV[4], ARGV[1])
return 1
";

/// KEYS: row, the items index. ARGV: id, partition, state, score, item JSON, the state the
/// row must be in. Returns 0, writing nothing, when the row is missing or in another state.
const TRANSITION_SCRIPT: &str = r"
if redis.call('HGET', KEYS[1], 'state') ~= ARGV[6] then return 0 end
redis.call('HSET', KEYS[1], 'partition', ARGV[2], 'state', ARGV[3], 'item', ARGV[5])
redis.call('ZADD', KEYS[2], ARGV[4], ARGV[1])
return 1
";

/// KEYS: row, the items index. ARGV: id. Returns 0 when the row does not exist.
const DELETE_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return 1
";

/// KEYS: the items index. ARGV: row key prefix, the partition (empty for any), the limit
/// (negative for none), then the states a row may be in (none for any). Returns the
/// matching items' JSON in index order.
const LIST_SCRIPT: &str = r"
local limit = tonumber(ARGV[3])
local states = {}
local any_state = true
for i = 4, #ARGV do states[ARGV[i]] = true; any_state = false end
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  if limit >= 0 and #out >= limit then break end
  local row = redis.call('HMGET', ARGV[1] .. id, 'partition', 'state', 'item')
  if row[3] and (ARGV[2] == '' or row[1] == ARGV[2]) and (any_state or states[row[2]]) then
    table.insert(out, row[3])
  end
end
return out
";

/// An [`AgendaStore`] in Redis, shared by every replica (spec 3.2 §7.1, §7.5, feature
/// `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless [`with_namespace`](Self::with_namespace)
/// sets another, inside the hash tag `{<namespace>}`, so the store's scripts touch one
/// Redis Cluster slot; braces in the namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:agenda:item:<id>` | Hash | One item: `partition`, `state` (its kind) and `item` (the item's JSON) |
/// | `{<ns>}:agenda:items` | Sorted set | Every id, scored by `created_at` in microseconds, so it ranges oldest first and ties in byte order |
/// | `{<ns>}:agenda:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// Every change runs as one Lua script, which Redis executes atomically; `transition`
/// compares the stored state's kind and writes in the same script, so two replicas moving
/// one item from the same state move it once.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::RedisAgendaStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisAgendaStore {
    client: Client,
    namespace: String,
}

impl RedisAgendaStore {
    /// The namespace keys sit under by default, the process store's.
    pub const DEFAULT_NAMESPACE: &'static str = "adk:kernel";

    /// A store over an initialized client, such as one shared with the process store.
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
            .eval(MIGRATE_SCRIPT, vec![self.key("agenda:layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the agenda layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the agenda keys under {} have layout v{stored}, but this release knows up \
                     to v{LAYOUT}; upgrade ADK-Rust",
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

    fn row_key(&self, id: &str) -> String {
        self.key(&format!("agenda:item:{id}"))
    }

    fn index_key(&self) -> String {
        self.key("agenda:items")
    }

    fn row_args(item: &AgendaItem) -> Result<Vec<String>> {
        Ok(vec![
            item.id.clone(),
            item.partition.as_str().to_string(),
            item.state.kind().as_str().to_string(),
            item.created_at.timestamp_micros().to_string(),
            serde_json::to_string(item)?,
        ])
    }

    async fn eval(&self, script: &str, keys: Vec<String>, args: Vec<String>) -> Result<i64> {
        self.client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("write the agenda", &error))
    }
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

fn decode(item: &str) -> Result<AgendaItem> {
    serde_json::from_str(item).map_err(|error| KernelError::Store {
        backend: "redis",
        reason: format!("an agenda item is corrupt: {error}"),
    })
}

#[async_trait]
impl AgendaStore for RedisAgendaStore {
    async fn put(&self, item: AgendaItem) -> Result<()> {
        let keys = vec![self.row_key(&item.id), self.index_key()];
        self.eval(PUT_SCRIPT, keys, Self::row_args(&item)?).await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        let item: Option<String> = self
            .client
            .hget(self.row_key(id), "item")
            .await
            .map_err(|error| store_error("read the agenda", &error))?;
        item.as_deref().map(decode).transpose()
    }

    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        let mut args = vec![
            self.key("agenda:item:"),
            filter.partition.as_ref().map(|p| p.as_str().to_string()).unwrap_or_default(),
            filter.limit.map_or(-1, |limit| i64::try_from(limit).unwrap_or(i64::MAX)).to_string(),
        ];
        args.extend(filter.states.iter().map(|state| state.as_str().to_string()));
        let reply: Value = self
            .client
            .eval(LIST_SCRIPT, vec![self.index_key()], args)
            .await
            .map_err(|error| store_error("list the agenda", &error))?;
        reply
            .into_array()
            .into_iter()
            .map(|value| {
                let text = value
                    .into_string()
                    .ok_or_else(|| store_error("list the agenda", &"an item is not text"))?;
                decode(&text)
            })
            .collect()
    }

    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool> {
        let keys = vec![self.row_key(&item.id), self.index_key()];
        let mut args = Self::row_args(&item)?;
        args.push(from.as_str().to_string());
        Ok(self.eval(TRANSITION_SCRIPT, keys, args).await? == 1)
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let keys = vec![self.row_key(id), self.index_key()];
        Ok(self.eval(DELETE_SCRIPT, keys, vec![id.to_string()]).await? == 1)
    }
}
