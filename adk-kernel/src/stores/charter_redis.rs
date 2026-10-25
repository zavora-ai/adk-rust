//! The Redis charter, KPI, autonomy and signal stores (spec 3.5 §7.10, §7.11, features
//! `charter` and `redis`).
//!
//! Keys sit under the namespace `adk:kernel` unless `with_namespace` sets another, inside
//! the hash tag `{<namespace>}`, so each script touches one Redis Cluster slot; braces in
//! the namespace are percent-encoded.
//!
//! | Key | Type | Contents |
//! |-----|------|----------|
//! | `{<ns>}:charters:<site>` | Hash | Each committed version's [`CharterVersion`](crate::charter::CharterVersion) as JSON, by number |
//! | `{<ns>}:charters:<site>:latest` | String | The site's latest version number |
//! | `{<ns>}:kpi:<site>:<kpi>` | Sorted set | Each sample as JSON, scored by its instant in microseconds |
//! | `{<ns>}:kpi:<site>:<kpi>:at` | Set | The instants the KPI has a sample at |
//! | `{<ns>}:kpi-states:<site>` | Hash | The tracker's state of each KPI (`kpi:<id>`) and guardrail (`guardrail:<id>`) |
//! | `{<ns>}:jobs:<site>`, `{<ns>}:job-revisions:<site>` | Hash | Each job's record as JSON, and its revision |
//! | `{<ns>}:job-history:<site>:<job>` | List | The job's changes as JSON, oldest first |
//! | `{<ns>}:job-decisions:<site>:<job>`, `…:at` | Hash, sorted set | Each decision as JSON by id, and the ids scored by instant |
//! | `{<ns>}:signal:<id>` | Hash | One signal record: `id`, `partition`, `kind`, `topic`, `count`, `signal` (JSON), `source`, `first_at`, `last_at`, `open` (`1` or `0`), `okey` |
//! | `{<ns>}:signal-open` | Hash | The open record of each key, by the key as JSON |
//! | `{<ns>}:signals` | Sorted set | Every record id, scored by its first arrival |
//! | `{<ns>}:signal-rate:<source>:<window>` | String | One source's count in one rate window; expires after a day |
//! | `{<ns>}:charter:layout` | String | The layout version `migrate` recorded |
//!
//! Every compare-and-set is one Lua script, which Redis runs atomically.

use fred::prelude::{
    Builder, Client, ClientLike as _, Config, HashesInterface as _, LuaInterface as _,
};
use fred::types::Value;
use std::collections::HashMap;

use crate::charter::SignalRecord;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;

/// The layout version this release writes.
const LAYOUT: i64 = 1;

/// KEYS: layout. ARGV: this release's layout version.
const MIGRATE_SCRIPT: &str = r"
local stored = tonumber(redis.call('GET', KEYS[1]) or '0')
local mine = tonumber(ARGV[1])
if stored > mine then return stored end
if stored < mine then redis.call('SET', KEYS[1], ARGV[1]) end
return mine
";

/// KEYS: versions hash, latest. ARGV: version, record. Appends only the next version.
const CHARTER_APPEND_SCRIPT: &str = r"
local latest = tonumber(redis.call('GET', KEYS[2]) or '0')
if latest + 1 ~= tonumber(ARGV[1]) then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('SET', KEYS[2], ARGV[1])
return 1
";

/// KEYS: versions hash, latest. Returns the latest version's record, or nil.
const CHARTER_CURRENT_SCRIPT: &str = r"
local latest = redis.call('GET', KEYS[2])
if not latest then return false end
return redis.call('HGET', KEYS[1], latest)
";

/// KEYS: samples, instants. ARGV: instant, sample JSON. Keeps the first sample per instant.
const KPI_RECORD_SCRIPT: &str = r"
if redis.call('SADD', KEYS[2], ARGV[1]) == 0 then return 0 end
redis.call('ZADD', KEYS[1], ARGV[1], ARGV[2])
return 1
";

/// KEYS: records, revisions, history. ARGV: job, revision, record, change.
const JOB_APPLY_SCRIPT: &str = r"
local stored = tonumber(redis.call('HGET', KEYS[2], ARGV[1]) or '0')
if stored + 1 ~= tonumber(ARGV[2]) then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[3])
redis.call('HSET', KEYS[2], ARGV[1], ARGV[2])
redis.call('RPUSH', KEYS[3], ARGV[4])
return 1
";

/// KEYS: decisions hash, decisions by instant. ARGV: id, instant, record.
const DECISION_SCRIPT: &str = r"
if redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[3]) == 0 then return 0 end
redis.call('ZADD', KEYS[2], ARGV[2], ARGV[1])
return 1
";

/// KEYS: open-records hash, records index. ARGV: the key's field, new id, row key prefix,
/// partition, kind, topic, signal JSON, source, instant. Counts on the key's open record or
/// opens one; returns `{1 when new, field, value, …}` of the record.
const SIGNAL_RECORD_SCRIPT: &str = r"
local id = redis.call('HGET', KEYS[1], ARGV[1])
if id then
  local row = ARGV[3] .. id
  if redis.call('HGET', row, 'open') == '1' then
    redis.call('HINCRBY', row, 'count', 1)
    redis.call('HSET', row, 'signal', ARGV[7], 'source', ARGV[8], 'last_at', ARGV[9])
    local fields = redis.call('HGETALL', row)
    table.insert(fields, 1, 0)
    return fields
  end
end
local row = ARGV[3] .. ARGV[2]
redis.call('HSET', row, 'id', ARGV[2], 'partition', ARGV[4], 'kind', ARGV[5], 'topic', ARGV[6],
  'count', 1, 'signal', ARGV[7], 'source', ARGV[8], 'first_at', ARGV[9], 'last_at', ARGV[9],
  'open', '1', 'okey', ARGV[1])
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('ZADD', KEYS[2], ARGV[9], ARGV[2])
local fields = redis.call('HGETALL', row)
table.insert(fields, 1, 1)
return fields
";

/// KEYS: open-records hash, the record's row. ARGV: id.
const SIGNAL_CLOSE_SCRIPT: &str = r"
if redis.call('HGET', KEYS[2], 'open') ~= '1' then return 0 end
redis.call('HSET', KEYS[2], 'open', '0')
local field = redis.call('HGET', KEYS[2], 'okey')
if field and redis.call('HGET', KEYS[1], field) == ARGV[1] then redis.call('HDEL', KEYS[1], field) end
return 1
";

/// KEYS: a sorted set. ARGV: the least and greatest score, as `ZRANGEBYSCORE` takes them.
const RANGE_SCRIPT: &str = "return redis.call('ZRANGEBYSCORE', KEYS[1], ARGV[1], ARGV[2])";

/// KEYS: the window's counter. ARGV: its lifetime in milliseconds.
const RATE_SCRIPT: &str = r"
local count = redis.call('INCR', KEYS[1])
if count == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
return count
";

fn store_error(action: &str, error: &dyn std::fmt::Display) -> KernelError {
    KernelError::Store { backend: "redis", reason: format!("could not {action}: {error}") }
}

fn decode<T: serde::de::DeserializeOwned>(what: &str, record: &str) -> Result<T> {
    serde_json::from_str(record).map_err(|error| KernelError::Store {
        backend: "redis",
        reason: format!("a stored {what} is corrupt: {error}"),
    })
}

/// The field-value pairs of a script's flat reply.
fn pairs(values: &[Value]) -> HashMap<String, String> {
    values
        .chunks(2)
        .filter_map(|pair| match pair {
            [name, value] => Some((name.as_string()?, value.as_string()?)),
            _ => None,
        })
        .collect()
}

/// A signal record from its hash's fields.
fn signal_record(fields: &HashMap<String, String>) -> Result<SignalRecord> {
    let text = |name: &str| {
        fields
            .get(name)
            .cloned()
            .ok_or_else(|| store_error("read a signal record", &format!("no `{name}` field")))
    };
    let number = |name: &str| -> Result<i64> {
        text(name)?
            .parse::<i64>()
            .map_err(|error| store_error("read a signal record", &format!("`{name}`: {error}")))
    };
    let instant = |name: &str| -> Result<chrono::DateTime<chrono::Utc>> {
        let micros = number(name)?;
        chrono::DateTime::from_timestamp_micros(micros).ok_or_else(|| {
            store_error("read a signal record", &format!("`{name}` is out of range"))
        })
    };
    Ok(SignalRecord {
        id: text("id")?,
        partition: PartitionId::new(text("partition")?),
        kind: text("kind")?,
        topic: text("topic")?,
        count: u32::try_from(number("count")?).unwrap_or(u32::MAX),
        signal: decode("signal", &text("signal")?)?,
        source: text("source")?,
        first_at: instant("first_at")?,
        last_at: instant("last_at")?,
        open: text("open")? == "1",
    })
}

/// Declares one Redis store type with `connect`, `from_client`, `with_namespace`, `client`,
/// `namespace`, `migrate` and its key builder.
macro_rules! redis_charter_store_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            client: Client,
            namespace: String,
        }

        impl $name {
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
                let config = Config::from_url(url)
                    .map_err(|error| store_error("parse the Redis URL", &error))?;
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

            /// Records the charter stores' layout version, refusing a database a newer
            /// release wrote.
            ///
            /// # Errors
            ///
            /// Returns [`KernelError::Store`] when the server fails or holds a newer layout.
            pub async fn migrate(&self) -> Result<()> {
                let stored: i64 = self
                    .client
                    .eval(MIGRATE_SCRIPT, vec![self.key("charter:layout")], vec![LAYOUT.to_string()])
                    .await
                    .map_err(|error| store_error("record the layout version", &error))?;
                if stored > LAYOUT {
                    return Err(KernelError::Store {
                        backend: "redis",
                        reason: format!(
                            "the charter keys under {} have layout v{stored}, but this release \
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
    };
}

redis_charter_store_type!(
    /// A [`CharterStore`](crate::charter::CharterStore) in Redis, shared by every replica
    /// (spec 3.5 §7.11, features `charter` and `redis`). See the module documentation for
    /// its keys.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # async fn example() -> adk_kernel::Result<()> {
    /// let store = adk_kernel::charter::RedisCharterStore::connect("redis://localhost:6379").await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    RedisCharterStore
);
redis_charter_store_type!(
    /// A [`KpiStore`](crate::charter::KpiStore) in Redis (spec 3.5 §7.11, features `charter`
    /// and `redis`).
    RedisKpiStore
);
redis_charter_store_type!(
    /// An [`AutonomyStore`](crate::charter::AutonomyStore) in Redis (spec 3.5 §7.11,
    /// features `charter` and `redis`).
    RedisAutonomyStore
);
redis_charter_store_type!(
    /// A [`SignalStore`](crate::charter::SignalStore) in Redis (spec 3.5 §7.10, features
    /// `charter` and `redis`).
    RedisSignalStore
);

#[async_trait::async_trait]
impl crate::charter::CharterStore for RedisCharterStore {
    async fn append(&self, version: crate::charter::CharterVersion) -> Result<bool> {
        let site = version.site();
        let keys = vec![
            self.key(&format!("charters:{site}")),
            self.key(&format!("charters:{site}:latest")),
        ];
        let added: i64 = self
            .client
            .eval(
                CHARTER_APPEND_SCRIPT,
                keys,
                vec![version.version().to_string(), serde_json::to_string(&version)?],
            )
            .await
            .map_err(|error| store_error("append a charter version", &error))?;
        Ok(added == 1)
    }

    async fn current(&self, site: uuid::Uuid) -> Result<Option<crate::charter::CharterVersion>> {
        let keys = vec![
            self.key(&format!("charters:{site}")),
            self.key(&format!("charters:{site}:latest")),
        ];
        let record: Option<String> = self
            .client
            .eval(CHARTER_CURRENT_SCRIPT, keys, Vec::<String>::new())
            .await
            .map_err(|error| store_error("read the current charter", &error))?;
        record.map(|record| decode("charter version", &record)).transpose()
    }

    async fn history(&self, site: uuid::Uuid) -> Result<Vec<crate::charter::CharterVersion>> {
        let all: std::collections::HashMap<String, String> = self
            .client
            .hgetall(self.key(&format!("charters:{site}")))
            .await
            .map_err(|error| store_error("read the charter history", &error))?;
        let mut versions = all
            .values()
            .map(|record| decode::<crate::charter::CharterVersion>("charter version", record))
            .collect::<Result<Vec<_>>>()?;
        versions.sort_by_key(crate::charter::CharterVersion::version);
        Ok(versions)
    }

    async fn get(
        &self,
        site: uuid::Uuid,
        version: u32,
    ) -> Result<Option<crate::charter::CharterVersion>> {
        let record: Option<String> = self
            .client
            .hget(self.key(&format!("charters:{site}")), version.to_string())
            .await
            .map_err(|error| store_error("read a charter version", &error))?;
        record.map(|record| decode("charter version", &record)).transpose()
    }
}

#[async_trait::async_trait]
impl crate::charter::KpiStore for RedisKpiStore {
    async fn record(&self, sample: crate::charter::KpiSample) -> Result<bool> {
        sample.validate()?;
        let base = format!("kpi:{}:{}", sample.site, sample.kpi);
        let keys = vec![self.key(&base), self.key(&format!("{base}:at"))];
        let added: i64 = self
            .client
            .eval(
                KPI_RECORD_SCRIPT,
                keys,
                vec![sample.at.timestamp_micros().to_string(), serde_json::to_string(&sample)?],
            )
            .await
            .map_err(|error| store_error("record a KPI sample", &error))?;
        Ok(added == 1)
    }

    async fn samples(
        &self,
        site: uuid::Uuid,
        kpi: &str,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::charter::KpiSample>> {
        if from >= to {
            return Ok(Vec::new());
        }
        let records: Vec<String> = self
            .client
            .eval(
                RANGE_SCRIPT,
                vec![self.key(&format!("kpi:{site}:{kpi}"))],
                vec![from.timestamp_micros().to_string(), format!("({}", to.timestamp_micros())],
            )
            .await
            .map_err(|error| store_error("read KPI samples", &error))?;
        records.iter().map(|record| decode("KPI sample", record)).collect()
    }

    async fn state(&self, site: uuid::Uuid, kpi: &str) -> Result<Option<crate::charter::KpiState>> {
        let record: Option<String> = self
            .client
            .hget(self.key(&format!("kpi-states:{site}")), format!("kpi:{kpi}"))
            .await
            .map_err(|error| store_error("read a KPI state", &error))?;
        record.map(|record| decode("KPI state", &record)).transpose()
    }

    async fn put_state(&self, state: crate::charter::KpiState) -> Result<()> {
        let key = self.key(&format!("kpi-states:{}", state.site));
        self.client
            .hset::<i64, _, _>(key, (format!("kpi:{}", state.kpi), serde_json::to_string(&state)?))
            .await
            .map_err(|error| store_error("write a KPI state", &error))?;
        Ok(())
    }

    async fn guardrail(
        &self,
        site: uuid::Uuid,
        guardrail: &str,
    ) -> Result<Option<crate::charter::GuardrailState>> {
        let record: Option<String> = self
            .client
            .hget(self.key(&format!("kpi-states:{site}")), format!("guardrail:{guardrail}"))
            .await
            .map_err(|error| store_error("read a guardrail state", &error))?;
        record.map(|record| decode("guardrail state", &record)).transpose()
    }

    async fn put_guardrail(&self, state: crate::charter::GuardrailState) -> Result<()> {
        let key = self.key(&format!("kpi-states:{}", state.site));
        self.client
            .hset::<i64, _, _>(
                key,
                (format!("guardrail:{}", state.guardrail), serde_json::to_string(&state)?),
            )
            .await
            .map_err(|error| store_error("write a guardrail state", &error))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::charter::AutonomyStore for RedisAutonomyStore {
    async fn get(
        &self,
        site: uuid::Uuid,
        job: &str,
    ) -> Result<Option<crate::charter::JobAutonomy>> {
        let record: Option<String> = self
            .client
            .hget(self.key(&format!("jobs:{site}")), job)
            .await
            .map_err(|error| store_error("read a job's autonomy", &error))?;
        record.map(|record| decode("job autonomy", &record)).transpose()
    }

    async fn list(&self, site: uuid::Uuid) -> Result<Vec<crate::charter::JobAutonomy>> {
        let all: std::collections::HashMap<String, String> = self
            .client
            .hgetall(self.key(&format!("jobs:{site}")))
            .await
            .map_err(|error| store_error("list job autonomy", &error))?;
        let mut jobs = all
            .values()
            .map(|record| decode::<crate::charter::JobAutonomy>("job autonomy", record))
            .collect::<Result<Vec<_>>>()?;
        jobs.sort_by(|a, b| a.job.cmp(&b.job));
        Ok(jobs)
    }

    async fn apply(
        &self,
        record: crate::charter::JobAutonomy,
        change: crate::charter::AutonomyChange,
    ) -> Result<bool> {
        crate::charter::check_autonomy_apply(&record, &change)?;
        let site = record.site;
        let keys = vec![
            self.key(&format!("jobs:{site}")),
            self.key(&format!("job-revisions:{site}")),
            self.key(&format!("job-history:{site}:{}", record.job)),
        ];
        let args = vec![
            record.job.clone(),
            record.revision.to_string(),
            serde_json::to_string(&record)?,
            serde_json::to_string(&change)?,
        ];
        let applied: i64 = self
            .client
            .eval(JOB_APPLY_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("change a job's autonomy", &error))?;
        Ok(applied == 1)
    }

    async fn history(
        &self,
        site: uuid::Uuid,
        job: &str,
    ) -> Result<Vec<crate::charter::AutonomyChange>> {
        use fred::prelude::ListInterface as _;

        let changes: Vec<String> = self
            .client
            .lrange(self.key(&format!("job-history:{site}:{job}")), 0, -1)
            .await
            .map_err(|error| store_error("read a job's autonomy history", &error))?;
        changes.iter().map(|change| decode("autonomy change", change)).collect()
    }

    async fn record_decision(&self, decision: crate::charter::JobDecision) -> Result<bool> {
        let base = format!("job-decisions:{}:{}", decision.site, decision.job);
        let keys = vec![self.key(&base), self.key(&format!("{base}:at"))];
        let args = vec![
            decision.id.clone(),
            decision.at.timestamp_micros().to_string(),
            serde_json::to_string(&decision)?,
        ];
        let added: i64 = self
            .client
            .eval(DECISION_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("record a job decision", &error))?;
        Ok(added == 1)
    }

    async fn decisions(
        &self,
        site: uuid::Uuid,
        job: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<crate::charter::JobDecision>> {
        let base = format!("job-decisions:{site}:{job}");
        let ids: Vec<String> = self
            .client
            .eval(
                RANGE_SCRIPT,
                vec![self.key(&format!("{base}:at"))],
                vec![since.timestamp_micros().to_string(), "+inf".to_string()],
            )
            .await
            .map_err(|error| store_error("read job decisions", &error))?;
        let mut decisions = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let records: Vec<Option<String>> = self
                .client
                .hmget(self.key(&base), chunk.to_vec())
                .await
                .map_err(|error| store_error("read job decisions", &error))?;
            for record in records.into_iter().flatten() {
                decisions.push(decode::<crate::charter::JobDecision>("job decision", &record)?);
            }
        }
        decisions.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        Ok(decisions)
    }
}

#[async_trait::async_trait]
impl crate::charter::SignalStore for RedisSignalStore {
    async fn record(
        &self,
        key: &crate::charter::SignalKey,
        signal: &awp_types::Signal,
        source: &str,
        at: chrono::DateTime<chrono::Utc>,
        new_id: &str,
    ) -> Result<(SignalRecord, bool)> {
        let field = serde_json::to_string(&(key.partition.as_str(), &key.kind, &key.topic))?;
        let keys = vec![self.key("signal-open"), self.key("signals")];
        let args = vec![
            field,
            new_id.to_string(),
            self.key("signal:"),
            key.partition.as_str().to_string(),
            key.kind.clone(),
            key.topic.clone(),
            serde_json::to_string(signal)?,
            source.to_string(),
            at.timestamp_micros().to_string(),
        ];
        let reply: Value = self
            .client
            .eval(SIGNAL_RECORD_SCRIPT, keys, args)
            .await
            .map_err(|error| store_error("record a signal", &error))?;
        let mut items = reply.into_array();
        if items.is_empty() {
            return Err(store_error("record a signal", &"the script answered nothing"));
        }
        let new = items.remove(0).as_i64() == Some(1);
        Ok((signal_record(&pairs(&items))?, new))
    }

    async fn get(&self, id: &str) -> Result<Option<SignalRecord>> {
        let fields: HashMap<String, String> = self
            .client
            .hgetall(self.key(&format!("signal:{id}")))
            .await
            .map_err(|error| store_error("read a signal record", &error))?;
        if fields.is_empty() { Ok(None) } else { signal_record(&fields).map(Some) }
    }

    async fn list(&self, filter: &crate::charter::SignalFilter) -> Result<Vec<SignalRecord>> {
        let ids: Vec<String> = self
            .client
            .eval(
                RANGE_SCRIPT,
                vec![self.key("signals")],
                vec!["-inf".to_string(), "+inf".to_string()],
            )
            .await
            .map_err(|error| store_error("list signal records", &error))?;
        let mut records = Vec::new();
        for id in ids {
            if filter.limit.is_some_and(|limit| records.len() >= limit) {
                break;
            }
            if let Some(record) = self.get(&id).await?
                && filter.matches(&record)
            {
                records.push(record);
            }
        }
        Ok(records)
    }

    async fn close(&self, id: &str) -> Result<bool> {
        let keys = vec![self.key("signal-open"), self.key(&format!("signal:{id}"))];
        let closed: i64 = self
            .client
            .eval(SIGNAL_CLOSE_SCRIPT, keys, vec![id.to_string()])
            .await
            .map_err(|error| store_error("close a signal record", &error))?;
        Ok(closed == 1)
    }

    async fn hit(&self, source: &str, window_start: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let key = self.key(&format!("signal-rate:{source}:{}", window_start.timestamp_micros()));
        let life = (crate::charter::RATE_HORIZON_MICROS / 1_000).to_string();
        let count: i64 = self
            .client
            .eval(RATE_SCRIPT, vec![key], vec![life])
            .await
            .map_err(|error| store_error("count a signal source", &error))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }
}
