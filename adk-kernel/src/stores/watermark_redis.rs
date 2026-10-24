//! The Redis [`CronWatermarkStore`] (spec 3.2 §7.4, §13 Q8, feature `redis`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::{Builder, Client, ClientLike as _, Config, LuaInterface as _};
use fred::types::Value;

use crate::error::{KernelError, Result};
use crate::watermark::CronWatermarkStore;

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

/// KEYS: the cursor hash. ARGV: source, cursor in microseconds. Keeps the later cursor and
/// returns it.
const ADVANCE_SCRIPT: &str = r"
local stored = redis.call('HGET', KEYS[1], ARGV[1])
if (not stored) or tonumber(stored) < tonumber(ARGV[2]) then
  redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
  return ARGV[2]
end
return stored
";

/// A [`CronWatermarkStore`] in Redis, shared by every replica (spec 3.2 §7.4, feature
/// `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`; braces in the namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:cron-watermarks` | Hash | Each source's cursor, in microseconds since the Unix epoch |
/// | `{<ns>}:cron-watermarks:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// An advance is one Lua script, which Redis runs atomically, so the later cursor wins.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::RedisCronWatermarkStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisCronWatermarkStore {
    client: Client,
    namespace: String,
}

impl RedisCronWatermarkStore {
    /// The namespace keys sit under by default, shared with the other kernel stores.
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
            .eval(
                MIGRATE_SCRIPT,
                vec![self.key("cron-watermarks:layout")],
                vec![LAYOUT.to_string()],
            )
            .await
            .map_err(|error| store_error("record the layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the cron watermark keys under {} have layout v{stored}, but this release \
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
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

/// The instant a stored cursor names.
fn instant(value: Value) -> Result<DateTime<Utc>> {
    let text = value
        .into_string()
        .ok_or_else(|| store_error("read a cron watermark", &"the cursor is not text"))?;
    text.parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| store_error("read a cron watermark", &format!("`{text}` is no cursor")))
}

#[async_trait]
impl CronWatermarkStore for RedisCronWatermarkStore {
    async fn read(&self, source: &str) -> Result<Option<DateTime<Utc>>> {
        use fred::prelude::HashesInterface as _;

        let value: Value = self
            .client
            .hget(self.key("cron-watermarks"), source)
            .await
            .map_err(|error| store_error("read a cron watermark", &error))?;
        if value.is_null() { Ok(None) } else { instant(value).map(Some) }
    }

    async fn advance(&self, source: &str, cursor: DateTime<Utc>) -> Result<DateTime<Utc>> {
        let value: Value = self
            .client
            .eval(
                ADVANCE_SCRIPT,
                vec![self.key("cron-watermarks")],
                vec![source.to_string(), cursor.timestamp_micros().to_string()],
            )
            .await
            .map_err(|error| store_error("advance a cron watermark", &error))?;
        instant(value)
    }

    async fn delete(&self, source: &str) -> Result<bool> {
        use fred::prelude::HashesInterface as _;

        let removed: i64 = self
            .client
            .hdel(self.key("cron-watermarks"), source)
            .await
            .map_err(|error| store_error("delete a cron watermark", &error))?;
        Ok(removed > 0)
    }
}
