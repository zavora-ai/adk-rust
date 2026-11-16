//! Goals at the console: [`HarnessLauncher::run_goal`] and
//! [`HarnessLauncher::resume_goal`] over `Harness::start_goal` (spec 3.1 §7.11, §8.1).

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    AdkError, ApprovalAction, ApprovalScope, ApprovalState, DurableRunStore, Enqueued, Event,
    EventStream, NewRun, OnConflict, Part, Result, RunCheckpointRef, RunFilter, RunState,
};
use adk_harness::{
    GOAL_QUEUE, GOAL_STATE_KEY, GOAL_TARGET, GoalSpec, GoalState, GoalStatus, HarnessMode,
    IsolationStatus,
};
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{ApprovalItem, AttachRequest, ProcessesQuery};
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::launcher::{HarnessLauncher, Shared, TurnOutcome};
use crate::console::ConsoleIo;
use crate::launcher::terminal_safe;

/// How often the console reads the session while a durable goal runs.
const TAIL_INTERVAL: Duration = Duration::from_millis(200);

/// How long a goal run a stopped process held stays claimed before `--resume` reclaims it.
const GOAL_LEASE: Duration = Duration::from_secs(10);

/// The message of a run that continues a goal no live run carries.
const CONTINUE_GOAL: &str = "Continue toward the goal.";

/// How often a goal on the kernel is read while this terminal follows it.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(500);

/// The exit code of a goal that waits for a decision, as `adk-rust goal` exits.
const EXIT_HELD: i32 = 3;

/// The exit code of a goal interrupted with Ctrl-C.
const EXIT_INTERRUPTED: i32 = 130;

/// `goal` of [`HarnessLauncher::run`]: work toward a goal until a check passes
/// (spec 3.1 §7.11, §8.1).
#[derive(clap::Args, Debug)]
pub(super) struct GoalCommand {
    /// What to achieve
    #[arg(required_unless_present = "resume")]
    goal: Option<String>,
    /// The check: the goal is met when this shell command exits 0
    #[arg(long, required_unless_present = "resume")]
    until: Option<String>,
    /// Iterations toward the goal before it counts as exhausted
    #[arg(long, default_value_t = 8)]
    max_iters: u32,
    /// Seconds one run of the check may take before it counts as failed
    #[arg(long, value_name = "SECS", default_value_t = 600)]
    check_timeout: u64,
    /// Continue the unfinished goal of --session, or of the latest session that has one
    #[arg(long)]
    resume: bool,
    /// The session to start or continue the goal in
    #[arg(long)]
    session: Option<String>,
    /// The session's mode: ask or auto
    #[arg(long, value_parser = goal_mode)]
    mode: Option<HarnessMode>,
    /// Run bash only in the OS sandbox
    #[arg(long)]
    sandbox: bool,
    /// Approve every call that reaches the prompt, without asking
    #[arg(long)]
    yes: bool,
    /// Let --yes approve an unsandboxed retry of a command the sandbox refused
    #[arg(long, requires = "yes")]
    allow_unsandboxed: bool,
}

/// `--mode`: a goal edits files and runs commands, so only `ask` and `auto` take one.
fn goal_mode(value: &str) -> std::result::Result<HarnessMode, String> {
    match value.parse::<HarnessMode>() {
        Ok(mode @ (HarnessMode::Ask | HarnessMode::Auto)) => Ok(mode),
        Ok(mode) => Err(format!(
            "a goal needs ask or auto mode, not {mode}: it edits files and runs commands"
        )),
        Err(error) => Err(error.message),
    }
}

/// How following a goal on the kernel ended.
enum Followed {
    /// The goal's process finished.
    Done,
    /// These calls wait for a decision nobody here gives.
    Held(Vec<ApprovalItem>),
    /// Ctrl-C: the goal carries on in the kernel.
    Detached,
}

impl HarnessLauncher {
    /// `goal` of [`run`](Self::run): the goal on the kernel answering at the home, else in
    /// this process. Returns the process's exit code.
    pub(super) async fn goal_command(mut self, command: GoalCommand) -> Result<i32> {
        let console = match &self.console {
            Some(console) => console.clone(),
            None => ConsoleIo::terminal()?,
        };
        self.console = Some(console.clone());
        if command.sandbox {
            self.require_sandbox(&console)?;
        }
        if let Some(mode) = command.mode {
            self.mode = Some(mode);
        }
        if command.session.is_some() {
            self.session_id = command.session.clone();
        }
        if self.run_store.is_none() {
            self.run_store = self.kernel_stores.as_ref().map(|stores| stores.runs.clone());
        }
        let spec = match (&command.goal, &command.until) {
            (Some(goal), Some(until)) => Some(GoalSpec {
                goal: goal.clone(),
                until: until.clone(),
                max_iterations: command.max_iters,
                check_timeout: Duration::from_secs(command.check_timeout),
            }),
            _ => None,
        };
        let home = self.kernel_home();
        if !command.resume
            && self.run_store.is_some()
            && let Some(spec) = spec.clone()
            && let Some(client) =
                ControlClient::connect_local(&home).await.map_err(AdkError::from)?
        {
            let unattended = command.yes.then_some(command.allow_unsandboxed);
            return self.goal_on_kernel(client, spec, unattended, &console).await;
        }
        if command.yes {
            console.set_approve_all(Some(command.allow_unsandboxed));
        }
        let outcome = match spec {
            Some(spec) if !command.resume => {
                console.print(&format!(
                    "goal:  {}\nuntil: {}  (budget: {} iterations)\n\n",
                    terminal_safe(&spec.goal),
                    terminal_safe(&spec.until),
                    spec.max_iterations
                ));
                self.run_goal(spec).await?
            }
            _ => {
                if self.session_id.is_none() {
                    self.session_id =
                        Some(self.latest_unfinished_goal().await?.ok_or_else(|| {
                            AdkError::config(
                                "no unfinished goal to resume; start one without --resume",
                            )
                        })?);
                }
                console.print(&format!(
                    "resuming the goal in session {}\n",
                    terminal_safe(self.session_id.as_deref().unwrap_or_default())
                ));
                self.resume_goal(Some(command.max_iters)).await?
            }
        };
        let state = self.goal().await?;
        let code = if !outcome.held.is_empty() {
            console.print(&format!(
                "\n{} call(s) wait for a decision in session {}; continue with `goal --resume \
                 --session {}` once they are decided.\n",
                outcome.held.len(),
                terminal_safe(&outcome.session_id),
                terminal_safe(&outcome.session_id)
            ));
            EXIT_HELD
        } else if outcome.interrupted {
            console.print("\ninterrupted; continue with `goal --resume`.\n");
            EXIT_INTERRUPTED
        } else {
            goal_exit(&console, state.as_ref())
        };
        self.close().await;
        Ok(code)
    }

    /// `--sandbox`: `bash` must run in the OS sandbox or a container. A harness on the host
    /// is rebuilt with the sandbox when the launcher has a rebuild; where no sandbox exists
    /// the commands run on the host behind approvals, as the harness's own fallback does.
    fn require_sandbox(&mut self, console: &ConsoleIo) -> Result<()> {
        let isolated = |agent: &adk_harness::HarnessAgent| {
            !matches!(agent.info().shell(), Some(IsolationStatus::Host))
        };
        if !isolated(&self.agent)
            && let Some(rebuild) = &self.rebuild
        {
            self.agent = Arc::new(rebuild(true)?);
        }
        match self.agent.info().shell() {
            Some(IsolationStatus::Host) => Err(AdkError::config(
                "--sandbox: this harness runs bash on the host; set `sandbox = true` in \
                 .adk/harness.toml or build it with HarnessAgentBuilder::shell",
            )),
            Some(IsolationStatus::Degraded { reason, .. }) => {
                console.print(&format!(
                    "note: the OS sandbox is unavailable here ({}); bash runs on the host and \
                     every command waits for approval\n",
                    terminal_safe(reason)
                ));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The most recently updated session of the launcher's user whose goal is not met.
    async fn latest_unfinished_goal(&self) -> Result<Option<String>> {
        let sessions = self.session_service.clone().ok_or_else(|| {
            AdkError::config(
                "--resume reads the sessions: give the launcher a persistent session service",
            )
        })?;
        let app_name = self.resolved_app_name();
        let mut listed = sessions
            .list(adk_session::ListRequest {
                app_name: app_name.clone(),
                user_id: self.user_id.clone(),
                limit: None,
                offset: None,
            })
            .await?;
        listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
        for session in listed {
            let full = sessions
                .get(adk_session::GetRequest {
                    app_name: app_name.clone(),
                    user_id: self.user_id.clone(),
                    session_id: session.id().to_string(),
                    num_recent_events: Some(1),
                    after: None,
                })
                .await?;
            let unfinished = full
                .state()
                .get(GOAL_STATE_KEY)
                .and_then(|value| serde_json::from_value::<GoalState>(value).ok())
                .is_some_and(|state| state.status != GoalStatus::Done);
            if unfinished {
                return Ok(Some(full.id().to_string()));
            }
        }
        Ok(None)
    }

    /// The goal as a durable run of this user on the kernel's `background` queue, followed
    /// here (spec 3.2 §8.1): the session's mode and standing rules apply on the kernel, and
    /// `unattended` (`--yes`) approves held calls through the inbox while this follows.
    async fn goal_on_kernel(
        mut self,
        client: ControlClient,
        spec: GoalSpec,
        unattended: Option<bool>,
        console: &ConsoleIo,
    ) -> Result<i32> {
        let home = self.kernel_home();
        self.goal_queue = Some(adk_kernel::QueueClass::BACKGROUND.to_string());
        self.goals_authenticated = true;
        let (goal, until, iterations) =
            (spec.goal.clone(), spec.until.clone(), spec.max_iterations);
        let pid = self.enqueue_goal(spec).await?;
        let session = self.session_id().unwrap_or_default().to_string();
        let mode = self.mode().await?;
        console.print(&format!(
            "goal on the kernel at {} · mode {mode}{}\ngoal:  {}\nuntil: {}  (budget: {iterations} \
             iterations)\nprocess {pid} · session {}\nCtrl-C detaches; the goal keeps running.\n\n",
            home.display(),
            if unattended.is_some() {
                " · --yes approves held calls while this follows"
            } else {
                ""
            },
            terminal_safe(&goal),
            terminal_safe(&until),
            terminal_safe(&session),
        ));
        let followed = follow_on_kernel(&client, console, &session, &pid, unattended).await?;
        let state = self.goal().await?;
        Ok(match followed {
            Followed::Done => goal_exit(console, state.as_ref()),
            Followed::Held(items) => {
                console.print(&format!(
                    "\n{} call(s) wait for a decision; the goal waits on the kernel and resumes \
                     once they are decided:\n",
                    items.len()
                ));
                for item in &items {
                    console.print(&format!(
                        "  {}  {} {}\n",
                        terminal_safe(&item.id),
                        terminal_safe(&item.tool_name),
                        terminal_safe(&one_line(&item.args_redacted.to_string()))
                    ));
                }
                console.print(&format!(
                    "Decide with `adk-rust approvals --home {} approve ID` (or `deny ID`), or \
                     from any surface.\n",
                    home.display()
                ));
                EXIT_HELD
            }
            Followed::Detached => {
                console.print(&format!(
                    "\ndetached; the goal keeps running on the kernel (process {pid}).\n"
                ));
                0
            }
        })
    }

    /// Starts `spec` as the session's goal and drives it until the goal is met, runs out of
    /// iterations, holds a call for a decision or is interrupted (spec 3.1 §7.11, §8.1).
    ///
    /// The harness iterates: a turn toward the goal, then the goal's `until` command, until
    /// the command exits 0. With a run store ([`with_run_store`](Self::with_run_store)) the
    /// goal is a durable run executed by a worker in this process, and its events are shown
    /// as the session records them; a call held without a prompter parks the run, which
    /// [`resume_goal`](Self::resume_goal) continues once the call is decided, in this
    /// process or a later one. Read the result with [`goal`](Self::goal).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::time::Duration;
    /// use adk_harness::GoalSpec;
    ///
    /// let spec = GoalSpec {
    ///     goal: "make the tests pass".into(),
    ///     until: "cargo test".into(),
    ///     max_iterations: 8,
    ///     check_timeout: Duration::from_secs(120),
    /// };
    /// let outcome = launcher.run_goal(spec).await?;
    /// let state = launcher.goal().await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the `Harness::start_goal` error (`harness.goal.mode` outside Ask and Auto,
    /// `harness.goal.duplicate` for a goal that already ran as a durable run in the
    /// session), the run store's error, or `harness.goal.failed` when the durable run fails.
    pub async fn run_goal(&mut self, spec: GoalSpec) -> Result<TurnOutcome> {
        let echo = self.echo;
        let shared = self.open().await?;
        let harness = shared.harness();
        let token = CancellationToken::new();
        shared.console.set_run(Some(token.clone()));
        let started = harness.start_goal(&shared.user, &shared.session, spec).await;
        let Some(runs) = shared.run_store.clone() else {
            return shared.drive(started, token, echo).await;
        };
        let mut started = match started {
            Ok(events) => events,
            Err(error) => {
                shared.console.set_run(None);
                return Err(error);
            }
        };
        let run_id = match started.next().await {
            Some(Ok(event)) => event.provider_metadata.get("harness.goal.run_id").cloned(),
            Some(Err(error)) => {
                shared.console.set_run(None);
                return Err(error);
            }
            None => None,
        };
        let Some(run_id) = run_id else {
            shared.console.set_run(None);
            return Err(AdkError::config("the goal was not enqueued as a durable run"));
        };
        shared.console.print(&format!("goal run {run_id}\n"));
        drive_durable(&shared, runs, run_id, token, echo).await
    }

    /// Starts `spec` as the session's goal as a durable run and returns the run's id without
    /// executing it here: a running kernel's worker for the goal queue claims it (spec 3.2
    /// §8.1, `goal` on a running kernel). Set the kernel's queue with
    /// [`with_goal_queue`](Self::with_goal_queue).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let launcher = launcher.with_run_store(runs).with_goal_queue("background");
    /// let run_id = launcher.enqueue_goal(spec).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error without a run store
    /// ([`with_run_store`](Self::with_run_store)), or the `Harness::start_goal` error.
    pub async fn enqueue_goal(&mut self, spec: GoalSpec) -> Result<String> {
        let shared = self.open().await?;
        if shared.run_store.is_none() {
            return Err(AdkError::config(
                "a goal is enqueued as a durable run: give the launcher a run store",
            ));
        }
        let mut started = shared.harness().start_goal(&shared.user, &shared.session, spec).await?;
        match started.next().await {
            Some(Ok(event)) => event
                .provider_metadata
                .get("harness.goal.run_id")
                .cloned()
                .ok_or_else(|| AdkError::config("the goal was not enqueued as a durable run")),
            Some(Err(error)) => Err(error),
            None => Err(AdkError::config("the goal was not enqueued as a durable run")),
        }
    }

    /// Continues the session's goal (spec 3.1 §7.11, §8.1): in the same session, at the
    /// iteration it reached, with its conversation.
    ///
    /// `max_iterations` replaces the goal's cap; an exhausted goal runs again when the new
    /// cap is above the iterations it used. A durable run parked on held calls is woken
    /// once every one of them is decided, for example with `adk-rust approvals approve`;
    /// while one is undecided nothing runs and the calls are returned in
    /// [`TurnOutcome::held`]. A run a stopped process left is reclaimed when its lease
    /// lapses, and a running goal without a run gets a new one.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the session has no goal, the goal was
    /// cancelled, or the cap is not above the iterations an exhausted goal used; or the
    /// store's error.
    pub async fn resume_goal(&mut self, max_iterations: Option<u32>) -> Result<TurnOutcome> {
        let echo = self.echo;
        let shared = self.open().await?;
        let harness = shared.harness();
        let goals = harness.agent().runtime().goal_store().clone();
        let (app, user, session) =
            (harness.app_name().to_string(), shared.user.to_string(), shared.session.to_string());
        let mut record = goals.get(&app, &user, &session).await?.ok_or_else(|| {
            AdkError::config(format!("session {session} has no goal; start one without --resume"))
        })?;
        let outcome = || TurnOutcome { session_id: session.clone(), ..TurnOutcome::default() };
        match record.state.status {
            GoalStatus::Done => return Ok(outcome()),
            GoalStatus::Cancelled => {
                return Err(AdkError::config(format!(
                    "the goal in session {session} was cancelled; start it again without --resume"
                )));
            }
            GoalStatus::Running | GoalStatus::Exhausted => {}
        }
        if let Some(max) = max_iterations
            && max != record.state.max_iterations
        {
            if record.state.status == GoalStatus::Exhausted && max <= record.state.iteration {
                return Err(AdkError::config(format!(
                    "the goal used {} iterations; resume with --max-iters above that",
                    record.state.iteration
                )));
            }
            record.spec.max_iterations = max;
            record.state.max_iterations = max;
            if record.state.status == GoalStatus::Exhausted {
                record.state.status = GoalStatus::Running;
            }
            record.state.updated_at = chrono::Utc::now();
            goals.set(&app, &user, &session, record.clone()).await?;
            mirror(&shared, &record.state).await?;
        }
        if record.state.status != GoalStatus::Running {
            return Err(AdkError::config(format!(
                "the goal used all {} iterations; resume with a higher --max-iters",
                record.state.iteration
            )));
        }

        let token = CancellationToken::new();
        shared.console.set_run(Some(token.clone()));
        let Some(runs) = shared.run_store.clone() else {
            let started = harness.resume(&shared.user, &shared.session).await;
            return shared.drive(started, token, echo).await;
        };
        let live = runs
            .list(
                RunFilter::default()
                    .queue(GOAL_QUEUE)
                    .app_name(app.clone())
                    .user_id(user.clone())
                    .session_id(session.clone())
                    .states([RunState::Queued, RunState::Running, RunState::Waiting]),
            )
            .await?
            .into_iter()
            .find(|run| run.target == GOAL_TARGET);
        let (run_id, reclaimed) = match live {
            Some(run) if run.state == RunState::Waiting => {
                let held = match &run.checkpoint {
                    Some(RunCheckpointRef::AwaitingApproval { fingerprints, .. }) => {
                        undecided(&shared, &app, fingerprints).await?
                    }
                    _ => Vec::new(),
                };
                if !held.is_empty() {
                    shared.console.set_run(None);
                    return Ok(TurnOutcome { held, ..outcome() });
                }
                runs.wake(&run.run_id).await?;
                (run.run_id, false)
            }
            Some(run) => (run.run_id, true),
            None => (continue_goal(runs.as_ref(), &app, &user, &session).await?, false),
        };
        shared.console.print(&format!("goal run {run_id}\n"));
        let outcome = drive_durable(&shared, runs.clone(), run_id.clone(), token, echo).await?;
        // A run a stopped process left can finish without running the goal on: its turn had
        // answered before the check was cut short. The goal then gets a new run.
        let unfinished = !outcome.held.is_empty() || outcome.interrupted || outcome.ended_by_denial;
        if !reclaimed
            || unfinished
            || goals.get(&app, &user, &session).await?.map(|record| record.state.status)
                != Some(GoalStatus::Running)
            || runs.get(&run_id).await?.map(|run| run.state) != Some(RunState::Completed)
        {
            return Ok(outcome);
        }
        let run_id = continue_goal(runs.as_ref(), &app, &user, &session).await?;
        shared.console.print(&format!("goal run {run_id}\n"));
        let token = CancellationToken::new();
        shared.console.set_run(Some(token.clone()));
        drive_durable(&shared, runs, run_id, token, echo).await
    }

    /// The goal of the session the launcher works in (spec 3.1 §7.11), read from the
    /// harness's goal store; `None` before a goal starts.
    ///
    /// # Errors
    ///
    /// Returns the session's or the goal store's error.
    pub async fn goal(&mut self) -> Result<Option<GoalState>> {
        let shared = self.open().await?;
        shared.harness().goal(&shared.user, &shared.session).await
    }

    /// The mode the session the launcher works in runs in, as the harness's mode store
    /// records it: the mode a worker sharing the store, such as a kernel's, decides its goal
    /// in.
    ///
    /// # Errors
    ///
    /// Returns the session's or the mode store's error.
    pub async fn mode(&mut self) -> Result<adk_harness::HarnessMode> {
        let shared = self.open().await?;
        shared.harness().mode(&shared.user, &shared.session).await
    }
}

/// Enqueues a run that continues the session's running goal, which no live run carries.
async fn continue_goal(
    runs: &dyn DurableRunStore,
    app: &str,
    user: &str,
    session: &str,
) -> Result<String> {
    let next = NewRun::new(GOAL_QUEUE, app, user, GOAL_TARGET, json!({ "text": CONTINUE_GOAL }))
        .with_session_id(session)
        .with_concurrency_key(session, OnConflict::Enqueue);
    Ok(match runs.enqueue(next).await? {
        Enqueued::Created(run) | Enqueued::Existing(run) => run.run_id,
        Enqueued::Rejected { blocking_run_id } => blocking_run_id,
    })
}

/// The held calls among `fingerprints` that have no decision yet.
async fn undecided(
    shared: &Shared,
    app: &str,
    fingerprints: &[String],
) -> Result<Vec<adk_core::ToolConfirmationRequest>> {
    let harness = shared.harness();
    let store = harness.agent().runtime().approval_store();
    let scope = ApprovalScope::new(app, shared.user.to_string(), shared.session.to_string());
    let pending = store.pending(&scope).await?;
    let mut held = Vec::new();
    for fingerprint in fingerprints {
        if !store.has_decision(&scope, fingerprint).await?
            && let Some(entry) = pending.iter().find(|entry| &entry.fingerprint == fingerprint)
        {
            held.push(entry.request.clone());
        }
    }
    Ok(held)
}

/// Records the goal's state in the session's `harness:goal` mirror.
async fn mirror(shared: &Shared, state: &GoalState) -> Result<()> {
    let harness = shared.harness();
    let mut event = Event::new(format!("harness-goal-{}", uuid::Uuid::new_v4()));
    event.author = harness.agent().info().name.clone();
    event.actions.state_delta.insert(
        GOAL_STATE_KEY.to_string(),
        serde_json::to_value(state).map_err(|error| AdkError::config(error.to_string()))?,
    );
    shared.sessions.append_event(shared.session.as_ref(), event).await
}

/// Runs the durable goal run `run_id` on a worker in this process and shows its events
/// until it finishes or parks.
async fn drive_durable(
    shared: &Arc<Shared>,
    runs: Arc<dyn DurableRunStore>,
    run_id: String,
    token: CancellationToken,
    echo: bool,
) -> Result<TurnOutcome> {
    let (target, runner) = shared.harness().goal_target();
    let worker = DurableWorker::new(runs.clone())
        .with_agent(target, runner)
        .with_config(
            DurableWorkerConfig::default()
                .with_queue(GOAL_QUEUE)
                .with_poll_interval(TAIL_INTERVAL)
                .with_lease_ttl(GOAL_LEASE)
                .with_max_concurrent_runs(1),
        )
        .spawn();
    let seen = match shared.events().await {
        Ok(events) => events.len(),
        Err(error) => {
            worker.abort();
            shared.console.set_run(None);
            return Err(error);
        }
    };
    let events = tail(shared.clone(), runs.clone(), run_id.clone(), seen);
    let outcome = shared.drive(Ok(events), token, echo).await;
    if outcome.as_ref().is_ok_and(|outcome| outcome.interrupted) {
        // As a crash would: the run's lease lapses and `resume_goal` reclaims it.
        worker.abort();
    } else {
        worker.shutdown().await;
    }
    let outcome = outcome?;
    if let Some(run) = runs.get(&run_id).await?
        && run.state == RunState::Failed
    {
        return Err(AdkError::new(
            adk_core::ErrorComponent::Agent,
            adk_core::ErrorCategory::Internal,
            "harness.goal.failed",
            format!(
                "goal run {run_id} failed: {}",
                run.error.unwrap_or_else(|| "no error was recorded".to_string())
            ),
        ));
    }
    Ok(outcome)
}

/// The session's events from index `seen` on, as the worker's run records them, until the
/// run finishes or parks.
fn tail(
    shared: Arc<Shared>,
    runs: Arc<dyn DurableRunStore>,
    run_id: String,
    seen: usize,
) -> EventStream {
    struct Tail {
        shared: Arc<Shared>,
        runs: Arc<dyn DurableRunStore>,
        run_id: String,
        seen: usize,
        buffered: VecDeque<Event>,
        settled: bool,
    }
    let start = Tail { shared, runs, run_id, seen, buffered: VecDeque::new(), settled: false };
    Box::pin(futures::stream::unfold(start, |mut tail| async move {
        loop {
            if let Some(event) = tail.buffered.pop_front() {
                return Some((Ok(event), tail));
            }
            if tail.settled {
                return None;
            }
            // The state is read before the events, so a run that settles in between still
            // has its last events read.
            let state = match tail.runs.get(&tail.run_id).await {
                Ok(run) => run.map_or(RunState::Cancelled, |run| run.state),
                Err(error) => {
                    tail.settled = true;
                    return Some((Err(error), tail));
                }
            };
            match tail.shared.events().await {
                Ok(events) => {
                    let total = events.len();
                    tail.buffered.extend(events.into_iter().skip(tail.seen));
                    tail.seen = total;
                }
                Err(error) => {
                    tail.settled = true;
                    return Some((Err(error), tail));
                }
            }
            tail.settled = state == RunState::Waiting || state.is_terminal();
            if tail.buffered.is_empty() && !tail.settled {
                tokio::time::sleep(TAIL_INTERVAL).await;
            }
        }
    }))
}

/// Prints how a goal stands and returns the exit code: `0` met, `1` otherwise.
fn goal_exit(console: &ConsoleIo, state: Option<&GoalState>) -> i32 {
    let (line, code) = match state {
        Some(state) if state.status == GoalStatus::Done => (
            format!("goal met after {} iteration(s): `{}` passed.", state.iteration, state.until),
            0,
        ),
        Some(state) if state.status == GoalStatus::Exhausted => (
            format!(
                "budget exhausted after {} iteration(s); `{}` still fails. Resume with --resume \
                 and a higher --max-iters.",
                state.iteration, state.until
            ),
            1,
        ),
        Some(state) => (
            format!(
                "the goal is {:?} at iteration {}; continue with --resume.",
                state.status, state.iteration
            ),
            1,
        ),
        None => ("the session has no goal.".to_string(), 1),
    };
    console.print(&format!("\n{}\n", terminal_safe(&line)));
    code
}

/// Follows the goal's process `pid` on the kernel until it finishes, printing what the agent
/// says and calls when the kernel lets this caller read the session; a held call ends the
/// follow, unless `unattended` (`--yes`) approves it through the inbox (an unsandboxed retry
/// only with `allow_unsandboxed`).
async fn follow_on_kernel(
    client: &ControlClient,
    console: &ConsoleIo,
    session: &str,
    pid: &str,
    unattended: Option<bool>,
) -> Result<Followed> {
    let mut cursor = None;
    let mut decided = HashSet::new();
    loop {
        // The transcript is the caller's own session's; another user's is not shown.
        let request = AttachRequest { after: cursor, ..AttachRequest::default() };
        if let Ok(attached) = client.attach(session, &request).await {
            render(console, &attached.transcript);
            if attached.cursor.is_some() {
                cursor = attached.cursor;
            }
        }
        let process = client
            .processes(&ProcessesQuery::default())
            .await
            .map_err(AdkError::from)?
            .into_iter()
            .find(|process| process.meta.pid == pid);
        match process.map(|process| process.run.state) {
            None => {}
            Some(state) if state.is_terminal() => return Ok(Followed::Done),
            Some(RunState::Waiting) => {
                let items: Vec<ApprovalItem> = client
                    .approvals()
                    .await
                    .map_err(AdkError::from)?
                    .into_iter()
                    .filter(|item| {
                        item.scope.session_id == session
                            && item.state == ApprovalState::Pending
                            && !decided.contains(&item.id)
                    })
                    .collect();
                match unattended {
                    None if !items.is_empty() => return Ok(Followed::Held(items)),
                    None => {}
                    Some(allow_unsandboxed) => {
                        for item in items {
                            decide_unattended(client, console, &item, allow_unsandboxed).await?;
                            decided.insert(item.id);
                        }
                    }
                }
            }
            Some(_) => {}
        }
        tokio::select! {
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => return Ok(Followed::Detached),
        }
    }
}

/// `--yes` on the kernel: approves `item` through the inbox, except an unsandboxed retry
/// without `allow_unsandboxed`, which it refuses.
async fn decide_unattended(
    client: &ControlClient,
    console: &ConsoleIo,
    item: &ApprovalItem,
    allow_unsandboxed: bool,
) -> Result<()> {
    let unsandboxed = item.tool_name == "bash"
        && item.args_redacted.get("unsandboxed").and_then(serde_json::Value::as_bool) == Some(true);
    let (action, reason, said) = if unsandboxed && !allow_unsandboxed {
        (
            ApprovalAction::Reject,
            Some("--yes does not approve unsandboxed retries without --allow-unsandboxed"),
            "denied: an unsandboxed retry needs --allow-unsandboxed",
        )
    } else {
        (ApprovalAction::Approve, None, "approved by --yes")
    };
    client.decide(&item.id, item.version, action, reason).await.map_err(AdkError::from)?;
    console.print(&format!(
        "[{said}] {} {}\n",
        terminal_safe(&item.tool_name),
        terminal_safe(&one_line(&item.args_redacted.to_string()))
    ));
    Ok(())
}

/// Prints what the agent says and the calls it makes.
fn render(console: &ConsoleIo, events: &[Event]) {
    for event in events.iter().filter(|event| event.author != "user") {
        let Some(content) = &event.llm_response.content else {
            continue;
        };
        for part in &content.parts {
            match part {
                Part::Text { text } if !text.trim().is_empty() => {
                    console.print(&format!("{}\n", terminal_safe(text.trim_end())));
                }
                Part::FunctionCall { name, args, .. } => console.print(&format!(
                    "· {} {}\n",
                    terminal_safe(name),
                    terminal_safe(&one_line(&args.to_string()))
                )),
                _ => {}
            }
        }
    }
}

/// `text` on one line, cut to 120 characters.
fn one_line(text: &str) -> String {
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(120) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use adk_core::{ApprovalService, StoreApprovalService, Tool, ToolEffect};
    use adk_harness::{HarnessAgent, InstructionsConfig, Workspace};
    use adk_model::{ScriptedLlm, ScriptedResponse};
    use adk_tool::FunctionTool;
    use clap::Parser;

    use super::*;
    use crate::harness::ConsolePrompter;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        goal: GoalCommand,
    }

    fn parse(args: &[&str]) -> std::result::Result<GoalCommand, clap::Error> {
        Cli::try_parse_from(std::iter::once("goal").chain(args.iter().copied())).map(|cli| cli.goal)
    }

    #[test]
    fn a_goal_takes_a_check_and_ask_or_auto() {
        let goal = parse(&["ship it", "--until", "true", "--mode", "auto", "--yes"]).unwrap();
        assert_eq!(
            (goal.goal.as_deref(), goal.until.as_deref(), goal.mode, goal.yes),
            (Some("ship it"), Some("true"), Some(HarnessMode::Auto), true)
        );
        assert!(parse(&["--resume"]).unwrap().resume);
        assert!(parse(&["ship it"]).is_err(), "a goal needs --until");
        let refused = parse(&["ship it", "--until", "true", "--mode", "plan"]).unwrap_err();
        assert!(refused.to_string().contains("ask or auto"), "{refused}");
        assert!(parse(&["ship it", "--until", "true", "--allow-unsandboxed"]).is_err());
    }

    /// A tool the harness governs as external, so Auto holds it for a decision; it writes
    /// `published.txt` and counts its runs.
    fn publish(dir: &Path, runs: Arc<AtomicUsize>) -> Arc<dyn Tool> {
        let out = dir.join("published.txt");
        Arc::new(
            FunctionTool::new("publish", "Publishes the status.", move |_ctx, _args| {
                let (out, runs) = (out.clone(), runs.clone());
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    std::fs::write(&out, "status").map_err(|e| AdkError::tool(e.to_string()))?;
                    Ok(json!({ "published": true }))
                }
            })
            .with_effect(ToolEffect::NonIdempotent),
        )
    }

    fn scripted() -> ScriptedLlm {
        ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("publish", json!({})),
                ScriptedResponse::text("Published."),
                ScriptedResponse::text("Done."),
            ],
        )
    }

    fn builder(dir: &Path) -> adk_harness::HarnessAgentBuilder {
        HarnessAgent::builder("ops")
            .workspace(Workspace::new(dir).allow_bash(true))
            .project_instructions(InstructionsConfig {
                user_file: None,
                ..InstructionsConfig::default()
            })
            .mode(HarnessMode::Auto)
            .without_default_members()
    }

    #[tokio::test]
    async fn yes_approves_a_held_call_through_the_console_in_process() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let console = ConsoleIo::scripted([], false);
        let agent = builder(dir.path())
            .model(Arc::new(scripted()))
            .tool(publish(dir.path(), runs.clone()))
            .prompter(Arc::new(ConsolePrompter::new(console.clone())))
            .build()
            .unwrap();
        let code = HarnessLauncher::new(agent)
            .with_console(console)
            .with_kernel_config(adk_kernel::KernelConfig::default().with_home(home.path()))
            .quiet()
            .run_with_args(["ops", "goal", "publish", "--until", "test -f published.txt", "--yes"])
            .await
            .unwrap();
        assert_eq!((code, runs.load(Ordering::SeqCst)), (0, 1));
    }

    #[tokio::test]
    async fn sandbox_refuses_a_harness_on_the_host_it_cannot_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let agent = builder(dir.path()).model(Arc::new(scripted())).build().unwrap();
        let error = HarnessLauncher::new(agent)
            .with_console(ConsoleIo::scripted([], false))
            .run_with_args(["ops", "goal", "x", "--until", "true", "--sandbox"])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--sandbox"), "{error}");
    }

    /// `up` and `goal` as two launchers over one SQLite file: the goal runs as a process of
    /// the kernel at the home, holds the publish call, and resumes when the inbox approves it.
    #[cfg(feature = "persist")]
    #[tokio::test]
    async fn a_goal_runs_on_the_kernel_at_the_home_and_resumes_on_approval() {
        use adk_kernel::stores::KernelStores;
        use adk_kernel::{KernelConfig, Trigger, Wake};

        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("state.db").display());
        let sessions = Arc::new(adk_session::SqliteSessionService::new(&url).await.unwrap());
        sessions.migrate().await.unwrap();
        let pool = sessions.pool().clone();
        let stores = KernelStores::from_sqlite_pool(pool.clone()).await.unwrap();
        let goals = Arc::new(adk_harness::SqliteGoalStore::new(pool.clone()));
        goals.migrate().await.unwrap();
        let home = dir.path().join("home");
        let config = KernelConfig::default().with_home(&home);
        let service = || -> Arc<dyn ApprovalService> {
            Arc::new(
                StoreApprovalService::new(stores.approvals.clone())
                    .with_run_store(stores.runs.clone()),
            )
        };
        let runs = Arc::new(AtomicUsize::new(0));
        let agent = |model: ScriptedLlm| {
            builder(dir.path())
                .model(Arc::new(model))
                .tool(publish(dir.path(), runs.clone()))
                .approval_service(service())
                .action_ledger(stores.actions.clone())
                .goal_store(goals.clone())
                .build()
                .unwrap()
        };

        let nightly = Trigger::new(
            "nightly",
            Wake::Cron { expr: "0 0 3 * * *".into(), missed: adk_core::MissedFirePolicy::Skip },
            NewRun::new("background", "ops", "user", "ops", json!({ "text": "check" })),
        );
        let up = HarnessLauncher::new(agent(scripted()))
            .with_session_service(sessions.clone())
            .with_kernel_stores(stores.clone())
            .with_kernel_config(config.clone())
            .with_trigger(nightly)
            .resident()
            .unwrap();
        let kernel = up.kernel.clone().unwrap();
        let inventory = kernel.inventory();
        assert_eq!(
            inventory.triggers.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["nightly"]
        );
        assert!(inventory.stores.iter().all(|store| !store.in_memory), "{:?}", inventory.stores);
        assert_eq!(kernel.config().home, home);
        kernel.start().await.unwrap();

        // The goal holds `publish` on the kernel; this terminal does not decide it.
        let code =
            HarnessLauncher::new(agent(ScriptedLlm::new("unused", Vec::<ScriptedResponse>::new())))
                .with_session_service(sessions.clone())
                .with_kernel_stores(stores.clone())
                .with_kernel_config(config)
                .with_console(ConsoleIo::scripted([], false))
                .quiet()
                .run_with_args(["ops", "goal", "publish", "--until", "test -f published.txt"])
                .await
                .unwrap();
        assert_eq!((code, runs.load(Ordering::SeqCst)), (EXIT_HELD, 0));

        // Another terminal approves it in the inbox; the kernel resumes the goal at once.
        let client = ControlClient::connect_local(&home).await.unwrap().unwrap();
        let item = client.approvals().await.unwrap().into_iter().next().unwrap();
        assert_eq!(item.tool_name, "publish");
        client.decide(&item.id, item.version, ApprovalAction::Approve, None).await.unwrap();
        let mut met = false;
        for _ in 0..120 {
            let finished = stores
                .runs
                .list(RunFilter::default().states([RunState::Completed]))
                .await
                .unwrap()
                .into_iter()
                .any(|run| run.target == GOAL_TARGET);
            if finished && dir.path().join("published.txt").exists() {
                met = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        kernel.stop().await.unwrap();
        assert!(met, "the goal did not resume after the approval");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }
}
