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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, Event, EventStream, InvocationContext,
    Result, Tool,
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

/// A loop-until goal (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::GoalSpec;
/// use std::time::Duration;
///
/// let goal = GoalSpec {
///     goal: "make the tests pass".into(),
///     until: "cargo test".into(),
///     max_iterations: 10,
///     check_timeout: Duration::from_secs(600),
/// };
/// assert_eq!(goal.max_iterations, 10);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSpec {
    /// What to achieve.
    pub goal: String,
    /// The operator's check command; exit 0 means done.
    pub until: String,
    /// Most iterations.
    pub max_iterations: u32,
    /// Timeout of one check.
    pub check_timeout: Duration,
}

impl GoalSpec {
    /// The goal's digest: SHA-256 of its canonical JSON, as the durable run's idempotency
    /// key uses it (`goal:<session>:<digest>`).
    ///
    /// # Example
    ///
    /// ```rust
    /// # use adk_harness::GoalSpec;
    /// # use std::time::Duration;
    /// let goal = GoalSpec { goal: "g".into(), until: "true".into(), max_iterations: 1, check_timeout: Duration::from_secs(1) };
    /// assert_eq!(goal.digest().len(), 64);
    /// ```
    pub fn digest(&self) -> String {
        crate::fsutil::sha256_hex(&serde_json::to_string(self).unwrap_or_default())
    }

    /// The message that starts the goal's run.
    pub fn prompt(&self) -> String {
        format!(
            "Goal: {goal}\n\nWork toward the goal. After each of your turns the harness runs \
             `{until}`; the goal is reached when it exits 0, and you get at most {max} \
             iterations. Read the check's output from the previous iteration before you \
             continue.",
            goal = self.goal,
            until = self.until,
            max = self.max_iterations,
        )
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
        if self.until.trim().is_empty() {
            return Err(invalid("a goal needs an until command"));
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
pub struct GoalState {
    /// What to achieve.
    pub goal: String,
    /// The check command.
    pub until: String,
    /// The current iteration.
    pub iteration: u32,
    /// Most iterations.
    pub max_iterations: u32,
    /// Where it stands.
    pub status: GoalStatus,
    /// The last check's exit code.
    pub last_check: Option<i32>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
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
/// use std::time::Duration;
///
/// let spec = GoalSpec { goal: "g".into(), until: "true".into(), max_iterations: 3, check_timeout: Duration::from_secs(5) };
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
/// use std::time::Duration;
///
/// # futures::executor::block_on(async {
/// let store = InMemoryGoalStore::new();
/// let spec = GoalSpec { goal: "g".into(), until: "true".into(), max_iterations: 3, check_timeout: Duration::from_secs(5) };
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
}

#[cfg(feature = "sqlite")]
impl SqliteGoalStore {
    /// A store over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
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
        // A record that does not parse is no goal: nothing runs on it.
        Ok(row.and_then(|(text,)| serde_json::from_str(&text).ok()))
    }

    async fn set(&self, app: &str, user: &str, session: &str, record: GoalRecord) -> Result<()> {
        let text = serde_json::to_string(&record)
            .map_err(|error| crate::fsutil::serialize_error("goal record", &error))?;
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

/// Runs a goal's `until` check after each iteration (spec 3.1 §7.11).
///
/// It checks the kill switch, runs `until` through the harness's command launcher with the
/// goal's `check_timeout`, updates the goal state, and sets `escalate` on exit 0 (Done) or
/// at the cap (Exhausted), which stops the `LoopAgent`. The command comes from the
/// operator, not the model, so it is not a governed tool call; it is audited as
/// `harness.bash.begin`/`.end` with `goalCheck: true`, and each iteration as
/// `harness.goal.iteration` (spec 3.1 §9.3).
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
}

impl std::fmt::Debug for UntilCheckAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UntilCheckAgent")
            .field("root", &self.workspace.root())
            .field("launcher", &self.launcher.isolation())
            .finish_non_exhaustive()
    }
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
        Self { goals, workspace: workspace.allow_bash(true), launcher }
    }

    fn audit(ctx: &dyn InvocationContext) -> Option<HarnessAudit> {
        let trail = ctx.run_config().audit.clone()?;
        Some(HarnessAudit::new(trail, AuditActor::for_context(ctx), ctx.session_id()))
    }

    /// Runs one check and records it. The event mirrors the goal state while it runs on.
    async fn check(&self, ctx: Arc<dyn InvocationContext>) -> Result<Event> {
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
        let mut event = Event::new(ctx.invocation_id());
        event.author = UNTIL_CHECK_AGENT.to_string();
        event.branch = ctx.branch().to_string();
        if record.state.status != GoalStatus::Running {
            event.actions.escalate = true;
            event.llm_response.content = Some(
                Content::new("model").with_text(format!("The goal is {:?}.", record.state.status)),
            );
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
        self.finish_check(ctx, record, event).instrument(span).await
    }

    /// Runs the check of `record`'s current iteration and records it.
    async fn finish_check(
        &self,
        ctx: Arc<dyn InvocationContext>,
        mut record: GoalRecord,
        mut event: Event,
    ) -> Result<Event> {
        let (app, user, session) =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let call_id = format!("goal-check-{}-{}", record.run_id, record.state.iteration);
        let audit = Self::audit(ctx.as_ref());
        let args = json!({
            "command": record.spec.until,
            "timeout_secs": record.spec.check_timeout.as_secs().max(1),
        });
        if let Some(audit) = &audit {
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
        if let Some(audit) = &audit {
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
        record.state.last_check = exit_code;
        record.state.updated_at = Utc::now();
        record.state.status = if exit_code == Some(0) {
            GoalStatus::Done
        } else if record.state.iteration >= record.state.max_iterations {
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
        let tail = tail(&output, CHECK_OUTPUT_TAIL);
        let summary = match record.state.status {
            GoalStatus::Done => format!(
                "Goal check `{}` passed on iteration {}. The goal is done.",
                record.spec.until, record.state.iteration
            ),
            GoalStatus::Exhausted => format!(
                "Goal check `{}` failed (exit {}) on iteration {} of {}; no iterations are left.\n{tail}",
                record.spec.until,
                exit_code.map_or_else(|| "none".to_string(), |code| code.to_string()),
                record.state.iteration,
                record.state.max_iterations
            ),
            GoalStatus::Running | GoalStatus::Cancelled => format!(
                "Goal check `{}` failed (exit {}) on iteration {} of {}. Its output:\n{tail}",
                record.spec.until,
                exit_code.map_or_else(|| "none".to_string(), |code| code.to_string()),
                record.state.iteration,
                record.state.max_iterations
            ),
        };
        event.llm_response.content = Some(Content::new("model").with_text(summary));
        event.actions.state_delta.insert(
            GOAL_STATE_KEY.to_string(),
            serde_json::to_value(&record.state).unwrap_or(Value::Null),
        );
        event.actions.escalate = record.state.status != GoalStatus::Running;
        Ok(event)
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
        let (held, _) = crate::plan_graph::run_agent_node(
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
        Ok(adk_graph::NodeOutput::new())
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
        let event = self.until.check(parent).await.map_err(|error| {
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
        let spec = GoalSpec {
            goal: "g".into(),
            until: "true".into(),
            max_iterations: 1,
            check_timeout: Duration::from_secs(1),
        };
        assert!(spec.validate().is_ok());
        assert!(GoalSpec { max_iterations: 0, ..spec.clone() }.validate().is_err());
        assert!(GoalSpec { until: " ".into(), ..spec.clone() }.validate().is_err());
        assert!(GoalSpec { check_timeout: Duration::ZERO, ..spec }.validate().is_err());
    }
}
