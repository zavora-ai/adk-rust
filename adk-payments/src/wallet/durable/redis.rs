//! The Redis wallet store (spec 3.5 §7.14, feature `redis`).
//!
//! Keys sit under the namespace `adk:wallet` unless `with_namespace` sets another. A wallet's
//! keys share the hash tag `{<wallet>}`, so every script touches one Redis Cluster slot; the
//! wallet id is percent-encoded (`%`, `:`, `{`, `}`) and braces in the namespace are too.
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `<ns>:{<wallet>}:payments` | Hash | Each payment's [`PaymentRecord`] as JSON, by id |
//! | `<ns>:{<wallet>}:keys` | Hash | Each idempotency key's payment id |
//! | `<ns>:{<wallet>}:seq` | String | The last insertion number |
//! | `<ns>:{<wallet>}:order` | Sorted set | Every payment id, scored by insertion number |
//! | `<ns>:{<wallet>}:at` | Sorted set | Every payment id, scored by its instant (settlement, else creation) in microseconds |
//! | `<ns>:{<wallet>}:state:<state>` | Sorted set | The ids of the payments in one state, scored by insertion number |
//! | `<ns>:{<wallet>}:pauses` | Hash | Each paused category's [`CategoryPause`] as JSON |
//! | `<ns>:layout` | String | The layout version `migrate` recorded |
//!
//! Insert, update, resume and every listing are one Lua script each, which Redis runs
//! atomically: a key is taken by one payment, an update moves the payment's index entries
//! with its record, and a pause is lifted once.

use adk_core::Result;
use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;

use super::super::policy::{SpendCategory, category_name};
use super::super::store::{CategoryPause, PaymentQuery, PaymentRecord, PaymentState, WalletStore};
use super::{
    decode, encode, in_category_order, instant_micros, micros, payment_not_found, state_name,
    unavailable,
};

const BACKEND: &str = "redis";

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// Every state, in the order the scripts take their index keys.
const STATES: [PaymentState; 5] = [
    PaymentState::Held,
    PaymentState::Executing,
    PaymentState::Settled,
    PaymentState::Rejected,
    PaymentState::Failed,
];

/// KEYS: layout. ARGV: this release's layout version.
const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// KEYS: payments, keys, seq, order, at, then the five state indexes. ARGV: id, key, record,
/// instant, the state's position among the indexes. Returns `{1}` when inserted, else
/// `{0, the stored record}`.
const INSERT_SCRIPT: &str = r"
local taken = redis.call('HGET', KEYS[2], ARGV[2])
if taken then return {0, redis.call('HGET', KEYS[1], taken)} end
redis.call('HSET', KEYS[2], ARGV[2], ARGV[1])
redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
local seq = redis.call('INCR', KEYS[3])
redis.call('ZADD', KEYS[4], seq, ARGV[1])
redis.call('ZADD', KEYS[5], ARGV[4], ARGV[1])
redis.call('ZADD', KEYS[5 + tonumber(ARGV[5])], seq, ARGV[1])
return {1}
";

/// KEYS: payments, order, at, then the five state indexes. ARGV: id, record, instant, the
/// state's position among the indexes. Returns 0 when the payment is not stored.
const UPDATE_SCRIPT: &str = r"
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then return 0 end
local seq = redis.call('ZSCORE', KEYS[2], ARGV[1]) or '0'
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('ZADD', KEYS[3], ARGV[3], ARGV[1])
for i = 4, 8 do redis.call('ZREM', KEYS[i], ARGV[1]) end
redis.call('ZADD', KEYS[3 + tonumber(ARGV[4])], seq, ARGV[1])
return 1
";

/// KEYS: payments, keys. ARGV: key. Returns the payment holding the key, or nil.
const BY_KEY_SCRIPT: &str = r"
local id = redis.call('HGET', KEYS[2], ARGV[1])
if not id then return false end
return redis.call('HGET', KEYS[1], id)
";

/// KEYS: payments, order, the index to read. ARGV: `all`, or `range` with the least and
/// greatest score. Returns `{insertion number, record, …}` of the candidates.
const LIST_SCRIPT: &str = r"
local ids
if ARGV[1] == 'range' then
  ids = redis.call('ZRANGEBYSCORE', KEYS[3], ARGV[2], ARGV[3])
else
  ids = redis.call('ZRANGE', KEYS[3], 0, -1)
end
local out = {}
for _, id in ipairs(ids) do
  local record = redis.call('HGET', KEYS[1], id)
  if record then
    table.insert(out, redis.call('ZSCORE', KEYS[2], id) or '0')
    table.insert(out, record)
  end
end
return out
";

/// KEYS: pauses. ARGV: category. Lifts the pause, returning it, or nil.
const RESUME_SCRIPT: &str = r"
local pause = redis.call('HGET', KEYS[1], ARGV[1])
if not pause then return false end
redis.call('HDEL', KEYS[1], ARGV[1])
return pause
";

/// A [`WalletStore`] in Redis, shared by every instance of a site (spec 3.5 §7.14, feature
/// `redis`). See the module documentation for its keys.
///
/// # Example
///
/// ```rust,no_run
/// use adk_payments::wallet::RedisWalletStore;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = RedisWalletStore::connect("redis://localhost:6379").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg_attr(docsrs, doc(cfg(feature = "redis")))]
#[derive(Debug, Clone)]
pub struct RedisWalletStore {
    client: Client,
    namespace: String,
}

impl RedisWalletStore {
    /// The namespace keys sit under by default.
    pub const DEFAULT_NAMESPACE: &'static str = "adk:wallet";

    /// A store over an initialized client, such as one shared with the spend ledger.
    pub fn from_client(client: Client) -> Self {
        Self { client, namespace: Self::DEFAULT_NAMESPACE.to_string() }
    }

    /// Connects to `url`.
    ///
    /// # Errors
    ///
    /// Returns `wallet.store_unavailable` when the URL is invalid or the server cannot be
    /// reached.
    pub async fn connect(url: &str) -> Result<Self> {
        let config = Config::from_url(url)
            .map_err(|error| unavailable(BACKEND, "parse the Redis URL", &error))?;
        let client = Builder::from_config(config)
            .build()
            .map_err(|error| unavailable(BACKEND, "build the Redis client", &error))?;
        client.init().await.map_err(|error| unavailable(BACKEND, "connect", &error))?;
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

    /// Records the store's layout version, refusing keys a newer release wrote.
    ///
    /// # Errors
    ///
    /// Returns `wallet.store_unavailable` when the server fails, or `wallet.store_corrupt`
    /// when the namespace holds a newer layout.
    pub async fn migrate(&self) -> Result<()> {
        let layout_key = format!("{}:layout", self.namespace_segment());
        let stored: i64 = self
            .client
            .eval(MIGRATE_SCRIPT, vec![layout_key], vec![LAYOUT.to_string()])
            .await
            .map_err(|error| unavailable(BACKEND, "record the layout version", &error))?;
        if stored > LAYOUT {
            return Err(super::corrupt(
                BACKEND,
                format!(
                    "under {} has layout v{stored}, but this release knows up to v{LAYOUT}; \
                     upgrade ADK-Rust",
                    self.namespace
                ),
            ));
        }
        Ok(())
    }

    /// The namespace with its braces percent-encoded.
    fn namespace_segment(&self) -> String {
        self.namespace.replace('{', "%7B").replace('}', "%7D")
    }

    /// `<ns>:{<wallet>}:<suffix>`.
    fn key(&self, wallet: &str, suffix: &str) -> String {
        let mut encoded = String::with_capacity(wallet.len());
        for character in wallet.chars() {
            match character {
                '%' => encoded.push_str("%25"),
                ':' => encoded.push_str("%3A"),
                '{' => encoded.push_str("%7B"),
                '}' => encoded.push_str("%7D"),
                other => encoded.push(other),
            }
        }
        format!("{}:{{{encoded}}}:{suffix}", self.namespace_segment())
    }

    /// The five state indexes of `wallet`, in [`STATES`] order.
    fn state_keys(&self, wallet: &str) -> Vec<String> {
        STATES
            .iter()
            .map(|state| self.key(wallet, &format!("state:{}", state_name(*state))))
            .collect()
    }
}

/// The position of `state` among the state indexes, from 1.
fn state_position(state: PaymentState) -> usize {
    STATES.iter().position(|candidate| *candidate == state).map_or(1, |index| index + 1)
}

/// A reply value as text.
fn text(value: &Value) -> Option<String> {
    value.as_string()
}

#[async_trait::async_trait]
impl WalletStore for RedisWalletStore {
    async fn insert(&self, record: PaymentRecord) -> Result<(PaymentRecord, bool)> {
        let wallet = &record.wallet;
        let mut keys = vec![
            self.key(wallet, "payments"),
            self.key(wallet, "keys"),
            self.key(wallet, "seq"),
            self.key(wallet, "order"),
            self.key(wallet, "at"),
        ];
        keys.extend(self.state_keys(wallet));
        let args = vec![
            record.id.clone(),
            record.key.clone(),
            encode(BACKEND, &record)?,
            instant_micros(&record).to_string(),
            state_position(record.state).to_string(),
        ];
        let reply: Vec<Value> = self
            .client
            .eval(INSERT_SCRIPT, keys, args)
            .await
            .map_err(|error| unavailable(BACKEND, "store a payment", &error))?;
        match reply.as_slice() {
            [inserted] if inserted.as_i64() == Some(1) => Ok((record, true)),
            [_, stored] => match text(stored) {
                Some(stored) => Ok((decode(BACKEND, "payment", &stored)?, false)),
                None => Err(super::corrupt(
                    BACKEND,
                    format!("maps key {} to a payment it does not hold", record.key),
                )),
            },
            _ => Err(super::corrupt(BACKEND, "answered an insert with an unexpected reply")),
        }
    }

    async fn update(&self, record: &PaymentRecord) -> Result<()> {
        let wallet = &record.wallet;
        let mut keys =
            vec![self.key(wallet, "payments"), self.key(wallet, "order"), self.key(wallet, "at")];
        keys.extend(self.state_keys(wallet));
        let args = vec![
            record.id.clone(),
            encode(BACKEND, record)?,
            instant_micros(record).to_string(),
            state_position(record.state).to_string(),
        ];
        let updated: i64 = self
            .client
            .eval(UPDATE_SCRIPT, keys, args)
            .await
            .map_err(|error| unavailable(BACKEND, "update a payment", &error))?;
        if updated == 0 {
            return Err(payment_not_found(record));
        }
        Ok(())
    }

    async fn get(&self, wallet: &str, id: &str) -> Result<Option<PaymentRecord>> {
        let record: Option<String> = self
            .client
            .hget(self.key(wallet, "payments"), id)
            .await
            .map_err(|error| unavailable(BACKEND, "read a payment", &error))?;
        record.map(|record| decode(BACKEND, "payment", &record)).transpose()
    }

    async fn by_key(&self, wallet: &str, key: &str) -> Result<Option<PaymentRecord>> {
        let record: Option<String> = self
            .client
            .eval(
                BY_KEY_SCRIPT,
                vec![self.key(wallet, "payments"), self.key(wallet, "keys")],
                vec![key.to_string()],
            )
            .await
            .map_err(|error| unavailable(BACKEND, "read a payment", &error))?;
        record.map(|record| decode(BACKEND, "payment", &record)).transpose()
    }

    async fn list(&self, query: &PaymentQuery) -> Result<Vec<PaymentRecord>> {
        let wallet = &query.wallet;
        // Few payments wait or execute at once, so their state index is the narrowest read;
        // otherwise a window narrows by instant, rounded so it is never narrower than asked.
        let few = matches!(query.state, Some(PaymentState::Held | PaymentState::Executing));
        let (index, args) = match (query.state, query.from.or(query.to)) {
            (Some(state), _) if few => {
                (self.key(wallet, &format!("state:{}", state_name(state))), vec!["all".into()])
            }
            (_, Some(_)) => {
                let least = query.from.map_or("-inf".to_string(), |from| micros(from).to_string());
                let greatest = query.to.map_or("+inf".to_string(), |to| micros(to).to_string());
                (self.key(wallet, "at"), vec!["range".to_string(), least, greatest])
            }
            (Some(state), None) => {
                (self.key(wallet, &format!("state:{}", state_name(state))), vec!["all".into()])
            }
            (None, None) => (self.key(wallet, "order"), vec!["all".to_string()]),
        };
        let keys = vec![self.key(wallet, "payments"), self.key(wallet, "order"), index];
        let reply: Vec<Value> = self
            .client
            .eval(LIST_SCRIPT, keys, args)
            .await
            .map_err(|error| unavailable(BACKEND, "list payments", &error))?;
        let mut candidates = Vec::with_capacity(reply.len() / 2);
        for pair in reply.chunks(2) {
            let [seq, record] = pair else {
                return Err(super::corrupt(BACKEND, "answered a listing with an odd reply"));
            };
            let seq = text(seq).and_then(|seq| seq.parse::<f64>().ok()).unwrap_or(0.0);
            let record = text(record)
                .ok_or_else(|| super::corrupt(BACKEND, "answered a listing without a record"))?;
            let record: PaymentRecord = decode(BACKEND, "payment", &record)?;
            if query.matches(&record) {
                candidates.push((seq, record));
            }
        }
        candidates.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        Ok(candidates.into_iter().map(|(_, record)| record).collect())
    }

    async fn pause(&self, pause: CategoryPause) -> Result<()> {
        self.client
            .hsetnx::<i64, _, _, _>(
                self.key(&pause.wallet, "pauses"),
                category_name(pause.category),
                encode(BACKEND, &pause)?,
            )
            .await
            .map_err(|error| unavailable(BACKEND, "pause a category", &error))?;
        Ok(())
    }

    async fn resume(&self, wallet: &str, category: SpendCategory) -> Result<Option<CategoryPause>> {
        let pause: Option<String> = self
            .client
            .eval(RESUME_SCRIPT, vec![self.key(wallet, "pauses")], vec![category_name(category)])
            .await
            .map_err(|error| unavailable(BACKEND, "resume a category", &error))?;
        pause.map(|pause| decode(BACKEND, "pause", &pause)).transpose()
    }

    async fn pauses(&self, wallet: &str) -> Result<Vec<CategoryPause>> {
        let all: Vec<String> = self
            .client
            .hvals(self.key(wallet, "pauses"))
            .await
            .map_err(|error| unavailable(BACKEND, "list paused categories", &error))?;
        let pauses = all
            .iter()
            .map(|pause| decode(BACKEND, "pause", pause))
            .collect::<Result<Vec<CategoryPause>>>()?;
        Ok(in_category_order(pauses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(namespace: &str) -> RedisWalletStore {
        let client = Builder::default_centralized().build().expect("a client builds offline");
        RedisWalletStore::from_client(client).with_namespace(namespace)
    }

    #[test]
    fn keys_share_one_hash_tag_per_wallet() {
        let default = store("adk:wallet");
        assert_eq!(default.key("site-1", "payments"), "adk:wallet:{site-1}:payments");
        assert_eq!(
            default.key("a:b{c}%", "keys"),
            "adk:wallet:{a%3Ab%7Bc%7D%25}:keys",
            "a wallet id cannot spill into the next segment or move the slot"
        );
        assert_eq!(store("ns{x}").key("w", "at"), "ns%7Bx%7D:{w}:at");
    }

    #[test]
    fn state_indexes_follow_the_scripts_order() {
        let keys = store("adk:wallet").state_keys("w");
        for (position, state) in STATES.iter().enumerate() {
            assert_eq!(state_position(*state), position + 1);
            assert_eq!(keys[position], format!("adk:wallet:{{w}}:state:{}", state_name(*state)));
        }
    }
}
