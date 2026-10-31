//! Learning (spec 3.5 §7.15, §7.20 "Playbook learning", §7.23, A-I13): the week's outcomes
//! become proposals to revise playbooks, which land only as the owner's decisions.
//!
//! The `playbook-learning` job runs with the weekly review ([`LEARNING_WORKFLOW`] on
//! [`LEARNING_CRON`]): [`PlaybookLearning::learn`] composes the review, reads the
//! experiments, and drafts one proposal per outcome that carries enough evidence. Each
//! proposal names the playbook, the change as a diff of its text, the evidence with links to
//! the reviews, and the expected effect.
//!
//! | Outcome | Enough evidence | Playbook | The change |
//! |---|---|---|---|
//! | An experiment | Stopped by the sequential test's decision with its fixed error budget (§7.12): the variant adopted, or the control kept because the variant is worse, with the primary metric ready | The playbook of the experiment's job | "Prefer" or "Avoid" what it tested |
//! | A KPI | A baseline and the owner's target (A-I14), and the means of [`KPI_MISS_WEEKS`] weekly reviews in a row short of the target in its direction | The playbook [`kpi_playbook`] names for its metric | Give the KPI priority |
//!
//! An experiment stopped by a guardrail or at its sample limit, a running one, and a KPI in
//! its measure-only period or missing its target for one week propose nothing. An experiment
//! proposes once; a KPI at most once in [`KPI_PROPOSAL_INTERVAL_DAYS`].
//!
//! | The learning job's level | A proposal |
//! |---|---|
//! | Observe | Reported only: no card (§7.11, "output goes to reports") |
//! | Propose and every level above | The owner's decision card ([`PLAYBOOK_REVISION`]): accept, edit or decline |
//!
//! A revision is applied only by the owner's decision, at every level (§7.15, A-I13): an
//! accepted or edited revision becomes the playbook's next version in [`PlaybookVersions`],
//! written into the workspace of every agent that runs the playbook; a declined or expired
//! one changes nothing. Each decision counts on the ladder for the learning job as an
//! approval, an edit or a rejection (§7.11).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::schedule::MissedFirePolicy;
use adk_core::{
    AdkError, ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, Clock, NewRun, Result, SystemClock,
    ToolConfirmationRequest,
};
use adk_eval::experiments::{Decision, MetricKind, StopReason};
use adk_harness::website::SiteRef;
use adk_kernel::QueueClass;
use adk_kernel::charter::{AutonomyLadder, CharterStore, DecisionOutcome, JobDecision, KpiPhase};
use adk_kernel::wake::{Trigger, Wake};
use awp_types::{AutonomyLevel, Direction, Kpi};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::experiments::{ExperimentRecord, ExperimentRunner, ExperimentStatus};
use crate::review::{KpiLine, ReviewComposer, ReviewInputs, WeeklyReview};
use crate::team::SiteTeam;

/// The job that learns (spec 3.5 §7.20).
pub const LEARNING_JOB: &str = "playbook-learning";
/// The workflow the weekly learning run executes in the site's kernel.
pub const LEARNING_WORKFLOW: &str = "website.steward.learning";
/// When the learning run fires: Mondays at 07:30 UTC, after the weekly review at 07:00.
pub const LEARNING_CRON: &str = "0 30 7 * * MON";
/// The tool a proposal's decision card names.
pub const PLAYBOOK_REVISION: &str = "website_playbook_revision";
/// The web home's weekly review page, which the evidence links to (`adk-surface`'s
/// `website` views).
pub const REVIEW_PATH: &str = "/surface/website/review";
/// The weekly reviews in a row a KPI must miss its target in before it proposes.
pub const KPI_MISS_WEEKS: usize = 2;
/// The days a KPI waits before it proposes again.
pub const KPI_PROPOSAL_INTERVAL_DAYS: i64 = 28;
/// The heading learned lines are kept under.
const LEARNED: &str = "## Learned";

fn config(error: impl std::fmt::Display) -> AdkError {
    AdkError::config(error.to_string())
}

/// The Monday of `day`'s week.
fn monday(day: NaiveDate) -> NaiveDate {
    day - chrono::Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// Whether one of `id`'s agents runs on the site.
fn runs(id: &str, team: &SiteTeam) -> bool {
    crate::playbooks::playbook(id).is_some_and(|p| p.agents.iter().any(|role| team.runs(*role)))
}

/// The playbook a KPI's metric is moved by: the job its metric's first segment names, else
/// the Steward's `goals`, and `goals` when no agent of the site runs the playbook.
///
/// | Metric | Playbook |
/// |---|---|
/// | `leads.*`, `events.*` | `capture` |
/// | `newsletter.*`, `subscriptions.*`, `donations.*`, `volunteers.*` | `nurture` |
/// | `orders.*`, `payments.*`, `tickets.*`, `bookings.*`, `services.*` | `sell` |
/// | `content.*`, `catalog.*`, `notices.*`, `corrections.*` | `freshness` |
/// | `visits.*`, `search.*` | `seo` |
/// | `pages.*` | `aeo` |
/// | `agents.*` | `agent-search` |
/// | `conversations.*`, `a11y.*` | `presentations` |
/// | `reviews.*` | `review-replies` |
/// | `email.*` | `email` |
/// | `spend.*`, `profit`, `revenue` | `cost-and-return` |
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::kinds::template;
/// use adk_website::learning::kpi_playbook;
/// use adk_website::team::SiteTeam;
/// use awp_types::SiteKind;
///
/// let site = SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example");
/// let business = SiteTeam::new(site.clone(), &template(SiteKind::Business));
/// assert_eq!(kpi_playbook("newsletter.signups", &business), "nurture");
/// let personal = SiteTeam::new(site, &template(SiteKind::Personal));
/// assert_eq!(kpi_playbook("orders.count", &personal), "goals", "no Revenue on a personal site");
/// ```
pub fn kpi_playbook(metric: &str, team: &SiteTeam) -> &'static str {
    let id = match metric.split('.').next().unwrap_or_default() {
        "leads" | "events" => "capture",
        "newsletter" | "subscriptions" | "donations" | "volunteers" => "nurture",
        "orders" | "payments" | "tickets" | "bookings" | "services" => "sell",
        "content" | "catalog" | "notices" | "corrections" => "freshness",
        "visits" | "search" => "seo",
        "pages" => "aeo",
        "agents" => "agent-search",
        "conversations" | "a11y" => "presentations",
        "reviews" => "review-replies",
        "email" => "email",
        "spend" | "profit" | "revenue" => "cost-and-return",
        _ => "goals",
    };
    if runs(id, team) { id } else { "goals" }
}

/// The playbook of `job`, else `goals` when the job has none the site runs.
fn job_playbook(job: &str, team: &SiteTeam) -> &'static str {
    crate::jobs::JOBS
        .iter()
        .find(|spec| spec.id == job)
        .map(|spec| spec.playbook)
        .filter(|id| runs(id, team))
        .unwrap_or("goals")
}

/// `text` with `line` under its `## Learned` heading, which is added at the end when the
/// playbook has none.
///
/// # Example
///
/// ```rust
/// use adk_website::learning::revise;
///
/// let once = revise("# SEO\n\nWrite titles.\n", "- Prefer short titles.");
/// assert_eq!(once, "# SEO\n\nWrite titles.\n\n## Learned\n\n- Prefer short titles.\n");
/// let twice = revise(&once, "- Avoid questions in titles.");
/// assert!(twice.ends_with("- Prefer short titles.\n- Avoid questions in titles.\n"));
/// ```
pub fn revise(text: &str, line: &str) -> String {
    let body = text.trim_end();
    match body.find(&format!("\n{LEARNED}\n")) {
        Some(_) => format!("{body}\n{line}\n"),
        None => format!("{body}\n\n{LEARNED}\n\n{line}\n"),
    }
}

/// The unified diff of `before` to `after` for playbook `id`.
fn diff(id: &str, before: &str, after: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{id}.md"), &format!("b/{id}.md"))
        .to_string()
}

/// What kind of outcome a proposal learns from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// An experiment's decision (spec 3.5 §7.12).
    Experiment,
    /// A KPI against the owner's target (spec 3.5 §7.11).
    Kpi,
}

/// The evidence behind a proposal, with a link to the report that holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// The kind of outcome.
    pub kind: OutcomeKind,
    /// The outcome in one line.
    pub summary: String,
    /// The weekly review that reports it, on the web home.
    pub link: String,
}

/// Where a proposal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    /// Reported only: the learning job is at Observe.
    Reported,
    /// Waiting for the owner's decision card.
    Proposed,
    /// Accepted, as proposed or edited: the playbook's next version.
    Accepted,
    /// Declined, or the card expired: the playbook is unchanged.
    Declined,
}

/// A proposal to revise one playbook (spec 3.5 §7.15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookProposal {
    /// The proposal's id, also its card's.
    pub id: String,
    /// The site.
    pub site: Uuid,
    /// The outcome it learns from: `experiment:{id}` or `kpi:{id}`.
    pub key: String,
    /// The playbook.
    pub playbook: String,
    /// The version it revises.
    pub base_version: u32,
    /// The line it adds under `## Learned`.
    pub addition: String,
    /// The playbook's text after the change.
    pub text: String,
    /// The change, as a unified diff of the playbook's text.
    pub diff: String,
    /// The evidence.
    pub evidence: Vec<Evidence>,
    /// What the change is expected to do.
    pub expected_effect: String,
    /// Where it stands.
    pub state: ProposalState,
    /// The owner's decision card, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// The version accepting it made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Whether the owner edited it before accepting.
    #[serde(default)]
    pub edited: bool,
    /// Who decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// When it was drafted.
    pub created_at: DateTime<Utc>,
    /// When the owner decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

/// One version of a playbook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybookVersion {
    /// The playbook.
    pub playbook: String,
    /// Its version: 1 is the text installed in the agents' workspaces.
    pub version: u32,
    /// The text.
    pub text: String,
    /// The proposal that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// The person who accepted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_by: Option<String>,
    /// When it was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
}

/// A site's playbooks with their versions (spec 3.5 §7.15): version 1 is the text in the
/// agents' workspaces (a developer's playbook, else the built-in one); each accepted revision
/// is the next, kept in `{dir}/{playbook}.json` and written to
/// `{workspaces}/{agent}/.skills/{playbook}.md` for every agent that runs it.
///
/// # Example
///
/// ```rust
/// use adk_website::learning::PlaybookVersions;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let dir = tempfile::tempdir().unwrap();
/// let versions = PlaybookVersions::new(dir.path().join("playbooks"), dir.path().join("agents"));
/// let seo = versions.current("seo").await?;
/// assert_eq!(seo.version, 1);
/// let next = versions.accept("seo", "# SEO\n", "p-1", "amina", chrono::Utc::now()).await?;
/// assert_eq!(next.version, 2);
/// assert_eq!(std::fs::read_to_string(dir.path().join("agents/growth/.skills/seo.md")).unwrap(), "# SEO\n");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PlaybookVersions {
    dir: PathBuf,
    workspaces: PathBuf,
}

impl PlaybookVersions {
    /// Versions under `dir`, for the agents' workspaces under `workspaces`.
    pub fn new(dir: impl Into<PathBuf>, workspaces: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), workspaces: workspaces.into() }
    }

    fn known(id: &str) -> Result<&'static crate::playbooks::Playbook> {
        crate::playbooks::playbook(id)
            .ok_or_else(|| config(format!("`{id}` is not one of the site's playbooks")))
    }

    /// The accepted revisions of `id`, oldest first; none until one is accepted.
    ///
    /// # Errors
    ///
    /// An unknown playbook, or a file that does not read.
    pub async fn history(&self, id: &str) -> Result<Vec<PlaybookVersion>> {
        Self::known(id)?;
        let path = self.dir.join(format!("{id}.json"));
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(config),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(config(format!("{}: {error}", path.display()))),
        }
    }

    /// The version of `id` in force.
    ///
    /// # Errors
    ///
    /// As [`history`](Self::history).
    pub async fn current(&self, id: &str) -> Result<PlaybookVersion> {
        let playbook = Self::known(id)?;
        if let Some(latest) = self.history(id).await?.pop() {
            return Ok(latest);
        }
        let mut text = playbook.text.to_string();
        for role in playbook.agents {
            let installed =
                self.workspaces.join(role.as_str()).join(".skills").join(format!("{id}.md"));
            if let Ok(found) = tokio::fs::read_to_string(&installed).await {
                text = found;
                break;
            }
        }
        Ok(PlaybookVersion {
            playbook: id.to_string(),
            version: 1,
            text,
            proposal: None,
            accepted_by: None,
            at: None,
        })
    }

    /// Makes `text` the next version of `id`, accepted by `by` from `proposal`, and writes it
    /// into the workspace of every agent that runs it.
    ///
    /// # Errors
    ///
    /// As [`history`](Self::history), or a file that does not write.
    pub async fn accept(
        &self,
        id: &str,
        text: &str,
        proposal: &str,
        by: &str,
        at: DateTime<Utc>,
    ) -> Result<PlaybookVersion> {
        let playbook = Self::known(id)?;
        let current = self.current(id).await?;
        let mut history = self.history(id).await?;
        let version = PlaybookVersion {
            playbook: id.to_string(),
            version: current.version + 1,
            text: text.to_string(),
            proposal: Some(proposal.to_string()),
            accepted_by: Some(by.to_string()),
            at: Some(at),
        };
        history.push(version.clone());
        write_json(&self.dir.join(format!("{id}.json")), &history).await?;
        for role in playbook.agents {
            let skills = self.workspaces.join(role.as_str()).join(".skills");
            write_atomic(&skills.join(format!("{id}.md")), text.as_bytes()).await?;
        }
        Ok(version)
    }
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(config)?;
    }
    let temporary = path.with_extension("tmp");
    tokio::fs::write(&temporary, bytes).await.map_err(config)?;
    tokio::fs::rename(&temporary, path).await.map_err(config)
}

async fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(value).map_err(config)?).await
}

/// What one outcome teaches, before it is a proposal.
struct Lesson {
    key: String,
    once: bool,
    week: NaiveDate,
    playbook: &'static str,
    line: String,
    evidence: Evidence,
    expected_effect: String,
}

/// The lesson of a decided experiment; `None` for one that is running, stopped by a
/// guardrail or at its sample limit, or decided before its primary metric was ready.
fn experiment_lesson(record: &ExperimentRecord, team: &SiteTeam) -> Option<Lesson> {
    let ExperimentStatus::Stopped { decision, at, .. } = &record.status else { return None };
    let report = record.latest.as_ref().filter(|report| report.primary.ready)?;
    let adopted = match decision {
        Decision::AdoptVariant => true,
        Decision::KeepControl { reason: StopReason::VariantWorse } => false,
        _ => return None,
    };
    let plan = &record.plan;
    let primary = &report.primary;
    let value = |v: f64| match primary.kind {
        MetricKind::Proportion => format!("{:.1}%", v * 100.0),
        _ => format!("{v:.2}"),
    };
    let (control, variant) = (value(primary.effect.control), value(primary.effect.variant));
    let lift = primary
        .effect
        .relative_lift
        .map(|lift| format!(" ({:+.1}% relative)", lift * 100.0))
        .unwrap_or_default();
    let what = if plan.hypothesis.is_empty() {
        format!("the variant of {}", plan.page)
    } else {
        plan.hypothesis.trim_end_matches('.').to_string()
    };
    let (verb, moved, expected_effect) = if adopted {
        (
            "Prefer",
            "improved",
            format!(
                "Pages this playbook drafts move {} from about {control} towards {variant}, as \
                 experiment `{}` measured",
                primary.metric, plan.id
            ),
        )
    } else {
        (
            "Avoid",
            "worsened",
            format!(
                "Pages this playbook drafts keep {} near {control} instead of the {variant} the \
                 variant of experiment `{}` reached",
                primary.metric, plan.id
            ),
        )
    };
    let line = format!(
        "- {verb} what experiment `{}` tested on {}: {what}. It {moved} {} from {control} to \
         {variant}{lift}.",
        plan.id, plan.page, primary.metric
    );
    let week = monday(at.date_naive());
    Some(Lesson {
        key: format!("experiment:{}", plan.id),
        once: true,
        week,
        playbook: job_playbook(&plan.job, team),
        line,
        evidence: Evidence {
            kind: OutcomeKind::Experiment,
            summary: format!(
                "Experiment `{}` on {} stopped on {} after {} looks: {decision}",
                plan.id,
                plan.page,
                at.date_naive(),
                report.look
            ),
            link: format!("{REVIEW_PATH}?week={week}#experiments"),
        },
        expected_effect,
    })
}

/// The lesson of a KPI short of the owner's target in [`KPI_MISS_WEEKS`] weekly reviews in a
/// row; `None` otherwise.
fn kpi_lesson(line: &KpiLine, kpi: &Kpi, week_start: NaiveDate, team: &SiteTeam) -> Option<Lesson> {
    let review = &line.review;
    if !matches!(review.phase, KpiPhase::Tracking) {
        return None;
    }
    let (baseline, target) = (review.baseline?, review.target?);
    let (week, previous) = (review.week?, review.previous_week?);
    let higher_is_better = match kpi.direction {
        Direction::Up => true,
        Direction::Down => false,
        _ => return None,
    };
    let short = |value: f64| if higher_is_better { value < target } else { value > target };
    if !(short(previous) && short(week)) {
        return None;
    }
    let name = &review.name;
    let previous_week = week_start - chrono::Duration::days(7);
    Some(Lesson {
        key: format!("kpi:{}", review.kpi),
        once: false,
        week: week_start,
        playbook: kpi_playbook(&kpi.metric.0, team),
        line: format!(
            "- {name} missed its target of {target:.2} in {KPI_MISS_WEEKS} weekly reviews in a \
             row ({previous:.2}, then {week:.2}): put the work that moves it first in each run, \
             and say in the run's report what was done for it."
        ),
        evidence: Evidence {
            kind: OutcomeKind::Kpi,
            summary: format!(
                "{name}: {previous:.2} in the week of {previous_week} and {week:.2} in the week \
                 of {week_start}, against a target of {target:.2} (baseline {baseline:.2})"
            ),
            link: format!("{REVIEW_PATH}?week={week_start}#kpis"),
        },
        expected_effect: format!("{name} moves back towards its target of {target:.2}"),
    })
}

/// What [`PlaybookLearning::open`] builds the learning job from.
pub struct LearningParts {
    /// The site.
    pub site: SiteRef,
    /// The site's team, which says whose playbooks run.
    pub team: SiteTeam,
    /// The person whose decision cards decide proposals.
    pub owner: String,
    /// The weekly review it reads the KPIs from.
    pub composer: ReviewComposer,
    /// The experiments it reads the decisions from.
    pub experiments: Arc<ExperimentRunner>,
    /// The charters, for each KPI's metric and direction.
    pub charters: Arc<dyn CharterStore>,
    /// The ladder: the learning job's level, and the owner's decisions on its proposals.
    pub ladder: AutonomyLadder,
    /// Where decision cards go: the kernel's approval service.
    pub approvals: Arc<dyn ApprovalService>,
    /// The project's `data/`: proposals in `learning/proposals.json`, versions in
    /// `playbooks/`, the agents' workspaces in `agents/`.
    pub data: PathBuf,
}

/// The `playbook-learning` job's code (spec 3.5 §7.15, §7.20): it reads the outcomes, drafts
/// the proposals and applies the owner's decisions on them.
pub struct PlaybookLearning {
    parts: LearningParts,
    versions: PlaybookVersions,
    journal: PathBuf,
    proposals: Mutex<Vec<PlaybookProposal>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for PlaybookLearning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybookLearning").field("site", &self.parts.site).finish_non_exhaustive()
    }
}

impl PlaybookLearning {
    /// The learning job over `parts`, with the proposals a restart left.
    ///
    /// # Errors
    ///
    /// A proposals file that does not read.
    pub fn open(parts: LearningParts) -> Result<Self> {
        let journal = parts.data.join("learning").join("proposals.json");
        let proposals = match std::fs::read(&journal) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(config)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(config(format!("{}: {error}", journal.display()))),
        };
        let versions =
            PlaybookVersions::new(parts.data.join("playbooks"), parts.data.join("agents"));
        Ok(Self {
            parts,
            versions,
            journal,
            proposals: Mutex::new(proposals),
            clock: Arc::new(SystemClock),
        })
    }

    /// Reads the time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The playbooks and their versions.
    pub fn versions(&self) -> &PlaybookVersions {
        &self.versions
    }

    /// Every proposal, oldest first.
    pub async fn proposals(&self) -> Vec<PlaybookProposal> {
        self.proposals.lock().await.clone()
    }

    /// The learning run of the week starting `week_start` (a Monday): composes its review,
    /// reads every experiment, and drafts the proposals ([`propose`](Self::propose)).
    ///
    /// # Errors
    ///
    /// A store's error, or the approval service's.
    pub async fn learn(&self, week_start: NaiveDate) -> Result<Vec<PlaybookProposal>> {
        let review = self.parts.composer.compose(week_start, ReviewInputs::default()).await?;
        let experiments = self.parts.experiments.list().await?;
        self.propose(&review, &experiments).await
    }

    /// Drafts a proposal for every outcome in `review` and `experiments` with enough evidence
    /// that has not proposed before, and cards each for the owner unless the learning job is
    /// at Observe. Returns the new proposals.
    ///
    /// # Errors
    ///
    /// A store's error, or the approval service's.
    pub async fn propose(
        &self,
        review: &WeeklyReview,
        experiments: &[ExperimentRecord],
    ) -> Result<Vec<PlaybookProposal>> {
        let team = &self.parts.team;
        let mut lessons: Vec<Lesson> =
            experiments.iter().filter_map(|record| experiment_lesson(record, team)).collect();
        let site = self.parts.site.id;
        if let Some(current) = self.parts.charters.current(site).await.map_err(config)? {
            for line in &review.kpis {
                let kpi = current.charter.kpis.iter().find(|kpi| kpi.id == line.review.kpi);
                if let Some(lesson) =
                    kpi.and_then(|kpi| kpi_lesson(line, kpi, review.week_start, team))
                {
                    lessons.push(lesson);
                }
            }
        }
        let level = self
            .parts
            .ladder
            .level(site, LEARNING_JOB)
            .await
            .map_err(config)?
            .unwrap_or(AutonomyLevel::Propose);
        let now = self.clock.now();
        let interval = chrono::Duration::days(KPI_PROPOSAL_INTERVAL_DAYS);
        let mut proposals = self.proposals.lock().await;
        let mut drafted = Vec::new();
        for lesson in lessons {
            let taken = proposals
                .iter()
                .any(|p| p.key == lesson.key && (lesson.once || now - p.created_at < interval));
            if taken {
                continue;
            }
            let current = self.versions.current(lesson.playbook).await?;
            if current.text.contains(&lesson.line) {
                continue;
            }
            let text = revise(&current.text, &lesson.line);
            let id =
                format!("site-playbook-{site}-{}-{}", lesson.key.replace(':', "-"), lesson.week);
            let mut proposal = PlaybookProposal {
                id: id.clone(),
                site,
                key: lesson.key,
                playbook: lesson.playbook.to_string(),
                base_version: current.version,
                diff: diff(lesson.playbook, &current.text, &text),
                addition: lesson.line,
                text,
                evidence: vec![lesson.evidence],
                expected_effect: lesson.expected_effect,
                state: ProposalState::Reported,
                card: None,
                version: None,
                edited: false,
                decided_by: None,
                created_at: now,
                decided_at: None,
            };
            // At Observe the job's output goes to reports; from Propose up every revision is
            // the owner's decision, never applied by the job (spec 3.5 §7.11, §7.15, A-I13).
            if level != AutonomyLevel::Observe {
                self.card(&proposal).await?;
                proposal.state = ProposalState::Proposed;
                proposal.card = Some(id);
            }
            tracing::info!(website.playbook = %proposal.playbook, website.proposal = %proposal.id, website.proposal.state = ?proposal.state, "a playbook revision is proposed");
            proposals.push(proposal.clone());
            drafted.push(proposal);
        }
        if !drafted.is_empty() {
            write_json(&self.journal, &*proposals).await?;
        }
        Ok(drafted)
    }

    /// Submits `proposal`'s decision card, or finds the one submitted before a restart.
    async fn card(&self, proposal: &PlaybookProposal) -> Result<()> {
        if self.parts.approvals.get(&proposal.id).await?.is_some() {
            return Ok(());
        }
        let summary = format!(
            "Revise the {} playbook: {}. Evidence: {}. Accept, edit or decline it.",
            proposal.playbook,
            proposal.expected_effect,
            proposal.evidence.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>().join("; ")
        );
        let args = json!({
            "playbook": proposal.playbook,
            "baseVersion": proposal.base_version,
            "diff": proposal.diff,
            "text": proposal.text,
            "evidence": proposal.evidence,
            "expectedEffect": proposal.expected_effect,
        });
        let call = ToolConfirmationRequest::new(PLAYBOOK_REVISION, args);
        let owner = self.parts.owner.clone();
        let scope =
            ApprovalScope::new(self.parts.site.name.clone(), owner.clone(), "playbook-learning");
        let policy = ApprovalPolicy::new([ApproverSelector::human(owner)]);
        let request = ApprovalRequest::new(proposal.id.clone(), scope, &call, policy)
            .with_summary(summary)
            .with_risk(ApprovalRisk::Low)
            .with_allowed_actions([
                ApprovalAction::Approve,
                ApprovalAction::Edit,
                ApprovalAction::Reject,
            ]);
        self.parts.approvals.submit(request).await?;
        Ok(())
    }

    /// Applies the owner's decided cards (spec 3.5 §7.15): an approval makes the proposed
    /// text the playbook's next version, an edit makes the owner's `text` it, a refusal or an
    /// expiry changes nothing; each person's decision counts on the ladder for the learning
    /// job. Returns the proposals it decided.
    ///
    /// # Errors
    ///
    /// The approval service's error, or the versions' or the journal's.
    pub async fn step(&self) -> Result<Vec<PlaybookProposal>> {
        let mut proposals = self.proposals.lock().await;
        let mut decided = Vec::new();
        for proposal in proposals.iter_mut() {
            if proposal.state != ProposalState::Proposed {
                continue;
            }
            let Some(card) = proposal.card.clone() else { continue };
            let Some(request) = self.parts.approvals.get(&card).await? else { continue };
            let decision = request.decisions.last();
            let now = self.clock.now();
            let outcome = match request.state {
                ApprovalState::Pending | ApprovalState::Responded => continue,
                ApprovalState::Expired | ApprovalState::Cancelled => None,
                ApprovalState::Rejected => Some(DecisionOutcome::Rejected),
                ApprovalState::Approved => {
                    let edited = decision
                        .filter(|d| d.kind == ApprovalAction::Edit)
                        .and_then(|d| d.edited_args.as_ref())
                        .and_then(|args| args.get("text"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let current = self.versions.current(&proposal.playbook).await?;
                    let text = match &edited {
                        Some(text) => text.clone(),
                        None if current.version == proposal.base_version => proposal.text.clone(),
                        // Another revision was accepted first: the line goes onto it.
                        None => revise(&current.text, &proposal.addition),
                    };
                    let by = decision.map_or(self.parts.owner.as_str(), |d| d.approver.id.as_str());
                    let version = self
                        .versions
                        .accept(&proposal.playbook, &text, &proposal.id, by, now)
                        .await?;
                    proposal.version = Some(version.version);
                    proposal.edited = edited.is_some();
                    proposal.text = text;
                    Some(if proposal.edited {
                        DecisionOutcome::ApprovedWithEdits
                    } else {
                        DecisionOutcome::ApprovedUnchanged
                    })
                }
            };
            proposal.state = match outcome {
                Some(DecisionOutcome::ApprovedUnchanged | DecisionOutcome::ApprovedWithEdits) => {
                    ProposalState::Accepted
                }
                _ => ProposalState::Declined,
            };
            proposal.decided_by = decision.map(|d| d.approver.id.clone());
            proposal.decided_at = Some(decision.map_or(now, |d| d.decided_at));
            if let (Some(outcome), Some(decision)) = (outcome, decision) {
                let recorded = self
                    .parts
                    .ladder
                    .record_decision(JobDecision::new(
                        self.parts.site.id,
                        LEARNING_JOB,
                        card.clone(),
                        outcome,
                        decision.approver.clone(),
                        decision.decided_at,
                    ))
                    .await;
                if let Err(error) = recorded {
                    tracing::warn!(website.proposal = %proposal.id, error = %error, "the owner's decision on a playbook proposal is not on the ladder");
                }
            }
            tracing::info!(website.playbook = %proposal.playbook, website.proposal = %proposal.id, website.proposal.state = ?proposal.state, "the owner decided a playbook proposal");
            decided.push(proposal.clone());
        }
        if !decided.is_empty() {
            write_json(&self.journal, &*proposals).await?;
        }
        Ok(decided)
    }

    /// The standing trigger of the weekly learning run on `expr` (six fields, UTC), on the
    /// maintenance queue of app `app_name`.
    pub fn trigger(&self, app_name: &str, expr: &str) -> Trigger {
        let site = self.parts.site.id;
        let run = NewRun::new(
            QueueClass::MAINTENANCE,
            app_name,
            "website:learning",
            LEARNING_WORKFLOW,
            json!({ "site": site }),
        );
        Trigger::new(
            format!("{LEARNING_WORKFLOW}:{site}"),
            Wake::Cron { expr: expr.to_string(), missed: MissedFirePolicy::FireOnce },
            run,
        )
    }

    /// The learning run: the proposals of last week's review, named in its output.
    ///
    /// # Errors
    ///
    /// As [`learn`](Self::learn).
    pub async fn run(&self) -> Result<Value> {
        let week = monday(self.clock.now().date_naive()) - chrono::Duration::days(7);
        let drafted = self.learn(week).await?;
        Ok(json!({
            "week": week,
            "proposals": drafted.iter().map(|p| json!({
                "id": p.id,
                "playbook": p.playbook,
                "state": p.state,
            })).collect::<Vec<_>>(),
        }))
    }

    /// Applies the owner's decisions every `idle` until aborted.
    pub fn spawn(self: Arc<Self>, idle: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(error) = self.step().await {
                    tracing::warn!(error = %error, "the learning job's step failed");
                }
                tokio::time::sleep(idle).await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_a_diff_of_one_added_line() {
        let before = "# SEO\n\nWrite titles.\n";
        let after = revise(before, "- Prefer short titles.");
        let diff = diff("seo", before, &after);
        assert!(diff.starts_with("--- a/seo.md\n+++ b/seo.md\n"), "{diff}");
        let added: Vec<&str> =
            diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).collect();
        assert_eq!(added, ["+", "+## Learned", "+", "+- Prefer short titles."]);
        assert!(!diff.lines().any(|l| l.starts_with('-') && !l.starts_with("---")));
    }

    #[test]
    fn weeks_start_on_monday() {
        let thursday = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        assert_eq!(monday(thursday), NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        assert_eq!(monday(monday(thursday)), monday(thursday));
    }
}
