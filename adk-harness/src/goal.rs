//! The goal engine (spec 3.1 §7.11): loop-until goals, durable and ambient.
//!
//! A goal iterates the harness agent until the operator's `until` command exits 0 or the
//! iteration cap is reached. Its state lives in a harness-owned [`GoalStore`], mirrored to
//! `harness:goal` in session state for display (spec 3.1 §6 D-3, §9.2 I1).
//!
//! | Engine | Shape |
//! |---|---|
//! | `GoalEngine::Loop` | `LoopAgent` over the harness agent and an [`UntilCheckAgent`]; a held call resumes at the right sub-agent through `workflow_positions` |
//! | `GoalEngine::Graph` (`graph`) | a checkpointed `StateGraph`: `iterate` → `check` → (`__end__` \| `iterate`) |
//!
//! While a session's goal runs and the session is in Ask or Auto, every turn of the harness
//! runs the goal engine, so the same `Runner` serves `Harness::start_goal`, a resumed goal
//! and a `DurableWorker` claiming target [`GOAL_TARGET`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, Event, EventStream, InvocationContext,
    Llm, Result, Tool,
};
use adk_devtools::{BashTool, CommandLauncher, Workspace};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::Instrument;

use crate::audit::{HARNESS_AUDIT_KIND, HarnessAudit};
use crate::mode::HarnessMode;
use crate::run_context::InvocationToolContext;

/// Session-state key of the goal's display mirror (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOAL_STATE_KEY, "harness:goal");
/// ```
pub const GOAL_STATE_KEY: &str = "harness:goal";

/// The durable-run target of goals, for `DurableWorker::with_agent` (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOAL_TARGET, "harness.goal");
/// ```
pub const GOAL_TARGET: &str = "harness.goal";

/// A queue dedicated to durable goal runs, for [`Harness::with_goal_queue`](crate::Harness::with_goal_queue)
/// and the `DurableWorker` that serves them (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOAL_QUEUE, "harness.goal");
/// ```
pub const GOAL_QUEUE: &str = "harness.goal";

/// Name of the agent that runs a goal's `until` check.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::UNTIL_CHECK_AGENT, "until_check");
/// ```
pub const UNTIL_CHECK_AGENT: &str = "until_check";

/// The `LoopAgent`'s own ceiling; the goal's `max_iterations` stops it first.
const GOAL_LOOP_CEILING: u32 = 10_000;

/// Most bytes of the check's output the next iteration reads.
const CHECK_OUTPUT_TAIL: usize = 4 * 1024;

/// How many iterations in a row a kernel-hosted goal may end with the same check result
/// before it counts as stalled (spec 3.2 §7.3, §10 HK3); a `Graph` goal thread keeps the
/// newest `2 * GOAL_STALL_WINDOW` checkpoints.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::GOAL_STALL_WINDOW, 3);
/// ```
pub const GOAL_STALL_WINDOW: u32 = 3;

/// The `harness.goal.stalled` reason of a check that repeated the last
/// [`GOAL_STALL_WINDOW`] results.
pub(crate) const STALL_SAME_CHECK: &str = "same_check";
/// The `harness.goal.stalled` reason of an iteration that made no tool call.
pub(crate) const STALL_NO_TOOL_CALL: &str = "no_tool_call";

/// Parks a stalled goal's run at its next model-call boundary: the kernel glue's handle on
/// the kernel's processes (spec 3.2 §10 HK3).
#[async_trait]
pub(crate) trait GoalParker: Send + Sync {
    /// Suspends the durable run `run_id`; `false` when the kernel is gone.
    async fn park(&self, run_id: &str) -> Result<bool>;
}

/// What the harness knows about one start of a goal beyond its [`GoalState`]: when it
/// started, the tail of its last check's output and its recent check results.
#[derive(Clone, Debug)]
pub(crate) struct GoalTrack {
    // Read by the kernel glue's goal progress (feature `kernel`).
    #[cfg_attr(not(feature = "kernel"), allow(dead_code))]
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) last_tail: Option<String>,
    digests: VecDeque<String>,
}

/// The goals this harness runs, by [`GoalRecord::run_id`], and the parker a kernel attached
/// (spec 3.2 §7.3, §10 HK3). Stalls are watched only while a parker is attached, so a goal
/// outside a kernel runs exactly as before.
#[derive(Default)]
pub(crate) struct GoalBook {
    goals: Mutex<HashMap<String, GoalTrack>>,
    parker: StdRwLock<Option<Arc<dyn GoalParker>>>,
}

impl std::fmt::Debug for GoalBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalBook").finish_non_exhaustive()
    }
}

impl GoalBook {
    /// Notes that goal `run_id` is running, from now if it was not known.
    pub(crate) fn started(&self, run_id: &str) {
        let mut goals = self.goals.lock().unwrap_or_else(|e| e.into_inner());
        if goals.len() > 4096 {
            goals.clear();
        }
        goals.entry(run_id.to_string()).or_insert_with(|| GoalTrack {
            started_at: Utc::now(),
            last_tail: None,
            digests: VecDeque::new(),
        });
    }

    /// What is known about goal `run_id`.
    #[cfg(feature = "kernel")]
    pub(crate) fn track(&self, run_id: &str) -> Option<GoalTrack> {
        self.goals.lock().unwrap_or_else(|e| e.into_inner()).get(run_id).cloned()
    }

    /// Attaches the kernel's parker, which also turns stall detection on.
    #[cfg(feature = "kernel")]
    pub(crate) fn set_parker(&self, parker: Arc<dyn GoalParker>) {
        *self.parker.write().unwrap_or_else(|e| e.into_inner()) = Some(parker);
    }

    fn parker(&self) -> Option<Arc<dyn GoalParker>> {
        self.parker.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Records a check of goal `run_id` and returns the stall reason when, with a parker
    /// attached and the goal still running, the iteration made no tool call or the last
    /// [`GOAL_STALL_WINDOW`] checks gave the same result. A stall restarts the window.
    fn record_check(
        &self,
        run_id: &str,
        exit_code: Option<i32>,
        output: &str,
        tool_calls: Option<usize>,
        running: bool,
    ) -> Option<&'static str> {
        let watching = self.parker().is_some();
        let mut goals = self.goals.lock().unwrap_or_else(|e| e.into_inner());
        let track = goals.entry(run_id.to_string()).or_insert_with(|| GoalTrack {
            started_at: Utc::now(),
            last_tail: None,
            digests: VecDeque::new(),
        });
        track.last_tail = Some(tail(output, CHECK_OUTPUT_TAIL).to_string());
        track.digests.push_back(crate::fsutil::sha256_hex(&format!("{exit_code:?}\n{output}")));
        while track.digests.len() > GOAL_STALL_WINDOW as usize {
            track.digests.pop_front();
        }
        if !(watching && running) {
            return None;
        }
        let same = track.digests.len() == GOAL_STALL_WINDOW as usize
            && track.digests.iter().all(|digest| *digest == track.digests[0]);
        let reason = if tool_calls == Some(0) {
            Some(STALL_NO_TOOL_CALL)
        } else if same {
            Some(STALL_SAME_CHECK)
        } else {
            None
        };
        if reason.is_some() {
            track.digests.clear();
        }
        reason
    }
}

/// The iterations a goal takes unless configured otherwise, as `adk-rust goal` defaults
/// `--max-iters`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::DEFAULT_GOAL_ITERATIONS, 8);
/// ```
pub const DEFAULT_GOAL_ITERATIONS: u32 = 8;

/// How long one run of a goal's `until` command may take unless configured otherwise.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::DEFAULT_CHECK_TIMEOUT.as_secs(), 600);
/// ```
pub const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(600);

/// The most bytes of one deliverable file the grader reads; the rest is cut.
const DELIVERABLE_MAX_BYTES: usize = 64 * 1024;

/// The most bytes of the iteration's final message the grader reads.
const FINAL_MESSAGE_MAX_BYTES: usize = 16 * 1024;

/// A loop-until goal (spec 3.1 §7.11): the harness iterates until the goal's checks pass —
/// the `until` command exits 0, the rubric's grader passes every criterion, or both when
/// both are set — or `max_iterations` is reached.
///
/// # Example
///
/// ```rust
/// use adk_harness::{GoalRubric, GoalSpec};
///
/// let tests = GoalSpec::new("make the tests pass").with_until("cargo test").with_max_iterations(10);
/// assert_eq!(tests.max_iterations, 10);
///
/// let blurb = GoalSpec::new("write the launch blurb to blurb.md").with_rubric(
///     GoalRubric::new(["names the product", "states the price"]).with_deliverables(["blurb.md"]),
/// );
/// assert!(blurb.until.is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GoalSpec {
    /// What to achieve.
    pub goal: String,
    /// The operator's check command; exit 0 passes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The rubric a grader checks each iteration's deliverable against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<GoalRubric>,
    /// Most iterations.
    pub max_iterations: u32,
    /// Timeout of one run of `until`.
    pub check_timeout: Duration,
}

impl GoalSpec {
    /// A goal with no check yet, [`DEFAULT_GOAL_ITERATIONS`] iterations and a
    /// [`DEFAULT_CHECK_TIMEOUT`]; give it [`with_until`](Self::with_until),
    /// [`with_rubric`](Self::with_rubric) or both.
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            until: None,
            rubric: None,
            max_iterations: DEFAULT_GOAL_ITERATIONS,
            check_timeout: DEFAULT_CHECK_TIMEOUT,
        }
    }

    /// The check command: the goal needs it to exit 0.
    #[must_use]
    pub fn with_until(mut self, command: impl Into<String>) -> Self {
        self.until = Some(command.into());
        self
    }

    /// The rubric: the goal needs a grader to pass every criterion.
    #[must_use]
    pub fn with_rubric(mut self, rubric: GoalRubric) -> Self {
        self.rubric = Some(rubric);
        self
    }

    /// The most iterations.
    #[must_use]
    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// How long one run of `until` may take.
    #[must_use]
    pub fn with_check_timeout(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
        self
    }

    /// The goal's digest: SHA-256 of its canonical JSON, as the durable run's idempotency
    /// key uses it (`goal:<session>:<digest>`).
    ///
    /// # Example
    ///
    /// ```rust
    /// let goal = adk_harness::GoalSpec::new("g").with_until("true");
    /// assert_eq!(goal.digest().len(), 64);
    /// ```
    pub fn digest(&self) -> String {
        crate::fsutil::sha256_hex(&serde_json::to_string(self).unwrap_or_default())
    }

    /// The message that starts the goal's run.
    pub fn prompt(&self) -> String {
        let mut checks = Vec::new();
        if let Some(until) = &self.until {
            checks.push(format!("the harness runs `{until}`, which must exit 0"));
        }
        if let Some(rubric) = &self.rubric {
            checks.push(rubric.prompt_line());
        }
        let mut prompt = format!(
            "Goal: {goal}\n\nWork toward the goal. After each of your turns {checks}; the goal \
             is reached when {all}, and you get at most {max} iterations. Read the feedback \
             from the previous iteration before you continue.",
            goal = self.goal,
            checks = checks.join(", then "),
            all = if checks.len() > 1 { "both pass" } else { "it passes" },
            max = self.max_iterations,
        );
        if let Some(rubric) = self.rubric.as_ref().filter(|rubric| !rubric.hidden) {
            prompt.push_str("\n\nThe grader's criteria:");
            for (index, criterion) in rubric.criteria.iter().enumerate() {
                prompt.push_str(&format!("\n{}. {criterion}", index + 1));
            }
        }
        prompt
    }

    #[cfg(feature = "runner")]
    fn validate(&self) -> Result<()> {
        let invalid = |message: &str| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.goal.invalid",
                message.to_string(),
            )
        };
        if self.goal.trim().is_empty() {
            return Err(invalid("a goal needs a goal"));
        }
        if self.until.is_none() && self.rubric.is_none() {
            return Err(invalid("a goal needs an until command, a rubric, or both"));
        }
        if self.until.as_deref().is_some_and(|until| until.trim().is_empty()) {
            return Err(invalid("a goal's until command is empty"));
        }
        if let Some(rubric) = &self.rubric {
            rubric.validate().map_err(|message| invalid(&message))?;
        }
        if self.max_iterations == 0 {
            return Err(invalid("a goal needs at least one iteration"));
        }
        if self.check_timeout.is_zero() {
            return Err(invalid("a goal's check needs a timeout"));
        }
        Ok(())
    }
}

/// The rubric of a goal: independently gradeable criteria that a grader checks each
/// iteration's deliverable against, in a context of its own.
///
/// The grader reads the goal, the criteria, the iteration's final message and the
/// `deliverables` files of the workspace — never the conversation that produced them. It
/// grades only once the goal's `until` command (if any) has passed, and at most
/// `max_evaluations` times ([`adk_eval::DEFAULT_OUTCOME_ITERATIONS`] by default, at most
/// [`adk_eval::MAX_OUTCOME_ITERATIONS`]); a goal that reaches the cap unsatisfied is
/// `Exhausted`. A `hidden` rubric is shown only to the grader: the agent learns the failed
/// criteria from the grader's feedback.
///
/// # Example
///
/// ```rust
/// use adk_harness::GoalRubric;
///
/// let rubric = GoalRubric::from_markdown("- names the product\n- states the price")?
///     .with_deliverables(["blurb.md"])
///     .with_max_evaluations(5);
/// assert_eq!(rubric.criteria, ["names the product", "states the price"]);
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GoalRubric {
    /// The criteria, each graded pass or fail on its own.
    pub criteria: Vec<String>,
    /// Workspace-relative files the grader reads besides the final message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliverables: Vec<String>,
    /// The most grading rounds before the goal counts as exhausted.
    pub max_evaluations: u32,
    /// Shows the criteria to the grader only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl GoalRubric {
    /// A rubric of `criteria`, graded at most [`adk_eval::DEFAULT_OUTCOME_ITERATIONS`]
    /// times. Blank criteria are dropped.
    pub fn new<I, S>(criteria: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            criteria: criteria
                .into_iter()
                .map(|criterion| criterion.into().trim().to_string())
                .filter(|criterion| !criterion.is_empty())
                .collect(),
            deliverables: Vec::new(),
            max_evaluations: adk_eval::DEFAULT_OUTCOME_ITERATIONS,
            hidden: false,
        }
    }

    /// Reads the criteria from Markdown, one per list item
    /// ([`adk_eval::RubricConfig::from_markdown`]).
    ///
    /// # Errors
    ///
    /// Returns `harness.goal.invalid` when `text` holds no criterion.
    pub fn from_markdown(text: &str) -> Result<Self> {
        let rubric = adk_eval::RubricConfig::from_markdown(text);
        if rubric.rubrics.is_empty() {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.goal.invalid",
                "the rubric holds no criteria; write one list item per criterion",
            ));
        }
        Ok(Self::new(rubric.rubrics.into_iter().map(|criterion| criterion.description)))
    }

    /// The criteria the goal text lists, one per list item; a goal that lists none is its
    /// own single criterion.
    ///
    /// # Example
    ///
    /// ```rust
    /// let rubric = adk_harness::GoalRubric::from_goal("Write blurb.md:\n- under 60 words\n- names the price");
    /// assert_eq!(rubric.criteria, ["under 60 words", "names the price"]);
    /// let whole = adk_harness::GoalRubric::from_goal("Write a haiku about Rust");
    /// assert_eq!(whole.criteria, ["The deliverable achieves the goal: Write a haiku about Rust"]);
    /// ```
    pub fn from_goal(goal: &str) -> Self {
        let listed = adk_eval::RubricConfig::from_markdown(goal);
        let has_items = goal.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("- ")
                || line.starts_with("* ")
                || line.starts_with("+ ")
                || line.split_once(['.', ')']).is_some_and(|(number, rest)| {
                    !number.is_empty()
                        && number.bytes().all(|byte| byte.is_ascii_digit())
                        && rest.starts_with(' ')
                })
        });
        if has_items && !listed.rubrics.is_empty() {
            Self::new(listed.rubrics.into_iter().map(|criterion| criterion.description))
        } else {
            Self::new([format!("The deliverable achieves the goal: {}", goal.trim())])
        }
    }

    /// The workspace files the grader reads.
    #[must_use]
    pub fn with_deliverables<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deliverables = paths.into_iter().map(Into::into).collect();
        self
    }

    /// The most grading rounds, from 1 to [`adk_eval::MAX_OUTCOME_ITERATIONS`].
    #[must_use]
    pub fn with_max_evaluations(mut self, max: u32) -> Self {
        self.max_evaluations = max;
        self
    }

    /// Shows the criteria to the grader only.
    #[must_use]
    pub fn hidden(mut self, yes: bool) -> Self {
        self.hidden = yes;
        self
    }

    /// The criteria as the grader takes them.
    pub(crate) fn config(&self) -> adk_eval::RubricConfig {
        adk_eval::RubricConfig::from_criteria(&self.criteria)
    }

    /// How the goal's prompt describes this rubric.
    fn prompt_line(&self) -> String {
        let reads = if self.deliverables.is_empty() {
            "your final message".to_string()
        } else {
            format!("your final message and {}", self.deliverables.join(", "))
        };
        let what = if self.hidden {
            "criteria you are not shown; its feedback names the ones that fail".to_string()
        } else {
            format!("the {} criteria below", self.criteria.len())
        };
        format!(
            "a separate grader that reads only {reads} grades the result against {what} \
             (at most {} grading rounds)",
            self.max_evaluations
        )
    }

    #[cfg(feature = "runner")]
    fn validate(&self) -> std::result::Result<(), String> {
        if self.criteria.is_empty() {
            return Err("a goal's rubric needs at least one criterion".to_string());
        }
        let max = adk_eval::MAX_OUTCOME_ITERATIONS;
        if !(1..=max).contains(&self.max_evaluations) {
            return Err(format!(
                "a goal's rubric allows {} grading rounds; set from 1 to {max}",
                self.max_evaluations
            ));
        }
        if let Some(path) = self.deliverables.iter().find(|path| path.trim().is_empty()) {
            return Err(format!("a goal's rubric names an empty deliverable path `{path}`"));
        }
        Ok(())
    }
}

/// Where a goal stands (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::GoalStatus::Exhausted).unwrap(), "exhausted");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    /// Iterating.
    Running,
    /// The check passed.
    Done,
    /// The iteration cap was reached.
    Exhausted,
    /// Cancelled.
    Cancelled,
}

/// A goal's state, as mirrored to `harness:goal` (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust,ignore
/// if let Some(state) = harness.goal(&user, &session).await? {
///     println!("{} of {}", state.iteration, state.max_iterations);
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GoalState {
    /// What to achieve.
    pub goal: String,
    /// The check command, when the goal has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// The current iteration.
    pub iteration: u32,
    /// Most iterations.
    pub max_iterations: u32,
    /// Where it stands.
    pub status: GoalStatus,
    /// The last check's exit code.
    pub last_check: Option<i32>,
    /// Grading rounds so far, when the goal has a rubric.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub evaluations: u32,
    /// The grader's last verdict, when the goal has a rubric and was graded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_grade: Option<adk_eval::OutcomeGrade>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

impl GoalState {
    /// How the goal ended, in one line, or `None` while it runs.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{GoalRecord, GoalSpec};
    ///
    /// let record = GoalRecord::start(GoalSpec::new("g").with_until("true"));
    /// assert_eq!(record.state.outcome(), None);
    /// ```
    pub fn outcome(&self) -> Option<String> {
        let grade = self.last_grade.as_ref();
        let checks = match (&self.until, grade) {
            (Some(until), Some(_)) => format!("`{until}` and the rubric"),
            (Some(until), None) => format!("`{until}`"),
            (None, _) => "the rubric".to_string(),
        };
        match self.status {
            GoalStatus::Running => None,
            GoalStatus::Done => {
                Some(format!("goal met after {} iteration(s): {checks} passed.", self.iteration))
            }
            GoalStatus::Exhausted => {
                let failing = match (self.last_check, grade) {
                    (Some(code), _) if code != 0 && self.until.is_some() => {
                        format!(
                            "`{}` still fails (exit {code})",
                            self.until.as_deref().unwrap_or_default()
                        )
                    }
                    (None, None) if self.until.is_some() => {
                        format!("`{}` still fails", self.until.as_deref().unwrap_or_default())
                    }
                    (_, Some(grade)) => format!(
                        "the rubric is not satisfied after {} grading round(s): {} of {} \
                         criteria fail\n{}",
                        self.evaluations,
                        grade.criteria.len() - grade.passed(),
                        grade.criteria.len(),
                        grade.feedback()
                    ),
                    _ => "the checks still fail".to_string(),
                };
                Some(format!("budget exhausted after {} iteration(s); {failing}", self.iteration))
            }
            GoalStatus::Cancelled => {
                Some(format!("the goal was cancelled at iteration {}.", self.iteration))
            }
        }
    }
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// The goal loop's engine (spec 3.1 §7.11, feature `graph`).
///
/// # Example
///
/// ```rust
/// let engine = adk_harness::GoalEngine::Loop;
/// # let _ = engine;
/// ```
#[cfg(feature = "graph")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalEngine {
    /// A `LoopAgent` over the harness agent and an until-check agent.
    Loop,
    /// A checkpointed `StateGraph`: iterate → check → (done | iterate).
    Graph,
}

/// A session's goal as the [`GoalStore`] keeps it: the spec and where it stands.
///
/// # Example
///
/// ```rust
/// use adk_harness::{GoalRecord, GoalSpec, GoalStatus};
///
/// let spec = GoalSpec::new("g").with_until("true").with_max_iterations(3);
/// let record = GoalRecord::start(spec);
/// assert_eq!(record.state.status, GoalStatus::Running);
/// assert_eq!(record.state.iteration, 0);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRecord {
    /// The goal.
    pub spec: GoalSpec,
    /// Where it stands.
    pub state: GoalState,
    /// Identifies this start of the goal; the `Graph` engine's thread is keyed by it.
    pub run_id: String,
}

impl GoalRecord {
    /// A goal that starts now, at iteration 0.
    pub fn start(spec: GoalSpec) -> Self {
        let state = GoalState {
            goal: spec.goal.clone(),
            until: spec.until.clone(),
            iteration: 0,
            max_iterations: spec.max_iterations,
            status: GoalStatus::Running,
            last_check: None,
            evaluations: 0,
            last_grade: None,
            updated_at: Utc::now(),
        };
        Self { spec, state, run_id: uuid::Uuid::new_v4().to_string() }
    }
}

/// Where a harness keeps each session's goal (spec 3.1 §7.11, §9.2 I1, I36).
///
/// Keyed by (`app`, `user`, `session`). Only the harness writes it: a session-state write,
/// a client `stateDelta` or a delegate merge cannot change a goal.
///
/// # Example
///
/// ```rust
/// use adk_harness::{GoalRecord, GoalSpec, GoalStore, InMemoryGoalStore};
///
/// # futures::executor::block_on(async {
/// let store = InMemoryGoalStore::new();
/// let spec = GoalSpec::new("g").with_until("true").with_max_iterations(3);
/// store.set("app", "alice", "s1", GoalRecord::start(spec)).await?;
/// assert!(store.get("app", "alice", "s1").await?.is_some());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait GoalStore: Send + Sync {
    /// The session's goal, if one was set.
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<GoalRecord>>;
    /// Replaces the session's goal.
    async fn set(&self, app: &str, user: &str, session: &str, record: GoalRecord) -> Result<()>;
}

type ScopeKey = (String, String, String);

/// A [`GoalStore`] in process memory, the builder default.
///
/// # Example
///
/// ```rust
/// let store = adk_harness::InMemoryGoalStore::new();
/// # let _ = store;
/// ```
#[derive(Debug, Default)]
pub struct InMemoryGoalStore {
    records: RwLock<HashMap<ScopeKey, GoalRecord>>,
}

impl InMemoryGoalStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GoalStore for InMemoryGoalStore {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<GoalRecord>> {
        let key = (app.to_string(), user.to_string(), session.to_string());
        Ok(self.records.read().await.get(&key).cloned())
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: GoalRecord) -> Result<()> {
        let key = (app.to_string(), user.to_string(), session.to_string());
        self.records.write().await.insert(key, record);
        Ok(())
    }
}

/// A [`GoalStore`] in SQLite (feature `sqlite`), beside the mode and rule stores.
///
/// Call [`migrate`](Self::migrate) once before use. The table is `adk_harness_goals`.
///
/// # Example
///
/// ```rust,ignore
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
/// let store = adk_harness::SqliteGoalStore::new(pool);
/// store.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteGoalStore {
    pool: sqlx::SqlitePool,
    #[cfg(feature = "encrypted")]
    envelope: Option<crate::encrypted::StoreEnvelope>,
}

#[cfg(feature = "sqlite")]
impl SqliteGoalStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(feature = "encrypted")]
            envelope: None,
        }
    }

    /// Seals every record under `keyring` (spec 3.1 §7.12).
    #[cfg(feature = "encrypted")]
    #[must_use]
    pub fn with_keyring(mut self, keyring: crate::encrypted::StoreKeyring) -> Self {
        self.envelope = Some(crate::encrypted::StoreEnvelope::new(keyring, "goals"));
        self
    }

    /// Re-seals every record a previous key sealed with the current key, seals records
    /// written before a key was configured, and returns how many rows changed (spec 3.1
    /// §7.12). Without a keyring, nothing changes.
    ///
    /// # Errors
    ///
    /// Returns `harness.store.decrypt_failed` when a row opens under no key the keyring
    /// holds, or the database error.
    #[cfg(feature = "encrypted")]
    pub async fn rewrap(&self) -> Result<u64> {
        let Some(envelope) = &self.envelope else {
            return Ok(0);
        };
        let rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT app_name, user_id, session_id, record FROM adk_harness_goals")
                .fetch_all(&self.pool)
                .await
                .map_err(crate::fsutil::db_error)?;
        let mut changed = 0;
        for (app, user, session, text) in rows {
            if let Some(resealed) = envelope.reseal_or_seal(&[&app, &user, &session], &text)? {
                sqlx::query("UPDATE adk_harness_goals SET record = ? WHERE app_name = ? AND user_id = ? AND session_id = ?")
                    .bind(resealed)
                    .bind(&app)
                    .bind(&user)
                    .bind(&session)
                    .execute(&self.pool)
                    .await
                    .map_err(crate::fsutil::db_error)?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    #[cfg(feature = "encrypted")]
    fn encode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::encode_row(self.envelope.as_ref(), address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn encode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    #[cfg(feature = "encrypted")]
    fn decode(&self, address: &[&str], text: String) -> Result<String> {
        crate::encrypted::decode_row(self.envelope.as_ref(), "goals", address, text)
    }

    #[cfg(not(feature = "encrypted"))]
    fn decode(&self, _address: &[&str], text: String) -> Result<String> {
        Ok(text)
    }

    /// Creates the store's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_goals (\
                app_name TEXT NOT NULL, user_id TEXT NOT NULL, session_id TEXT NOT NULL, \
                record TEXT NOT NULL, PRIMARY KEY (app_name, user_id, session_id))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl GoalStore for SqliteGoalStore {
    async fn get(&self, app: &str, user: &str, session: &str) -> Result<Option<GoalRecord>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT record FROM adk_harness_goals WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        let Some((text,)) = row else {
            return Ok(None);
        };
        let text = self.decode(&[app, user, session], text)?;
        // A record that does not parse is no goal: nothing runs on it.
        Ok(serde_json::from_str(&text).ok())
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: GoalRecord) -> Result<()> {
        let text = serde_json::to_string(&record)
            .map_err(|error| crate::fsutil::serialize_error("goal record", &error))?;
        let text = self.encode(&[app, user, session], text)?;
        sqlx::query(
            "INSERT INTO adk_harness_goals (app_name, user_id, session_id, record) VALUES (?, ?, ?, ?) \
             ON CONFLICT (app_name, user_id, session_id) DO UPDATE SET record = excluded.record",
        )
        .bind(app)
        .bind(user)
        .bind(session)
        .bind(text)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

/// Runs a goal's checks after each iteration (spec 3.1 §7.11): its `until` command, then,
/// once that passes, its rubric's grader.
///
/// It checks the kill switch, runs `until` through the harness's command launcher with the
/// goal's `check_timeout`, grades the iteration's deliverable when the goal has a
/// [`GoalRubric`], updates the goal state, and sets `escalate` once every check passes
/// (Done) or at a cap (Exhausted), which stops the `LoopAgent`. The command comes from the
/// operator, not the model, so it is not a governed tool call; it is audited as
/// `harness.bash.begin`/`.end` with `goalCheck: true`, each grading round as
/// `harness.goal.grade`, and each iteration as `harness.goal.iteration` (spec 3.1 §9.3).
///
/// The check's result reaches the next iteration as a system notice
/// ([`adk_core::history::system_notice`], authored `system`), so the agent reads the command's
/// output and the grader's feedback as the harness's own notice rather than as another agent's
/// output quoted to it as data.
///
/// The grader ([`with_grader`](Self::with_grader); the harness model by default) works in a
/// context of its own: its request holds the goal, the criteria, the iteration's final
/// message and the rubric's deliverable files, never the conversation. Its call counts
/// against the run's budget and spend ledger, and its per-criterion feedback is what the
/// next iteration reads.
///
/// # Example
///
/// ```rust
/// use adk_harness::{InMemoryGoalStore, UntilCheckAgent};
/// use std::sync::Arc;
///
/// let check = UntilCheckAgent::new(
///     Arc::new(InMemoryGoalStore::new()),
///     adk_devtools::Workspace::new("."),
///     Arc::new(adk_devtools::HostLauncher),
/// );
/// assert_eq!(adk_core::Agent::name(&check), "until_check");
/// ```
pub struct UntilCheckAgent {
    goals: Arc<dyn GoalStore>,
    workspace: Workspace,
    launcher: Arc<dyn CommandLauncher>,
    book: Arc<GoalBook>,
    grader: Option<Arc<dyn Llm>>,
    worker: Option<String>,
}

impl std::fmt::Debug for UntilCheckAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UntilCheckAgent")
            .field("root", &self.workspace.root())
            .field("launcher", &self.launcher.isolation())
            .field("grader", &self.grader.as_ref().map(|model| model.name().to_string()))
            .finish_non_exhaustive()
    }
}

/// What one run of the `until` command found.
struct CommandCheck {
    exit_code: Option<i32>,
    output: String,
}

impl UntilCheckAgent {
    /// A check over the goals in `goals`, running commands in `workspace` through
    /// `launcher`. The command is the operator's, so it runs whether or not the model may
    /// use `bash`.
    pub fn new(
        goals: Arc<dyn GoalStore>,
        workspace: Workspace,
        launcher: Arc<dyn CommandLauncher>,
    ) -> Self {
        Self {
            goals,
            workspace: workspace.allow_bash(true),
            launcher,
            book: Arc::new(GoalBook::default()),
            grader: None,
            worker: None,
        }
    }

    /// The model that grades goals with a [`GoalRubric`]. Without one, a rubric goal's check
    /// fails with `harness.goal.no_grader`.
    #[must_use]
    pub fn with_grader(mut self, model: Arc<dyn Llm>) -> Self {
        self.grader = Some(model);
        self
    }

    /// The agent whose final message of each iteration the grader reads; by default the
    /// agent the invocation runs.
    #[must_use]
    pub fn with_worker(mut self, name: impl Into<String>) -> Self {
        self.worker = Some(name.into());
        self
    }

    /// The goals this check has seen, and the kernel's parker when one is attached.
    pub(crate) fn book(&self) -> &Arc<GoalBook> {
        &self.book
    }

    fn audit(ctx: &dyn InvocationContext) -> Option<HarnessAudit> {
        let trail = ctx.run_config().audit.clone()?;
        Some(HarnessAudit::new(trail, AuditActor::for_context(ctx), ctx.session_id()))
    }

    /// Runs one check and records it. The event mirrors the goal state while it runs on.
    async fn check(&self, ctx: Arc<dyn InvocationContext>) -> Result<Event> {
        self.check_with(ctx, None, None).await
    }

    /// [`check`](Self::check) for an iteration that made `tool_calls` tool calls and ended
    /// with `final_message`, when the engine collected them.
    async fn check_with(
        &self,
        ctx: Arc<dyn InvocationContext>,
        tool_calls: Option<usize>,
        final_message: Option<String>,
    ) -> Result<Event> {
        ctx.run_config().check_governance()?;
        let (app, user, session) =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let mut record = self.goals.get(&app, &user, &session).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::NotFound,
                "harness.goal.missing",
                "the session has no goal to check",
            )
        })?;
        // The check speaks as the harness, in a system notice: the agent reads its result as
        // the runtime's own notice, not as another agent's output quoted for it as data.
        let mut event = Event::new(ctx.invocation_id());
        event.author = adk_core::history::SYSTEM_NOTICE_AUTHOR.to_string();
        event.branch = ctx.branch().to_string();
        if record.state.status != GoalStatus::Running {
            event.actions.escalate = true;
            event.set_content(adk_core::history::system_notice(&format!(
                "The goal is {:?}.",
                record.state.status
            )));
            return Ok(event);
        }
        record.state.iteration += 1;
        // The `harness.goal` span of spec 3.1 §7.13, around the check and its records.
        #[cfg(feature = "observe")]
        let span = crate::observe::goal_iteration_span(
            record.state.iteration,
            record.state.max_iterations,
        );
        #[cfg(not(feature = "observe"))]
        let span = tracing::Span::none();
        self.finish_check(ctx, record, event, tool_calls, final_message).instrument(span).await
    }

    /// Runs the `until` command of `record`'s current iteration and records its run.
    async fn run_until(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        record: &GoalRecord,
        until: &str,
        audit: Option<&HarnessAudit>,
    ) -> Result<CommandCheck> {
        let call_id = format!("goal-check-{}-{}", record.run_id, record.state.iteration);
        let args = json!({
            "command": until,
            "timeout_secs": record.spec.check_timeout.as_secs().max(1),
        });
        if let Some(audit) = audit {
            let draft = AuditRecordDraft::new(
                AuditRecordKind::Other(HARNESS_AUDIT_KIND.to_string()),
                "harness.bash.begin",
            )
            .args_digest(audit.trail().digest(&args));
            audit
                .record_draft(
                    draft,
                    json!({
                        "functionCallId": call_id,
                        "enforcer": self.launcher.isolation(),
                        "goalCheck": true,
                    }),
                )
                .await?;
        }
        // The goal's check_timeout bounds the check, not the shell's own limit for the model.
        let workspace = self.workspace.clone().bash_timeout(record.spec.check_timeout);
        let tool = BashTool::new(workspace).with_launcher(self.launcher.clone());
        let tool_ctx = Arc::new(InvocationToolContext::new(ctx.clone(), call_id.clone()));
        let started = Instant::now();
        let outcome = tool.execute(tool_ctx, args).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (exit_code, output, timed_out) = match &outcome {
            Ok(value) => {
                let code = value.get("exit_code").and_then(Value::as_i64).map(|code| code as i32);
                let mut output = String::new();
                for stream in ["stdout", "stderr"] {
                    if let Some(text) = value.get(stream).and_then(Value::as_str) {
                        output.push_str(text);
                    }
                }
                (code, output, false)
            }
            Err(error) => (None, error.to_string(), error.code.contains("timeout")),
        };
        if let Some(audit) = audit {
            audit
                .record(
                    "harness.bash.end",
                    json!({
                        "functionCallId": call_id,
                        "exitCode": exit_code,
                        "durationMs": duration_ms,
                        "timedOut": timed_out,
                        "goalCheck": true,
                    }),
                )
                .await?;
        }
        Ok(CommandCheck { exit_code, output })
    }

    /// Grades the iteration's deliverable against `rubric` in a context of its own, metered
    /// against the run's budget and spend ledger, and records the round.
    async fn grade(
        &self,
        ctx: &Arc<dyn InvocationContext>,
        record: &GoalRecord,
        rubric: &GoalRubric,
        final_message: Option<String>,
        audit: Option<&HarnessAudit>,
    ) -> Result<adk_eval::OutcomeGrade> {
        let grader = self.grader.clone().ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                "harness.goal.no_grader",
                "the goal has a rubric but the check has no grader model; build the harness \
                 with a model or give UntilCheckAgent::with_grader one",
            )
        })?;
        let worker = self.worker.clone().unwrap_or_else(|| ctx.agent_name().to_string());
        let final_message = final_message
            .or_else(|| latest_text(&ctx.session().conversation_history_for_agent(&worker)));
        let mut deliverable = String::from("Final message of the iteration:\n");
        match final_message.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => deliverable.push_str(tail(text, FINAL_MESSAGE_MAX_BYTES)),
            None => deliverable.push_str("(none)"),
        }
        for path in &rubric.deliverables {
            deliverable.push_str(&format!("\n\nFile `{path}`:\n"));
            match self.workspace.read_for_review(path).await {
                Ok(Some(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    if text.len() > DELIVERABLE_MAX_BYTES {
                        let mut end = DELIVERABLE_MAX_BYTES;
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        deliverable.push_str(&text[..end]);
                        deliverable.push_str(&format!(
                            "\n(cut: the file has {} bytes; the grader read the first {end})",
                            text.len()
                        ));
                    } else {
                        deliverable.push_str(&text);
                    }
                }
                Ok(None) => deliverable.push_str("(the file does not exist)"),
                Err(error) => {
                    deliverable.push_str(&format!("(the file could not be read: {error})"))
                }
            }
        }
        let scope = adk_core::ModelCallScope::for_run(ctx.run_config(), ctx.app_name(), &worker);
        let metered: Arc<dyn Llm> = Arc::new(adk_core::MeteredLlm::new(grader).with_scope(scope));
        let model = metered.name().to_string();
        let grade = adk_eval::OutcomeGrader::new(metered)
            .grade(&record.spec.goal, &deliverable, &rubric.config())
            .await?;
        let failed: Vec<usize> = grade
            .criteria
            .iter()
            .enumerate()
            .filter(|(_, criterion)| !criterion.passed)
            .map(|(index, _)| index + 1)
            .collect();
        tracing::info!(
            harness.goal.iteration = record.state.iteration,
            goal.evaluation = record.state.evaluations,
            goal.criteria = grade.criteria.len(),
            goal.passed = grade.passed(),
            "goal graded"
        );
        if let Some(audit) = audit {
            audit
                .record(
                    "harness.goal.grade",
                    json!({
                        "goal": record.run_id,
                        "iteration": record.state.iteration,
                        "evaluation": record.state.evaluations,
                        "maxEvaluations": rubric.max_evaluations,
                        "model": model,
                        "criteria": grade.criteria.len(),
                        "failed": failed,
                        "satisfied": grade.satisfied(),
                    }),
                )
                .await?;
        }
        Ok(grade)
    }

    /// Runs the checks of `record`'s current iteration and records them.
    async fn finish_check(
        &self,
        ctx: Arc<dyn InvocationContext>,
        mut record: GoalRecord,
        mut event: Event,
        tool_calls: Option<usize>,
        final_message: Option<String>,
    ) -> Result<Event> {
        let (app, user, session) =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let audit = Self::audit(ctx.as_ref());
        let command = match record.spec.until.clone() {
            Some(until) => Some(self.run_until(&ctx, &record, &until, audit.as_ref()).await?),
            None => None,
        };
        let command_passed = command.as_ref().is_none_or(|check| check.exit_code == Some(0));
        // The grader runs only once the command passes, so a failing command costs no
        // model call.
        let grade = match record.spec.rubric.clone() {
            Some(rubric) if command_passed => {
                record.state.evaluations += 1;
                Some(self.grade(&ctx, &record, &rubric, final_message, audit.as_ref()).await?)
            }
            _ => None,
        };
        let exit_code = command.as_ref().and_then(|check| check.exit_code);
        let graded_out = record
            .spec
            .rubric
            .as_ref()
            .is_some_and(|rubric| record.state.evaluations >= rubric.max_evaluations);
        record.state.last_check = exit_code;
        if grade.is_some() {
            record.state.last_grade = grade.clone();
        }
        record.state.updated_at = Utc::now();
        let satisfied = grade.as_ref().is_none_or(adk_eval::OutcomeGrade::satisfied);
        record.state.status = if command_passed && satisfied {
            GoalStatus::Done
        } else if record.state.iteration >= record.state.max_iterations || graded_out {
            GoalStatus::Exhausted
        } else {
            GoalStatus::Running
        };
        self.goals.set(&app, &user, &session, record.clone()).await?;
        if let Some(audit) = &audit {
            audit
                .record(
                    "harness.goal.iteration",
                    json!({
                        "iteration": record.state.iteration,
                        "max": record.state.max_iterations,
                        "checkExitCode": exit_code,
                        "rubricSatisfied": grade.as_ref().map(adk_eval::OutcomeGrade::satisfied),
                        "status": record.state.status,
                    }),
                )
                .await?;
        }
        tracing::info!(
            harness.goal.iteration = record.state.iteration,
            goal.status = ?record.state.status,
            check.exit_code = ?exit_code,
            "goal check finished"
        );
        // A rubric goal's progress is its grade, so a repeated verdict counts as a stall.
        let observed = match (&command, &grade) {
            (_, Some(grade)) => grade.feedback(),
            (Some(check), None) => check.output.clone(),
            (None, None) => String::new(),
        };
        let stalled = self.book.record_check(
            &record.run_id,
            exit_code,
            &observed,
            tool_calls,
            record.state.status == GoalStatus::Running,
        );
        let mut summary = check_summary(&record, command.as_ref(), grade.as_ref());
        if let Some(reason) = stalled {
            self.stall(ctx.as_ref(), &record, reason, audit.as_ref()).await?;
            summary.push_str(&format!(
                "\nThe goal made no progress ({reason}); it is parked until a person resumes it."
            ));
        }
        event.set_content(adk_core::history::system_notice(&summary));
        event.actions.state_delta.insert(
            GOAL_STATE_KEY.to_string(),
            serde_json::to_value(&record.state).unwrap_or(Value::Null),
        );
        event.actions.escalate = record.state.status != GoalStatus::Running;
        Ok(event)
    }
}

/// What the next iteration reads about this iteration's checks.
fn check_summary(
    record: &GoalRecord,
    command: Option<&CommandCheck>,
    grade: Option<&adk_eval::OutcomeGrade>,
) -> String {
    let state = &record.state;
    let (iteration, max) = (state.iteration, state.max_iterations);
    let mut lines = Vec::new();
    if let (Some(until), Some(check)) = (&record.spec.until, command) {
        let exit = check.exit_code.map_or_else(|| "none".to_string(), |code| code.to_string());
        if check.exit_code == Some(0) {
            lines.push(format!("Goal check `{until}` passed on iteration {iteration}."));
        } else {
            lines.push(format!(
                "Goal check `{until}` failed (exit {exit}) on iteration {iteration} of {max}. \
                 Its output:\n{}",
                tail(&check.output, CHECK_OUTPUT_TAIL)
            ));
        }
    }
    if let (Some(rubric), Some(grade)) = (&record.spec.rubric, grade) {
        let total = grade.criteria.len();
        if grade.satisfied() {
            lines.push(format!(
                "The grader passed all {total} criteria on grading round {} of {}.",
                state.evaluations, rubric.max_evaluations
            ));
        } else {
            lines.push(format!(
                "The grader failed {} of {total} criteria on grading round {} of {}. Revise \
                 to address its feedback:\n{}",
                total - grade.passed(),
                state.evaluations,
                rubric.max_evaluations,
                grade.feedback()
            ));
        }
        if !grade.summary.is_empty() {
            lines.push(format!("Grader summary: {}", grade.summary));
        }
    }
    match state.status {
        GoalStatus::Done => lines.push("The goal is done.".to_string()),
        GoalStatus::Exhausted => {
            let cap = match &record.spec.rubric {
                Some(rubric) if state.evaluations >= rubric.max_evaluations => {
                    format!("the rubric's {} grading rounds are used up", rubric.max_evaluations)
                }
                _ => format!("no iterations are left ({iteration} of {max})"),
            };
            lines.push(format!("The goal is exhausted: {cap}."));
        }
        GoalStatus::Running | GoalStatus::Cancelled => {}
    }
    lines.join("\n")
}

/// The text of the latest model turn in `history`.
fn latest_text(history: &[Content]) -> Option<String> {
    history.iter().rev().filter(|content| content.role == "model").find_map(|content| {
        let text: String = content.parts.iter().filter_map(adk_core::Part::text).collect();
        (!text.trim().is_empty()).then_some(text)
    })
}

impl UntilCheckAgent {
    /// Records the stall of `record` and parks its durable run at its next model-call
    /// boundary, where the kernel shows it `Waiting`, suspended manually (spec 3.2 §7.3,
    /// §9.3 `harness.goal.stalled`).
    async fn stall(
        &self,
        ctx: &dyn InvocationContext,
        record: &GoalRecord,
        reason: &str,
        audit: Option<&HarnessAudit>,
    ) -> Result<()> {
        let pid = ctx.run_config().durable_run_id.clone();
        tracing::warn!(
            harness.goal.iteration = record.state.iteration,
            goal.stall = reason,
            run.id = ?pid,
            "goal made no progress; parking it"
        );
        if let Some(audit) = audit {
            audit
                .record(
                    "harness.goal.stalled",
                    json!({
                        "pid": pid,
                        "goal": record.run_id,
                        "iteration": record.state.iteration,
                        "reason": reason,
                    }),
                )
                .await?;
        }
        if let (Some(parker), Some(pid)) = (self.book.parker(), pid) {
            match parker.park(&pid).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(run.id = %pid, "the kernel is gone; the stalled goal runs on")
                }
                Err(error) => {
                    tracing::warn!(run.id = %pid, error = %error, "could not park the stalled goal")
                }
            }
        }
        Ok(())
    }
}

/// The last `max` bytes of `text`, on a character boundary.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[async_trait]
impl Agent for UntilCheckAgent {
    fn name(&self) -> &str {
        UNTIL_CHECK_AGENT
    }

    fn description(&self) -> &str {
        "Runs the goal's until command and records the goal's progress."
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let event = self.check(ctx).await?;
        Ok(Box::pin(futures::stream::iter(vec![Ok(event)])))
    }
}

/// How a harness runs goals.
#[derive(Clone)]
pub(crate) struct GoalRuntime {
    pub(crate) store: Arc<dyn GoalStore>,
    pub(crate) until: Arc<UntilCheckAgent>,
    #[cfg(feature = "graph")]
    pub(crate) engine: GoalEngine,
    #[cfg(feature = "graph")]
    pub(crate) checkpointer: Arc<dyn adk_graph::Checkpointer>,
}

impl GoalRuntime {
    /// Runs this turn as the session's goal, when one is running and the mode lets it.
    pub(crate) async fn route(
        &self,
        mode: HarnessMode,
        harness: &str,
        root: Arc<dyn Agent>,
        ctx: Arc<dyn InvocationContext>,
    ) -> Result<Option<EventStream>> {
        if !matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
            return Ok(None);
        }
        let Some(record) = self.store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await?
        else {
            return Ok(None);
        };
        if record.state.status != GoalStatus::Running {
            return Ok(None);
        }
        self.until.book().started(&record.run_id);
        #[cfg(feature = "graph")]
        if self.engine == GoalEngine::Graph {
            return Ok(Some(self.run_graph(&record, harness, root, ctx)?));
        }
        // The loop is named like the harness, so a hold records its position under one name
        // every turn and the next turn resumes there.
        let engine = adk_agent::LoopAgent::new(harness, vec![root, self.until.clone()])
            .with_max_iterations(GOAL_LOOP_CEILING);
        Ok(Some(engine.run(ctx).await?))
    }

    #[cfg(feature = "graph")]
    fn run_graph(
        &self,
        record: &GoalRecord,
        harness: &str,
        root: Arc<dyn Agent>,
        ctx: Arc<dyn InvocationContext>,
    ) -> Result<EventStream> {
        use adk_graph::{END, ExecutionConfig, START, State, StateGraph, StateSchema};

        use crate::plan_graph::drive;

        let (sink, forwarded) = tokio::sync::mpsc::unbounded_channel();
        let graph = StateGraph::new(StateSchema::simple(&["input"]))
            .add_node(IterateNode {
                root,
                harness: harness.to_string(),
                prompt: record.spec.prompt(),
                sink: sink.clone(),
            })
            .add_node(CheckNode { until: self.until.clone(), sink })
            .add_edge(START, "iterate")
            .add_edge("iterate", "check")
            .add_conditional_edges(
                "check",
                |state: &State| {
                    if state.get("goalStatus").and_then(Value::as_str) == Some("running") {
                        "iterate".to_string()
                    } else {
                        END.to_string()
                    }
                },
                [("iterate", "iterate"), (END, END)],
            )
            .compile()?
            .with_checkpointer_arc(self.checkpointer.clone())
            // Iterate and check checkpoint each iteration; the stall window and its
            // predecessor are all a resume needs (spec 3.2 §10 HK3).
            .with_checkpoint_retention(adk_graph::RetentionPolicy::keep_last(
                2 * GOAL_STALL_WINDOW as usize,
            ))
            .with_recursion_limit(2 * record.spec.max_iterations as usize + 2);
        let thread = format!(
            "{}:goal:{}",
            adk_graph::agent::session_thread_id(ctx.app_name(), ctx.user_id(), ctx.session_id()),
            record.run_id
        );
        let config = ExecutionConfig::new(&thread).with_parent_context(ctx);
        let run = async move { graph.invoke(State::new(), config).await };
        Ok(drive(run, forwarded, |outcome| {
            async_stream::stream! {
                match outcome {
                    Ok(_) | Err(adk_graph::GraphError::Interrupted(_)) => {}
                    Err(error) => yield Err(error.into()),
                }
            }
        }))
    }
}

/// The graph state key of the tool calls the last iteration made.
#[cfg(feature = "graph")]
const ITERATION_TOOL_CALLS: &str = "iterationToolCalls";

/// The graph state key of the last iteration's final message, which a rubric's grader reads.
#[cfg(feature = "graph")]
const ITERATION_OUTPUT: &str = "iterationOutput";

/// One iteration of the `Graph` engine: the harness agent works on the goal.
#[cfg(feature = "graph")]
struct IterateNode {
    root: Arc<dyn Agent>,
    harness: String,
    prompt: String,
    sink: crate::plan_graph::Sink,
}

#[cfg(feature = "graph")]
#[async_trait]
impl adk_graph::Node for IterateNode {
    fn name(&self) -> &str {
        "iterate"
    }

    async fn execute(
        &self,
        ctx: &adk_graph::NodeContext,
    ) -> adk_graph::Result<adk_graph::NodeOutput> {
        let parent = ctx.config.parent_context.clone().ok_or_else(|| {
            adk_graph::GraphError::NodeExecutionFailed {
                node: "iterate".into(),
                message: "a goal iteration runs only inside a harness turn".into(),
            }
        })?;
        let (held, output, tool_calls) = crate::plan_graph::run_agent_node_counted(
            "iterate",
            &self.root,
            &self.harness,
            parent,
            Content::new("user").with_text(self.prompt.clone()),
            &self.sink,
        )
        .await?;
        if held {
            return Ok(adk_graph::NodeOutput::interrupt_with_data(
                "goal iteration holds a tool call",
                json!({}),
            ));
        }
        Ok(adk_graph::NodeOutput::new()
            .with_update(ITERATION_TOOL_CALLS, json!(tool_calls))
            .with_update(ITERATION_OUTPUT, json!(output)))
    }
}

/// The `Graph` engine's check.
#[cfg(feature = "graph")]
struct CheckNode {
    until: Arc<UntilCheckAgent>,
    sink: crate::plan_graph::Sink,
}

#[cfg(feature = "graph")]
#[async_trait]
impl adk_graph::Node for CheckNode {
    fn name(&self) -> &str {
        "check"
    }

    async fn execute(
        &self,
        ctx: &adk_graph::NodeContext,
    ) -> adk_graph::Result<adk_graph::NodeOutput> {
        let parent = ctx.config.parent_context.clone().ok_or_else(|| {
            adk_graph::GraphError::NodeExecutionFailed {
                node: "check".into(),
                message: "a goal check runs only inside a harness turn".into(),
            }
        })?;
        let tool_calls = ctx
            .state
            .get(ITERATION_TOOL_CALLS)
            .and_then(Value::as_u64)
            .and_then(|calls| usize::try_from(calls).ok());
        let output = ctx.state.get(ITERATION_OUTPUT).and_then(Value::as_str).map(str::to_string);
        let event = self.until.check_with(parent, tool_calls, output).await.map_err(|error| {
            adk_graph::GraphError::AgentFailed { node: "check".into(), source: Box::new(error) }
        })?;
        // A check of a goal that no longer runs mirrors nothing, and ends the graph.
        let status = event
            .actions
            .state_delta
            .get(GOAL_STATE_KEY)
            .and_then(|state| state.get("status"))
            .cloned()
            .unwrap_or_else(|| json!("done"));
        let _ = self.sink.send(crate::plan_graph::Forward::Event(event));
        Ok(adk_graph::NodeOutput::new().with_update("goalStatus", status))
    }
}

/// Starts `spec` for the session: validates it, requires Ask or Auto, and writes the goal
/// state (spec 3.1 §7.11).
#[cfg(feature = "runner")]
pub(crate) async fn begin(
    store: &dyn GoalStore,
    mode: HarnessMode,
    (app, user, session): (&str, &str, &str),
    spec: GoalSpec,
) -> Result<GoalRecord> {
    spec.validate()?;
    if !matches!(mode, HarnessMode::Ask | HarnessMode::Auto) {
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Forbidden,
            "harness.goal.mode",
            format!("a goal runs only in ask or auto mode; the session is in {mode} mode"),
        ));
    }
    let record = GoalRecord::start(spec);
    store.set(app, user, session, record.clone()).await?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_on_a_character_boundary() {
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("aé", 1), "");
    }

    #[cfg(feature = "runner")]
    #[test]
    fn a_goal_spec_must_be_complete() {
        let spec = GoalSpec::new("g")
            .with_until("true")
            .with_max_iterations(1)
            .with_check_timeout(Duration::from_secs(1));
        assert!(spec.validate().is_ok());
        assert!(GoalSpec { max_iterations: 0, ..spec.clone() }.validate().is_err());
        assert!(GoalSpec { until: Some(" ".into()), ..spec.clone() }.validate().is_err());
        assert!(GoalSpec { until: None, ..spec.clone() }.validate().is_err());
        assert!(GoalSpec { check_timeout: Duration::ZERO, ..spec }.validate().is_err());
    }

    #[cfg(feature = "runner")]
    #[test]
    fn a_rubric_goal_needs_criteria_and_a_cap_of_at_most_twenty() {
        let rubric = GoalRubric::new(["names the price"]);
        let spec = GoalSpec::new("write blurb.md").with_rubric(rubric.clone());
        assert!(spec.validate().is_ok(), "a rubric alone is a check");
        assert_eq!(rubric.max_evaluations, adk_eval::DEFAULT_OUTCOME_ITERATIONS);
        for (max, valid) in [(0, false), (1, true), (20, true), (21, false)] {
            let spec = spec.clone().with_rubric(rubric.clone().with_max_evaluations(max));
            assert_eq!(spec.validate().is_ok(), valid, "max_evaluations {max}");
        }
        let empty = spec.clone().with_rubric(GoalRubric::new(["  "]));
        assert!(empty.validate().is_err());
        let both = spec.with_until("test -f blurb.md");
        assert!(both.validate().is_ok());
    }

    #[test]
    fn an_until_goal_keeps_its_digest() {
        // The digest names a goal's durable run, so adding the rubric must not move it.
        let spec = GoalSpec::new("make main green")
            .with_until("cargo test")
            .with_max_iterations(5)
            .with_check_timeout(Duration::from_secs(900));
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            json!({
                "goal": "make main green",
                "until": "cargo test",
                "maxIterations": 5,
                "checkTimeout": {"secs": 900, "nanos": 0},
            })
        );
    }

    #[test]
    fn the_prompt_shows_a_visible_rubric_and_hides_a_hidden_one() {
        let rubric = GoalRubric::new(["names the product", "states the price"])
            .with_deliverables(["blurb.md"]);
        let visible = GoalSpec::new("write blurb.md").with_rubric(rubric.clone()).prompt();
        assert!(visible.contains("reads only your final message and blurb.md"), "{visible}");
        assert!(visible.contains("1. names the product\n2. states the price"), "{visible}");
        let hidden = GoalSpec::new("write blurb.md").with_rubric(rubric.hidden(true)).prompt();
        assert!(hidden.contains("criteria you are not shown"), "{hidden}");
        assert!(!hidden.contains("states the price"), "{hidden}");
        let both = GoalSpec::new("g")
            .with_until("cargo test")
            .with_rubric(GoalRubric::new(["c"]))
            .prompt();
        assert!(both.contains("runs `cargo test`, which must exit 0, then a separate grader"));
        assert!(both.contains("the goal is reached when both pass"), "{both}");
    }

    #[test]
    fn a_rubric_reads_from_markdown_and_from_the_goal_text() {
        let rubric = GoalRubric::from_markdown("# Rubric\n\n- one\n- two\n").unwrap();
        assert_eq!(rubric.criteria, ["one", "two"]);
        assert!(GoalRubric::from_markdown("# Only a heading\n").is_err());
        let listed = GoalRubric::from_goal("Ship it:\n1. tests pass\n2) docs updated");
        assert_eq!(listed.criteria, ["tests pass", "docs updated"]);
    }
}
