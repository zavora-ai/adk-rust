//! The Redis [`ControlStore`] (spec 3.3 §10 K3, K4; feature `redis`).

use async_trait::async_trait;
use fred::prelude::{Client, HashesInterface as _};

use super::{AreaSettings, AwayCursor, ControlStore};
use crate::error::Result;
use crate::pairing::redis::{
    connect_client, decode, eval_text, migrate_layout, namespaced, store_error,
};
use crate::partition::PartitionId;

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: cursors hash, sequence hash. ARGV: field, sequence number, the cursor's JSON.
/// Stores the cursor unless the stored one is past it, and returns the stored cursor.
const ADVANCE_SCRIPT: &str = r"
local stored = tonumber(redis.call('HGET', KEYS[2], ARGV[1]) or '-1')
if stored > tonumber(ARGV[2]) then return redis.call('HGET', KEYS[1], ARGV[1]) end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
return ARGV[3]
";

/// A [`ControlStore`] in Redis, shared by every replica (spec 3.3 §10 K3, K4; feature
/// `redis`).
///
/// Keys sit under the namespace [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's script touches one Redis Cluster slot; braces in the
/// namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:area-settings` | Hash | Each area's changed settings as JSON, by partition id |
/// | `{<ns>}:away-cursors` | Hash | Each viewer's cursor as JSON, by user and viewer |
/// | `{<ns>}:away-cursor-seqs` | Hash | Each viewer's sequence number, compared by the advance script |
/// | `{<ns>}:control-layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// A cursor advances in one Lua script, which Redis executes atomically, so two concurrent
/// advances keep the higher.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::control::RedisControlStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisControlStore {
    client: Client,
    namespace: String,
}

impl RedisControlStore {
    /// The namespace keys sit under by default, shared with the other kernel stores.
    pub const DEFAULT_NAMESPACE: &'static str = "adk:kernel";

    /// A store over an initialized client, such as one shared with the process store.
    pub fn from_client(client: Client) -> Self {
        Self { client, namespace: Self::DEFAULT_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the URL is invalid or
    /// the server cannot be reached.
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::from_client(connect_client(url).await?))
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
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the server fails or
    /// holds a newer layout.
    pub async fn migrate(&self) -> Result<()> {
        migrate_layout(&self.client, self.key("control-layout"), LAYOUT, "control", &self.namespace)
            .await
    }

    fn key(&self, suffix: &str) -> String {
        namespaced(&self.namespace, suffix)
    }
}

/// The hash field of `viewer` of `user`: the unit separator never appears in either.
fn cursor_field(user: &str, viewer: &str) -> String {
    format!("{user}\u{1f}{viewer}")
}

#[async_trait]
impl ControlStore for RedisControlStore {
    async fn area_settings(&self, partition: &PartitionId) -> Result<Option<AreaSettings>> {
        let text: Option<String> = self
            .client
            .hget(self.key("area-settings"), partition.as_str())
            .await
            .map_err(|error| store_error("read the area settings", &error))?;
        text.map(|text| decode("area's settings", &text)).transpose()
    }

    async fn list_area_settings(&self) -> Result<Vec<AreaSettings>> {
        let all: std::collections::HashMap<String, String> = self
            .client
            .hgetall(self.key("area-settings"))
            .await
            .map_err(|error| store_error("list the area settings", &error))?;
        let mut settings = all
            .values()
            .map(|text| decode::<AreaSettings>("area's settings", text))
            .collect::<Result<Vec<_>>>()?;
        settings.sort_by(|a, b| a.partition.as_str().cmp(b.partition.as_str()));
        Ok(settings)
    }

    async fn put_area_settings(&self, settings: AreaSettings) -> Result<()> {
        let json = serde_json::to_string(&settings)?;
        self.client
            .hset::<(), _, _>(self.key("area-settings"), (settings.partition.as_str(), json))
            .await
            .map_err(|error| store_error("write the area settings", &error))
    }

    async fn cursor(&self, user: &str, viewer: &str) -> Result<Option<AwayCursor>> {
        let text: Option<String> = self
            .client
            .hget(self.key("away-cursors"), cursor_field(user, viewer))
            .await
            .map_err(|error| store_error("read the away cursors", &error))?;
        text.map(|text| decode("cursor", &text)).transpose()
    }

    async fn advance_cursor(&self, cursor: AwayCursor) -> Result<AwayCursor> {
        let keys = vec![self.key("away-cursors"), self.key("away-cursor-seqs")];
        let args = vec![
            cursor_field(&cursor.user, &cursor.viewer),
            cursor.seq.to_string(),
            serde_json::to_string(&cursor)?,
        ];
        let stored = eval_text(&self.client, ADVANCE_SCRIPT, keys, args).await?;
        match stored {
            Some(text) => decode("cursor", &text),
            None => Err(store_error("advance a cursor", &"the script answered nothing")),
        }
    }
}
