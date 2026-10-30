//! The engine over in-process stores (spec 3.3 §6, §7.1–§7.4, §8, §11): cards in the words of
//! §8, one write path with the version seen, what each surface may decide, queued input,
//! stopping and resuming, the session-owner check, and the audit record of every answer.

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService, ApprovalState,
    ApproverSelector, Content, Event, EventActions, GovernanceControl, InMemoryApprovalStore,
    ManualClock, StoreApprovalService, ToolConfirmationRequest,
};
use adk_kernel::QuietHours;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_surface::{
    Answer, Caller, Command, CommandOutcome, DecidedAnswer, LocalSource, Mode, Origin, Place,
    SurfaceEngine, SurfaceKind, ThreadItem,
};
use chrono::{FixedOffset, NaiveTime, TimeZone, Utc};
use serde_json::{Value, json};

const APP: &str = "home";

struct Home {
    engine: SurfaceEngine,
    service: Arc<dyn ApprovalService>,
    sessions: Arc<dyn SessionService>,
    audit: InMemoryAuditRecorder,
    governance: GovernanceControl,
}

fn at(hour: u32, minute: u32, day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0).unwrap()
}

/// A home over in-memory stores, its clock at 07:31 the morning after.
async fn home() -> Home {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    for (user, session) in [("ana", "s1"), ("bo", "s2")] {
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
    let service: Arc<dyn ApprovalService> = Arc::new(
        // The inbox's clock reads 18:00 the evening before, when the calls were held.
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_clock(Arc::new(ManualClock::new(at(18, 0, 23))))
            .with_router(
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            ),
    );
    let governance = GovernanceControl::new();
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&sessions))
        .approvals(Arc::clone(&service))
        .governance(governance.clone())
        .build()
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let engine = SurfaceEngine::builder(Arc::new(source))
        .audit(AuditTrail::new(Arc::new(audit.clone())))
        .places([Place::phone(), Place::chat("Telegram")])
        .utc_offset(FixedOffset::east_opt(0).unwrap())
        .quiet_hours(
            QuietHours::new(
                NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            )
            .with_utc_offset_minutes(0),
        )
        .clock(Arc::new(ManualClock::new(at(7, 31, 24))))
        .voice_cancel_window(Duration::from_millis(80))
        .build();
    Home { engine, service, sessions, audit, governance }
}

fn call(tool: &str, args: Value) -> ToolConfirmationRequest {
    ToolConfirmationRequest {
        tool_name: tool.into(),
        function_call_id: Some(format!("call-{tool}")),
        args,
        origin: None,
        acting: None,
    }
}

/// A held call of `user`'s session in the inbox, asked at 18:00 the evening before, and the
/// session event that held it.
async fn hold(
    home: &Home,
    user: &str,
    session: &str,
    call: &ToolConfirmationRequest,
    risk: ApprovalRisk,
) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(
        format!("apr-{user}-{}", call.tool_name),
        ApprovalScope::new(APP, user, session),
        call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    )
    .with_risk(risk);
    request.created_at = at(18, 0, 23);
    let mut event = Event::new("inv-1");
    event.author = "finance".into();
    event.actions =
        EventActions { tool_confirmation: Some(call.clone()), ..EventActions::default() };
    home.sessions.append_event(session, event).await.unwrap();
    home.service.submit(request).await.unwrap()
}

fn invoice() -> ToolConfirmationRequest {
    call("send_email", json!({ "to": "[client email]", "subject": "Invoice INV-[NUMBER]" }))
}

fn web() -> Caller {
    Caller::human("ana", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"])
}

fn phone() -> Caller {
    Caller::human("ana", SurfaceKind::Phone, Origin::Touch).with_groups(["owner"])
}

fn telegram() -> Caller {
    Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })
        .with_groups(["owner"])
}

fn decide(request: &ApprovalRequest, answer: Answer) -> Command {
    Command::Decide {
        decision_id: request.id.clone(),
        version: request.version,
        answer,
        edited_args: None,
        reason: None,
        expanded: false,
    }
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|record| record.action == action).collect()
}

#[tokio::test]
async fn a_card_is_drawn_from_the_inbox_entry_in_the_words_of_section_8() {
    let home = home().await;
    hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;

    let cards = home.engine.decisions(&web()).await.unwrap();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert_eq!(card.agent.display_name, "Home");
    assert_eq!(card.since_text, "since 18:00");
    assert_eq!(card.status_text, "Waiting for you");
    assert_eq!(card.heading, "Send email to [client email]");
    assert!(card.call.text.starts_with("send_email("));
    assert!(
        card.call.text.contains(r#"to: "[client email]""#)
            && card.call.text.contains(r#"subject: "Invoice INV-[NUMBER]""#)
    );
    assert!(!card.call.truncated);
    let timeline: Vec<&str> = card.timeline.iter().map(|step| step.text.as_str()).collect();
    assert_eq!(timeline, ["Asked at 18:00", "Waited through quiet time", "Waiting for you"]);
    let labels: Vec<&str> =
        card.allowed.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Allow once", "Edit", "Deny"]);
    assert_eq!(card.risk_text, "Medium risk");
    assert_eq!(card.reversibility_text, "Can't be unsent");
    assert_eq!(card.also_shown.as_deref(), Some("Also on your phone and in Telegram"));

    // The same entry from chat: allow once or deny, and the home for the rest (§7.2).
    let chat = &home.engine.decisions(&telegram()).await.unwrap()[0];
    let labels: Vec<&str> =
        chat.allowed.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels, ["Allow once", "Deny"]);
    assert!(chat.allowed.open_in_home);
    assert_eq!(chat.also_shown.as_deref(), Some("Also on your phone"));
    // Chat gets the agent, the risk and a summary, never the arguments (S-I5).
    assert_eq!(chat.heading, "Send email");
    assert_eq!((chat.call.args.clone(), chat.call.text.as_str()), (Value::Null, "send_email"));
    assert!(!serde_json::to_string(chat).unwrap().contains("client email"));

    // No internal identifiers in the words on the card (§8).
    let text =
        serde_json::to_string(&(&card.heading, &card.timeline, &card.since_text, &card.also_shown))
            .unwrap();
    let fingerprint = home.service.get(&card.id).await.unwrap().unwrap().fingerprint;
    assert!(!text.contains(&fingerprint) && !text.contains(&card.id));
}

#[tokio::test]
async fn the_second_surface_is_told_who_decided_and_never_retries() {
    let home = home().await;
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;

    // Chat answers first.
    let CommandOutcome::Answered { card } =
        home.engine.execute(&telegram(), decide(&request, Answer::AllowOnce)).await.unwrap()
    else {
        panic!("the first answer decides");
    };
    let decided = card.decided.unwrap();
    assert_eq!(decided.answer, DecidedAnswer::Yes);

    // The web home answers the version it saw: decided elsewhere, shown, not retried.
    let outcome = home.engine.execute(&web(), decide(&request, Answer::Deny)).await.unwrap();
    let CommandOutcome::AlreadyDecided { decided, card } = outcome else {
        panic!("a second answer is never recorded: {outcome:?}");
    };
    assert!(decided.by_you);
    assert_eq!(decided.via.as_ref().unwrap().origin, Origin::Chat { channel: "Telegram".into() });
    assert!(decided.text.starts_with("You said yes in Telegram at "), "{}", decided.text);
    assert_eq!(card.status_text, "Decided");
    assert!(card.allowed.options.is_empty());
    let stored = home.service.get(&request.id).await.unwrap().unwrap();
    assert_eq!(stored.state, ApprovalState::Approved);
    assert_eq!(stored.decisions.len(), 1, "exactly one answer was recorded");

    // Both answers are on record with the surface and how they were given (§11).
    let answers = records(&home.audit, adk_surface::SURFACE_AUDIT_DECIDE);
    assert_eq!(answers.len(), 2);
    assert_eq!(answers[0].ext["outcome"], "decided");
    assert_eq!(answers[0].ext["surface"], "chat");
    assert_eq!(answers[0].ext["origin"], json!({ "kind": "chat", "channel": "Telegram" }));
    assert_eq!(answers[1].ext["outcome"], "already_decided");
    assert_eq!(answers[1].ext["surface"], "web");
    assert_eq!(answers[0].actor.on_behalf_of.as_ref().unwrap().id, "ana");
}

/// A recorder that refuses every record, as a signed log that is down.
struct Broken;

#[async_trait::async_trait]
impl adk_core::audit_log::AuditRecorder for Broken {
    fn digest(&self, _value: &Value) -> String {
        String::new()
    }

    async fn record(&self, _draft: AuditRecordDraft) -> adk_core::Result<()> {
        Err(adk_core::AdkError::new(
            adk_core::ErrorComponent::Auth,
            adk_core::ErrorCategory::Unavailable,
            "audit.down",
            "the audit log is down",
        ))
    }
}

/// An answer is on record before it takes effect: `surface.answer` precedes the inbox's
/// decision and its `surface.decide`, and an answer that cannot be recorded is not given
/// (spec 3.3 §11). Fails on `origin/main`, which decides first and only logs a failed record.
#[tokio::test]
async fn an_answer_is_recorded_before_it_takes_effect() {
    let home = home().await;
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;
    let source = LocalSource::builder(APP)
        .sessions(Arc::clone(&home.sessions))
        .approvals(Arc::clone(&home.service))
        .governance(home.governance.clone())
        .build()
        .unwrap();
    let unrecorded =
        SurfaceEngine::builder(Arc::new(source)).audit(AuditTrail::new(Arc::new(Broken))).build();
    let error = unrecorded.execute(&web(), decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "audit.record_failed", "{error:?}");
    let stored = home.service.get(&request.id).await.unwrap().unwrap();
    assert_eq!(stored.state, ApprovalState::Pending, "an unrecorded answer is not given");

    home.engine.execute(&web(), decide(&request, Answer::AllowOnce)).await.unwrap();
    let actions: Vec<(String, Value)> = home
        .audit
        .records()
        .into_iter()
        .filter(|record| record.ext.get("approvalId") == Some(&json!(request.id)))
        .map(|record| (record.action, record.ext["answer"].clone()))
        .collect();
    assert_eq!(
        actions,
        [
            (adk_surface::SURFACE_AUDIT_ANSWER.to_string(), json!("allowOnce")),
            (adk_surface::SURFACE_AUDIT_DECIDE.to_string(), json!("allowOnce")),
        ]
    );
    let given = records(&home.audit, adk_surface::SURFACE_AUDIT_ANSWER).remove(0);
    assert_eq!(
        (&given.ext["surface"], &given.ext["origin"]),
        (&json!("web"), &json!({ "kind": "pointer" }))
    );
}

#[tokio::test]
async fn chat_voice_and_the_tray_never_approve_high_risk() {
    let home = home().await;
    let request = hold(
        &home,
        "ana",
        "s1",
        &call("publish_release", json!({ "crate": "x" })),
        ApprovalRisk::High,
    )
    .await;
    for caller in [
        telegram(),
        Caller::human("ana", SurfaceKind::Tray, Origin::Pointer).with_groups(["owner"]),
    ] {
        let error =
            home.engine.execute(&caller, decide(&request, Answer::AllowOnce)).await.unwrap_err();
        assert_eq!(error.code(), "surface.answer_not_allowed", "{caller:?}");
    }
    // A spoken answer is recorded only with its transcript (spec 3.3 §5.6).
    let voice = Caller::human("ana", SurfaceKind::Web, Origin::Voice).with_groups(["owner"]);
    let error = home.engine.execute(&voice, decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "surface.invalid");
    // The phone approves after the person confirms it is them.
    let error =
        home.engine.execute(&phone(), decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "surface.confirm_presence");
    let confirmed = phone().with_presence_confirmed();
    assert!(matches!(
        home.engine.execute(&confirmed, decide(&request, Answer::AllowOnce)).await.unwrap(),
        CommandOutcome::Answered { .. }
    ));
    let refused = records(&home.audit, adk_surface::SURFACE_AUDIT_DECIDE)
        .into_iter()
        .filter(|record| record.ext["outcome"] == "refused")
        .count();
    assert_eq!(refused, 3, "every refused answer is on record");
}

#[tokio::test]
async fn no_agent_answers_a_decision() {
    let home = home().await;
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Low).await;
    let agent = Caller::human("ana", SurfaceKind::Web, Origin::Agent { agent: "finance".into() })
        .with_groups(["owner"]);
    let error = home.engine.execute(&agent, decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "surface.not_here");
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().state, ApprovalState::Pending);
}

#[tokio::test]
async fn a_truncated_call_is_approved_only_once_expanded() {
    let home = home().await;
    let long = call("write_file", json!({ "path": "notes.md", "content": "x".repeat(3_000) }));
    let request = hold(&home, "ana", "s1", &long, ApprovalRisk::Low).await;
    let card = home.engine.decision(&web(), &request.id).await.unwrap();
    assert!(card.call.truncated);
    let error = home.engine.execute(&web(), decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "surface.call_not_expanded");
    let expanded = Command::Decide {
        decision_id: request.id.clone(),
        version: request.version,
        answer: Answer::AllowOnce,
        edited_args: None,
        reason: None,
        expanded: true,
    };
    assert!(matches!(
        home.engine.execute(&web(), expanded).await.unwrap(),
        CommandOutcome::Answered { .. }
    ));
}

#[tokio::test]
async fn typing_while_a_decision_is_open_is_queued_never_an_answer() {
    let home = home().await;
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;
    let outcome = home
        .engine
        .execute(&web(), Command::Send { session_id: "s1".into(), text: "yes".into() })
        .await
        .unwrap();
    let CommandOutcome::Queued { position, text, .. } = outcome else {
        panic!("input during an open decision is queued: {outcome:?}");
    };
    assert_eq!((position, text.as_str()), (1, "Sent after this card is answered"));
    assert_eq!(home.engine.queued(&web(), "s1"), ["yes"]);
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().state, ApprovalState::Pending);
}

#[tokio::test]
async fn stop_from_anywhere_resume_only_from_the_home() {
    let home = home().await;
    let outcome =
        home.engine.execute(&telegram(), Command::StopAll { reason: None }).await.unwrap();
    assert_eq!(outcome, CommandOutcome::Stopped { text: "All agents stopped".into() });
    assert!(home.governance.is_frozen());
    let status = home.engine.status(&phone(), None).await.unwrap();
    assert_eq!(status.stopped_text.as_deref(), Some("All agents stopped"));

    for elsewhere in
        [telegram(), phone(), Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard)]
    {
        let error = home.engine.execute(&elsewhere, Command::ResumeAll).await.unwrap_err();
        assert_eq!(error.code(), "surface.not_here", "{elsewhere:?}");
    }
    assert!(home.governance.is_frozen());
    assert_eq!(
        home.engine.execute(&web(), Command::ResumeAll).await.unwrap(),
        CommandOutcome::Resumed
    );
    assert!(!home.governance.is_frozen());
    let local = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
    home.engine.execute(&telegram(), Command::StopAll { reason: None }).await.unwrap();
    assert_eq!(
        home.engine.execute(&local, Command::ResumeAll).await.unwrap(),
        CommandOutcome::Resumed
    );

    let stops = records(&home.audit, adk_surface::SURFACE_AUDIT_STOP_ALL);
    assert_eq!(stops[0].ext["origin"], json!({ "kind": "chat", "channel": "Telegram" }));
}

#[tokio::test]
async fn nobody_sees_or_answers_another_persons_decisions() {
    let home = home().await;
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Low).await;
    let bo = Caller::human("bo", SurfaceKind::Web, Origin::Pointer).with_groups(["owner"]);
    assert!(home.engine.decisions(&bo).await.unwrap().is_empty());
    assert_eq!(
        home.engine.decision(&bo, &request.id).await.unwrap_err().code(),
        "surface.not_found"
    );
    let error = home.engine.execute(&bo, decide(&request, Answer::AllowOnce)).await.unwrap_err();
    assert_eq!(error.code(), "surface.not_found");
    assert_eq!(home.engine.thread(&bo, "s1").await.unwrap_err().code(), "surface.not_found");
    let error = home
        .engine
        .execute(&bo, Command::SetMode { session_id: "s1".into(), mode: Mode::GoAhead })
        .await
        .unwrap_err();
    assert_eq!(error.code(), "surface.not_found");
}

#[tokio::test]
async fn the_thread_links_a_held_call_to_its_card() {
    let home = home().await;
    let mut said = Event::new("inv-0");
    said.author = "user".into();
    said.llm_response.content = Some(Content::new("user").with_text("Invoice the client"));
    home.sessions.append_event("s1", said).await.unwrap();
    let request = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;
    let thread = home.engine.thread(&web(), "s1").await.unwrap();
    assert_eq!(thread.iter().map(|entry| entry.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(
        thread[0].items,
        [ThreadItem::Message {
            from: adk_surface::Speaker::You,
            author: "You".into(),
            text: "Invoice the client".into()
        }]
    );
    assert_eq!(
        thread[1].items,
        [ThreadItem::Decision {
            decision_id: Some(request.id),
            tool: "send_email".into(),
            text: "Waiting for you".into()
        }]
    );
}

#[cfg(feature = "voice")]
#[tokio::test]
async fn a_spoken_allow_once_waits_out_its_cancel_window() {
    let home = home().await;
    let voice = Caller::human("ana", SurfaceKind::Voice, Origin::Voice).with_groups(["owner"]);
    let first = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Low).await;
    let second =
        hold(&home, "ana", "s1", &call("send_sms", json!({ "to": "[number]" })), ApprovalRisk::Low)
            .await;

    let spoken = async |request: &ApprovalRequest| {
        home.engine
            .decide_spoken(&voice, &request.id, request.version, Answer::AllowOnce, "allow once")
            .await
    };
    let CommandOutcome::Pending { text, .. } = spoken(&first).await.unwrap().outcome else {
        panic!("a spoken yes waits");
    };
    assert_eq!(text, "Allowing once in 1 second");
    assert_eq!(
        home.engine
            .execute(&voice, Command::CancelAnswer { decision_id: first.id.clone() })
            .await
            .unwrap(),
        CommandOutcome::AnswerCancelled { decision_id: first.id.clone() }
    );
    spoken(&second).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(home.service.get(&first.id).await.unwrap().unwrap().state, ApprovalState::Pending);
    assert_eq!(home.service.get(&second.id).await.unwrap().unwrap().state, ApprovalState::Approved);
}

#[tokio::test]
async fn an_answer_to_a_changed_open_decision_is_shown_again_not_retried() {
    let home = home().await;
    let policy = ApprovalPolicy::new([ApproverSelector::group("owner")])
        .allow_self_approval()
        .with_quorum(2);
    let mut request = ApprovalRequest::new(
        "apr-quorum",
        ApprovalScope::new(APP, "ana", "s1"),
        &invoice(),
        policy,
    );
    request.created_at = at(18, 0, 23);
    let request = home.service.submit(request).await.unwrap();
    // Someone else approves first; the request stays open at a new version.
    home.service
        .decide(
            &request.id,
            request.version,
            adk_core::ApprovalDecision::approve(adk_core::principal::SubjectRef::human("cy"))
                .with_groups(["owner"]),
        )
        .await
        .unwrap();
    let outcome = home.engine.execute(&web(), decide(&request, Answer::AllowOnce)).await.unwrap();
    let CommandOutcome::Changed { card } = outcome else {
        panic!("a stale version is shown again: {outcome:?}");
    };
    assert_eq!(card.version, request.version + 1);
    assert_eq!(home.service.get(&request.id).await.unwrap().unwrap().decisions.len(), 1);
}

/// The source itself reads one session's requests (spec 3.3 §10 C1), not the engine's filter
/// after it: `LocalSource` asks the approval service for that session only.
#[tokio::test]
async fn the_local_source_reads_one_sessions_requests() {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let service: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    for (id, session) in [("apr-1", "s1"), ("apr-2", "s3")] {
        let request = ApprovalRequest::new(
            id,
            ApprovalScope::new(APP, "ana", session),
            &invoice(),
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        );
        service.submit(request).await.unwrap();
    }
    let source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&service))
        .build()
        .unwrap();
    let ids = |requests: Vec<ApprovalRequest>| -> Vec<String> {
        requests.into_iter().map(|request| request.id).collect()
    };
    use adk_surface::Source;
    assert_eq!(ids(source.requests(&web(), Some("s1"), true).await.unwrap()), ["apr-1"]);
    assert_eq!(ids(source.requests(&web(), Some("s1"), false).await.unwrap()), ["apr-1"]);
    assert_eq!(source.requests(&web(), None, true).await.unwrap().len(), 2);
}

/// A session is an application's, user's and session ID (spec 3.3 §10 C1): another user's
/// session with the same ID is not the caller's, even for an approver who may answer it.
#[tokio::test]
async fn the_local_source_reads_the_callers_session_only() {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let service: Arc<dyn ApprovalService> =
        Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
    for (id, user) in [("apr-ana", "ana"), ("apr-bo", "bo")] {
        let request = ApprovalRequest::new(
            id,
            ApprovalScope::new(APP, user, "s1"),
            &invoice(),
            ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
        );
        service.submit(request).await.unwrap();
    }
    let source = LocalSource::builder(APP)
        .sessions(sessions)
        .approvals(Arc::clone(&service))
        .build()
        .unwrap();
    use adk_surface::Source;
    let requests = source.requests(&web(), Some("s1"), false).await.unwrap();
    let ids: Vec<String> = requests.into_iter().map(|request| request.id).collect();
    assert_eq!(ids, ["apr-ana"]);
}

/// The record of an answer to a decision the kernel itself asks names what it is about and
/// carries the card's summary, never only the kernel's tool; an agent's call names nothing
/// more than before (spec 3.3 §8, §11).
#[tokio::test]
async fn an_answer_to_a_kernel_decision_is_recorded_by_what_it_is_about() {
    let home = home().await;
    let mount = call("kernel:driver.mount", json!({ "id": "email", "partitions": ["default"] }));
    let mount = hold(&home, "ana", "s1", &mount, ApprovalRisk::Medium).await;
    let email = hold(&home, "ana", "s1", &invoice(), ApprovalRisk::Medium).await;
    for request in [&mount, &email] {
        home.engine.execute(&web(), decide(request, Answer::Deny)).await.unwrap();
    }
    let named: Vec<(Value, Option<Value>, Option<Value>)> = records(&home.audit, "surface.decide")
        .into_iter()
        .map(|record| {
            (
                record.ext["tool"].clone(),
                record.ext.get("title").cloned(),
                record.ext.get("summary").cloned(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            (json!("kernel:driver.mount"), Some(json!("email")), Some(json!(mount.summary))),
            (json!("send_email"), None, None),
        ]
    );
}
