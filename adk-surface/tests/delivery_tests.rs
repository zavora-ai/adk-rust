//! What happens after an answer (spec 3.3 §6, §7.2, §7.4, §8.2, §11): input queued behind a
//! decision or a running turn is sent, oldest first, however the decision is answered and
//! however long the turn runs; a recorded answer always resumes its call and sends what is
//! queued; "Always allow" grants its rule only once the request was approved, and never for a
//! request a quorum must approve; spoken answers are withdrawn by Stop all and shown again
//! when the decision changed; a decision found decided after a conflict reads decided.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditRecorder, AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope,
    ApprovalService, ApprovalState, ApproverSelector, ErrorCategory, ErrorComponent, Event,
    EventActions, InMemoryApprovalStore, ManualClock, StoreApprovalService,
    ToolConfirmationRequest,
};
use adk_harness::HarnessMode;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{
    AlwaysAllowOffer, Answer, Caller, Command, CommandOutcome, LocalSource, Mode, Origin,
    RuleCommit, Source, SurfaceEngine, SurfaceError, SurfaceKind,
};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde_json::json;

const APP: &str = "home";
const WITHIN: Duration = Duration::from_secs(10);

/// A source over in-memory stores that records the turns it starts, the decided calls it
/// resumes and the standing rules it grants; its busy flag, its failures and whether it still
/// finds a resolved request are set by the test.
struct Recording {
    inner: LocalSource,
    sent: Mutex<Vec<String>>,
    resumed: Mutex<Vec<String>>,
    granted: Arc<Mutex<Vec<String>>>,
    busy: AtomicBool,
    /// Errors the next turns fail with, in order.
    start_failures: Mutex<Vec<SurfaceError>>,
    /// How many of the next inbox reads fail.
    read_failures: AtomicUsize,
    offers_always: bool,
    /// Finds only open requests, as the control plane over a named pipe does.
    open_only: bool,
    /// Someone else answers the next decision first, between the engine's read and its own
    /// answer.
    answered_first_by: Mutex<Option<String>>,
    mode: Mutex<HarnessMode>,
}

struct Grant {
    id: String,
    granted: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl RuleCommit for Grant {
    async fn commit(self: Box<Self>) -> adk_surface::Result<()> {
        self.granted.lock().unwrap().push(self.id);
        Ok(())
    }
}

fn unavailable() -> SurfaceError {
    SurfaceError::Source(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unavailable,
        "test.unavailable",
        "the kernel is restarting",
    ))
}

#[async_trait]
impl Source for Recording {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(
        &self,
        user: &str,
        session: &str,
    ) -> adk_surface::Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> adk_surface::Result<Vec<ApprovalRequest>> {
        if self
            .read_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
            .is_ok()
        {
            return Err(unavailable());
        }
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> adk_surface::Result<Option<ApprovalRequest>> {
        let found = self.inner.request(id).await?;
        Ok(found.filter(|request| !self.open_only || request.state == ApprovalState::Pending))
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> adk_surface::Result<ApprovalRequest> {
        let first = self.answered_first_by.lock().unwrap().take();
        if let Some(who) = first {
            self.inner
                .decide(
                    id,
                    expected_version,
                    ApprovalDecision::approve(SubjectRef::human(who)).with_groups(["owner"]),
                )
                .await?;
        }
        self.inner.decide(id, expected_version, decision).await
    }

    async fn resolved_by(
        &self,
        _caller: &Caller,
        request: &ApprovalRequest,
    ) -> adk_surface::Result<()> {
        self.resumed.lock().unwrap().push(request.id.clone());
        Ok(())
    }

    async fn always_allow_offer(&self, _request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        self.offers_always
            .then(|| AlwaysAllowOffer { label: "Always allow for this person".into() })
    }

    async fn prepare_always_allow(
        &self,
        _caller: &Caller,
        request: &ApprovalRequest,
    ) -> adk_surface::Result<Box<dyn RuleCommit>> {
        Ok(Box::new(Grant { id: request.id.clone(), granted: Arc::clone(&self.granted) }))
    }

    async fn busy(&self, _user: &str, _session: &str) -> adk_surface::Result<bool> {
        Ok(self.busy.load(Ordering::SeqCst))
    }

    async fn waiting_on_decision(&self, user: &str, session: &str) -> adk_surface::Result<bool> {
        self.inner.waiting_on_decision(user, session).await
    }

    async fn start_turn(
        &self,
        _caller: &Caller,
        _session: &str,
        text: &str,
    ) -> adk_surface::Result<()> {
        if let Some(error) = self.start_failures.lock().unwrap().pop() {
            return Err(error);
        }
        self.sent.lock().unwrap().push(text.to_string());
        Ok(())
    }

    async fn stop_all(&self, caller: &Caller, reason: &str) -> adk_surface::Result<()> {
        self.inner.stop_all(caller, reason).await
    }

    async fn mode(
        &self,
        _user: &str,
        _session: Option<&str>,
    ) -> adk_surface::Result<Option<HarnessMode>> {
        Ok(Some(*self.mode.lock().unwrap()))
    }

    async fn set_mode(
        &self,
        _caller: &Caller,
        _session: &str,
        mode: HarnessMode,
    ) -> adk_surface::Result<()> {
        *self.mode.lock().unwrap() = mode;
        Ok(())
    }
}

/// Records every draft except those `refuse` matches, which fail as a full disk would.
#[derive(Clone)]
struct Refusing {
    records: InMemoryAuditRecorder,
    refuse: Refuse,
}

#[async_trait]
impl AuditRecorder for Refusing {
    fn digest(&self, value: &serde_json::Value) -> String {
        self.records.digest(value)
    }

    async fn record(&self, draft: AuditRecordDraft) -> adk_core::Result<()> {
        if (self.refuse)(&draft) {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Unavailable,
                "audit.disk_full",
                "the audit log's disk is full",
            ));
        }
        self.records.record(draft).await
    }
}

struct Home {
    engine: SurfaceEngine,
    source: Arc<Recording>,
    service: Arc<dyn ApprovalService>,
    sessions: Arc<dyn SessionService>,
    audit: InMemoryAuditRecorder,
}

/// Which audit drafts fail.
type Refuse = Arc<dyn Fn(&AuditRecordDraft) -> bool + Send + Sync>;

#[derive(Default)]
struct Setup {
    offers_always: bool,
    open_only: bool,
    refuse: Option<Refuse>,
    voice_window: Option<Duration>,
}

async fn home_with(setup: Setup) -> Home {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_clock(Arc::new(ManualClock::new(
                Utc.with_ymd_and_hms(2026, 10, 23, 18, 0, 0).unwrap(),
            )))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
    let inner = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .governance(adk_core::GovernanceControl::new())
        .build()
        .unwrap();
    let source = Arc::new(Recording {
        inner,
        sent: Mutex::default(),
        resumed: Mutex::default(),
        granted: Arc::default(),
        busy: AtomicBool::new(false),
        start_failures: Mutex::default(),
        read_failures: AtomicUsize::new(0),
        offers_always: setup.offers_always,
        open_only: setup.open_only,
        answered_first_by: Mutex::default(),
        mode: Mutex::new(HarnessMode::Ask),
    });
    let audit = InMemoryAuditRecorder::new();
    let recorder = Refusing {
        records: audit.clone(),
        refuse: setup.refuse.unwrap_or_else(|| Arc::new(|_: &AuditRecordDraft| false)),
    };
    let engine = SurfaceEngine::builder(Arc::clone(&source) as Arc<dyn Source>)
        .audit(AuditTrail::new(Arc::new(recorder)))
        .poll_interval(Duration::from_millis(10))
        .voice_cancel_window(setup.voice_window.unwrap_or(Duration::from_millis(80)))
        .build();
    Home { engine, source, service, sessions, audit }
}

async fn home() -> Home {
    home_with(Setup::default()).await
}

fn call(tool: &str) -> ToolConfirmationRequest {
    ToolConfirmationRequest {
        tool_name: tool.into(),
        function_call_id: Some(format!("call-{tool}")),
        args: json!({ "to": "[client email]" }),
        origin: None,
        acting: None,
    }
}

/// A call of ana's session `s1` held for `policy`, in the inbox and in the session.
async fn hold_for(
    home: &Home,
    tool: &str,
    risk: ApprovalRisk,
    policy: ApprovalPolicy,
) -> ApprovalRequest {
    let call = call(tool);
    let request = ApprovalRequest::new(
        format!("apr-{tool}"),
        ApprovalScope::new(APP, "ana", "s1"),
        &call,
        policy,
    )
    .with_risk(risk);
    let mut event = Event::new("inv-1");
    event.author = "finance".into();
    event.actions = EventActions { tool_confirmation: Some(call), ..EventActions::default() };
    home.sessions.append_event("s1", event).await.unwrap();
    home.service.submit(request).await.unwrap()
}

async fn hold(home: &Home, tool: &str, risk: ApprovalRisk) -> ApprovalRequest {
    hold_for(
        home,
        tool,
        risk,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .await
}

fn caller(surface: SurfaceKind, origin: Origin) -> Caller {
    Caller::human("ana", surface, origin).with_groups(["owner"])
}

fn web() -> Caller {
    caller(SurfaceKind::Web, Origin::Pointer)
}

fn voice() -> Caller {
    caller(SurfaceKind::Voice, Origin::Voice)
}

fn send(text: &str) -> Command {
    Command::Send { session_id: "s1".into(), text: text.into() }
}

fn decide(request: &ApprovalRequest, version: u64, answer: Answer) -> Command {
    Command::Decide {
        decision_id: request.id.clone(),
        version,
        answer,
        edited_args: None,
        reason: None,
        expanded: true,
    }
}

/// Another approver's answer, recorded outside this engine: another process's engine, the
/// kernel's Telegram gateway, or an approval through the inbox API.
async fn answer_elsewhere(home: &Home, request: &ApprovalRequest, who: &str) -> ApprovalRequest {
    home.service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human(who)).with_groups(["owner"]),
        )
        .await
        .unwrap()
}

async fn eventually_sent(home: &Home, expected: &[&str]) {
    let started = tokio::time::Instant::now();
    loop {
        if *home.source.sent.lock().unwrap() == expected {
            return;
        }
        assert!(
            started.elapsed() < WITHIN,
            "sent {:?}, expected {expected:?}",
            home.source.sent.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wave 2 review findings 4 and 5: a decision answered in Telegram, on the phone or in another
/// engine still sends what this engine queued behind it, before anything typed later.
#[tokio::test]
async fn input_queued_behind_a_decision_answered_elsewhere_is_sent_first() {
    let home = home().await;
    let request = hold(&home, "send_email", ApprovalRisk::Medium).await;
    let outcome = home.engine.execute(&web(), send("also attach the timesheet")).await.unwrap();
    assert!(
        matches!(&outcome, CommandOutcome::Queued { text, position: 1, .. } if text == "Sent after this card is answered"),
        "{outcome:?}"
    );

    answer_elsewhere(&home, &request, "ana").await;
    // Typed at once after the answer: it never overtakes the message queued before it.
    let outcome = home.engine.execute(&web(), send("thanks")).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Queued { .. } | CommandOutcome::Sent { .. }));
    eventually_sent(&home, &["also attach the timesheet", "thanks"]).await;
    assert!(home.engine.queued(&web(), "s1").is_empty());
}

/// Wave 2 review finding 7: a message typed during a turn that runs longer than ten minutes
/// is sent when the turn ends.
#[tokio::test(start_paused = true)]
async fn input_queued_behind_a_long_turn_is_sent_when_it_ends() {
    let home = home().await;
    home.source.busy.store(true, Ordering::SeqCst);
    let outcome = home.engine.execute(&web(), send("also update the changelog")).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Queued { .. }), "{outcome:?}");
    tokio::time::sleep(Duration::from_secs(15 * 60)).await;
    assert!(home.source.sent.lock().unwrap().is_empty(), "nothing is sent while the turn runs");
    home.source.busy.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(*home.source.sent.lock().unwrap(), ["also update the changelog"]);
}

/// Wave 2 review finding 7: one failed read of the session's decisions does not strand the
/// queued input.
#[tokio::test]
async fn a_failed_read_does_not_strand_queued_input() {
    let home = home().await;
    home.source.busy.store(true, Ordering::SeqCst);
    home.engine.execute(&web(), send("use the October numbers")).await.unwrap();
    home.source.read_failures.store(2, Ordering::SeqCst);
    home.source.busy.store(false, Ordering::SeqCst);
    eventually_sent(&home, &["use the October numbers"]).await;
}

/// Wave 2 review finding 13: a message whose turn cannot start yet stays queued and is sent
/// once it can.
#[tokio::test]
async fn a_message_whose_turn_cannot_start_stays_queued() {
    let home = home().await;
    home.source.start_failures.lock().unwrap().push(unavailable());
    home.source.busy.store(true, Ordering::SeqCst);
    home.engine.execute(&web(), send("send it to Bo too")).await.unwrap();
    home.source.busy.store(false, Ordering::SeqCst);
    eventually_sent(&home, &["send it to Bo too"]).await;
}

/// Wave 2 review finding 15: input queued behind a running turn, with no card, says when it
/// is really sent.
#[tokio::test]
async fn input_queued_behind_a_turn_says_when_it_is_sent() {
    let home = home().await;
    home.source.busy.store(true, Ordering::SeqCst);
    let outcome = home.engine.execute(&web(), send("use the October numbers")).await.unwrap();
    let CommandOutcome::Queued { text, .. } = outcome else {
        panic!("a message during a turn is queued: {outcome:?}");
    };
    assert_eq!(text, "Sent when your agent is free");
}

/// Wave 2 review finding 20: a call held for approvers other than the session's owner is
/// still an open decision of the session; typing meanwhile is queued (§7.4 rule 2).
#[tokio::test]
async fn typing_while_a_call_waits_for_someone_else_is_queued() {
    let home = home().await;
    hold_for(
        &home,
        "transfer",
        ApprovalRisk::Medium,
        ApprovalPolicy::new([ApproverSelector::group("finance")]),
    )
    .await;
    assert!(home.engine.decisions(&web()).await.unwrap().is_empty(), "not ana's to decide");
    let outcome = home.engine.execute(&web(), send("and email the receipt")).await.unwrap();
    assert!(
        matches!(&outcome, CommandOutcome::Queued { text, .. } if text == "Sent after this card is answered"),
        "{outcome:?}"
    );
    assert!(home.source.sent.lock().unwrap().is_empty());
}

/// Wave 2 review finding 19: a message whose audit record failed is not queued, so it is
/// never sent unrecorded.
#[tokio::test]
async fn a_message_whose_record_fails_is_not_queued() {
    let home = home_with(Setup {
        refuse: Some(Arc::new(|draft: &AuditRecordDraft| {
            draft.action == adk_surface::SURFACE_AUDIT_SEND
        })),
        ..Setup::default()
    })
    .await;
    hold(&home, "send_email", ApprovalRisk::Medium).await;
    let error = home.engine.execute(&web(), send("pay the gas bill too")).await.unwrap_err();
    assert_eq!(error.code(), "audit.record_failed");
    assert!(home.engine.queued(&web(), "s1").is_empty());
}

/// Wave 2 review finding 19: a cancelled spoken answer stays cancelled, and says so, when its
/// audit record fails.
#[tokio::test]
async fn a_spoken_answer_cancelled_while_the_record_fails_stays_cancelled() {
    let home = home_with(Setup {
        refuse: Some(Arc::new(|draft: &AuditRecordDraft| {
            draft.ext.get("outcome") == Some(&json!("cancelled"))
        })),
        ..Setup::default()
    })
    .await;
    let request = hold(&home, "send_email", ApprovalRisk::Low).await;
    let outcome = home
        .engine
        .execute(&voice(), decide(&request, request.version, Answer::AllowOnce))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Pending { .. }), "{outcome:?}");
    let cancelled = home
        .engine
        .execute(&voice(), Command::CancelAnswer { decision_id: request.id.clone() })
        .await
        .unwrap();
    assert_eq!(cancelled, CommandOutcome::AnswerCancelled { decision_id: request.id.clone() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().state, ApprovalState::Pending);
}

/// Wave 2 review finding 12: an answer recorded while the audit log refuses its surface
/// record still reports the answer, resumes the call and sends the queued input.
#[tokio::test]
async fn a_recorded_answer_resumes_and_sends_queued_input_whatever_its_record() {
    let home = home_with(Setup {
        refuse: Some(Arc::new(|draft: &AuditRecordDraft| {
            draft.action == adk_surface::SURFACE_AUDIT_DECIDE
                && draft.ext.get("outcome") == Some(&json!("decided"))
        })),
        ..Setup::default()
    })
    .await;
    let request = hold(&home, "write_file", ApprovalRisk::Medium).await;
    home.engine.execute(&web(), send("then email it")).await.unwrap();
    let outcome =
        home.engine.execute(&web(), decide(&request, request.version, Answer::AllowOnce)).await;
    assert!(matches!(outcome, Ok(CommandOutcome::Answered { .. })), "{outcome:?}");
    assert_eq!(*home.source.resumed.lock().unwrap(), std::slice::from_ref(&request.id));
    eventually_sent(&home, &["then email it"]).await;
}

/// Wave 2 review finding 18: Stop all withdraws a spoken "allow once" still in its cancel
/// window.
#[tokio::test]
async fn stop_all_withdraws_a_spoken_allow_once() {
    let home = home().await;
    let request = hold(&home, "send_email", ApprovalRisk::Low).await;
    let outcome = home
        .engine
        .execute(&voice(), decide(&request, request.version, Answer::AllowOnce))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Pending { .. }), "{outcome:?}");
    home.engine.execute(&web(), Command::StopAll { reason: None }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().state, ApprovalState::Pending);
    let stops: Vec<AuditRecordDraft> = home
        .audit
        .records()
        .into_iter()
        .filter(|record| record.action == adk_surface::SURFACE_AUDIT_STOP_ALL)
        .collect();
    assert_eq!(stops[0].ext["cancelledAnswers"], json!([request.id]));
}

/// Wave 2 review finding 21: a spoken "allow once" to a decision that changed since it was
/// read is shown again at once, never promised and then dropped (S-I2).
#[tokio::test]
async fn a_spoken_answer_to_a_changed_decision_is_shown_again() {
    let home = home().await;
    let request = hold_for(
        &home,
        "send_email",
        ApprovalRisk::Low,
        ApprovalPolicy::new([ApproverSelector::group("owner")])
            .allow_self_approval()
            .with_quorum(2),
    )
    .await;
    answer_elsewhere(&home, &request, "bo").await;
    let outcome = home
        .engine
        .execute(&voice(), decide(&request, request.version, Answer::AllowOnce))
        .await
        .unwrap();
    let CommandOutcome::Changed { card } = outcome else {
        panic!("a stale spoken answer is shown again: {outcome:?}");
    };
    assert_eq!(card.version, request.version + 1);
}

/// Wave 2 review findings 2 and 6: a request that needs two approvers never offers "Always
/// allow", and one approval never grants a standing rule.
#[tokio::test]
async fn always_allow_is_never_offered_when_a_quorum_must_approve() {
    let home = home_with(Setup { offers_always: true, ..Setup::default() }).await;
    let request = hold_for(
        &home,
        "deploy_service",
        ApprovalRisk::Medium,
        ApprovalPolicy::new([ApproverSelector::group("owner")])
            .allow_self_approval()
            .with_quorum(2),
    )
    .await;
    let card = home.engine.decision(&web(), &request.id).await.unwrap();
    assert!(card.allowed.option(Answer::AlwaysAllow).is_none(), "{:?}", card.allowed);
    let error = home
        .engine
        .execute(&web(), decide(&request, request.version, Answer::AlwaysAllow))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "surface.answer_not_allowed");
    assert!(home.source.granted.lock().unwrap().is_empty());
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().decisions.len(), 0);
}

/// Wave 2 review finding 17: "Always allow" grants its rule once the request is approved,
/// and an answer that lost to another surface grants none.
#[tokio::test]
async fn always_allow_grants_its_rule_only_with_the_approval() {
    let home = home_with(Setup { offers_always: true, ..Setup::default() }).await;
    let first = hold(&home, "send_email", ApprovalRisk::Medium).await;
    let outcome = home
        .engine
        .execute(&web(), decide(&first, first.version, Answer::AlwaysAllow))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    assert_eq!(*home.source.granted.lock().unwrap(), std::slice::from_ref(&first.id));

    let second = hold(&home, "send_sms", ApprovalRisk::Medium).await;
    home.service
        .decide(
            &second.id,
            second.version,
            ApprovalDecision::reject(SubjectRef::human("ana"), "no").with_groups(["owner"]),
        )
        .await
        .unwrap();
    let outcome = home
        .engine
        .execute(&web(), decide(&second, second.version, Answer::AlwaysAllow))
        .await
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::AlreadyDecided { .. }), "{outcome:?}");
    assert_eq!(*home.source.granted.lock().unwrap(), [first.id], "a lost answer grants nothing");
}

/// Wave 2 review finding 17: chat and voice tighten the mode but never loosen it (§7.4
/// rule 4); the web home does both.
#[tokio::test]
async fn chat_and_voice_never_loosen_the_mode() {
    let home = home().await;
    let telegram = caller(SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
    let set = |mode| Command::SetMode { session_id: "s1".into(), mode };
    for away in [telegram.clone(), voice()] {
        let error = home.engine.execute(&away, set(Mode::GoAhead)).await.unwrap_err();
        assert_eq!(error.code(), "surface.not_here", "{away:?}");
        assert_eq!(*home.source.mode.lock().unwrap(), HarnessMode::Ask);
    }
    assert!(matches!(
        home.engine.execute(&telegram, set(Mode::JustLook)).await.unwrap(),
        CommandOutcome::ModeSet { .. }
    ));
    assert_eq!(*home.source.mode.lock().unwrap(), HarnessMode::ReadOnly);
    assert!(matches!(
        home.engine.execute(&web(), set(Mode::GoAhead)).await.unwrap(),
        CommandOutcome::ModeSet { .. }
    ));
    assert_eq!(*home.source.mode.lock().unwrap(), HarnessMode::Auto);
}

/// Wave 2 review finding 22: a decision the source no longer finds after a conflict reads
/// decided all through: its timeline, its note, its cancel window.
#[tokio::test]
async fn a_decision_gone_after_a_conflict_reads_decided() {
    let home = home_with(Setup { open_only: true, ..Setup::default() }).await;
    let request = hold(&home, "publish_release", ApprovalRisk::High).await;
    let terminal = caller(SurfaceKind::Terminal, Origin::Keyboard);
    let card = home.engine.decision(&terminal, &request.id).await.unwrap();
    assert!(card.allowed.note.is_some(), "an unpaired terminal says where to approve");
    *home.source.answered_first_by.lock().unwrap() = Some("bo".into());
    let outcome = home
        .engine
        .execute(&terminal, decide(&request, request.version, Answer::Deny))
        .await
        .unwrap();
    let CommandOutcome::AlreadyDecided { card, decided } = outcome else {
        panic!("decided elsewhere: {outcome:?}");
    };
    assert_eq!(card.timeline.last().unwrap().text, "Decided");
    assert_eq!((card.allowed.note, card.allowed.cancel_window_secs), (None, None));
    assert_eq!(decided.at_text, "", "when it was decided is not known");
}

/// Wave 2 review finding 31: a voice card advertises the cancel window the engine waits.
#[tokio::test]
async fn a_voice_card_shows_the_engines_cancel_window() {
    let home =
        home_with(Setup { voice_window: Some(Duration::from_secs(2)), ..Setup::default() }).await;
    let request = hold(&home, "send_email", ApprovalRisk::Low).await;
    let card = home.engine.decision(&voice(), &request.id).await.unwrap();
    assert_eq!(card.allowed.cancel_window_secs, Some(2));
}

/// Wave 2 review findings 28 and 30: a spoken Deny and the tray's Deny work on a high-risk
/// call (owner decision 2026-10-25: saying no is always safe).
#[tokio::test]
async fn voice_and_the_tray_deny_a_high_risk_call() {
    for refuser in [voice(), caller(SurfaceKind::Tray, Origin::Pointer)] {
        let home = home().await;
        let request = hold(&home, "publish_release", ApprovalRisk::High).await;
        let outcome = home
            .engine
            .execute(&refuser, decide(&request, request.version, Answer::Deny))
            .await
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{refuser:?}: {outcome:?}");
        let stored = home.service.get(&request.id).await.unwrap().unwrap();
        assert_eq!(stored.state, ApprovalState::Rejected);
    }
}

/// An agent whose turn runs until the test releases it.
struct Blocking {
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl adk_core::Agent for Blocking {
    fn name(&self) -> &str {
        "finance"
    }

    fn description(&self) -> &str {
        "works until released"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let release = Arc::clone(&self.release);
        Ok(Box::pin(futures::stream::once(async move {
            release.notified().await;
            let mut event = Event::new("inv");
            event.author = "finance".into();
            Ok(event)
        })))
    }
}

/// A source over a plain runner whose turns run until released, with sessions `s1` of ana
/// and `b1` of bo.
async fn runner_source() -> (LocalSource, Arc<tokio::sync::Notify>) {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    for (user, session) in [("ana", "s1"), ("bo", "b1")] {
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await
            .unwrap();
    }
    let release = Arc::new(tokio::sync::Notify::new());
    let runner = adk_runner::Runner::builder()
        .app_name(APP)
        .agent(Arc::new(Blocking { release: Arc::clone(&release) }))
        .session_service(Arc::clone(&sessions))
        .build()
        .unwrap();
    let source =
        LocalSource::builder(APP).sessions(sessions).runner(Arc::new(runner)).build().unwrap();
    (source, release)
}

/// Wave 2 review finding 3: a turn a surface starts, or the resume of a decided call, runs by
/// the time the command returns, so input queued behind it waits for it instead of starting
/// a second turn of the session beside it (S-I4).
#[tokio::test]
async fn a_turn_started_from_a_surface_runs_when_the_command_returns() {
    let (source, release) = runner_source().await;
    source.start_turn(&web(), "s1", "write the invoice").await.unwrap();
    // Read at once, with nothing in between that lets a spawned task run first.
    assert!(source.busy("ana", "s1").await.unwrap(), "the turn is running");
    release.notify_one();
}

/// Wave 2 review finding 23: without a kernel, one person's running turn is not another
/// person's: neither their agent row nor their session reads busy.
#[tokio::test]
async fn one_persons_running_turn_is_not_anothers() {
    let (source, release) = runner_source().await;
    source.start_turn(&web(), "s1", "write the invoice").await.unwrap();
    let started = tokio::time::Instant::now();
    while !source.busy("ana", "s1").await.unwrap() {
        assert!(started.elapsed() < WITHIN, "ana's turn never ran");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!source.busy("bo", "s1").await.unwrap(), "bo has no turn in s1");
    let ana = source.agents("ana").await.unwrap();
    assert_eq!(
        (ana[0].state, ana[0].session_id.as_deref()),
        (adk_core::RunState::Running, Some("s1"))
    );
    let bo = source.agents("bo").await.unwrap();
    assert_eq!((bo[0].state, bo[0].session_id.as_deref()), (adk_core::RunState::Completed, None));
    release.notify_one();
}

/// An audit log store that counts the reads of its entries.
struct CountingLog {
    inner: adk_core::audit_log::InMemoryAuditLogStore,
    reads: AtomicUsize,
}

#[async_trait]
impl adk_core::audit_log::AuditLogStore for CountingLog {
    async fn append(
        &self,
        batch: adk_core::audit_log::AuditAppend,
    ) -> adk_core::Result<std::ops::Range<u64>> {
        self.inner.append(batch).await
    }

    async fn size(&self, log_id: &str) -> adk_core::Result<u64> {
        self.inner.size(log_id).await
    }

    async fn read(
        &self,
        log_id: &str,
        range: std::ops::Range<u64>,
    ) -> adk_core::Result<Vec<adk_core::audit_log::AuditLogEntry>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(log_id, range).await
    }

    async fn hash_at(
        &self,
        log_id: &str,
        level: u8,
        index: u64,
    ) -> adk_core::Result<Option<adk_core::audit_log::AuditHash>> {
        self.inner.hash_at(log_id, level, index).await
    }

    async fn put_checkpoint(
        &self,
        checkpoint: adk_core::audit_log::AuditCheckpoint,
    ) -> adk_core::Result<()> {
        self.inner.put_checkpoint(checkpoint).await
    }

    async fn latest_checkpoint(
        &self,
        log_id: &str,
    ) -> adk_core::Result<Option<adk_core::audit_log::AuditCheckpoint>> {
        self.inner.latest_checkpoint(log_id).await
    }

    async fn checkpoints(
        &self,
        log_id: &str,
    ) -> adk_core::Result<Vec<adk_core::audit_log::AuditCheckpoint>> {
        self.inner.checkpoints(log_id).await
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> adk_core::Result<u64> {
        self.inner.prune(log_id, before_seq).await
    }

    async fn logs(&self) -> adk_core::Result<Vec<String>> {
        self.inner.logs().await
    }
}

/// Wave 2 review finding 10: the card of a decision answered outside the surfaces reads the
/// audit log back to the decision's own time once, not the whole log on every read.
#[tokio::test]
async fn a_decision_answered_elsewhere_reads_the_audit_log_once() {
    use adk_core::audit_log::{
        AuditActor, AuditAppend, AuditHash, AuditLogEntry, AuditLogStore, AuditRecordKind,
    };
    let log = Arc::new(CountingLog {
        inner: adk_core::audit_log::InMemoryAuditLogStore::new(),
        reads: AtomicUsize::new(0),
    });
    // A day of records from before the decision was asked: other tools' records.
    let before = Utc.with_ymd_and_hms(2026, 10, 22, 18, 0, 0).unwrap();
    let entries: Vec<AuditLogEntry> = (0..2_000_u64)
        .map(|seq| {
            let record = AuditRecordDraft::new(AuditRecordKind::Approval, "harness.tool_call")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .into_record("replica-1", seq, before, None);
            AuditLogEntry {
                seq,
                leaf_hash: AuditHash([0; 32]),
                record: Some(serde_json::to_string(&record).unwrap()),
            }
        })
        .collect();
    log.append(AuditAppend { log_id: "replica-1".into(), entries, nodes: vec![] }).await.unwrap();
    let home = home().await;
    let engine = SurfaceEngine::builder(Arc::clone(&home.source) as Arc<dyn Source>)
        .answers(Arc::new(adk_surface::AuditAnswerLog::new(
            Arc::clone(&log) as Arc<dyn adk_core::audit_log::AuditLogStore>
        )))
        .build();
    let request = hold(&home, "send_email", ApprovalRisk::Medium).await;
    // Answered from the command line, so no surface wrote an answer record.
    answer_elsewhere(&home, &request, "ana").await;
    for _ in 0..3 {
        let card = engine.decision(&web(), &request.id).await.unwrap();
        assert_eq!(card.status_text, "Decided");
    }
    assert!(log.reads.load(Ordering::SeqCst) <= 1, "reads: {}", log.reads.load(Ordering::SeqCst));
}
