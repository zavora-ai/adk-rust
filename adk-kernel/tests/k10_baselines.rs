//! Scenario K10 (spec 3.2 §5, §7.10, §9.2 K-I20, §11): "it notices drift without judging".
//!
//! A kernel on a `ManualClock` keeps a content-free ledger of six weeks of a person's
//! working days: four weeks ending around 17:30, then two weeks of late finishes ending
//! around 20:30 on weekdays. Each night the clock passes midnight, the maintenance tick
//! enqueues the day's `kernel.proc` pass, the maintenance worker runs it, and the test
//! reads what it recorded:
//!
//! | Days | Rule | Expected |
//! |---|---|---|
//! | 0–27 | the 14-day warm-up, then a steady routine | no observation |
//! | 28–40 | the late finishes deviate from day 32, in one dimension | no observation: one dimension is meaningful only after ten days |
//! | 41 | the tenth day | one observation, phrased by the model as a neutral question |
//! | 42–47 | the late finishes go on | no observation: the seven-day cooldown |
//! | 48 | the cooldown ends | the question is offered again |
//!
//! Each observation is put to the person as an `Observation` agenda item: in their Auto
//! partition it waits in the inbox and no run starts; a service cannot answer it and it
//! cannot be scheduled; confirmed, it is `Done` and remembered once through the audience;
//! rejected, it is `Rejected` and nothing is remembered.
//!
//! Without `proc-baselines` the same six weeks yield no pass and no observation at all.

#![cfg(feature = "proc")]

use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, InMemoryRunStore, ManualClock, RunFilter};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::proc::{
    Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerOffset, LedgerQuery,
    LedgerSink, LedgerStore,
};
use adk_kernel::{Kernel, KernelConfig};
use adk_server::background::DurableWorkerConfig;
use chrono::{DateTime, Days, FixedOffset, NaiveDate, TimeZone as _, Utc};

/// The user's local offset: Nairobi.
fn offset() -> FixedOffset {
    FixedOffset::east_opt(3 * 3600).unwrap()
}

/// Monday 2026-08-31, day 0.
fn day(n: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap() + Days::new(n)
}

fn local(day: NaiveDate, minute: u32) -> DateTime<Utc> {
    let time = day.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
    offset().from_local_datetime(&time).unwrap().to_utc()
}

fn weekend(day: NaiveDate) -> bool {
    matches!(chrono::Datelike::weekday(&day), chrono::Weekday::Sat | chrono::Weekday::Sun)
}

/// One day of activity: a request at 09:00, twelve tool calls with their durations, and
/// the last request at `last` — 17:30 on a usual weekday, 20:30 from day 28, 11:30 on a
/// weekend. Only the end of the day moves.
async fn live_day(ledger: &Ledger, n: u64) {
    let day = day(n);
    let (calls, last) = match (weekend(day), n >= 28) {
        (true, _) => (2, 11 * 60 + 30),
        (false, false) => (12, 17 * 60 + 30),
        (false, true) => (12, 20 * 60 + 30),
    };
    let agent = "urn:adk:agent:adk-rust/harness";
    let first = 9 * 60;
    ledger.record(Activity::new(LedgerKind::Intent, agent).at(local(day, first))).await.unwrap();
    for call in 1..=calls {
        let at = first + (last - first) * call / (calls + 1);
        ledger
            .record(
                Activity::new(LedgerKind::ToolCall, agent)
                    .with_effect("read_only")
                    .with_duration(Duration::from_millis(150 + u64::from(call) * 10))
                    .with_subject(format!("read_file\u{1f}{{\"path\":\"notes/day-{n}.md\"}}"))
                    .at(local(day, at)),
            )
            .await
            .unwrap();
    }
    ledger.record(Activity::new(LedgerKind::Intent, agent).at(local(day, last))).await.unwrap();
}

/// A kernel on `clock` over `runs` and `ledger`, with fast polls and no control plane.
fn kernel(
    clock: &ManualClock,
    runs: &Arc<dyn DurableRunStore>,
    ledger: &Ledger,
    extend: impl FnOnce(adk_kernel::KernelBuilder) -> adk_kernel::KernelBuilder,
) -> Kernel {
    let worker = DurableWorkerConfig::default()
        .with_worker_id("k10")
        .with_lease_ttl(Duration::from_secs(60))
        .with_poll_interval(Duration::from_millis(20));
    let config = KernelConfig::default()
        .with_worker(worker)
        .with_scheduler_tick(Duration::from_millis(20))
        .with_control(ControlPlaneConfig::default().without_socket())
        .with_clock(Arc::new(clock.clone()));
    extend(Kernel::builder().config(config).run_store(Arc::clone(runs)).ledger(ledger.clone()))
        .build()
        .unwrap()
}

/// Polls `probe` every 10 ms until it returns `Some`, for at most five seconds.
#[cfg(feature = "proc-baselines")]
async fn eventually<T, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    mut probe: impl FnMut() -> Fut,
) -> T {
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(5) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[cfg(feature = "proc-baselines")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k10_two_weeks_of_late_finishes_yield_one_observation_after_the_warm_up_and_cooldown() {
    use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
    use adk_core::{
        ApprovalAction, ApprovalDecision, ApprovalService, InMemoryApprovalStore,
        StoreApprovalService, SubjectRef,
    };
    use adk_kernel::proc::baselines::{BaselineSnapshot, LlmPhraser};
    use adk_kernel::proc::{Direction, Observation, Phrasing, is_neutral_question};
    use adk_kernel::{
        AgendaFilter, AgendaOrigin, AgendaState, KernelError, PartitionId, Wake, WakeSet,
    };
    use adk_model::{ScriptedLlm, ScriptedResponse};

    let clock = ManualClock::new(local(day(0), 7 * 60));
    let runs: Arc<dyn DurableRunStore> =
        Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
    let store = Arc::new(InMemoryLedgerStore::new());
    let ledger = Ledger::new(store.clone(), LedgerKey::from_bytes([10; 32]))
        .with_offset(LedgerOffset::Fixed(offset()));
    let question = "Your days have been ending later than usual these past two weeks. Would \
                    you like to review what changed?";
    let model = Arc::new(ScriptedLlm::new(
        "scripted-phraser",
        vec![ScriptedResponse::text(question), ScriptedResponse::text(question)],
    ));
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let published = Arc::new(Observed::default());
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_run_store(Arc::clone(&runs)),
    );
    let alice = Arc::new(Alice::default());
    let kernel = kernel(&clock, &runs, &ledger, |builder| {
        builder
            .observation_phraser(Arc::new(LlmPhraser::new(model.clone())))
            .audit(AuditTrail::new(recorder.clone()))
            .event_sink(published.clone())
            .approval_service(Arc::clone(&approvals))
            .observation_audience(PartitionId::default(), alice.clone())
    });
    kernel.start().await.unwrap();

    let mut observed_on = Vec::new();
    for n in 0..49 {
        live_day(&ledger, n).await;
        // Five past midnight: the maintenance tick runs the pass of the day that ended.
        clock.set(local(day(n + 1), 5));
        let state = eventually(&format!("the pass of day {n}"), || async {
            store
                .pass_state(&Default::default())
                .await
                .unwrap()
                .filter(|state| state.last_day == day(n))
        })
        .await;
        let observations = store.observations(&Default::default()).await.unwrap();
        if observations.len() > observed_on.len() {
            observed_on.push(n);
        }
        assert_eq!(observations.len(), observed_on.len(), "at most one observation a day");
        if n == 41 {
            // The baselines the pass computed on the day of the first observation.
            let snapshot: BaselineSnapshot = serde_json::from_value(state.baselines).unwrap();
            let last = snapshot
                .dimensions
                .iter()
                .find(|test| test.dimension == "activity.last_minute")
                .unwrap();
            assert!(last.warm_up.is_complete() && last.deviating);
            let weekday = last.classes.iter().find(|class| class.deviating).unwrap();
            assert_eq!(weekday.baseline.unwrap().median, f64::from(17 * 60 + 30));
        }
    }
    assert_eq!(observed_on, [41, 48], "one observation, and again only after the cooldown");
    assert_eq!(model.requests().len(), 2, "the model is called only after a detection");

    let observations = store.observations(&Default::default()).await.unwrap();
    let first = observations.iter().find(|o| o.as_of == day(41)).unwrap();
    assert_eq!(first.question, question);
    assert!(is_neutral_question(&first.question));
    assert_eq!(first.phrasing, Phrasing::Model { model: "scripted-phraser".into() });
    assert_eq!(first.windows, [7, 14]);
    assert_eq!(first.dimensions.len(), 1);
    assert_eq!(
        (first.dimensions[0].dimension.as_str(), first.dimensions[0].direction),
        ("activity.last_minute", Direction::Higher)
    );
    assert_eq!((first.dimensions[0].since, first.dimensions[0].days), (day(32), 10));

    // The observation is an event, a content-free ledger row and a `kernel.observation`
    // record naming its dimensions and windows and no value.
    let ids: Vec<String> = observations.iter().map(|o| o.id.clone()).collect();
    eventually("the observation events", || {
        let seen = published.0.lock().unwrap().clone();
        let ids = ids.clone();
        async move { (seen.len() == 2 && ids.iter().all(|id| seen.contains(id))).then_some(()) }
    })
    .await;
    let rows = store.list(&LedgerQuery::default().kind("observation")).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.agent == "kernel" && row.subject_hash.is_some()));
    let audited: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.action == adk_kernel::audit::OBSERVATION)
        .collect();
    assert_eq!(audited.len(), 2);
    assert_eq!(audited[0].ext["dimensions"], serde_json::json!(["activity.last_minute"]));
    assert_eq!(audited[0].ext["windows"], serde_json::json!([7, 14]));
    assert!(!serde_json::to_string(&audited[0].ext).unwrap().contains("1050"), "no values");

    // Each pass was a `Maintenance` process, one per day, and finished.
    let passes: Vec<_> = runs
        .list(RunFilter::default().queue("maintenance"))
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.target == adk_kernel::proc::baselines::PROC_TARGET)
        .filter(|run| run.input["asOf"].as_str() >= Some("2026-08-31"))
        .collect();
    assert_eq!(passes.len(), 49);
    assert!(passes.iter().all(|run| run.state == adk_core::RunState::Completed));

    // Each observation is a question for alice: in her Auto partition it waits in the
    // inbox, open, and becomes no run (spec 3.2 §7.10).
    let items = kernel.agenda().list(&AgendaFilter::default()).await.unwrap();
    assert_eq!(items.len(), 2);
    for item in &items {
        let observation = observations.iter().find(|o| o.id == item.id).unwrap();
        assert_eq!(item.origin, AgendaOrigin::Observation { id: observation.id.clone() });
        assert_eq!(item.state, AgendaState::Proposed, "open in Auto");
        assert_eq!(item.subject, SubjectRef::human("alice"));
        assert_eq!(
            (&item.title, &item.rationale),
            (&observation.question, &observation.statement())
        );
        assert!(approvals.get(&format!("agenda_{}", item.id)).await.unwrap().is_some());
    }
    let first_asked = Observation::id_for(&PartitionId::default(), day(41));
    let second_asked = Observation::id_for(&PartitionId::default(), day(48));
    assert_eq!(
        kernel.agenda().get(&first_asked).await.unwrap().unwrap().rationale,
        "Since 2026-10-02, compared with the 28 days before: the day's last activity has been \
         later."
    );
    let error = kernel
        .agenda()
        .schedule(&first_asked, WakeSet::any_of([Wake::Manual]), &SubjectRef::human("alice"))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::AgendaState { .. }), "{error}");
    assert!(error.to_string().contains("never scheduled"), "{error}");
    let error =
        kernel.agenda().approve(&first_asked, &SubjectRef::service("scheduler")).await.unwrap_err();
    assert!(matches!(error, KernelError::Unauthorized { .. }), "{error}");

    // Confirmed in the inbox: remembered once, through the audience, and closed.
    let request = approvals.get(&format!("agenda_{first_asked}")).await.unwrap().unwrap();
    assert_eq!(request.allowed_actions, [ApprovalAction::Approve, ApprovalAction::Reject]);
    approvals
        .decide(&request.id, request.version, ApprovalDecision::approve(SubjectRef::human("alice")))
        .await
        .unwrap();
    let confirmed = eventually("the confirmation", || async {
        kernel
            .agenda()
            .get(&first_asked)
            .await
            .unwrap()
            .filter(|item| item.state == AgendaState::Done)
    })
    .await;
    let answer = confirmed.answer.unwrap();
    assert!(answer.confirmed);
    assert_eq!((answer.by, answer.fact.as_deref()), (SubjectRef::human("alice"), Some("fact-1")));
    assert_eq!(*alice.0.lock().unwrap(), [(first_asked.clone(), "alice".to_string())]);

    // Rejected: closed, and nothing is remembered.
    let rejected = kernel
        .agenda()
        .reject(&second_asked, &SubjectRef::human("alice"), Some("that was a one-off"))
        .await
        .unwrap();
    assert_eq!(rejected.state, AgendaState::Rejected);
    let answer = rejected.answer.unwrap();
    assert!(!answer.confirmed && answer.fact.is_none());
    assert_eq!(alice.0.lock().unwrap().len(), 1, "a rejection remembers nothing");

    // No run but the kernel's own passes and wake-ups, ever.
    let others: Vec<_> = runs
        .list(RunFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|run| {
            ![adk_kernel::proc::baselines::PROC_TARGET, adk_kernel::KERNEL_WAKE_TARGET]
                .contains(&run.target.as_str())
        })
        .map(|run| run.target)
        .collect();
    assert!(others.is_empty(), "{others:?}");
    kernel.stop().await.unwrap();
}

/// Alice, to whom the observations of her Auto partition are put; keeps what she confirms.
#[cfg(feature = "proc-baselines")]
#[derive(Default)]
struct Alice(std::sync::Mutex<Vec<(String, String)>>);

#[cfg(feature = "proc-baselines")]
#[async_trait::async_trait]
impl adk_kernel::ObservationAudience for Alice {
    async fn audience(
        &self,
        _partition: &adk_kernel::PartitionId,
    ) -> adk_kernel::Result<Option<(adk_core::SubjectRef, adk_kernel::PartitionMode)>> {
        Ok(Some((adk_core::SubjectRef::human("alice"), adk_kernel::PartitionMode::Auto)))
    }

    async fn confirm(
        &self,
        item: &adk_kernel::AgendaItem,
        by: &adk_core::SubjectRef,
    ) -> adk_kernel::Result<String> {
        let mut confirmed = self.0.lock().unwrap();
        confirmed.push((item.id.clone(), by.id.clone()));
        Ok(format!("fact-{}", confirmed.len()))
    }
}

/// Collects the ids of the `Observation` events the kernel publishes.
#[cfg(feature = "proc-baselines")]
#[derive(Default)]
struct Observed(std::sync::Mutex<Vec<String>>);

#[cfg(feature = "proc-baselines")]
#[async_trait::async_trait]
impl adk_kernel::EventSink for Observed {
    async fn emit(&self, event: &adk_kernel::KernelEvent) -> adk_kernel::Result<()> {
        if let adk_kernel::KernelEvent::Observation { id, .. } = event {
            self.0.lock().unwrap().push(id.clone());
        }
        Ok(())
    }
}

#[cfg(not(feature = "proc-baselines"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn k10_without_proc_baselines_runs_no_pass_and_observes_nothing() {
    let clock = ManualClock::new(local(day(0), 7 * 60));
    let runs: Arc<dyn DurableRunStore> =
        Arc::new(InMemoryRunStore::new().with_clock(Arc::new(clock.clone())));
    let store = Arc::new(InMemoryLedgerStore::new());
    let ledger = Ledger::new(store.clone(), LedgerKey::from_bytes([10; 32]))
        .with_offset(LedgerOffset::Fixed(offset()));
    let kernel = kernel(&clock, &runs, &ledger, |builder| builder);
    kernel.start().await.unwrap();
    for n in 0..42 {
        live_day(&ledger, n).await;
        clock.set(local(day(n + 1), 5));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(store.observations(&Default::default()).await.unwrap().is_empty());
    assert!(store.pass_states().await.unwrap().is_empty());
    let passes = runs
        .list(RunFilter::default())
        .await
        .unwrap()
        .into_iter()
        .filter(|run| run.target == "kernel.proc")
        .count();
    assert_eq!(passes, 0);
    // 30 weekdays of 12 calls and 12 weekend days of 2.
    assert_eq!(
        store.list(&LedgerQuery::default().kind("tool_call")).await.unwrap().len(),
        384,
        "the ledger itself is kept"
    );
    kernel.stop().await.unwrap();
}
