//! The Redis [`ChannelBindingStore`] (spec 3.3 §10 K1; features `channels`, `redis`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::{Client, HashesInterface as _, KeysInterface as _};

use super::{ChannelBinding, ChannelBindingStore, ChannelPairingRequest};
use crate::error::Result;
use crate::pairing::redis::{
    LIST_SCRIPT, PUT_SCRIPT, connect_client, decode, eval_count, eval_list, eval_text,
    migrate_layout, namespaced, store_error,
};

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: request, expiry index, the code's key. ARGV: member, expiry score, the request's
/// JSON, the code key prefix, the code hash. Replaces the account's request and moves its
/// code key.
const PUT_REQUEST_SCRIPT: &str = r"
local old = redis.call('HGET', KEYS[1], 'code')
if old then redis.call('DEL', ARGV[4] .. old) end
redis.call('DEL', KEYS[1])
redis.call('HSET', KEYS[1], 'json', ARGV[3], 'code', ARGV[5])
redis.call('SET', KEYS[3], ARGV[1])
redis.call('ZADD', KEYS[2], ARGV[2], ARGV[1])
return 1
";

/// KEYS: the code's key, expiry index. ARGV: request key prefix. Removes and returns the
/// request the code names, or false.
const TAKE_REQUEST_SCRIPT: &str = r"
local member = redis.call('GET', KEYS[1])
if not member then return false end
local json = redis.call('HGET', ARGV[1] .. member, 'json')
redis.call('DEL', ARGV[1] .. member)
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], member)
return json or false
";

/// KEYS: expiry index. ARGV: request key prefix, code key prefix, the highest expiry
/// removed. Returns how many requests it removed.
const PRUNE_REQUESTS_SCRIPT: &str = r"
local members = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[3])
for _, member in ipairs(members) do
  local code = redis.call('HGET', ARGV[1] .. member, 'code')
  if code then redis.call('DEL', ARGV[2] .. code) end
  redis.call('DEL', ARGV[1] .. member)
  redis.call('ZREM', KEYS[1], member)
end
return #members
";

/// KEYS: expiry index. ARGV: request key prefix. Returns every request's JSON.
const LIST_REQUESTS_SCRIPT: &str = r"
local out = {}
for _, member in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local json = redis.call('HGET', ARGV[1] .. member, 'json')
  if json then table.insert(out, json) end
end
return out
";

/// A [`ChannelBindingStore`] in Redis, shared by every replica (spec 3.3 §10 K1, features
/// `channels`, `redis`).
///
/// Keys sit under the namespace [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:channel-binding:<id>` | String | One binding as JSON |
/// | `{<ns>}:channel-bindings` | Sorted set | Every binding id, scored by its pairing time in microseconds |
/// | `{<ns>}:channel-request:<member>` | Hash | One pairing request: `json`, and `code`, its code's hash |
/// | `{<ns>}:channel-requests` | Sorted set | Every request member, scored by its expiry in microseconds |
/// | `{<ns>}:channel-request-code:<hash>` | String | The request member a code names |
/// | `{<ns>}:channels-layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// A request member is its channel and account, each percent-encoded and joined by `/`, so
/// no identifier can spill into another key. Every change runs as one Lua script, which
/// Redis executes atomically.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::channels::RedisChannelBindingStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisChannelBindingStore {
    client: Client,
    namespace: String,
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

impl RedisChannelBindingStore {
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
        migrate_layout(
            &self.client,
            self.key("channels-layout"),
            LAYOUT,
            "channel",
            &self.namespace,
        )
        .await
    }

    fn key(&self, suffix: &str) -> String {
        namespaced(&self.namespace, suffix)
    }

    fn member(channel: &str, account: &str) -> String {
        format!("{}/{}", encode(channel), encode(account))
    }
}

#[async_trait]
impl ChannelBindingStore for RedisChannelBindingStore {
    async fn put_binding(&self, binding: ChannelBinding) -> Result<()> {
        let keys = vec![
            self.key(&format!("channel-binding:{}", binding.id)),
            self.key("channel-bindings"),
        ];
        let args = vec![
            binding.id.clone(),
            binding.paired_at.timestamp_micros().to_string(),
            serde_json::to_string(&binding)?,
        ];
        eval_count(&self.client, PUT_SCRIPT, keys, args).await?;
        Ok(())
    }

    async fn get_binding(&self, id: &str) -> Result<Option<ChannelBinding>> {
        let text: Option<String> = self
            .client
            .get(self.key(&format!("channel-binding:{id}")))
            .await
            .map_err(|error| store_error("read the channel binding table", &error))?;
        text.map(|text| decode("channel binding", &text)).transpose()
    }

    async fn list_bindings(&self) -> Result<Vec<ChannelBinding>> {
        let rows = eval_list(
            &self.client,
            LIST_SCRIPT,
            vec![self.key("channel-bindings")],
            vec![self.key("channel-binding:")],
        )
        .await?;
        rows.iter().map(|text| decode("channel binding", text)).collect()
    }

    async fn put_request(&self, request: ChannelPairingRequest) -> Result<()> {
        let member = Self::member(&request.channel, &request.account);
        let keys = vec![
            self.key(&format!("channel-request:{member}")),
            self.key("channel-requests"),
            self.key(&format!("channel-request-code:{}", request.code_hash)),
        ];
        let args = vec![
            member,
            request.expires_at.timestamp_micros().to_string(),
            serde_json::to_string(&request)?,
            self.key("channel-request-code:"),
            request.code_hash.clone(),
        ];
        eval_count(&self.client, PUT_REQUEST_SCRIPT, keys, args).await?;
        Ok(())
    }

    async fn get_request(
        &self,
        channel: &str,
        account: &str,
    ) -> Result<Option<ChannelPairingRequest>> {
        let member = Self::member(channel, account);
        let text: Option<String> = self
            .client
            .hget(self.key(&format!("channel-request:{member}")), "json")
            .await
            .map_err(|error| store_error("read the channel request table", &error))?;
        text.map(|text| decode("channel pairing request", &text)).transpose()
    }

    async fn take_request(&self, code_hash: &str) -> Result<Option<ChannelPairingRequest>> {
        let keys = vec![
            self.key(&format!("channel-request-code:{code_hash}")),
            self.key("channel-requests"),
        ];
        let text =
            eval_text(&self.client, TAKE_REQUEST_SCRIPT, keys, vec![self.key("channel-request:")])
                .await?;
        text.map(|text| decode("channel pairing request", &text)).transpose()
    }

    async fn list_requests(&self) -> Result<Vec<ChannelPairingRequest>> {
        let rows = eval_list(
            &self.client,
            LIST_REQUESTS_SCRIPT,
            vec![self.key("channel-requests")],
            vec![self.key("channel-request:")],
        )
        .await?;
        let mut requests = rows
            .iter()
            .map(|text| decode::<ChannelPairingRequest>("channel pairing request", text))
            .collect::<Result<Vec<_>>>()?;
        requests.sort_by(|a, b| {
            (a.requested_at, &a.channel, &a.account).cmp(&(b.requested_at, &b.channel, &b.account))
        });
        Ok(requests)
    }

    async fn prune_requests(&self, before: DateTime<Utc>) -> Result<usize> {
        let args = vec![
            self.key("channel-request:"),
            self.key("channel-request-code:"),
            before.timestamp_micros().to_string(),
        ];
        eval_count(&self.client, PRUNE_REQUESTS_SCRIPT, vec![self.key("channel-requests")], args)
            .await
    }
}
