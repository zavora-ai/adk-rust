//! The in-memory charter, KPI, autonomy and signal stores (spec 3.5 §7.10, §7.11).

use std::collections::BTreeMap;

use async_trait::async_trait;
use awp_types::Signal;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::charter::{
    AutonomyChange, AutonomyStore, CharterStore, CharterVersion, GuardrailState, JobAutonomy,
    JobDecision, KpiSample, KpiState, KpiStore, SignalFilter, SignalKey, SignalRecord, SignalStore,
};
use crate::error::Result;

/// A [`CharterStore`] in process memory, for tests and a kernel with no durable store (spec
/// 3.5 §7.11). Its versions end with the process.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::{CharterStore, InMemoryCharterStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryCharterStore::new();
/// assert!(store.current(uuid::Uuid::nil()).await?.is_none());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryCharterStore {
    versions: RwLock<BTreeMap<Uuid, Vec<CharterVersion>>>,
}

impl InMemoryCharterStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CharterStore for InMemoryCharterStore {
    async fn append(&self, version: CharterVersion) -> Result<bool> {
        let mut versions = self.versions.write().await;
        let site = versions.entry(version.site()).or_default();
        if version.version() as usize != site.len() + 1 {
            return Ok(false);
        }
        site.push(version);
        Ok(true)
    }

    async fn current(&self, site: Uuid) -> Result<Option<CharterVersion>> {
        Ok(self.versions.read().await.get(&site).and_then(|versions| versions.last().cloned()))
    }

    async fn history(&self, site: Uuid) -> Result<Vec<CharterVersion>> {
        Ok(self.versions.read().await.get(&site).cloned().unwrap_or_default())
    }

    async fn get(&self, site: Uuid, version: u32) -> Result<Option<CharterVersion>> {
        Ok(self
            .versions
            .read()
            .await
            .get(&site)
            .and_then(|versions| versions.get((version as usize).checked_sub(1)?).cloned()))
    }
}

/// A [`KpiStore`] in process memory (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::{InMemoryKpiStore, KpiSample, KpiStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryKpiStore::new();
/// assert!(store.record(KpiSample::new(uuid::Uuid::nil(), "leads", chrono::Utc::now(), 1.0, "commerce")).await?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryKpiStore {
    /// (site, KPI) → instant in microseconds → sample.
    samples: RwLock<BTreeMap<(Uuid, String), BTreeMap<i64, KpiSample>>>,
    states: RwLock<BTreeMap<(Uuid, String), KpiState>>,
    guardrails: RwLock<BTreeMap<(Uuid, String), GuardrailState>>,
}

impl InMemoryKpiStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl KpiStore for InMemoryKpiStore {
    async fn record(&self, sample: KpiSample) -> Result<bool> {
        sample.validate()?;
        let mut samples = self.samples.write().await;
        let series = samples.entry((sample.site, sample.kpi.clone())).or_default();
        let at = sample.at.timestamp_micros();
        if series.contains_key(&at) {
            return Ok(false);
        }
        series.insert(at, sample);
        Ok(true)
    }

    async fn samples(
        &self,
        site: Uuid,
        kpi: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<KpiSample>> {
        let (from, to) = (from.timestamp_micros(), to.timestamp_micros());
        if from >= to {
            return Ok(Vec::new());
        }
        Ok(self
            .samples
            .read()
            .await
            .get(&(site, kpi.to_string()))
            .map(|series| series.range(from..to).map(|(_, sample)| sample.clone()).collect())
            .unwrap_or_default())
    }

    async fn state(&self, site: Uuid, kpi: &str) -> Result<Option<KpiState>> {
        Ok(self.states.read().await.get(&(site, kpi.to_string())).cloned())
    }

    async fn put_state(&self, state: KpiState) -> Result<()> {
        self.states.write().await.insert((state.site, state.kpi.clone()), state);
        Ok(())
    }

    async fn guardrail(&self, site: Uuid, guardrail: &str) -> Result<Option<GuardrailState>> {
        Ok(self.guardrails.read().await.get(&(site, guardrail.to_string())).cloned())
    }

    async fn put_guardrail(&self, state: GuardrailState) -> Result<()> {
        self.guardrails.write().await.insert((state.site, state.guardrail.clone()), state);
        Ok(())
    }
}

/// An [`AutonomyStore`] in process memory (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::{AutonomyStore, InMemoryAutonomyStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryAutonomyStore::new();
/// assert!(store.get(uuid::Uuid::nil(), "seo").await?.is_none());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryAutonomyStore {
    jobs: RwLock<AutonomyTables>,
}

#[derive(Debug, Default)]
struct AutonomyTables {
    records: BTreeMap<(Uuid, String), JobAutonomy>,
    history: BTreeMap<(Uuid, String), Vec<AutonomyChange>>,
    decisions: BTreeMap<(Uuid, String), BTreeMap<String, JobDecision>>,
}

impl InMemoryAutonomyStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AutonomyStore for InMemoryAutonomyStore {
    async fn get(&self, site: Uuid, job: &str) -> Result<Option<JobAutonomy>> {
        Ok(self.jobs.read().await.records.get(&(site, job.to_string())).cloned())
    }

    async fn list(&self, site: Uuid) -> Result<Vec<JobAutonomy>> {
        Ok(self
            .jobs
            .read()
            .await
            .records
            .values()
            .filter(|record| record.site == site)
            .cloned()
            .collect())
    }

    async fn apply(&self, record: JobAutonomy, change: AutonomyChange) -> Result<bool> {
        crate::charter::check_autonomy_apply(&record, &change)?;
        let mut tables = self.jobs.write().await;
        let key = (record.site, record.job.clone());
        let stored = tables.records.get(&key).map_or(0, |stored| stored.revision);
        if stored + 1 != record.revision {
            return Ok(false);
        }
        tables.records.insert(key.clone(), record);
        tables.history.entry(key).or_default().push(change);
        Ok(true)
    }

    async fn history(&self, site: Uuid, job: &str) -> Result<Vec<AutonomyChange>> {
        Ok(self
            .jobs
            .read()
            .await
            .history
            .get(&(site, job.to_string()))
            .cloned()
            .unwrap_or_default())
    }

    async fn record_decision(&self, decision: JobDecision) -> Result<bool> {
        let mut tables = self.jobs.write().await;
        let decisions = tables.decisions.entry((decision.site, decision.job.clone())).or_default();
        if decisions.contains_key(&decision.id) {
            return Ok(false);
        }
        decisions.insert(decision.id.clone(), decision);
        Ok(true)
    }

    async fn decisions(
        &self,
        site: Uuid,
        job: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<JobDecision>> {
        let mut decisions: Vec<JobDecision> = self
            .jobs
            .read()
            .await
            .decisions
            .get(&(site, job.to_string()))
            .map(|decisions| decisions.values().filter(|d| d.at >= since).cloned().collect())
            .unwrap_or_default();
        decisions.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        Ok(decisions)
    }
}

/// A [`SignalStore`] in process memory (spec 3.5 §7.10).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::{InMemorySignalStore, SignalFilter, SignalStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemorySignalStore::new();
/// assert!(store.list(&SignalFilter::default()).await?.is_empty());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemorySignalStore {
    signals: RwLock<SignalTables>,
}

#[derive(Debug, Default)]
struct SignalTables {
    records: BTreeMap<String, SignalRecord>,
    /// The open record of each key.
    open: BTreeMap<SignalKey, String>,
    /// (source, window start in microseconds) → count.
    rates: BTreeMap<(String, i64), u64>,
}

impl InMemorySignalStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SignalStore for InMemorySignalStore {
    async fn record(
        &self,
        key: &SignalKey,
        signal: &Signal,
        source: &str,
        at: DateTime<Utc>,
        new_id: &str,
    ) -> Result<(SignalRecord, bool)> {
        let mut tables = self.signals.write().await;
        if let Some(id) = tables.open.get(key).cloned()
            && let Some(record) = tables.records.get_mut(&id)
        {
            record.count = record.count.saturating_add(1);
            record.signal = signal.clone();
            record.source = source.to_string();
            record.last_at = at;
            return Ok((record.clone(), false));
        }
        let record = SignalRecord {
            id: new_id.to_string(),
            partition: key.partition.clone(),
            kind: key.kind.clone(),
            topic: key.topic.clone(),
            count: 1,
            signal: signal.clone(),
            source: source.to_string(),
            first_at: at,
            last_at: at,
            open: true,
        };
        tables.open.insert(key.clone(), new_id.to_string());
        tables.records.insert(new_id.to_string(), record.clone());
        Ok((record, true))
    }

    async fn get(&self, id: &str) -> Result<Option<SignalRecord>> {
        Ok(self.signals.read().await.records.get(id).cloned())
    }

    async fn list(&self, filter: &SignalFilter) -> Result<Vec<SignalRecord>> {
        let mut records: Vec<SignalRecord> = self
            .signals
            .read()
            .await
            .records
            .values()
            .filter(|record| filter.matches(record))
            .cloned()
            .collect();
        records.sort_by(|a, b| a.first_at.cmp(&b.first_at).then_with(|| a.id.cmp(&b.id)));
        records.truncate(filter.limit.unwrap_or(usize::MAX));
        Ok(records)
    }

    async fn close(&self, id: &str) -> Result<bool> {
        let mut tables = self.signals.write().await;
        let Some(record) = tables.records.get_mut(id).filter(|record| record.open) else {
            return Ok(false);
        };
        record.open = false;
        let key = SignalKey {
            partition: record.partition.clone(),
            kind: record.kind.clone(),
            topic: record.topic.clone(),
        };
        if tables.open.get(&key).is_some_and(|open| open == id) {
            tables.open.remove(&key);
        }
        Ok(true)
    }

    async fn hit(&self, source: &str, window_start: DateTime<Utc>) -> Result<u64> {
        let start = window_start.timestamp_micros();
        let mut tables = self.signals.write().await;
        let horizon = start - crate::charter::RATE_HORIZON_MICROS;
        tables.rates.retain(|(_, window), _| *window >= horizon);
        let count = tables.rates.entry((source.to_string(), start)).or_insert(0);
        *count += 1;
        Ok(*count)
    }
}
