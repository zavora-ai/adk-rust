//! The charter's invariants (spec 3.5 §9.2), each proven by a test that fails if the rule is
//! removed: A-I13 (the site never raises a job's autonomy), A-I14 (no target before a
//! baseline, measure-only first), A-I15 (a guardrail breach pauses the job before its next
//! action, whatever the growth target) and A-I23 (approval chains by distinct people, no
//! agent holding a role).

#![cfg(feature = "charter")]

use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{Clock, ManualClock, PolicyDecision, ToolPolicy, ToolPolicyRequest};
use adk_kernel::KernelError;
use adk_kernel::charter::{
    AutonomyLadder, AutonomyReason, CHARTER_ACTION, CharterEvent, CharterEventSink, CharterStore,
    DecisionOutcome, InMemoryAutonomyStore, InMemoryCharterStore, InMemoryKpiStore, JobControl,
    JobDecision, JobPauses, KpiPhase, KpiSource, KpiStore, KpiTracker, OwnerDecision, RoleApproval,
    audit, check_chain,
};
use adk_kernel::conformance::charter::fixtures::{
    authority, charter, decision, guardrail, kpi, with_chain, with_role, with_target,
};
use awp_types::{AutonomyLevel, Role};
use chrono::{DateTime, Duration, TimeZone, Utc};
use uuid::Uuid;

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 2, 3, 0, 0).single().expect("valid")
}

/// Moves `clock` forward by `by`.
fn ahead(clock: &ManualClock, by: Duration) {
    clock.set(clock.now() + by);
}

/// Every event, in order.
#[derive(Default)]
struct Events(Mutex<Vec<CharterEvent>>);

#[async_trait::async_trait]
impl CharterEventSink for Events {
    async fn emit(&self, event: &CharterEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

impl Events {
    fn names(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|event| {
                serde_json::to_value(event).unwrap()["event"].as_str().unwrap().to_string()
            })
            .collect()
    }
}

/// A source whose value the test sets.
#[derive(Default)]
struct Dial(Mutex<f64>);

#[async_trait::async_trait]
impl KpiSource for Dial {
    fn name(&self) -> &str {
        "dial"
    }

    async fn read(
        &self,
        _: Uuid,
        _: &str,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(Some(*self.0.lock().unwrap()))
    }
}

impl Dial {
    fn set(&self, value: f64) {
        *self.0.lock().unwrap() = value;
    }
}

// ── A-I13 ──────────────────────────────────────────────────────────────────────────────

/// A promotion needs a person who owns the site; the site lowers itself freely.
#[tokio::test]
async fn a_i13_the_site_never_raises_a_jobs_autonomy() {
    let site = Uuid::new_v4();
    let charters = Arc::new(InMemoryCharterStore::new());
    charters
        .commit(
            with_role(charter(site, &["amina"]), Role::Editor, "eva"),
            decision("v1", start(), &[(Role::Owner, "amina")]),
        )
        .await
        .unwrap();
    let recorder = InMemoryAuditRecorder::new();
    let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()))
        .with_charters(charters)
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    ladder.register(site, "review-replies", AutonomyLevel::Propose, Some("serve")).await.unwrap();
    let refused = [
        OwnerDecision::new(
            "d-agent",
            SubjectRef::new(adk_core::principal::SubjectKind::Agent, "urn:adk:agent:site/steward"),
            start(),
        ),
        OwnerDecision::new("d-urn", SubjectRef::human("urn:adk:agent:site/steward"), start()),
        OwnerDecision::new("d-service", SubjectRef::service("kernel:autonomy"), start()),
        OwnerDecision::new("d-editor", SubjectRef::human("eva"), start()),
        OwnerDecision::new("", SubjectRef::human("amina"), start()),
    ];
    for refused in &refused {
        let error =
            ladder.promote(site, "review-replies", refused).await.expect_err(&refused.decision_id);
        assert!(matches!(error, KernelError::Unauthorized { .. }), "{error}");
    }
    assert_eq!(ladder.level(site, "review-replies").await.unwrap(), Some(AutonomyLevel::Propose));
    let owner = OwnerDecision::new("d-owner", SubjectRef::human("amina"), start())
        .with_signed_note("note-9");
    let promoted = ladder.promote(site, "review-replies", &owner).await.unwrap();
    assert_eq!(promoted.level, AutonomyLevel::ActWithApproval);
    let history = ladder.store().history(site, "review-replies").await.unwrap();
    assert_eq!(
        history.last().unwrap().reason,
        AutonomyReason::Promoted { decision: owner.clone() },
        "the change carries the owner's decision, from which the harness makes the standing rule"
    );
    let lowered = ladder.lower(site, "review-replies", "the site lowers itself").await.unwrap();
    assert_eq!(lowered.unwrap().level, AutonomyLevel::Propose);
    let actions: Vec<String> = recorder.records().into_iter().map(|record| record.action).collect();
    assert_eq!(actions, [audit::JOB_PROMOTED, audit::JOB_DEMOTED]);
}

/// Only an owner's decisions count towards a promotion; two rejections in a row demote.
#[tokio::test]
async fn a_i13_promotion_is_earned_by_owner_decisions_and_lost_by_rejections() {
    let site = Uuid::new_v4();
    let clock = Arc::new(ManualClock::new(start()));
    let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()))
        .with_clock(clock.clone() as Arc<dyn Clock>);
    ladder.register(site, "replies", AutonomyLevel::Propose, None).await.unwrap();
    let decide = |id: usize, outcome, by: SubjectRef, at| {
        JobDecision::new(site, "replies", format!("d-{id}"), outcome, by, at)
    };
    let agent = decide(
        0,
        DecisionOutcome::ApprovedUnchanged,
        SubjectRef::service("urn:adk:agent:site/steward"),
        start(),
    );
    assert!(matches!(ladder.record_decision(agent).await, Err(KernelError::Unauthorized { .. })));
    for id in 1..=19 {
        ahead(&clock, Duration::hours(30));
        let at = clock.now();
        ladder
            .record_decision(decide(
                id,
                DecisionOutcome::ApprovedUnchanged,
                SubjectRef::human("amina"),
                at,
            ))
            .await
            .unwrap();
    }
    let nineteen = ladder.eligibility(site, "replies").await.unwrap();
    assert_eq!((nineteen.streak, nineteen.eligible), (19, false));
    ahead(&clock, Duration::hours(1));
    ladder
        .record_decision(decide(
            20,
            DecisionOutcome::ApprovedUnchanged,
            SubjectRef::human("amina"),
            clock.now(),
        ))
        .await
        .unwrap();
    let twenty = ladder.eligibility(site, "replies").await.unwrap();
    assert_eq!(
        (twenty.streak, twenty.eligible, twenty.next),
        (20, true, Some(AutonomyLevel::ActWithApproval))
    );
    assert_eq!(ladder.offers(site).await.unwrap().len(), 1);
    // Thirty days later the oldest approvals fall out of the window.
    ahead(&clock, Duration::days(10));
    assert!(!ladder.eligibility(site, "replies").await.unwrap().eligible);
    // An edit breaks the streak; the second rejection in a row demotes.
    ladder
        .record_decision(decide(
            21,
            DecisionOutcome::ApprovedWithEdits,
            SubjectRef::human("amina"),
            clock.now(),
        ))
        .await
        .unwrap();
    assert_eq!(ladder.eligibility(site, "replies").await.unwrap().streak, 0);
    assert!(
        ladder
            .record_decision(decide(
                22,
                DecisionOutcome::Rejected,
                SubjectRef::human("amina"),
                clock.now()
            ))
            .await
            .unwrap()
            .is_none()
    );
    let demoted = ladder
        .record_decision(decide(
            23,
            DecisionOutcome::Rejected,
            SubjectRef::human("amina"),
            clock.now(),
        ))
        .await
        .unwrap()
        .expect("two rejections in a row demote");
    assert_eq!(demoted.level, AutonomyLevel::Observe);
    let history = ladder.store().history(site, "replies").await.unwrap();
    assert_eq!(
        history.last().unwrap().reason,
        AutonomyReason::OwnerRejections { decisions: vec!["d-22".into(), "d-23".into()] }
    );
}

/// An incident lowers the jobs of its area one level; a charter that sets a lower level
/// lowers a job, and one that sets a higher level raises nothing.
#[tokio::test]
async fn a_i13_incidents_and_charters_only_lower() {
    let site = Uuid::new_v4();
    let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()));
    ladder
        .register(site, "rollback", AutonomyLevel::ActReversibleOnly, Some("run-and-protect"))
        .await
        .unwrap();
    ladder.register(site, "seo", AutonomyLevel::ActWithinBudget, Some("be-found")).await.unwrap();
    let lowered = ladder.incident(site, "run-and-protect", "incident-7").await.unwrap();
    assert_eq!(lowered.len(), 1);
    assert_eq!(lowered[0].level, AutonomyLevel::ActWithApproval);
    assert_eq!(ladder.level(site, "seo").await.unwrap(), Some(AutonomyLevel::ActWithinBudget));
    let mut document = charter(site, &["amina"]);
    document.version = 2;
    document.authority = vec![
        authority("seo", AutonomyLevel::Propose),
        authority("rollback", AutonomyLevel::ActWithinBudget),
        authority("weekly-review", AutonomyLevel::Observe),
    ];
    let jobs = ladder.sync(&document).await.unwrap();
    let levels: Vec<(String, AutonomyLevel)> =
        jobs.into_iter().map(|job| (job.job, job.level)).collect();
    assert_eq!(
        levels,
        [
            ("seo".to_string(), AutonomyLevel::Propose),
            ("rollback".to_string(), AutonomyLevel::ActWithApproval),
            ("weekly-review".to_string(), AutonomyLevel::Observe),
        ]
    );
}

/// Syncing the charter in force again — every start of a site does — keeps the owner's
/// promotions made after it took effect; a later charter that sets a lower level still
/// lowers the job (A-I13, §7.11).
#[tokio::test]
async fn a_i13_a_resync_keeps_the_owners_later_promotion() {
    let site = Uuid::new_v4();
    let clock = Arc::new(ManualClock::new(start()));
    let charters = Arc::new(InMemoryCharterStore::new());
    let mut first = charter(site, &["amina"]);
    first.authority = vec![authority("social", AutonomyLevel::Propose)];
    let v1 = charters
        .commit(first.clone(), decision("dec-v1", clock.now(), &[(Role::Owner, "amina")]))
        .await
        .unwrap();
    let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()))
        .with_charters(charters.clone())
        .with_clock(clock.clone() as Arc<dyn Clock>);
    ladder.sync(&v1.charter).await.unwrap();
    ahead(&clock, Duration::days(30));
    let owner = OwnerDecision::new("dec-promote", SubjectRef::human("amina"), clock.now());
    ladder.promote(site, "social", &owner).await.unwrap();

    ahead(&clock, Duration::days(1));
    ladder.sync(&v1.charter).await.unwrap();
    assert_eq!(
        ladder.level(site, "social").await.unwrap(),
        Some(AutonomyLevel::ActWithApproval),
        "the owner promoted the job after charter version 1 took effect"
    );

    ahead(&clock, Duration::days(1));
    let v2 = charters
        .commit(first, decision("dec-v2", clock.now(), &[(Role::Owner, "amina")]))
        .await
        .unwrap();
    let jobs = ladder.sync(&v2.charter).await.unwrap();
    assert_eq!(jobs[0].level, AutonomyLevel::Propose, "charter version 2 sets it lower");
}

// ── A-I14 ──────────────────────────────────────────────────────────────────────────────

/// A KPI without a baseline is measured for 14 days with no target, then its baseline is
/// locked; a target counts only once the owner's charter gives it a baseline.
#[tokio::test]
async fn a_i14_no_target_before_a_baseline() {
    let site = Uuid::new_v4();
    let clock = Arc::new(ManualClock::new(start()));
    let charters = Arc::new(InMemoryCharterStore::new());
    let mut document = charter(site, &["amina"]);
    document.kpis.push(kpi("leads", "leads.qualified", "dial"));
    charters
        .commit(document.clone(), decision("v1", start(), &[(Role::Owner, "amina")]))
        .await
        .unwrap();
    // A target set before the baseline is refused.
    let mut early = document.clone();
    early.kpis[0].target = with_target(early.kpis[0].clone(), 1.0, 2.0, start()).target;
    let error = charters
        .commit(early, decision("v2-early", start(), &[(Role::Owner, "amina")]))
        .await
        .expect_err("a target without a baseline (A-I14)");
    assert!(matches!(error, KernelError::Config { .. }), "{error}");
    let dial = Arc::new(Dial::default());
    let events = Arc::new(Events::default());
    let kpis = Arc::new(InMemoryKpiStore::new());
    let tracker = KpiTracker::new(site, charters.clone(), kpis.clone())
        .with_source(dial.clone())
        .with_events(events.clone())
        .with_clock(clock.clone() as Arc<dyn Clock>);
    for day in 0..14 {
        dial.set(f64::from(day % 3 + 4));
        let report = tracker.tick().await.unwrap();
        let status = &report.kpis[0];
        assert!(matches!(status.phase, KpiPhase::MeasureOnly { .. }), "day {day}: {status:?}");
        assert_eq!((status.baseline, status.target), (None, None), "day {day}");
        assert!(report.locked.is_empty(), "day {day}");
        ahead(&clock, Duration::days(1));
    }
    dial.set(9.0);
    let report = tracker.tick().await.unwrap();
    assert_eq!(report.locked, ["leads"]);
    let samples = kpis.samples(site, "leads", start(), clock.now()).await.unwrap();
    let mean = samples.iter().map(|s| s.value).sum::<f64>() / samples.len() as f64;
    assert_eq!(report.kpis[0].baseline, Some(mean));
    assert_eq!(report.kpis[0].phase, KpiPhase::Tracking);
    assert_eq!(report.kpis[0].target, None, "the owner has not set one");
    assert_eq!(events.names(), ["baselineLocked"]);
    // The owner records the baseline and sets a target.
    let mut with_goal = document.clone();
    with_goal.kpis[0] = with_target(with_goal.kpis[0].clone(), mean, 8.0, clock.now());
    charters
        .commit(with_goal, decision("v2", clock.now(), &[(Role::Owner, "amina")]))
        .await
        .unwrap();
    ahead(&clock, Duration::days(1));
    let report = tracker.tick().await.unwrap();
    assert_eq!(report.kpis[0].target, Some(8.0));
    assert_eq!(report.kpis[0].meets_target, Some(true));
}

// ── A-I15 ──────────────────────────────────────────────────────────────────────────────

/// A breach pauses its job before the job's next call, before anything else the breach
/// causes, even while the job's KPI beats its target; the job is demoted one level.
#[tokio::test]
async fn a_i15_a_breach_pauses_the_job_before_its_next_action() {
    let site = Uuid::new_v4();
    let clock = Arc::new(ManualClock::new(start()));
    let charters = Arc::new(InMemoryCharterStore::new());
    let mut document = charter(site, &["amina"]);
    document.kpis.push(with_target(
        kpi("signups", "newsletter.signups", "dial"),
        10.0,
        12.0,
        start(),
    ));
    document.kpis.push(kpi("unsubscribes", "newsletter.unsubscribe_rate", "unsubscribes"));
    document.guardrails.push(guardrail(
        "unsubscribes",
        "newsletter.unsubscribe_rate",
        0.02,
        7,
        &["nurture"],
    ));
    document.authority.push(authority("nurture", AutonomyLevel::ActWithinBudget));
    charters
        .commit(document.clone(), decision("v1", start(), &[(Role::Owner, "amina")]))
        .await
        .unwrap();

    struct Unsubscribes(Mutex<f64>);
    #[async_trait::async_trait]
    impl KpiSource for Unsubscribes {
        fn name(&self) -> &str {
            "unsubscribes"
        }
        async fn read(
            &self,
            _: Uuid,
            _: &str,
            _: DateTime<Utc>,
            _: DateTime<Utc>,
        ) -> adk_kernel::Result<Option<f64>> {
            Ok(Some(*self.0.lock().unwrap()))
        }
    }
    let rate = Arc::new(Unsubscribes(Mutex::new(0.01)));
    let signups = Arc::new(Dial::default());
    signups.set(20.0);
    let pauses = Arc::new(JobPauses::new().with_agent("nurture", "revenue"));
    let events = Arc::new(Events::default());
    let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new()))
        .with_clock(clock.clone() as Arc<dyn Clock>)
        .with_events(events.clone());
    ladder.sync(&charters.current(site).await.unwrap().unwrap().charter).await.unwrap();
    let recorder = InMemoryAuditRecorder::new();
    let tracker = KpiTracker::new(site, charters.clone(), Arc::new(InMemoryKpiStore::new()))
        .with_source(signups.clone())
        .with_source(rate.clone())
        .with_control(pauses.clone())
        .with_ladder(ladder.clone())
        .with_events(events.clone())
        .with_audit(AuditTrail::new(Arc::new(recorder.clone())))
        .with_clock(clock.clone() as Arc<dyn Clock>);
    let mut call = ToolPolicyRequest::new("send_sequence_email", serde_json::json!({}));
    call.agent_name = "revenue".into();

    let calm = tracker.tick().await.unwrap();
    assert!(calm.breached.is_empty());
    assert_eq!(pauses.evaluate(&call).await, PolicyDecision::Allow);

    ahead(&clock, Duration::days(1));
    *rate.0.lock().unwrap() = 0.09;
    let breach = tracker.tick().await.unwrap();
    assert_eq!(breach.breached, ["unsubscribes"]);
    assert_eq!(breach.paused, ["nurture"]);
    assert_eq!(breach.kpis[0].meets_target, Some(true), "the growth target is met, and overridden");
    assert!(
        matches!(pauses.evaluate(&call).await, PolicyDecision::Deny { .. }),
        "the next call is denied"
    );
    assert!(pauses.paused(site, "nurture").await.unwrap().is_some());
    assert_eq!(
        ladder.level(site, "nurture").await.unwrap(),
        Some(AutonomyLevel::ActReversibleOnly)
    );
    assert_eq!(
        events.names(),
        ["jobPaused", "guardrailBreached", "autonomyChanged"],
        "the pause comes first"
    );
    assert!(
        events.0.lock().unwrap().iter().all(|e| !matches!(e, CharterEvent::JobPaused { .. })
            || e.urgency() == awp_types::Urgency::P2)
    );
    assert_eq!(recorder.records().iter().filter(|r| r.action == audit::JOB_PAUSED).count(), 1);

    // A breach acts once: the next tick neither pauses nor demotes again.
    ahead(&clock, Duration::days(1));
    let still = tracker.tick().await.unwrap();
    assert!(still.breached.is_empty() && still.paused.is_empty());
    assert_eq!(
        ladder.level(site, "nurture").await.unwrap(),
        Some(AutonomyLevel::ActReversibleOnly)
    );

    let owner = OwnerDecision::new("resume-1", SubjectRef::human("amina"), clock.now());
    assert!(pauses.resume(site, "nurture", &owner).await.unwrap());
    assert_eq!(pauses.evaluate(&call).await, PolicyDecision::Allow);
    let agent = OwnerDecision::new(
        "resume-2",
        SubjectRef::service("urn:adk:agent:site/steward"),
        clock.now(),
    );
    assert!(pauses.resume(site, "nurture", &agent).await.is_err(), "only a person resumes a job");
}

/// Pauses survive a restart: the breach the store holds pauses the job again.
#[tokio::test]
async fn a_i15_a_restart_restores_the_pause() {
    let site = Uuid::new_v4();
    let clock = Arc::new(ManualClock::new(start()));
    let charters = Arc::new(InMemoryCharterStore::new());
    let mut document = charter(site, &["amina"]);
    document.kpis.push(kpi("complaints", "reviews.complaints", "dial"));
    document.guardrails.push(guardrail(
        "complaints",
        "reviews.complaints",
        3.0,
        7,
        &["review-replies"],
    ));
    charters.commit(document, decision("v1", start(), &[(Role::Owner, "amina")])).await.unwrap();
    let kpis: Arc<dyn KpiStore> = Arc::new(InMemoryKpiStore::new());
    let dial = Arc::new(Dial::default());
    dial.set(5.0);
    let before = Arc::new(JobPauses::new());
    KpiTracker::new(site, charters.clone(), kpis.clone())
        .with_source(dial.clone())
        .with_control(before.clone())
        .with_clock(clock.clone() as Arc<dyn Clock>)
        .tick()
        .await
        .unwrap();
    assert!(before.paused(site, "review-replies").await.unwrap().is_some());
    let after = Arc::new(JobPauses::new());
    let restored = KpiTracker::new(site, charters, kpis)
        .with_control(after.clone())
        .restore_pauses()
        .await
        .unwrap();
    assert_eq!(restored, ["review-replies"]);
    assert!(after.paused(site, "review-replies").await.unwrap().is_some());
}

// ── A-I23 ──────────────────────────────────────────────────────────────────────────────

/// A chain completes only when every role approved, each by a distinct person who holds
/// it, no agent among them, and not the proposer.
#[tokio::test]
async fn a_i23_approval_chains_need_distinct_people() {
    let site = Uuid::new_v4();
    let governance = with_chain(
        with_role(
            with_role(
                with_role(charter(site, &["amina"]), Role::Editor, "eva"),
                Role::Publisher,
                "paul",
            ),
            Role::Publisher,
            "eva",
        ),
        "publish",
        &[Role::Editor, Role::Publisher],
    )
    .governance;
    let now = start();
    let publish = awp_types::ActionClass::Publish;
    let by = |role, who: &str| RoleApproval::new(role, SubjectRef::human(who), now);
    let ok = check_chain(
        &governance,
        &publish,
        &[by(Role::Publisher, "eva"), by(Role::Editor, "eva")],
        None,
    );
    assert!(ok.is_err(), "one person fills one role");
    assert!(
        check_chain(
            &governance,
            &publish,
            &[by(Role::Editor, "eva"), by(Role::Publisher, "paul")],
            None
        )
        .is_ok()
    );
    // Eva approves as editor and publisher, Paul as publisher: a matching exists.
    assert!(
        check_chain(
            &governance,
            &publish,
            &[by(Role::Publisher, "eva"), by(Role::Editor, "eva"), by(Role::Publisher, "paul")],
            None
        )
        .is_ok()
    );
    assert!(
        check_chain(
            &governance,
            &publish,
            &[by(Role::Editor, "eva"), by(Role::Publisher, "paul")],
            Some("eva")
        )
        .is_err(),
        "the proposer cannot approve"
    );
    assert!(
        check_chain(&governance, &publish, &[by(Role::Editor, "eva")], None).is_err(),
        "every role approves"
    );
    assert!(
        check_chain(
            &governance,
            &publish,
            &[by(Role::Editor, "eva"), by(Role::Publisher, "amina")],
            None
        )
        .is_err(),
        "amina holds no publisher role"
    );
    let agent = RoleApproval::new(
        Role::Publisher,
        SubjectRef::new(adk_core::principal::SubjectKind::Agent, "paul"),
        now,
    );
    assert!(
        check_chain(&governance, &publish, &[by(Role::Editor, "eva"), agent], None).is_err(),
        "no agent approves"
    );
    // The charter chain defaults to one owner.
    assert!(check_chain(&governance, &CHARTER_ACTION, &[by(Role::Owner, "amina")], None).is_ok());
    assert!(check_chain(&governance, &CHARTER_ACTION, &[], None).is_err());
}

/// An audited commit records the satisfied chain and the commit, or the refusal, naming the
/// approvers and never the charter's text.
#[tokio::test]
async fn a_i23_commits_and_refusals_are_recorded() {
    use adk_kernel::charter::commit_audited;

    let site = Uuid::new_v4();
    let recorder = InMemoryAuditRecorder::new();
    let trail = AuditTrail::new(Arc::new(recorder.clone()));
    let store = InMemoryCharterStore::new();
    let draft = with_chain(
        charter(site, &["amina", "baraka"]),
        CHARTER_ACTION,
        &[Role::Owner, Role::Owner],
    );
    let refused = commit_audited(
        &store,
        draft.clone(),
        decision("d-1", start(), &[(Role::Owner, "amina")]),
        Some(&trail),
    )
    .await;
    assert!(matches!(refused, Err(KernelError::Unauthorized { .. })));
    commit_audited(
        &store,
        draft,
        decision("d-2", start(), &[(Role::Owner, "amina"), (Role::Owner, "baraka")]),
        Some(&trail),
    )
    .await
    .unwrap();
    let records = recorder.records();
    let actions: Vec<&str> = records.iter().map(|record| record.action.as_str()).collect();
    assert_eq!(
        actions,
        [audit::APPROVAL_CHAIN_REFUSED, audit::APPROVAL_CHAIN_SATISFIED, audit::CHARTER_COMMITTED]
    );
    assert_eq!(records[2].ext["charterVersion"], 1);
    assert_eq!(records[2].ext["approvers"], serde_json::json!(["amina", "baraka"]));
    let written =
        serde_json::to_string(&records.iter().map(|r| &r.ext).collect::<Vec<_>>()).unwrap();
    assert!(!written.contains("solar"), "no charter text in the records");
}

/// A-I23 over the approval service (#1280): with a `CharterDirectory`, a chain's slots go to
/// the people the charter in force names, whatever `role:` groups a surface asserts; an
/// agent holds none; one person fills one slot; a request for a site with no charter gets no
/// groups; before any charter is in force, the charter a signing request carries names its
/// own signers.
#[tokio::test]
async fn the_charter_directory_decides_who_fills_a_chains_slots() {
    use adk_core::{
        ApprovalDecision, ApprovalRequest, ApprovalScope, ApprovalService, ApprovalState,
        ApproverDirectory, InMemoryApprovalStore, StoreApprovalService, ToolConfirmationRequest,
    };
    use adk_kernel::charter::{CharterDirectory, chain_policy, role_group};
    use awp_types::ActionClass;

    let site = Uuid::new_v4();
    let store = Arc::new(InMemoryCharterStore::new());
    let directory = Arc::new(CharterDirectory::new(store.clone()));
    let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
        .with_directory(directory.clone());
    let first = with_role(charter(site, &["amina"]), Role::Editor, "eva");
    let first = with_role(first, Role::Publisher, "eva");
    let first = with_role(first, Role::Publisher, "paul");
    let first = with_chain(first, ActionClass::Publish, &[Role::Editor, Role::Publisher]);
    let call = |args: serde_json::Value| ToolConfirmationRequest::new("site_release", args);
    let scope = ApprovalScope::new("solar", "amina", "site-approvals");

    // Before a charter is in force, a signing request's own charter names its signers.
    let signing = ApprovalRequest::new(
        "sign-1",
        scope.clone(),
        &call(serde_json::json!({ "site": site, "charter": first })),
        chain_policy(&first.governance, &CHARTER_ACTION),
    );
    assert_eq!(
        directory.groups(&SubjectRef::human("amina"), &signing).await.unwrap(),
        [role_group(Role::Owner)]
    );
    store.commit(first.clone(), decision("d-1", start(), &[(Role::Owner, "amina")])).await.unwrap();

    let release = service
        .submit(
            ApprovalRequest::new(
                "release-1",
                scope.clone(),
                &call(serde_json::json!({ "site": site, "items": ["pages/hours/en"] })),
                chain_policy(&first.governance, &ActionClass::Publish),
            )
            .with_requester(SubjectRef::human("baraka")),
        )
        .await
        .unwrap();
    let claimed = ApprovalDecision::approve(SubjectRef::human("mallory"))
        .with_groups([role_group(Role::Editor)]);
    assert_eq!(
        service.decide(&release.id, release.version, claimed).await.unwrap_err().code,
        "approval.not_an_approver",
        "a group the surface asserts does not count"
    );
    let agent = SubjectRef::human("urn:adk:agent:solar/steward");
    assert!(directory.groups(&agent, &release).await.unwrap().is_empty(), "no agent holds a role");
    let other_site = ApprovalRequest::new(
        "release-2",
        scope,
        &call(serde_json::json!({ "site": Uuid::new_v4() })),
        chain_policy(&first.governance, &ActionClass::Publish),
    );
    assert!(directory.groups(&SubjectRef::human("eva"), &other_site).await.unwrap().is_empty());

    let eva = service
        .decide(&release.id, release.version, ApprovalDecision::approve(SubjectRef::human("eva")))
        .await
        .unwrap();
    assert_eq!(eva.state, ApprovalState::Pending, "eva fills one slot, not both");
    let paul = service
        .decide(&eva.id, eva.version, ApprovalDecision::approve(SubjectRef::human("paul")))
        .await
        .unwrap();
    assert_eq!(paul.state, ApprovalState::Approved, "eva and paul fill one slot each");
}
