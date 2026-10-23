//! The Redis [`LedgerStore`] (spec 3.2 §7.10, §13 Q8, features `proc` and `redis`).

use std::collections::HashMap;

use async_trait::async_trait;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;

use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::proc::{
    LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery, PassState,
    order_observations,
};

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// How many rows one read fetches.
const CHUNK: usize = 512;

/// KEYS: layout. ARGV: this release's layout version. Records it unless the stored one is
/// newer, and returns the stored one.
const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// KEYS: counter, rows hash, sequence index, time index. ARGV: the row's JSON, its UTC
/// milliseconds. Takes the next sequence number and stores the row under it.
const APPEND_SCRIPT: &str = r"
local seq = redis.call('INCR', KEYS[1])
redis.call('HSET', KEYS[2], seq, ARGV[1])
redis.call('ZADD', KEYS[3], seq, seq)
redis.call('ZADD', KEYS[4], ARGV[2], seq)
return seq
";

/// KEYS: sequence index, rows hash. ARGV: the cursor, the count. Returns up to `count`
/// sequence numbers after the cursor, each followed by its row.
const PAGE_SCRIPT: &str = r"
local seqs = redis.call('ZRANGEBYSCORE', KEYS[1], '(' .. ARGV[1], '+inf', 'LIMIT', 0, tonumber(ARGV[2]))
local out = {}
for _, seq in ipairs(seqs) do
  table.insert(out, seq)
  table.insert(out, redis.call('HGET', KEYS[2], seq) or '')
end
return out
";

/// KEYS: time index. ARGV: the lowest and the highest score, as `ZRANGEBYSCORE` takes them.
/// Returns the sequence numbers of the rows in the window.
const WINDOW_SCRIPT: &str = r"
return redis.call('ZRANGEBYSCORE', KEYS[1], ARGV[1], ARGV[2])
";

/// KEYS: rows hash. ARGV: sequence numbers. Returns each one's row, in order.
const ROWS_SCRIPT: &str = r"
local out = {}
for _, seq in ipairs(ARGV) do
  table.insert(out, redis.call('HGET', KEYS[1], seq) or '')
end
return out
";

/// A [`LedgerStore`] in Redis, shared by every replica (spec 3.2 §7.1, §7.10, features
/// `proc` and `redis`).
///
/// It opens from the URL of `adk-rust serve --store`. Keys sit under the namespace
/// [`DEFAULT_NAMESPACE`](Self::DEFAULT_NAMESPACE) unless
/// [`with_namespace`](Self::with_namespace) sets another, inside the hash tag
/// `{<namespace>}`, so the store's scripts touch one Redis Cluster slot.
///
/// | Key | Type | Contents |
/// |-----|------|----------|
/// | `{<ns>}:ledger:seq` | String | The last sequence number |
/// | `{<ns>}:ledger:rows` | Hash | Sequence number → the row's JSON |
/// | `{<ns>}:ledger:by_seq` | Sorted set | Every sequence number, scored by itself |
/// | `{<ns>}:ledger:by_ts` | Sorted set | Every sequence number, scored by the row's UTC milliseconds |
/// | `{<ns>}:observations` | Hash | Observation id → its JSON |
/// | `{<ns>}:proc_state` | Hash | Partition → its pass state's JSON |
/// | `{<ns>}:ledger:layout` | String | The layout version [`migrate`](Self::migrate) recorded |
///
/// An append is one Lua script: it takes the next sequence number and stores the row under
/// it in both indexes atomically. A read walks the sequence index, or the time index when
/// the query names a window, and filters the rows.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::proc::RedisLedgerStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct RedisLedgerStore {
    client: Client,
    namespace: String,
}

impl RedisLedgerStore {
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
            .eval(MIGRATE_SCRIPT, vec![self.key("ledger:layout")], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| store_error("record the ledger layout version", &error))?;
        if stored > LAYOUT {
            return Err(KernelError::Store {
                backend: "redis",
                reason: format!(
                    "the ledger keys under {} have layout v{stored}, but this release knows up \
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

    async fn script(&self, script: &str, keys: Vec<String>, args: Vec<String>) -> Result<Value> {
        self.client
            .eval(script, keys, args)
            .await
            .map_err(|error| store_error("read the ledger", &error))
    }

    /// The rows of `seqs`, in order, decoded.
    async fn rows(&self, seqs: &[u64]) -> Result<Vec<LedgerEntry>> {
        let mut entries = Vec::with_capacity(seqs.len());
        for chunk in seqs.chunks(CHUNK) {
            let args = chunk.iter().map(u64::to_string).collect();
            let reply = self.script(ROWS_SCRIPT, vec![self.key("ledger:rows")], args).await?;
            for (seq, row) in chunk.iter().zip(reply.into_array()) {
                entries.push(decode(*seq, row)?);
            }
        }
        Ok(entries)
    }
}

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

fn decode(seq: u64, row: Value) -> Result<LedgerEntry> {
    let text = row.into_string().unwrap_or_default();
    let mut entry: LedgerEntry = serde_json::from_str(&text)
        .map_err(|_| store_error("read the ledger", &format!("row {seq} is corrupt")))?;
    entry.seq = seq;
    Ok(entry)
}

fn json<T: serde::de::DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|error| store_error(&format!("read {what}"), &error))
}

#[async_trait]
impl LedgerStore for RedisLedgerStore {
    async fn append(&self, mut entry: LedgerEntry) -> Result<LedgerEntry> {
        entry.seq = 0;
        let keys = vec![
            self.key("ledger:seq"),
            self.key("ledger:rows"),
            self.key("ledger:by_seq"),
            self.key("ledger:by_ts"),
        ];
        let args = vec![serde_json::to_string(&entry)?, entry.ts.timestamp_millis().to_string()];
        let seq: i64 = self
            .client
            .eval(APPEND_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("append to the ledger", &error))?;
        entry.seq = u64::try_from(seq).unwrap_or_default();
        Ok(entry)
    }

    async fn list(&self, query: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        let limit = query.limit.unwrap_or(usize::MAX);
        let mut found = Vec::new();
        if limit == 0 {
            return Ok(found);
        }
        if query.from.is_some() || query.to.is_some() {
            let min =
                query.from.map_or("-inf".to_string(), |from| from.timestamp_millis().to_string());
            let max =
                query.to.map_or("+inf".to_string(), |to| format!("({}", to.timestamp_millis()));
            let reply =
                self.script(WINDOW_SCRIPT, vec![self.key("ledger:by_ts")], vec![min, max]).await?;
            let mut seqs: Vec<u64> = reply
                .into_array()
                .into_iter()
                .filter_map(|seq| seq.into_string().and_then(|seq| seq.parse().ok()))
                .filter(|seq| query.after.is_none_or(|after| *seq > after))
                .collect();
            seqs.sort_unstable();
            for chunk in seqs.chunks(CHUNK) {
                for entry in self.rows(chunk).await? {
                    if query.matches(&entry) {
                        found.push(entry);
                        if found.len() == limit {
                            return Ok(found);
                        }
                    }
                }
            }
            return Ok(found);
        }
        let mut cursor = query.after.unwrap_or(0);
        loop {
            let keys = vec![self.key("ledger:by_seq"), self.key("ledger:rows")];
            let reply =
                self.script(PAGE_SCRIPT, keys, vec![cursor.to_string(), CHUNK.to_string()]).await?;
            let items = reply.into_array();
            if items.is_empty() {
                return Ok(found);
            }
            let mut items = items.into_iter();
            while let (Some(seq), Some(row)) = (items.next(), items.next()) {
                let seq: u64 =
                    seq.into_string().and_then(|seq| seq.parse().ok()).ok_or_else(|| {
                        store_error("read the ledger", &"a sequence number is corrupt")
                    })?;
                cursor = seq;
                let entry = decode(seq, row)?;
                if query.matches(&entry) {
                    found.push(entry);
                    if found.len() == limit {
                        return Ok(found);
                    }
                }
            }
        }
    }

    async fn put_observation(&self, observation: Observation) -> Result<()> {
        let body = serde_json::to_string(&observation)?;
        self.client
            .hset::<(), _, _>(self.key("observations"), (observation.id.as_str(), body))
            .await
            .map_err(|error| store_error("write an observation", &error))
    }

    async fn observations(&self, query: &ObservationQuery) -> Result<Vec<Observation>> {
        let rows: HashMap<String, String> = self
            .client
            .hgetall(self.key("observations"))
            .await
            .map_err(|error| store_error("read the observations", &error))?;
        let mut observations = rows
            .values()
            .map(|body| json::<Observation>("an observation", body))
            .collect::<Result<Vec<_>>>()?;
        observations.retain(|observation| query.matches(observation));
        order_observations(&mut observations);
        observations.truncate(query.limit.unwrap_or(usize::MAX));
        Ok(observations)
    }

    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>> {
        let body: Option<String> = self
            .client
            .hget(self.key("proc_state"), partition.as_str())
            .await
            .map_err(|error| store_error("read the pass state", &error))?;
        body.map(|body| json("the pass state", &body)).transpose()
    }

    async fn pass_states(&self) -> Result<Vec<PassState>> {
        let rows: HashMap<String, String> = self
            .client
            .hgetall(self.key("proc_state"))
            .await
            .map_err(|error| store_error("read the pass states", &error))?;
        let mut states = rows
            .values()
            .map(|body| json::<PassState>("a pass state", body))
            .collect::<Result<Vec<_>>>()?;
        states.sort_by(|a, b| a.partition.cmp(&b.partition));
        Ok(states)
    }

    async fn put_pass_state(&self, state: PassState) -> Result<()> {
        let body = serde_json::to_string(&state)?;
        self.client
            .hset::<(), _, _>(self.key("proc_state"), (state.partition.as_str(), body))
            .await
            .map_err(|error| store_error("write the pass state", &error))
    }
}
