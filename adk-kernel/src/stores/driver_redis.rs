//! The Redis [`DriverStore`] (spec 3.2 §7.7, §13 Q8; features `drivers`, `redis`).

use adk_core::ApprovalScope;
use async_trait::async_trait;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, KeysInterface as _,
    LuaInterface as _,
};
use fred::types::Value;

use crate::drivers::store::park_key;
use crate::drivers::{DriverPark, DriverRecord, DriverStore};
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

/// KEYS: record, all-ids index. ARGV: id, the record's JSON.
const PUT_SCRIPT: &str = r"
redis.call('SET', KEYS[1], ARGV[2])
redis.call('ZADD', KEYS[2], 0, ARGV[1])
return 1
";

/// KEYS: record, all-ids index. ARGV: id. Returns 0 when there was no record.
const DELETE_SCRIPT: &str = r"
local existed = redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return existed
";

/// KEYS: park, all-parks index, the driver's park index. ARGV: member, the park's JSON.
/// Replaces the park and moves its index entry.
const PUT_PARK_SCRIPT: &str = r"
local old = redis.call('HGET', KEYS[1], 'idx')
if old then redis.call('SREM', old, ARGV[1]) end
redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[1], 'json', ARGV[2], 'idx', KEYS[3])
redis.call('SADD', KEYS[3], ARGV[1])
redis.call('ZADD', KEYS[2], 0, ARGV[1])
return 1
";

/// KEYS: park, all-parks index. ARGV: member. Returns 0 when there was no park.
const DELETE_PARK_SCRIPT: &str = r"
local old = redis.call('HGET', KEYS[1], 'idx')
if not old then return 0 end
redis.call('SREM', old, ARGV[1])
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return 1
";

/// KEYS: the index to walk (a sorted set when ARGV[2] is `z`, else a set). ARGV: park key
/// prefix, index kind. Returns every listed park's JSON.
const LIST_PARKS_SCRIPT: &str = r"
local members
if ARGV[2] == 'z' then members = redis.call('ZRANGE', KEYS[1], 0, -1)
else members = redis.call('SMEMBERS', KEYS[1]) end
local out = {}
for _, member in ipairs(members) do
  local json = redis.call('HGET', ARGV[1] .. member, 'json')
  if json then table.insert(out, json) end
end
return out
";

/// KEYS: the all-ids index. ARGV: record key prefix. Returns every record's JSON in id order.
const LIST_SCRIPT: &str = r"
local out = {}
for _, id in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local json = redis.call('GET', ARGV[1] .. id)
  if json then table.insert(out, json) end
end
return out
";

/// A [`DriverStore`] in Redis, shared by every replica (spec 3.2 §7.7, features `drivers`,
/// `redis`).
///
/// Keys sit under the namespace [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot; braces in the
/// namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:driver:<id>` | String | One driver's record as JSON |
/// | `{<ns>}:drivers` | Sorted set | Every driver id, scored 0, so it ranges in byte order |
/// | `{<ns>}:driver-park:<key>` | Hash | One parked call: `json`, and `idx`, the driver index it is in |
/// | `{<ns>}:driver-parks` | Sorted set | Every park key, scored 0 |
/// | `{<ns>}:driver-parks:driver:<id>` | Set | The park keys of one driver |
/// | `{<ns>}:drivers-layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// A park key is its scope's app, user and session and its call id, each percent-encoded
/// and joined by `/`, so no identifier can spill into another key. Every change runs as
/// one Lua script, which Redis executes atomically.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::RedisDriverStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisDriverStore {
    client: Client,
    namespace: String,
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

/// `text` with every byte outside `[A-Za-z0-9-._~]` percent-encoded.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl RedisDriverStore {
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
            .eval(MIGRATE_SCRIPT, vec![self.key("drivers-layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the driver keys under {} have layout v{stored}, but this release knows up \
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

    fn park_member(scope: &ApprovalScope, call_id: &str) -> String {
        let (app, user, session, call) = park_key(scope, call_id);
        [app, user, session, call].iter().map(|part| encode(part)).collect::<Vec<_>>().join("/")
    }

    async fn eval(&self, script: &str, keys: Vec<String>, args: Vec<String>) -> Result<i64> {
        self.client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("write the driver table", &error))
    }

    async fn eval_list(
        &self,
        script: &str,
        keys: Vec<String>,
        args: Vec<String>,
    ) -> Result<Vec<String>> {
        let reply: Value = self
            .client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("list the driver table", &error))?;
        reply
            .into_array()
            .into_iter()
            .map(|value| {
                value
                    .into_string()
                    .ok_or_else(|| store_error("list the driver table", &"a value is not text"))
            })
            .collect()
    }
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend: "redis",
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

#[async_trait]
impl DriverStore for RedisDriverStore {
    async fn put(&self, record: DriverRecord) -> Result<()> {
        let id = record.id().to_string();
        let keys = vec![self.key(&format!("driver:{id}")), self.key("drivers")];
        self.eval(PUT_SCRIPT, keys, vec![id, serde_json::to_string(&record)?]).await?;
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<Option<DriverRecord>> {
        let text: Option<String> = self
            .client
            .get(self.key(&format!("driver:{id}")))
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        text.map(|text| decode("driver", &text)).transpose()
    }

    async fn list(&self) -> Result<Vec<DriverRecord>> {
        let rows = self
            .eval_list(LIST_SCRIPT, vec![self.key("drivers")], vec![self.key("driver:")])
            .await?;
        rows.iter().map(|text| decode("driver", text)).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let keys = vec![self.key(&format!("driver:{id}")), self.key("drivers")];
        Ok(self.eval(DELETE_SCRIPT, keys, vec![id.to_string()]).await? > 0)
    }

    async fn put_park(&self, park: DriverPark) -> Result<()> {
        let member = Self::park_member(&park.scope, &park.call_id);
        let keys = vec![
            self.key(&format!("driver-park:{member}")),
            self.key("driver-parks"),
            self.key(&format!("driver-parks:driver:{}", park.driver)),
        ];
        self.eval(PUT_PARK_SCRIPT, keys, vec![member, serde_json::to_string(&park)?]).await?;
        Ok(())
    }

    async fn get_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<Option<DriverPark>> {
        let member = Self::park_member(scope, call_id);
        let text: Option<String> = self
            .client
            .hget(self.key(&format!("driver-park:{member}")), "json")
            .await
            .map_err(|error| store_error("read the driver table", &error))?;
        text.map(|text| decode("parked driver call", &text)).transpose()
    }

    async fn list_parks(&self, driver: Option<&str>) -> Result<Vec<DriverPark>> {
        let (index, kind) = match driver {
            Some(driver) => (self.key(&format!("driver-parks:driver:{driver}")), "s"),
            None => (self.key("driver-parks"), "z"),
        };
        let rows = self
            .eval_list(LIST_PARKS_SCRIPT, vec![index], vec![self.key("driver-park:"), kind.into()])
            .await?;
        let mut parks = rows
            .iter()
            .map(|text| decode::<DriverPark>("parked driver call", text))
            .collect::<Result<Vec<_>>>()?;
        parks.sort_by_key(|park| park_key(&park.scope, &park.call_id));
        Ok(parks)
    }

    async fn delete_park(&self, scope: &ApprovalScope, call_id: &str) -> Result<bool> {
        let member = Self::park_member(scope, call_id);
        let keys = vec![self.key(&format!("driver-park:{member}")), self.key("driver-parks")];
        Ok(self.eval(DELETE_PARK_SCRIPT, keys, vec![member]).await? > 0)
    }
}
