//! Goals at the console: [`HarnessLauncher::run_goal`] and
//! [`HarnessLauncher::resume_goal`] over `Harness::start_goal` (spec 3.1 §7.11, §8.1).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    AdkError, ApprovalScope, DurableRunStore, Enqueued, Event, EventStream, NewRun, OnConflict,
    Result, RunCheckpointRef, RunFilter, RunState,
};
use adk_harness::{GOAL_QUEUE, GOAL_STATE_KEY, GOAL_TARGET, GoalSpec, GoalState, GoalStatus};
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::launcher::{HarnessLauncher, Shared, TurnOutcome};

/// How often the console reads the session while a durable goal runs.
const TAIL_INTERVAL: Duration = Duration::from_millis(200);

/// How long a goal run a stopped process held stays claimed before `--resume` reclaims it.
const GOAL_LEASE: Duration = Duration::from_secs(10);

/// The message of a run that continues a goal no live run carries.
const CONTINUE_GOAL: &str = "Continue toward the goal.";

impl HarnessLauncher {
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
        let shared = self.open().await?;
        let harness = shared.harness();
        let token = CancellationToken::new();
        shared.console.set_run(Some(token.clone()));
        let started = harness.start_goal(&shared.user, &shared.session, spec).await;
        let Some(runs) = shared.run_store.clone() else {
            return shared.drive(started, token, true).await;
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
        drive_durable(&shared, runs, run_id, token).await
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
            return shared.drive(started, token, true).await;
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
        let outcome = drive_durable(&shared, runs.clone(), run_id.clone(), token).await?;
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
        drive_durable(&shared, runs, run_id, token).await
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
    let outcome = shared.drive(Ok(events), token, true).await;
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
