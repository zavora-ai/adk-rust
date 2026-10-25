//! The Redis [`AgentStore`] (spec 3.3 §5.7, §10 K7, feature `redis`).

use async_trait::async_trait;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;

use crate::agents::{AgentRecord, AgentStore};
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

/// KEYS: row, the ids index. ARGV: id, version, state, record JSON, the expected version
/// (`0` for none). Returns 0, writing nothing, when the stored version is another.
const PUT_SCRIPT: &str = r"
local stored = tonumber(redis.call('HGET', KEYS[1], 'version') or '0')
if stored ~= tonumber(ARGV[5]) then return 0 end
redis.call('HSET', KEYS[1], 'version', ARGV[2], 'state', ARGV[3], 'record', ARGV[4])
redis.call('ZADD', KEYS[2], 0, ARGV[1])
return 1
";

/// KEYS: row, the ids index. ARGV: id. Returns 0 unless the row is a suggestion.
const DELETE_SCRIPT: &str = r"
if redis.call('HGET', KEYS[1], 'state') ~= 'proposed' then return 0 end
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return 1
";

/// KEYS: the ids index. ARGV: row key prefix. Returns every record's JSON in id order.
const LIST_SCRIPT: &str = r"
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local record = redis.call('HGET', ARGV[1] .. id, 'record')
  if record then table.insert(out, record) end
end
return out
";

/// An [`AgentStore`] in Redis, shared by every replica (spec 3.3 §5.7, §10 K7, feature
/// `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless [`with_namespace`](Self::with_namespace)
/// sets another, inside the hash tag `{<namespace>}`, so the store's scripts touch one Redis
/// Cluster slot; braces in the namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:agents:agent:<id>` | Hash | One agent: `version`, `state` and `record` (the record's JSON) |
/// | `{<ns>}:agents:ids` | Sorted set | Every id at score 0, so it ranges in byte order |
/// | `{<ns>}:agents:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// Every change runs as one Lua script, which Redis executes atomically; `put_if_version`
/// compares the stored version and writes in the same script, so two replicas changing one
/// agent from the same version change it once.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::RedisAgentStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisAgentStore {
    client: Client,
    namespace: String,
}

impl RedisAgentStore {
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
            .eval(MIGRATE_SCRIPT, vec![self.key("agents:layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the agent layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the agent keys under {} have layout v{stored}, but this release knows up \
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
        self.key(&format!("agents:agent:{id}"))
    }

    fn index_key(&self) -> String {
        self.key("agents:ids")
    }

    async fn eval(&self, script: &str, keys: Vec<String>, args: Vec<String>) -> Result<i64> {
        self.client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("write the agent table", &error))
    }
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

fn decode(record: &str) -> Result<AgentRecord> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend: "redis",
        reason: format!("a stored agent is corrupt: {error}"),
    })
}

#[async_trait]
impl AgentStore for RedisAgentStore {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool> {
        let keys = vec![self.row_key(&record.id), self.index_key()];
        let args = vec![
            record.id.clone(),
            record.version.to_string(),
            record.state.as_str().to_string(),
            serde_json::to_string(&record)?,
            expected.to_string(),
        ];
        Ok(self.eval(PUT_SCRIPT, keys, args).await? == 1)
    }

    async fn get(&self, id: &str) -> Result<Option<AgentRecord>> {
        let record: Option<String> = self
            .client
            .hget(self.row_key(id), "record")
            .await
            .map_err(|error| store_error("read the agent table", &error))?;
        record.as_deref().map(decode).transpose()
    }

    async fn list(&self) -> Result<Vec<AgentRecord>> {
        let reply: Value = self
            .client
            .eval(LIST_SCRIPT, vec![self.index_key()], vec![self.key("agents:agent:")])
            .await
            .map_err(|error| store_error("list the agent table", &error))?;
        reply
            .into_array()
            .into_iter()
            .map(|value| {
                let text = value
                    .into_string()
                    .ok_or_else(|| store_error("list the agent table", &"a record is not text"))?;
                decode(&text)
            })
            .collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let keys = vec![self.row_key(id), self.index_key()];
        Ok(self.eval(DELETE_SCRIPT, keys, vec![id.to_string()]).await? == 1)
    }
}
