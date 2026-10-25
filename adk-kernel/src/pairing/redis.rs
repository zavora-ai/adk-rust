//! The Redis [`DeviceStore`] (spec 3.3 §10 K2; feature `redis`).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, KeysInterface as _, LuaInterface as _,
};
use fred::types::Value;

use super::{AttachmentHolder, DeviceAttachment, DeviceRecord, DeviceStore, PairingCode};
use crate::error::{KernelError, Result};

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: layout. ARGV: this release's layout version. Records it unless the stored one is
/// newer, and returns the stored one.
pub(crate) const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// KEYS: record, index. ARGV: member, score, the record's JSON.
pub(crate) const PUT_SCRIPT: &str = r"
redis.call('SET', KEYS[1], ARGV[3])
redis.call('ZADD', KEYS[2], ARGV[2], ARGV[1])
return 1
";

/// KEYS: record, index. ARGV: member. Removes and returns the record, or false.
pub(crate) const TAKE_SCRIPT: &str = r"
local json = redis.call('GET', KEYS[1])
if not json then return false end
redis.call('DEL', KEYS[1])
redis.call('ZREM', KEYS[2], ARGV[1])
return json
";

/// KEYS: index. ARGV: record key prefix, the highest score removed (`+inf` for every
/// member). Returns how many records it removed.
pub(crate) const PRUNE_SCRIPT: &str = r"
local members = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[2])
for _, member in ipairs(members) do
  redis.call('DEL', ARGV[1] .. member)
  redis.call('ZREM', KEYS[1], member)
end
return #members
";

/// Writes an attachment into its holder's hash of its kind, and notes the kind: `ARGV[4]` is
/// `1` to write only when absent. Returns whether it wrote.
const PUT_ATTACHMENT_SCRIPT: &str = r"
local wrote = 1
if ARGV[4] == '1' then
  wrote = redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2])
else
  redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
end
if wrote == 1 then redis.call('SADD', KEYS[2], ARGV[3]) end
return wrote
";

const LIST_ATTACHMENTS_SCRIPT: &str = r"
return redis.call('HVALS', KEYS[1])
";

const REMOVE_ATTACHMENT_SCRIPT: &str = r"
return redis.call('HDEL', KEYS[1], ARGV[1])
";

/// Removes every hash of a holder's kinds, and the kinds; returns how many attachments went.
const REMOVE_ATTACHMENTS_SCRIPT: &str = r"
local removed = 0
for _, kind in ipairs(redis.call('SMEMBERS', KEYS[1])) do
  local hash = ARGV[1] .. kind
  removed = removed + redis.call('HLEN', hash)
  redis.call('DEL', hash)
end
redis.call('DEL', KEYS[1])
return removed
";

/// KEYS: index. ARGV: record key prefix. Returns every record's JSON in index order.
pub(crate) const LIST_SCRIPT: &str = r"
local out = {}
for _, member in ipairs(redis.call('ZRANGE', KEYS[1], 0, -1)) do
  local json = redis.call('GET', ARGV[1] .. member)
  if json then table.insert(out, json) end
end
return out
";

/// A [`DeviceStore`] in Redis, shared by every replica (spec 3.3 §10 K2, feature `redis`).
///
/// Keys sit under the namespace [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot; braces in the
/// namespace are percent-encoded.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:device:<id>` | String | One device's record as JSON |
/// | `{<ns>}:devices` | Sorted set | Every device id, scored by its pairing time in microseconds |
/// | `{<ns>}:pairing-code:<hash>` | String | One waiting code's record as JSON |
/// | `{<ns>}:pairing-codes` | Sorted set | Every waiting code's hash, scored by its expiry in microseconds |
/// | `{<ns>}:pairing-attempts` | String | The count of claims since the last code was issued or claimed |
/// | `{<ns>}:devices-layout` | String | The layout version [`migrate`](Self::migrate) recorded |
/// | `{<ns>}:attachments:<holder hex>:<kind>` | Hash | One holder's attachments of one kind, each key's record as JSON |
/// | `{<ns>}:attachment-kinds:<holder hex>` | Set | The kinds a holder has attachments of, so a revocation removes every one |
///
/// Device ids are hex and code hashes are SHA-256 hex, so no identifier can spill into
/// another key. Every change runs as one Lua script, which Redis executes atomically, so
/// two claims of one code take it once.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::pairing::RedisDeviceStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisDeviceStore {
    client: Client,
    namespace: String,
}

pub(crate) fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

pub(crate) fn decode<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| KernelError::Store {
        backend: "redis",
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// `{<namespace>}:<suffix>`, braces in the namespace percent-encoded.
pub(crate) fn namespaced(namespace: &str, suffix: &str) -> String {
    let namespace = namespace.replace('{', "%7B").replace('}', "%7D");
    format!("{{{namespace}}}:{suffix}")
}

/// Connects a client to `url`.
pub(crate) async fn connect_client(url: &str) -> Result<Client> {
    let config =
        Config::from_url(url).map_err(|error| store_error("parse the Redis URL", &error))?;
    let client = Builder::from_config(config)
        .build()
        .map_err(|error| store_error("build the Redis client", &error))?;
    client.init().await.map_err(|error| store_error("connect to Redis", &error))?;
    Ok(client)
}

/// Records layout `layout` under `key`, refusing one a newer release wrote.
pub(crate) async fn migrate_layout(
    client: &Client,
    key: String,
    layout: i64,
    what: &str,
    namespace: &str,
) -> Result<()> {
    let stored: i64 = client
        .eval(MIGRATE_SCRIPT, vec![key], vec![layout.to_string()])
        .await
        .map_err(|error| store_error("record the layout version", &error))?;
    if stored > layout {
        return Err(KernelError::Store {
            backend: "redis",
            reason: format!(
                "the {what} keys under {namespace} have layout v{stored}, but this release knows \
                 up to v{layout}; upgrade ADK-Rust"
            ),
        });
    }
    Ok(())
}

/// Runs `script` and reads its answer as a list of text.
pub(crate) async fn eval_list(
    client: &Client,
    script: &str,
    keys: Vec<String>,
    args: Vec<String>,
) -> Result<Vec<String>> {
    let reply: Value =
        client.eval(script, keys, args).await.map_err(|error| store_error("list", &error))?;
    reply
        .into_array()
        .into_iter()
        .map(|value| value.into_string().ok_or_else(|| store_error("list", &"a value is not text")))
        .collect()
}

/// Runs `script` and reads its answer as optional text.
pub(crate) async fn eval_text(
    client: &Client,
    script: &str,
    keys: Vec<String>,
    args: Vec<String>,
) -> Result<Option<String>> {
    let reply: Value =
        client.eval(script, keys, args).await.map_err(|error| store_error("read", &error))?;
    Ok(match reply {
        Value::Null | Value::Boolean(false) => None,
        other => other.into_string(),
    })
}

/// Runs `script` and reads its answer as a count.
pub(crate) async fn eval_count(
    client: &Client,
    script: &str,
    keys: Vec<String>,
    args: Vec<String>,
) -> Result<usize> {
    let count: i64 =
        client.eval(script, keys, args).await.map_err(|error| store_error("write", &error))?;
    Ok(usize::try_from(count).unwrap_or_default())
}

impl RedisDeviceStore {
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
    /// Returns [`KernelError::Store`] when the server fails or holds a newer layout.
    pub async fn migrate(&self) -> Result<()> {
        migrate_layout(&self.client, self.key("devices-layout"), LAYOUT, "device", &self.namespace)
            .await
    }

    fn key(&self, suffix: &str) -> String {
        namespaced(&self.namespace, suffix)
    }

    /// The prefix of `holder`'s attachment hashes, one per kind; the holder is hex-encoded, so
    /// no holder's prefix is another's.
    fn attachments_prefix(&self, holder: &AttachmentHolder) -> String {
        self.key(&format!("attachments:{}:", hex_of(&holder.key())))
    }

    fn attachment_kinds(&self, holder: &AttachmentHolder) -> String {
        self.key(&format!("attachment-kinds:{}", hex_of(&holder.key())))
    }

    async fn write_attachment(
        &self,
        attachment: DeviceAttachment,
        only_if_absent: bool,
    ) -> Result<bool> {
        let keys = vec![
            format!("{}{}", self.attachments_prefix(&attachment.holder), attachment.kind),
            self.attachment_kinds(&attachment.holder),
        ];
        let args = vec![
            attachment.key.clone(),
            serde_json::to_string(&attachment)?,
            attachment.kind.clone(),
            if only_if_absent { "1" } else { "0" }.to_string(),
        ];
        Ok(eval_count(&self.client, PUT_ATTACHMENT_SCRIPT, keys, args).await? == 1)
    }
}

fn hex_of(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[async_trait]
impl DeviceStore for RedisDeviceStore {
    async fn put_code(&self, code: PairingCode) -> Result<()> {
        let keys =
            vec![self.key(&format!("pairing-code:{}", code.code_hash)), self.key("pairing-codes")];
        let args = vec![
            code.code_hash.clone(),
            code.expires_at.timestamp_micros().to_string(),
            serde_json::to_string(&code)?,
        ];
        eval_count(&self.client, PUT_SCRIPT, keys, args).await?;
        Ok(())
    }

    async fn take_code(&self, code_hash: &str) -> Result<Option<PairingCode>> {
        let keys = vec![self.key(&format!("pairing-code:{code_hash}")), self.key("pairing-codes")];
        let text = eval_text(&self.client, TAKE_SCRIPT, keys, vec![code_hash.to_string()]).await?;
        text.map(|text| decode("pairing code", &text)).transpose()
    }

    async fn prune_codes(&self, before: DateTime<Utc>) -> Result<usize> {
        let args = vec![self.key("pairing-code:"), before.timestamp_micros().to_string()];
        eval_count(&self.client, PRUNE_SCRIPT, vec![self.key("pairing-codes")], args).await
    }

    async fn clear_codes(&self) -> Result<usize> {
        let args = vec![self.key("pairing-code:"), "+inf".to_string()];
        eval_count(&self.client, PRUNE_SCRIPT, vec![self.key("pairing-codes")], args).await
    }

    async fn put_device(&self, device: DeviceRecord) -> Result<()> {
        let keys = vec![self.key(&format!("device:{}", device.id)), self.key("devices")];
        let args = vec![
            device.id.clone(),
            device.created_at.timestamp_micros().to_string(),
            serde_json::to_string(&device)?,
        ];
        eval_count(&self.client, PUT_SCRIPT, keys, args).await?;
        Ok(())
    }

    async fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        let text: Option<String> = self
            .client
            .get(self.key(&format!("device:{id}")))
            .await
            .map_err(|error| store_error("read the device table", &error))?;
        text.map(|text| decode("device", &text)).transpose()
    }

    async fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
        let rows = eval_list(
            &self.client,
            LIST_SCRIPT,
            vec![self.key("devices")],
            vec![self.key("device:")],
        )
        .await?;
        rows.iter().map(|text| decode("device", text)).collect()
    }

    async fn count_claim_attempt(&self) -> Result<u64> {
        let count: i64 = self
            .client
            .incr(self.key("pairing-attempts"))
            .await
            .map_err(|error| store_error("count a pairing claim", &error))?;
        Ok(u64::try_from(count).unwrap_or_default())
    }

    async fn reset_claim_attempts(&self) -> Result<()> {
        let _: () = self
            .client
            .set(self.key("pairing-attempts"), 0, None, None, false)
            .await
            .map_err(|error| store_error("reset the count of pairing claims", &error))?;
        Ok(())
    }

    async fn put_attachment(&self, attachment: DeviceAttachment) -> Result<()> {
        self.write_attachment(attachment, false).await?;
        Ok(())
    }

    async fn put_attachment_if_absent(&self, attachment: DeviceAttachment) -> Result<bool> {
        self.write_attachment(attachment, true).await
    }

    async fn attachments(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
    ) -> Result<Vec<DeviceAttachment>> {
        let hash = format!("{}{kind}", self.attachments_prefix(holder));
        let rows = eval_list(&self.client, LIST_ATTACHMENTS_SCRIPT, vec![hash], Vec::new()).await?;
        let mut attachments: Vec<DeviceAttachment> =
            rows.iter().map(|text| decode("attachment", text)).collect::<Result<_>>()?;
        attachments.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(attachments)
    }

    async fn remove_attachment(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
        key: &str,
    ) -> Result<bool> {
        let hash = format!("{}{kind}", self.attachments_prefix(holder));
        let removed =
            eval_count(&self.client, REMOVE_ATTACHMENT_SCRIPT, vec![hash], vec![key.to_string()])
                .await?;
        Ok(removed > 0)
    }

    async fn remove_attachments(&self, holder: &AttachmentHolder) -> Result<usize> {
        eval_count(
            &self.client,
            REMOVE_ATTACHMENTS_SCRIPT,
            vec![self.attachment_kinds(holder)],
            vec![self.attachments_prefix(holder)],
        )
        .await
    }
}
