//! K2, K8 and K9 on a live model with the kernel in this process (spec 3.2 §5, §11). Each
//! needs a control the `adk-rust up` binary does not expose: K2 a manual clock the run store
//! and the kernel share, so an hour asleep takes seconds; K8 the first model call of each
//! background job held until the scenario releases it; K9 two kernel replicas over one
//! SQLite file with a mirrored kill switch. Every kernel is built with `Harness::kernel`, as
//! `adk-rust up` builds it, over the provider's model.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::audit_log::{AuditActor, InMemoryAuditRecorder};
use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, Clock, DurableRunStore, Llm, LlmRequest,
    LlmResponseStream, ManualClock, MissedFirePolicy, NewRun, Part, RunFilter, RunState, Schedule,
    ScheduleSpec, ScheduleStore, SchemaAdapter, SubjectRef, SystemClock, Tool, ToolContext,
    ToolEffect,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, InstructionsConfig, ToolKind};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    ExpiryAction, Kernel, KernelConfig, KernelEvent, PartitionId, ProcessFilter, ProcessKind,
    ProcessSpec, QueueClass, Quotas, Suspension, Wake, WakeSet,
};
use adk_managed::resolver::{DefaultModelResolver, ModelResolver};
use adk_managed::types::ModelRef;
use adk_session::{SqliteActionLedger, SqliteGovernanceStateStore};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{Timelike, Utc};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::Checks;
use crate::scripted::{self, Boot, Stores, until};

/// The app, user and target every process runs under, as `adk-rust up` registers them.
const APP: &str = "adk-rust";
const USER: &str = "ana";

/// The live scenarios of this module.
pub const LIVE: &[&str] = &[
    "live_k2_sleep_is_free_for_an_hour",
    "live_k8_a_message_preempts_background_work",
    "live_k9_one_switch_stops_everything",
];

pub async fn run_live(name: &str, provider: &str, model: &str) -> Option<Result<Checks>> {
    if !LIVE.contains(&name) {
        return None;
    }
    let model = match live_model(provider, model).await {
        Ok(model) => model,
        Err(error) => return Some(Err(error)),
    };
    Some(match name {
        "live_k2_sleep_is_free_for_an_hour" => k2(model).await,
        "live_k8_a_message_preempts_background_work" => k8(model).await,
        _ => k9(model).await,
    })
}

/// The provider's model, built from the key in the environment.
async fn live_model(provider: &str, model: &str) -> Result<Arc<dyn Llm>> {
    DefaultModelResolver::new()
        .resolve(&ModelRef::Shorthand(model.into()))
        .await
        .map_err(|error| anyhow!("{provider}'s model {model} cannot be built: {error}"))
}

/// The live model, recording the first user text of every request it forwards, when it
/// started and when its response ended. A gated key's first request waits until
/// [`release`](Self::release) (K8).
struct Live {
    inner: Arc<dyn Llm>,
    started: Mutex<Vec<(String, Instant)>>,
    ended: Arc<Mutex<Vec<(Instant, Instant)>>>,
    gates: Vec<(&'static str, Arc<tokio::sync::Semaphore>)>,
    held: AtomicUsize,
}

impl Live {
    fn new(inner: Arc<dyn Llm>) -> Self {
        Self::gated(inner, &[])
    }

    fn gated(inner: Arc<dyn Llm>, keys: &[&'static str]) -> Self {
        let gates =
            keys.iter().map(|key| (*key, Arc::new(tokio::sync::Semaphore::new(0)))).collect();
        Self {
            inner,
            started: Mutex::default(),
            ended: Arc::default(),
            gates,
            held: AtomicUsize::new(0),
        }
    }

    fn release(&self, key: &str) {
        if let Some((_, gate)) = self.gates.iter().find(|(name, _)| *name == key) {
            gate.add_permits(1);
        }
    }

    fn started(&self) -> Vec<(String, Instant)> {
        self.started.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    /// Requests forwarded so far.
    fn calls(&self) -> usize {
        self.started().len()
    }

    /// Requests forwarded whose prompt contains `key`.
    fn calls_for(&self, key: &str) -> usize {
        self.started().iter().filter(|(prompt, _)| prompt.contains(key)).count()
    }

    /// When the first request whose prompt contains `key` was forwarded.
    fn first(&self, key: &str) -> Option<Instant> {
        self.started().iter().find(|(prompt, _)| prompt.contains(key)).map(|(_, at)| *at)
    }

    /// Requests forwarded after `at`.
    fn started_after(&self, at: Instant) -> usize {
        self.started().iter().filter(|(_, started)| *started > at).count()
    }

    /// The requests in flight at `at`, and how long after it the last part of the last of
    /// them arrived.
    fn in_flight(&self, at: Instant) -> (usize, Option<Duration>) {
        let ended = self.ended.lock().unwrap_or_else(|error| error.into_inner());
        let spanning: Vec<_> =
            ended.iter().filter(|(start, end)| *start <= at && *end > at).collect();
        let last = spanning.iter().map(|(_, end)| end.duration_since(at)).max();
        (spanning.len(), last)
    }
}

#[async_trait]
impl Llm for Live {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let prompt = request
            .contents
            .iter()
            .filter(|content| content.role == "user")
            .flat_map(|content| content.parts.iter())
            .find_map(|part| match part {
                Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let gate = self.gates.iter().find(|(key, _)| prompt.contains(key));
        if let Some((key, gate)) = gate
            && self.calls_for(key) == 0
        {
            self.held.fetch_add(1, Ordering::SeqCst);
            gate.acquire()
                .await
                .map_err(|error| adk_core::AdkError::model(error.to_string()))?
                .forget();
            self.held.fetch_sub(1, Ordering::SeqCst);
        }
        let start = Instant::now();
        self.started.lock().unwrap_or_else(|error| error.into_inner()).push((prompt, start));
        let response = self.inner.generate_content(request, stream).await?;
        let span = Span { start, last: Arc::new(Mutex::new(start)), ended: self.ended.clone() };
        let last = span.last.clone();
        Ok(Box::pin(response.map(move |item| {
            let _ = &span;
            *last.lock().unwrap_or_else(|error| error.into_inner()) = Instant::now();
            item
        })))
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    fn uses_interactions_api(&self) -> bool {
        self.inner.uses_interactions_api()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }
}

/// One forwarded request: when it started and when its last response part arrived, recorded
/// when its stream is dropped.
struct Span {
    start: Instant,
    last: Arc<Mutex<Instant>>,
    ended: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

impl Drop for Span {
    fn drop(&mut self) {
        let last = *self.last.lock().unwrap_or_else(|error| error.into_inner());
        self.ended.lock().unwrap_or_else(|error| error.into_inner()).push((self.start, last));
    }
}

/// A boot of the kernel in `home` over `stores` on `model`, in `mode`, on `clock`.
fn setup(model: Arc<dyn Llm>, mode: HarnessMode, clock: Arc<dyn Clock>) -> Boot {
    Boot {
        model,
        mode,
        control: ControlPlaneConfig::default().without_socket(),
        audit: Arc::new(InMemoryAuditRecorder::new()),
        clock,
        proactivity: None,
        quotas: None,
        drivers: None,
    }
}

fn run(queue: &str, text: &str, session: &str) -> NewRun {
    NewRun::new(queue, APP, USER, APP, json!({ "text": text })).with_session_id(session)
}

/// The model's text in `session`.
async fn answer(stores: &Stores, session: &str) -> Result<String> {
    Ok(transcript(stores, session)
        .await?
        .iter()
        .filter(|event| event.author != "user")
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" "))
}

/// How many of `session`'s tool calls `tool` answered.
async fn answered(stores: &Stores, session: &str, tool: &str) -> Result<usize> {
    Ok(transcript(stores, session)
        .await?
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter(|part| {
            matches!(part, Part::FunctionResponse { function_response, .. }
                if function_response.name == tool)
        })
        .count())
}

async fn transcript(stores: &Stores, session: &str) -> Result<Vec<adk_core::Event>> {
    use adk_session::SessionService as _;
    let session = stores
        .sessions
        .get(adk_session::GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: session.into(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(session.events().all())
}

async fn state_is(runs: &Arc<impl DurableRunStore>, pid: &str, state: RunState) -> Result<bool> {
    Ok(runs.get(pid).await?.is_some_and(|run| run.state == state))
}

/// K2 live (spec 3.2 §5): ten processes asleep on ten kinds of wake set make no model call,
/// spend nothing and hold no lease through an hour on the manual clock the run store and the
/// kernel share; `ps` lists all ten `Waiting` with their wake sets. Past the hour, the time
/// wake, the expiry and a schedule fire on the same clock, and only then is the model called.
async fn k2(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace, repo) =
        (dir.path().join("home"), dir.path().join("ws"), dir.path().join("repo"));
    for path in [&workspace, &repo] {
        std::fs::create_dir_all(path)?;
    }
    std::fs::write(repo.join("Cargo.lock"), "# lockfile\n")?;
    let start = Utc::now();
    let manual = Arc::new(ManualClock::new(start));
    let clock: Arc<dyn Clock> = manual.clone();
    let stores = scripted::stores(&home, clock.clone()).await?;
    let live = Arc::new(Live::new(model));
    let kernel =
        scripted::boot(&stores, &home, &workspace, setup(live.clone(), HarnessMode::Ask, clock))?;
    let report_at = start + chrono::Duration::minutes(90);
    let report = Schedule::new(
        "report",
        ScheduleSpec::At(report_at),
        run(QueueClass::BACKGROUND, "Reply with the single word: lighthouse", "k2-report"),
        MissedFirePolicy::FireOnce,
    )
    .with_next_fire_at(report_at);
    stores.schedules.upsert(report).await?;
    kernel.start().await?;

    let two_hours = start + chrono::Duration::hours(2);
    // A daily firing twelve hours away, so neither the hour nor the wake reaches it.
    let cron = format!("0 0 {} * * *", (start.hour() + 12) % 24);
    let sets = [
        WakeSet::any_of([Wake::Webhook { hook: "github".into() }]),
        WakeSet::any_of([Wake::Queue { source: "orders".into() }]),
        WakeSet::any_of([Wake::FileChange {
            root: repo.clone(),
            glob: "Cargo.lock".into(),
            debounce: Duration::from_millis(100),
        }]),
        WakeSet::any_of([Wake::Cron { expr: cron, missed: MissedFirePolicy::Skip }]),
        WakeSet::any_of([Wake::Approval { fingerprint: "sha256:held-call".into() }]),
        WakeSet::any_of([Wake::RunFinished { pid: "run-elsewhere".into() }]),
        WakeSet::any_of([Wake::BudgetReplenished { partition: PartitionId::new("default") }]),
        WakeSet::any_of([Wake::Manual]),
        WakeSet::any_of([Wake::At(two_hours)]),
        WakeSet::any_of([Wake::Webhook { hook: "linear".into() }])
            .with_expiry(two_hours, ExpiryAction::Run),
    ];
    // The two sleepers the clock wakes answer with a codeword; the rest never run.
    let words = ["heliotrope", "marigold"];
    let mut pids = Vec::new();
    for (n, set) in sets.iter().enumerate() {
        let word = match n {
            8 => words[0].to_string(),
            9 => words[1].to_string(),
            _ => format!("sleeper{n}"),
        };
        let process = kernel
            .processes()
            .spawn(
                run(
                    QueueClass::BACKGROUND,
                    &format!("Reply with the single word: {word}"),
                    &format!("k2-sleeper-{n}"),
                ),
                ProcessSpec::new(ProcessKind::Goal).with_wake(set.clone()),
            )
            .await?;
        pids.push(process.meta.pid);
    }
    let asleep = until(Duration::from_secs(20), || async {
        let mut all = true;
        for pid in &pids {
            all &= state_is(&stores.runs, pid, RunState::Waiting).await?;
        }
        Ok(all)
    })
    .await?;
    checks.check("ten processes sleep on ten kinds of wake set", asleep);

    // An hour in one-minute steps; every ten minutes, no run holds a lease.
    let mut leased = Vec::new();
    for minute in 1..=60 {
        manual.advance(Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(25)).await;
        if minute % 10 == 0 {
            leased.extend(
                stores
                    .runs
                    .list(RunFilter::default())
                    .await?
                    .into_iter()
                    .filter(|run| run.lease_owner.is_some())
                    .map(|run| format!("{} at minute {minute}", run.run_id)),
            );
        }
    }
    checks.check("no model call in the hour", live.calls() == 0);
    checks.check("no lease at any ten-minute mark of the hour", leased.is_empty());
    let runs = stores.runs.list(RunFilter::default()).await?;
    checks.check(
        "nothing but the ten sleepers exists, all `Waiting` with no lease",
        runs.len() == 10
            && runs.iter().all(|run| run.state == RunState::Waiting && run.lease_owner.is_none()),
    );
    let waiting =
        kernel.processes().ps(ProcessFilter::default().states([RunState::Waiting])).await?;
    checks.check("`ps` lists ten `Waiting` processes", waiting.len() == 10);
    let listed = pids.iter().zip(&sets).all(|(pid, set)| {
        waiting
            .iter()
            .find(|process| &process.meta.pid == pid)
            .is_some_and(|process| process.meta.wake.as_ref() == Some(set))
    });
    checks.check("`ps` shows each with its wake set", listed);
    checks.check(
        "each spent nothing and made no model call, asleep an hour",
        waiting.iter().all(|process| {
            process.view.spend_today == 0
                && process.view.model_calls == 0
                && process.view.age >= Duration::from_secs(59 * 60)
        }),
    );

    // Past two hours on the same clock: the time wake and the expiry wake their sleepers,
    // and the schedule due at 90 minutes fires once.
    let calls_asleep = live.calls();
    manual.advance(Duration::from_secs(61 * 60));
    let woke = until(Duration::from_secs(180), || async {
        Ok(state_is(&stores.runs, &pids[8], RunState::Completed).await?
            && state_is(&stores.runs, &pids[9], RunState::Completed).await?)
    })
    .await?;
    checks.check("the time wake and the expiry woke their sleepers, which completed", woke);
    let fired = until(Duration::from_secs(180), || async {
        Ok(stores.runs.list(RunFilter::default()).await?.iter().any(|run| {
            run.idempotency_key.as_deref().is_some_and(|key| key.starts_with("schedule:report:"))
                && run.state == RunState::Completed
        }))
    })
    .await?;
    let scheduled: Vec<_> = stores
        .runs
        .list(RunFilter::default())
        .await?
        .into_iter()
        .filter(|run| {
            run.idempotency_key.as_deref().is_some_and(|key| key.starts_with("schedule:report:"))
        })
        .collect();
    checks.check(
        "the schedule due at 90 minutes fired once and completed",
        fired && scheduled.len() == 1,
    );
    let answers = [
        answer(&stores, "k2-sleeper-8").await?,
        answer(&stores, "k2-sleeper-9").await?,
        answer(&stores, "k2-report").await?,
    ];
    println!("    woken answers: {answers:?}");
    checks.check(
        "the woken turns answered on the live model",
        answers
            .iter()
            .zip([words[0], words[1], "lighthouse"])
            .all(|(text, word)| text.to_lowercase().contains(word)),
    );
    let woken: Vec<_> = kernel
        .processes()
        .ps(ProcessFilter::default())
        .await?
        .into_iter()
        .filter(|process| process.meta.pid == pids[8] || process.meta.pid == pids[9])
        .collect();
    println!(
        "    model calls {calls_asleep} asleep, {} after the wake; woken views: {:?}",
        live.calls(),
        woken
            .iter()
            .map(|process| (process.view.model_calls, process.view.spend_today))
            .collect::<Vec<_>>()
    );
    checks.check(
        "the model was called only after the wake, and `ps` counts the woken turns' calls",
        live.calls() >= 3
            && woken.len() == 2
            && woken.iter().all(|process| process.view.model_calls >= 1),
    );
    checks.check(
        "`ps` meters the woken turns' spend, so the zero while asleep is measured",
        woken.len() == 2 && woken.iter().all(|process| process.view.spend_today > 0),
    );
    let mut still = true;
    for pid in &pids[..8] {
        let run = stores.runs.get(pid).await?;
        still &= run.is_some_and(|run| run.state == RunState::Waiting && run.lease_owner.is_none());
    }
    checks.check("the other eight still sleep with no lease", still);
    kernel.stop().await?;
    Ok(checks)
}

/// The processes `kernel` has admitted: running, and not on their way to park.
async fn admitted(kernel: &Kernel) -> Result<usize> {
    Ok(kernel
        .processes()
        .ps(ProcessFilter::default())
        .await?
        .iter()
        .filter(|process| {
            process.run.state == RunState::Running
                && process.run.target != adk_kernel::KERNEL_WAKE_TARGET
                && process.meta.wake.is_none()
                && !process.meta.suspend_requested
                && process.meta.suspended_by.is_none()
        })
        .count())
}

/// K8 live (spec 3.2 §5): three background jobs hold their first model call at the
/// partition's quota of three; a typed message starts within one scheduler tick and completes
/// on the live model while they wait; the youngest job is preempted, parks at its next
/// checkpoint and is requeued once the message's run finished; every job completes, the
/// preempted one reading its file once; the partition never runs more than three.
async fn k8(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let (home, workspace) = (dir.path().join("home"), dir.path().join("ws"));
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("README.md"), "a project\n")?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let stores = scripted::stores(&home, clock.clone()).await?;
    let live = Arc::new(Live::gated(model, &["job-0", "job-1", "job-2"]));
    let recorder = InMemoryAuditRecorder::new();
    let boot = Boot {
        audit: Arc::new(recorder.clone()),
        quotas: Some(Quotas::new().with_max_running(3)),
        ..setup(live.clone(), HarnessMode::Ask, clock)
    };
    let kernel = scripted::boot(&stores, &home, &workspace, boot)?;
    let tick = kernel.config().scheduler_tick;
    kernel.start().await?;

    // Samples the admitted processes.
    let sampling = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicUsize::new(0));
    let sampler = {
        let (kernel, sampling, peak) = (kernel.clone(), sampling.clone(), peak.clone());
        tokio::spawn(async move {
            while sampling.load(Ordering::SeqCst) {
                if let Ok(admitted) = admitted(&kernel).await {
                    peak.fetch_max(admitted, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    let mut jobs = Vec::new();
    for n in 0..3 {
        let process = kernel
            .processes()
            .spawn(
                run(
                    QueueClass::BACKGROUND,
                    &format!(
                        "job-{n}: Read README.md with the read_file tool, once, then reply \
                         with its first line and nothing else."
                    ),
                    &format!("k8-job-{n}"),
                ),
                ProcessSpec::new(ProcessKind::Goal),
            )
            .await?;
        jobs.push(process.meta.pid);
        let started = until(Duration::from_secs(20), || async {
            Ok(live.held.load(Ordering::SeqCst) == n + 1)
        })
        .await?;
        if !started {
            bail!("background job {n} did not reach its first model call");
        }
    }
    checks.check(
        "three background jobs hold their first model call at the quota of three",
        admitted(&kernel).await? == 3,
    );

    let typed = Instant::now();
    let message = kernel
        .processes()
        .spawn(
            run(
                QueueClass::INTERACTIVE,
                "question: Reply with the single word: ready",
                "k8-message",
            ),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await?
        .meta
        .pid;
    let reached =
        until(Duration::from_secs(10), || async { Ok(live.first("question").is_some()) }).await?;
    let latency = live.first("question").map(|at| at.duration_since(typed));
    println!("    the message reached the model {latency:?} after it was typed (tick {tick:?})");
    checks.check(
        "the message's run starts within one scheduler tick",
        reached && latency.is_some_and(|latency| latency <= tick),
    );
    let answered_msg = until(Duration::from_secs(120), || async {
        state_is(&stores.runs, &message, RunState::Completed).await
    })
    .await?;
    let mut jobs_wait = true;
    for pid in &jobs {
        jobs_wait &= !stores.runs.get(pid).await?.is_some_and(|run| run.state.is_terminal());
    }
    checks.check(
        "the message's run completes on the live model while the jobs still wait",
        answered_msg
            && jobs_wait
            && answer(&stores, "k8-message").await?.to_lowercase().contains("ready"),
    );
    let youngest = jobs[2].clone();
    let preempted = kernel.processes().get(&youngest).await?;
    checks.check(
        "the youngest job is preempted by the message",
        preempted.is_some_and(|process| {
            process.meta.suspended_by == Some(Suspension::Preempted { by: message.clone() })
        }),
    );

    // The youngest job reaches its checkpoint, parks, and is requeued: the message finished.
    live.release("job-2");
    let requeued = until(Duration::from_secs(180), || async {
        state_is(&stores.runs, &youngest, RunState::Completed).await
    })
    .await?;
    for key in ["job-0", "job-1"] {
        live.release(key);
    }
    let finished = until(Duration::from_secs(180), || async {
        let mut all = true;
        for pid in &jobs[..2] {
            all &= state_is(&stores.runs, pid, RunState::Completed).await?;
        }
        Ok(all)
    })
    .await?;
    sampling.store(false, Ordering::SeqCst);
    sampler.await?;
    let records = recorder.records();
    let recorded = |action: &str| {
        records.iter().any(|record| {
            record.action == action
                && record.ext.get("pid").and_then(Value::as_str) == Some(youngest.as_str())
        })
    };
    checks.check(
        "the youngest job parked at its checkpoint and was requeued",
        recorded(adk_kernel::audit::PROCESS_SUSPENDED)
            && recorded(adk_kernel::audit::PROCESS_REQUEUED)
            && requeued,
    );
    checks.check("every job completed", requeued && finished);
    let mut reads = Vec::new();
    for n in 0..3 {
        reads.push(answered(&stores, &format!("k8-job-{n}"), "read_file").await?);
    }
    println!("    read_file answers per job: {reads:?}");
    checks.check("the preempted job's read ran once", reads[2] == 1);
    checks.check("every job read its file", reads.iter().all(|reads| *reads >= 1));
    checks.check(
        "the partition never ran more than three admitted processes",
        peak.load(Ordering::SeqCst) <= 3,
    );
    kernel.stop().await?;
    Ok(checks)
}

/// The processes and steps of K9.
const K9_PROCESSES: usize = 6;
const K9_STEPS: u64 = 10;
/// The step tool's publisher, classed as an edit, which Auto allows (spec 3.2 §7.7).
const STEP_ORIGIN: &str = "registry:k9-steps";

/// What the `step` tool saw: executions per call key, per session and step, and when each
/// execution started on which replica.
#[derive(Default)]
struct StepLog {
    by_key: BTreeMap<String, usize>,
    by_step: BTreeMap<(String, u64), usize>,
    next: BTreeMap<String, u64>,
    starts: Vec<(&'static str, Instant)>,
}

/// One numbered step of a process's work, non-idempotent: each result names the next step.
struct Step {
    replica: &'static str,
    log: Arc<Mutex<StepLog>>,
}

#[async_trait]
impl Tool for Step {
    fn name(&self) -> &str {
        "step"
    }

    fn description(&self) -> &str {
        "Does one numbered step of the work. Call it with n = 1 first; each result names the \
         next n, or says the work is finished."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "n": { "type": "integer", "description": "the step to do" } },
            "required": ["n"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn origin(&self) -> &str {
        STEP_ORIGIN
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let n = args["n"].as_u64().unwrap_or_default();
        let session = ctx.session_id().to_string();
        let mut log = self.log.lock().unwrap_or_else(|error| error.into_inner());
        log.starts.push((self.replica, Instant::now()));
        *log.by_key.entry(ctx.idempotency_key()).or_default() += 1;
        let expected = log.next.get(&session).copied().unwrap_or(1);
        if n != expected {
            return Ok(
                json!({ "error": format!("step {n} is not next: call step with n = {expected}") }),
            );
        }
        *log.by_step.entry((session.clone(), n)).or_default() += 1;
        log.next.insert(session, n + 1);
        Ok(if n == K9_STEPS {
            json!({ "step": n, "status": "done", "finished": true })
        } else {
            json!({
                "step": n,
                "status": "done",
                "finished": false,
                "next": n + 1,
                "remaining": K9_STEPS - n,
            })
        })
    }
}

/// Classes the step tool's publisher as an edit.
#[derive(Debug)]
struct StepClass;

impl adk_harness::OriginClasses for StepClass {
    fn class_of(&self, source: &str, _tool: &str) -> Option<ToolKind> {
        (source == STEP_ORIGIN).then_some(ToolKind::Edit)
    }
}

/// An action ledger that counts how often each `step` call began.
#[derive(Debug)]
struct CountingLedger {
    inner: Arc<dyn ActionLedger>,
    begins: Arc<Mutex<BTreeMap<String, usize>>>,
}

#[async_trait]
impl ActionLedger for CountingLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        if record.tool_name == "step" {
            *self
                .begins
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(record.key.clone())
                .or_default() += 1;
        }
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

/// One kernel replica of K9: its own pool on the shared `harness.db` in `db`, its own home,
/// its kill switch mirrored from the governance store every replica shares, Auto mode with the
/// `step` tool, and an action ledger in the database.
struct Replica {
    kernel: Kernel,
    control: adk_core::GovernanceControl,
    stores: Stores,
    ledger: Arc<dyn ActionLedger>,
    live: Arc<Live>,
}

async fn replica(
    name: &'static str,
    db: &Path,
    dir: &Path,
    model: Arc<dyn Llm>,
    log: Arc<Mutex<StepLog>>,
    begins: Arc<Mutex<BTreeMap<String, usize>>>,
) -> Result<Replica> {
    let (home, workspace) = (dir.join(name), dir.join("ws"));
    std::fs::create_dir_all(&workspace)?;
    let stores = scripted::stores(db, Arc::new(SystemClock)).await?;
    let pool = stores.sessions.pool().clone();
    let governance = SqliteGovernanceStateStore::from_pool(pool.clone());
    governance.migrate().await?;
    let sqlite_ledger = SqliteActionLedger::from_pool(pool);
    sqlite_ledger.migrate().await?;
    let ledger: Arc<dyn ActionLedger> = Arc::new(sqlite_ledger);
    let counted: Arc<dyn ActionLedger> = Arc::new(CountingLedger { inner: ledger.clone(), begins });
    let control = KernelConfig::mirrored_governance(Arc::new(governance));
    let live = Arc::new(Live::new(model));
    let agent = HarnessAgent::builder(APP)
        .model(live.clone())
        .workspace(adk_devtools::Workspace::new(&workspace))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .without_default_members()
        .mode(HarnessMode::Auto)
        .approval_store(stores.approvals.clone())
        .action_ledger(counted.clone())
        .tool(Arc::new(Step { replica: name, log }))
        .tool_classes(Arc::new(StepClass))
        .max_iterations(3 * K9_STEPS as u32);
    let agent = Arc::new(agent.build()?);
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(agent.agent())
        .session_service(stores.sessions.clone())
        .approval_store(agent.runtime().approval_store().clone())
        .action_ledger(counted)
        .governance(control.clone())
        .build()?;
    let harness = Harness::with_runner(agent, runner)?
        .with_run_store(stores.runs.clone())
        .with_goal_queue(QueueClass::BACKGROUND);
    let mut config = KernelConfig::default()
        .with_app_name(APP)
        .with_home(&home)
        .with_scheduler_tick(Duration::from_millis(250))
        .with_control(ControlPlaneConfig::default().without_socket());
    // Three slots each, so each replica runs three of the six.
    config.worker = config.worker.clone().with_worker_id(name).with_max_concurrent_runs(3);
    let kernel = harness
        .kernel()
        .config(config)
        .process_store(stores.processes.clone())
        .agenda_store(stores.agenda.clone())
        .schedule_store(stores.schedules.clone())
        .build()?;
    Ok(Replica { kernel, control, stores, ledger, live })
}

/// K-I8's bound on a freeze stopping every process on every replica.
const FREEZE_STOP: Duration = Duration::from_secs(1);

/// K9 live (spec 3.2 §5): two kernel replicas over one SQLite file run six processes, three
/// each, working through numbered non-idempotent steps on the live model. A freeze on one
/// replica reaches the other within a second, and no model or tool call starts on either
/// more than a second after it; each process parks `Waiting` under the freeze on the replica
/// that ran it. The unfreeze resumes all six to completion, and the action ledger and the
/// tool show every call executed once.
async fn k9(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("db");
    let log = Arc::new(Mutex::new(StepLog::default()));
    let begins = Arc::new(Mutex::new(BTreeMap::new()));
    let mut replicas = Vec::new();
    for name in ["replica-a", "replica-b"] {
        replicas.push(
            replica(name, &db, dir.path(), model.clone(), log.clone(), begins.clone()).await?,
        );
    }
    let mut events: Vec<_> = replicas.iter().map(|replica| replica.kernel.events()).collect();
    for replica in &replicas {
        replica.kernel.start().await?;
    }

    let mut pids = Vec::new();
    for i in 0..K9_PROCESSES {
        // A freeze-parked run resumes with an empty user turn after the refused call's `not
        // run` answer, which says nothing of the unfreeze; told nothing about it, a live model
        // ends the task at that answer.
        let text = format!(
            "Process {i}: work through the numbered steps with the step tool. Call step with \
             n = 1. Each result names the next n: call step again with exactly that n, one call \
             per turn. An operator may freeze execution while you work; a step refused because \
             execution is frozen did not run, and when you are resumed, call that same step \
             again. Keep going until a result says finished, then reply with the single word: \
             done."
        );
        let process = replicas[i % 2]
            .kernel
            .processes()
            .spawn(
                run(QueueClass::BACKGROUND, &text, &format!("k9-process-{i}")),
                ProcessSpec::new(ProcessKind::Goal),
            )
            .await?;
        pids.push(process.meta.pid);
    }
    let runs = replicas[0].stores.runs.clone();
    let stepped = |at_least: u64| {
        let log = log.lock().unwrap_or_else(|error| error.into_inner());
        (0..K9_PROCESSES)
            .all(|i| log.next.get(&format!("k9-process-{i}")).copied().unwrap_or(1) > at_least)
    };
    let working = until(Duration::from_secs(180), || async {
        for pid in &pids {
            if !state_is(&runs, pid, RunState::Running).await? {
                return Ok(false);
            }
        }
        Ok(stepped(2))
    })
    .await?;
    let mut owners = BTreeMap::<String, usize>::new();
    for pid in &pids {
        if let Some(owner) = runs.get(pid).await?.and_then(|run| run.lease_owner) {
            *owners.entry(owner).or_default() += 1;
        }
    }
    println!("    leases at the freeze: {owners:?}");
    checks.check(
        "six processes run, three on each replica, each past its second step",
        working
            && owners
                == BTreeMap::from([
                    ("replica-a:background".to_string(), 3),
                    ("replica-b:background".to_string(), 3),
                ]),
    );
    if !working {
        for replica in &replicas {
            replica.kernel.stop().await?;
        }
        return Ok(checks);
    }

    let operator = AuditActor::subject(SubjectRef::human("operator"));
    let frozen = Instant::now();
    replicas[0].kernel.freeze("incident 9", operator.clone()).await?;
    let mirrored =
        until(Duration::from_secs(5), || async { Ok(replicas[1].control.is_frozen()) }).await?;
    let seen = frozen.elapsed();
    println!("    replica B saw the freeze {seen:?} after replica A recorded it");
    checks.check(
        "replica B sees replica A's freeze within a second",
        mirrored && seen <= FREEZE_STOP,
    );

    // A model call in flight at the freeze finishes; nothing starts after the bound.
    let parked = until(Duration::from_secs(120), || async {
        for pid in &pids {
            let Some(run) = runs.get(pid).await? else { return Ok(false) };
            if run.state != RunState::Waiting || run.lease_owner.is_some() {
                return Ok(false);
            }
            let meta = replicas[1].kernel.processes().get(pid).await?;
            if !meta.is_some_and(|process| process.meta.suspended_by == Some(Suspension::Freeze)) {
                return Ok(false);
            }
        }
        Ok(true)
    })
    .await?;
    let parked_in = frozen.elapsed();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let flight: Vec<_> = replicas.iter().map(|replica| replica.live.in_flight(frozen)).collect();
    println!(
        "    all six parked {parked_in:?} after the freeze; model calls in flight at it, per \
         replica, and when the last ended: {flight:?}"
    );
    checks.check("every process parks `Waiting` under the freeze, with no lease", parked);
    let bound = frozen + FREEZE_STOP;
    let model_after: Vec<usize> =
        replicas.iter().map(|replica| replica.live.started_after(bound)).collect();
    let tools_after: Vec<usize> = {
        let log = log.lock().unwrap_or_else(|error| error.into_inner());
        ["replica-a", "replica-b"]
            .iter()
            .map(|name| {
                log.starts.iter().filter(|(replica, at)| replica == name && *at > bound).count()
            })
            .collect()
    };
    println!("    calls started after the bound: model {model_after:?}, tool {tools_after:?}");
    checks.check(
        "no model call starts on either replica more than a second after the freeze",
        model_after == [0, 0],
    );
    checks.check(
        "no tool call starts on either replica more than a second after the freeze",
        tools_after == [0, 0],
    );
    for (replica, events) in events.iter_mut().enumerate() {
        let mut count = 0;
        while let Ok(Some(record)) =
            tokio::time::timeout(Duration::from_millis(100), events.recv()).await
        {
            if let KernelEvent::Process { to: RunState::Waiting, reason: Some(reason), .. } =
                record.event
                && reason == "frozen"
            {
                count += 1;
            }
        }
        checks.check(&format!("replica {} parked its own three", ["A", "B"][replica]), count == 3);
    }

    replicas[0].kernel.unfreeze(operator).await?;
    let completed = until(Duration::from_secs(300), || async {
        let mut all = true;
        for pid in &pids {
            all &= state_is(&runs, pid, RunState::Completed).await?;
        }
        Ok(all)
    })
    .await?;
    checks.check("the unfreeze resumes all six to completion", completed);
    let log = std::mem::take(&mut *log.lock().unwrap_or_else(|error| error.into_inner()));
    let begins = begins.lock().unwrap_or_else(|error| error.into_inner()).clone();
    checks.check("no tool call executed twice", log.by_key.values().all(|count| *count == 1));
    let every_step = (0..K9_PROCESSES).all(|i| {
        (1..=K9_STEPS).all(|n| log.by_step.get(&(format!("k9-process-{i}"), n)) == Some(&1))
    });
    if !every_step {
        for i in 0..K9_PROCESSES {
            let session = format!("k9-process-{i}");
            let done: Vec<u64> = log
                .by_step
                .keys()
                .filter(|(owner, _)| *owner == session)
                .map(|(_, n)| *n)
                .collect();
            println!("    {session}: steps {done:?}");
            for event in transcript(&replicas[0].stores, &session).await? {
                let Some(content) = &event.llm_response.content else { continue };
                for part in &content.parts {
                    match part {
                        Part::Text { text } => println!("      {}: {}", event.author, text.trim()),
                        Part::FunctionCall { name, args, .. } => {
                            println!("      {} -> {name} {args}", event.author)
                        }
                        Part::FunctionResponse { function_response, .. } => println!(
                            "      {} <- {} {}",
                            event.author, function_response.name, function_response.response
                        ),
                        _ => {}
                    }
                }
                if let Some(code) = &event.llm_response.error_code {
                    println!("      {} stopped: {code}", event.author);
                }
            }
        }
    }
    checks.check("every process did each of its steps exactly once", every_step);
    let mut recorded =
        begins.keys().eq(log.by_key.keys()) && begins.values().all(|count| *count == 1);
    for key in begins.keys() {
        recorded &= replicas[1].ledger.get(key).await?.is_some_and(|record| record.is_completed());
    }
    println!("    {} step calls executed, {} ledger records", log.by_key.len(), begins.len());
    checks.check(
        "the action ledger began each executed call once and recorded its outcome",
        recorded,
    );
    for replica in &replicas {
        replica.kernel.stop().await?;
    }
    Ok(checks)
}
