//! Where online evaluation results are kept, and their summary.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::sampling::RunOutcome;
use crate::conversation_scorer::ConversationMetrics;
use crate::error::{EvalError, Result};
use crate::report::Failure;
use crate::trace_analyzer::TraceAnalysis;

/// The verdict online evaluation reached on one sampled run.
///
/// The record identifies the run by its session and invocation ids and holds scores,
/// failures and the judges' explanations, never the run's messages; an explanation may
/// paraphrase them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OnlineEvalRecord {
    /// Unique id of this evaluation.
    pub id: String,
    /// When the run was evaluated.
    pub evaluated_at: chrono::DateTime<chrono::Utc>,
    /// When the run finished.
    pub finished_at: chrono::DateTime<chrono::Utc>,
    /// The runner's application name.
    pub app_name: String,
    /// The session the run belongs to.
    pub session_id: String,
    /// The run's invocation id.
    pub invocation_id: String,
    /// The agent the run started.
    pub agent_name: String,
    /// How the run ended.
    pub outcome: RunOutcome,
    /// `true` when no criterion failed.
    pub passed: bool,
    /// Score of each criterion, by name.
    pub scores: BTreeMap<String, f64>,
    /// The criteria that failed, with the judges' explanations.
    pub failures: Vec<Failure>,
    /// The run's trajectory analysis, when trajectory efficiency is a criterion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_analysis: Option<TraceAnalysis>,
    /// The session's conversation metrics, when thread-level criteria are configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ConversationMetrics>,
    /// Judge model calls this evaluation made.
    pub judge_model_calls: u64,
}

/// Which records [`OnlineEvalStore::list`] returns, newest first.
///
/// # Example
///
/// ```
/// use adk_eval::online::OnlineEvalQuery;
///
/// let query = OnlineEvalQuery::default().agent("support").failed_only().limit(50);
/// assert!(query.failed_only);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OnlineEvalQuery {
    /// Only records of this agent.
    pub agent: Option<String>,
    /// Only records evaluated at or after this time.
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// Only records that failed.
    pub failed_only: bool,
    /// At most this many records.
    pub limit: Option<usize>,
}

impl OnlineEvalQuery {
    /// Only records of `agent`.
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Only records evaluated at or after `since`.
    pub fn since(mut self, since: chrono::DateTime<chrono::Utc>) -> Self {
        self.since = Some(since);
        self
    }

    /// Only records that failed.
    pub fn failed_only(mut self) -> Self {
        self.failed_only = true;
        self
    }

    /// At most `limit` records.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    fn matches(&self, record: &OnlineEvalRecord) -> bool {
        self.agent.as_ref().is_none_or(|agent| &record.agent_name == agent)
            && self.since.is_none_or(|since| record.evaluated_at >= since)
            && (!self.failed_only || !record.passed)
    }

    /// Filters, orders newest first, and truncates `records`.
    fn select(&self, records: impl Iterator<Item = OnlineEvalRecord>) -> Vec<OnlineEvalRecord> {
        let mut selected: Vec<OnlineEvalRecord> =
            records.filter(|record| self.matches(record)).collect();
        selected.sort_by(|a, b| b.evaluated_at.cmp(&a.evaluated_at).then_with(|| b.id.cmp(&a.id)));
        if let Some(limit) = self.limit {
            selected.truncate(limit);
        }
        selected
    }
}

/// Where online evaluation records are kept.
///
/// Implementations append records and list them newest first; the shipped backends are
/// [`InMemoryOnlineEvalStore`] and [`JsonlOnlineEvalStore`]. A self-improvement loop reads
/// failed records with `list(&OnlineEvalQuery::default().failed_only())`.
#[async_trait]
pub trait OnlineEvalStore: Send + Sync {
    /// Appends one record.
    ///
    /// # Errors
    ///
    /// Returns an error when the record cannot be written.
    async fn append(&self, record: &OnlineEvalRecord) -> Result<()>;

    /// Lists the records `query` selects, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error when the records cannot be read.
    async fn list(&self, query: &OnlineEvalQuery) -> Result<Vec<OnlineEvalRecord>>;
}

/// Keeps the newest `capacity` records in memory.
///
/// # Example
///
/// ```
/// use adk_eval::online::{InMemoryOnlineEvalStore, OnlineEvalQuery, OnlineEvalStore};
///
/// async fn latest_failures(store: &InMemoryOnlineEvalStore) -> adk_eval::Result<usize> {
///     Ok(store.list(&OnlineEvalQuery::default().failed_only().limit(10)).await?.len())
/// }
/// let store = InMemoryOnlineEvalStore::new(1_000);
/// ```
#[derive(Debug)]
pub struct InMemoryOnlineEvalStore {
    capacity: usize,
    records: Mutex<VecDeque<OnlineEvalRecord>>,
}

impl InMemoryOnlineEvalStore {
    /// Keeps at most `capacity` records (at least one), dropping the oldest.
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), records: Mutex::new(VecDeque::new()) }
    }
}

#[async_trait]
impl OnlineEvalStore for InMemoryOnlineEvalStore {
    async fn append(&self, record: &OnlineEvalRecord) -> Result<()> {
        let mut records = self.records.lock().await;
        if records.len() == self.capacity {
            records.pop_front();
        }
        records.push_back(record.clone());
        Ok(())
    }

    async fn list(&self, query: &OnlineEvalQuery) -> Result<Vec<OnlineEvalRecord>> {
        let records = self.records.lock().await;
        Ok(query.select(records.iter().cloned()))
    }
}

/// Appends records to a JSON Lines file, one record per line.
///
/// Every line is a complete [`OnlineEvalRecord`], so the file can be shipped to a log
/// pipeline or read back with [`list`](OnlineEvalStore::list), which reads the whole file.
#[derive(Debug)]
pub struct JsonlOnlineEvalStore {
    path: PathBuf,
    write: Mutex<()>,
}

impl JsonlOnlineEvalStore {
    /// Appends to the file at `path`, creating it on the first record.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), write: Mutex::new(()) }
    }
}

#[async_trait]
impl OnlineEvalStore for JsonlOnlineEvalStore {
    async fn append(&self, record: &OnlineEvalRecord) -> Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let _guard = self.write.lock().await;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| store_error(&self.path, "open", &e))?;
        file.write_all(line.as_bytes()).await.map_err(|e| store_error(&self.path, "write", &e))?;
        file.flush().await.map_err(|e| store_error(&self.path, "write", &e))
    }

    async fn list(&self, query: &OnlineEvalQuery) -> Result<Vec<OnlineEvalRecord>> {
        let contents = match tokio::fs::read_to_string(&self.path).await {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(store_error(&self.path, "read", &e)),
        };
        let records = contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_str::<OnlineEvalRecord>(line).map_err(|e| {
                    EvalError::ParseError(format!(
                        "line {} of {} is not an online evaluation record: {e}",
                        index + 1,
                        self.path.display()
                    ))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(query.select(records.into_iter()))
    }
}

fn store_error(path: &std::path::Path, action: &str, error: &std::io::Error) -> EvalError {
    EvalError::ExecutionError(format!(
        "cannot {action} the online evaluation store {}: {error}",
        path.display()
    ))
}

/// One criterion across a set of online evaluation records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CriterionSummary {
    /// Records that scored the criterion.
    pub evaluated: usize,
    /// Records in which it failed.
    pub failed: usize,
    /// Mean score.
    pub mean_score: f64,
}

/// Pass rate and per-criterion results over a set of online evaluation records — the
/// numbers an operator dashboard or a self-improvement loop watches.
///
/// # Example
///
/// ```
/// use adk_eval::online::OnlineEvalSummary;
///
/// let summary = OnlineEvalSummary::from_records(&[]);
/// assert_eq!((summary.evaluated, summary.pass_rate), (0, 0.0));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OnlineEvalSummary {
    /// Records summarised.
    pub evaluated: usize,
    /// Records that passed.
    pub passed: usize,
    /// `passed / evaluated`, or 0.0 for no records.
    pub pass_rate: f64,
    /// Results per criterion, by name.
    pub criteria: BTreeMap<String, CriterionSummary>,
}

impl OnlineEvalSummary {
    /// Summarises `records`.
    pub fn from_records(records: &[OnlineEvalRecord]) -> Self {
        let passed = records.iter().filter(|record| record.passed).count();
        let mut sums: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
        for record in records {
            for (criterion, score) in &record.scores {
                let entry = sums.entry(criterion.clone()).or_default();
                entry.0 += 1;
                entry.2 += score;
            }
            let mut failed: Vec<&str> =
                record.failures.iter().map(|failure| failure.criterion.as_str()).collect();
            failed.sort_unstable();
            failed.dedup();
            for criterion in failed {
                sums.entry(criterion.to_string()).or_default().1 += 1;
            }
        }
        Self {
            evaluated: records.len(),
            passed,
            pass_rate: if records.is_empty() { 0.0 } else { passed as f64 / records.len() as f64 },
            criteria: sums
                .into_iter()
                .map(|(criterion, (evaluated, failed, total))| {
                    let mean_score = if evaluated == 0 { 0.0 } else { total / evaluated as f64 };
                    (criterion, CriterionSummary { evaluated, failed, mean_score })
                })
                .collect(),
        }
    }
}
