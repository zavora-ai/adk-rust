//! Experiment records between looks (spec 3.5 §7.12): the plan, the fixed analysis, the
//! cumulative per-arm counts, the analyzer's `ExperimentState` and the latest report.

use std::collections::BTreeMap;
use std::path::PathBuf;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_eval::experiments::{
    Decision, ExperimentConfig, ExperimentReport, ExperimentState, Tolerance,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::{Arm, ExperimentPlan};

/// A value per arm (spec 3.5 §7.12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arms<T> {
    /// The control's.
    pub control: T,
    /// The variant's.
    pub variant: T,
}

impl<T> Arms<T> {
    /// The value of `arm`.
    pub fn get(&self, arm: Arm) -> &T {
        match arm {
            Arm::Control => &self.control,
            Arm::Variant => &self.variant,
        }
    }

    /// The value of `arm`, to change.
    pub fn get_mut(&mut self, arm: Arm) -> &mut T {
        match arm {
            Arm::Control => &mut self.control,
            Arm::Variant => &mut self.variant,
        }
    }
}

/// The cumulative counts of an experiment since it started (spec 3.5 §7.12): exposures, and
/// the goal events of each metric, per arm.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmCounts {
    /// Tab sessions shown each arm.
    pub exposures: Arms<u64>,
    /// Goal events per metric.
    pub events: BTreeMap<String, Arms<u64>>,
}

/// A charter guardrail an experiment monitors, as it was fixed at the start (spec 3.5 §7.12,
/// A-I15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardedMetric {
    /// The charter guardrail's id.
    pub guardrail: String,
    /// The metric it watches.
    pub metric: String,
    /// The charter's threshold.
    pub threshold: f64,
    /// How much worse than the control the variant may be.
    pub tolerance: Tolerance,
}

/// Where an experiment stands (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status", rename_all_fields = "camelCase")]
pub enum ExperimentStatus {
    /// Its variant is served and its counts are read every day.
    Running,
    /// It reached a final decision; the next build serves the control alone.
    Stopped {
        /// The decision.
        decision: Decision,
        /// When.
        at: DateTime<Utc>,
        /// The job a guardrail breach paused (A-I15).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        paused_job: Option<String>,
    },
}

impl ExperimentStatus {
    /// Whether the experiment is running.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Everything about one experiment that outlives a look (spec 3.5 §7.12). It serialises to
/// camelCase JSON, its latest report included, for the web home.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentRecord {
    /// The site.
    pub site: Uuid,
    /// The plan.
    pub plan: ExperimentPlan,
    /// The analysis fixed at the start.
    pub config: ExperimentConfig,
    /// The guardrails it monitors.
    pub guardrails: Vec<GuardedMetric>,
    /// The charter version it started under.
    pub charter_version: u32,
    /// Where it stands.
    pub status: ExperimentStatus,
    /// When it started.
    pub started_at: DateTime<Utc>,
    /// The first day not yet counted: every whole day before it is in `counts`.
    pub counted_to: NaiveDate,
    /// The cumulative counts.
    pub counts: ArmCounts,
    /// The analyzer's state between looks.
    pub state: ExperimentState,
    /// The latest look's report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<ExperimentReport>,
}

impl ExperimentRecord {
    /// When it stopped, if it has.
    pub fn stopped_at(&self) -> Option<DateTime<Utc>> {
        match &self.status {
            ExperimentStatus::Running => None,
            ExperimentStatus::Stopped { at, .. } => Some(*at),
        }
    }
}

/// Keeps experiment records (spec 3.5 §7.12): the runner writes one after every look and the
/// generator reads which experiments are running.
#[async_trait]
pub trait ExperimentStore: Send + Sync {
    /// Stores `record`, replacing the one with its site and id.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn put(&self, record: &ExperimentRecord) -> Result<()>;

    /// The record of experiment `id` on `site`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn get(&self, site: Uuid, id: &str) -> Result<Option<ExperimentRecord>>;

    /// Every record of `site`, by id.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn list(&self, site: Uuid) -> Result<Vec<ExperimentRecord>>;
}

/// Experiment records in process memory, for tests and single-process sites.
#[derive(Debug, Default)]
pub struct InMemoryExperimentStore {
    records: RwLock<BTreeMap<(Uuid, String), ExperimentRecord>>,
}

impl InMemoryExperimentStore {
    /// No records.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ExperimentStore for InMemoryExperimentStore {
    async fn put(&self, record: &ExperimentRecord) -> Result<()> {
        self.records.write().await.insert((record.site, record.plan.id.clone()), record.clone());
        Ok(())
    }

    async fn get(&self, site: Uuid, id: &str) -> Result<Option<ExperimentRecord>> {
        Ok(self.records.read().await.get(&(site, id.to_string())).cloned())
    }

    async fn list(&self, site: Uuid) -> Result<Vec<ExperimentRecord>> {
        Ok(self
            .records
            .read()
            .await
            .iter()
            .filter(|((owner, _), _)| *owner == site)
            .map(|(_, record)| record.clone())
            .collect())
    }
}

/// Experiment records as JSON files, `{dir}/{site}/{id}.json`, each replaced atomically by a
/// write to a temporary file and a rename: a site's data directory keeps its experiments
/// across restarts.
#[derive(Debug, Clone)]
pub struct FileExperimentStore {
    dir: PathBuf,
}

impl FileExperimentStore {
    /// A store under `dir`, created on the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn site_dir(&self, site: Uuid) -> PathBuf {
        self.dir.join(site.to_string())
    }
}

fn io_error(path: &std::path::Path, error: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "website.experiment.store",
        format!(
            "the experiment store at '{}' failed: {error}; check the directory exists and is \
             writable",
            path.display()
        ),
    )
}

#[async_trait]
impl ExperimentStore for FileExperimentStore {
    async fn put(&self, record: &ExperimentRecord) -> Result<()> {
        // Ids are validated plans' ids: lowercase letters, digits and '-', so a safe name.
        record.plan.validate()?;
        let dir = self.site_dir(record.site);
        tokio::fs::create_dir_all(&dir).await.map_err(|e| io_error(&dir, e))?;
        let path = dir.join(format!("{}.json", record.plan.id));
        let temp = dir.join(format!(".{}.json.tmp", record.plan.id));
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| io_error(&path, e))?;
        tokio::fs::write(&temp, bytes).await.map_err(|e| io_error(&temp, e))?;
        tokio::fs::rename(&temp, &path).await.map_err(|e| io_error(&path, e))
    }

    async fn get(&self, site: Uuid, id: &str) -> Result<Option<ExperimentRecord>> {
        if !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return Ok(None);
        }
        let path = self.site_dir(site).join(format!("{id}.json"));
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| io_error(&path, e)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(&path, error)),
        }
    }

    async fn list(&self, site: Uuid) -> Result<Vec<ExperimentRecord>> {
        let dir = self.site_dir(site);
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(&dir, error)),
        };
        let mut records = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| io_error(&dir, e))? {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json").filter(|id| !id.starts_with('.'))
                && let Some(record) = self.get(site, id).await?
            {
                records.push(record);
            }
        }
        records.sort_by(|a, b| a.plan.id.cmp(&b.plan.id));
        Ok(records)
    }
}
