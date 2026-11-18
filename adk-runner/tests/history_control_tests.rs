//! `HistoryControl` refuses to fork, rewind, or edit a session while a turn runs on it, sees the
//! runs of every runner sharing its `SessionActivity` and of a durable run store, rewinds a whole
//! invocation, and records each operation's decision and outcome in the audit log.

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditFailureMode, AuditRecordDraft, AuditRecordKind,
    AuditRecordOutcome, AuditRecorder, InMemoryAuditRecorder,
};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, AdkIdentity, Agent, AppName, Content, DurableRunStore, ErrorCategory, ErrorComponent,
    Event, EventStream, InMemoryRunStore, InvocationContext, NewRun, Result, SessionId, UserId,
};
use adk_runner::{HistoryControl, RewindTarget, Runner};
use adk_session::{
    CreateRequest, ForkRequest, HistoryEdit, InMemorySessionService, SessionService,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

const APP: &str = "history";

/// Holds the next run at its first event until the test opens it.
#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}

/// Answers each turn with the text it was asked, after its gate when armed.
struct EchoAgent {
    gate: Arc<Gate>,
}

#[async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "echoes the user"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let gate = self.gate.clone();
        let asked: String =
            ctx.user_content().parts.iter().filter_map(|part| part.text()).collect();
        Ok(Box::pin(async_stream::stream! {
            if gate.armed.swap(false, Ordering::SeqCst) {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            let mut answer = Event::new(ctx.invocation_id());
            answer.author = "agent".to_string();
            answer.llm_response.content = Some(Content::new("model").with_text(format!("echo: {asked}")));
            yield Ok(answer);
        }))
    }
}

/// An audit recorder whose every write fails.
struct BrokenRecorder;

#[async_trait]
impl AuditRecorder for BrokenRecorder {
    fn digest(&self, value: &serde_json::Value) -> String {
        adk_core::json_digest(value)
    }

    async fn record(&self, _draft: AuditRecordDraft) -> Result<()> {
        Err(AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::Unavailable,
            "test.audit_down",
            "the audit log is down",
        ))
    }
}

struct Fixture {
    runner: Runner,
    sessions: Arc<InMemorySessionService>,
    gate: Arc<Gate>,
}

async fn fixture(recorder: Option<Arc<dyn AuditRecorder>>) -> Fixture {
    let sessions = Arc::new(InMemorySessionService::new());
    for user in ["alice", "bob"] {
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: Some("s".into()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
    }
    let gate = Arc::new(Gate::default());
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(EchoAgent { gate: gate.clone() }) as Arc<dyn Agent>)
        .session_service(sessions.clone() as Arc<dyn SessionService>);
    if let Some(recorder) = recorder {
        builder = builder.audit_log(recorder).audit_failure_mode(AuditFailureMode::Block);
    }
    Fixture { runner: builder.build().unwrap(), sessions, gate }
}

fn identity(user: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(user).unwrap(),
        SessionId::try_from("s").unwrap(),
    )
}

fn actor(user: &str) -> AuditActor {
    AuditActor::subject(SubjectRef::human(user))
}

impl Fixture {
    async fn turn(&self, user: &str, text: &str) {
        let events = self
            .runner
            .run(
                UserId::try_from(user).unwrap(),
                SessionId::try_from("s").unwrap(),
                Content::new("user").with_text(text),
            )
            .await
            .unwrap();
        let events: Vec<_> = events.collect().await;
        assert!(events.iter().all(Result::is_ok), "{events:?}");
    }

    /// Starts a turn for `user` and returns once it waits at the gate.
    async fn start_waiting(&self, user: &str) -> tokio::task::JoinHandle<()> {
        self.gate.armed.store(true, Ordering::SeqCst);
        let events = self
            .runner
            .run(
                UserId::try_from(user).unwrap(),
                SessionId::try_from("s").unwrap(),
                Content::new("user").with_text("slow"),
            )
            .await
            .unwrap();
        let run = tokio::spawn(async move {
            let _: Vec<_> = events.collect().await;
        });
        tokio::time::timeout(Duration::from_secs(5), self.gate.entered.notified())
            .await
            .expect("the run reaches its gate");
        run
    }

    async fn finish(&self, run: tokio::task::JoinHandle<()>) {
        self.gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), run).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn a_turn_in_flight_blocks_every_history_operation_on_its_session() {
    let fixture = fixture(None).await;
    fixture.turn("alice", "first").await;
    let control = fixture.runner.history_control();
    let run = fixture.start_waiting("alice").await;

    let first_event =
        fixture.sessions.history(&identity("alice")).await.unwrap().events[0].id.clone();
    let refusals = [
        control.fork(&actor("alice"), ForkRequest::new(identity("alice"))).await.err(),
        control
            .rewind(&actor("alice"), &identity("alice"), RewindTarget::Steps(1), None)
            .await
            .err(),
        control
            .edit(
                &actor("alice"),
                &identity("alice"),
                vec![HistoryEdit::Remove { event_id: first_event }],
                None,
            )
            .await
            .err(),
    ];
    for error in refusals {
        let error = error.expect("a turn in flight refuses the operation");
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::Conflict, "session.history.run_active")
        );
    }
    // Bob's session with the same ID has no turn in flight.
    control.rewind(&actor("bob"), &identity("bob"), RewindTarget::Steps(0), None).await.unwrap();
    // A control over the sessions alone sees the run through the shared activity.
    let shared = HistoryControl::new(fixture.sessions.clone())
        .with_activity(fixture.runner.session_activity().clone());
    let error = shared
        .rewind(&actor("alice"), &identity("alice"), RewindTarget::Steps(1), None)
        .await
        .expect_err("the shared activity sees the run");
    assert_eq!(error.code, "session.history.run_active");

    fixture.finish(run).await;
    let rewound = control
        .rewind(&actor("alice"), &identity("alice"), RewindTarget::Steps(2), None)
        .await
        .expect("the turn has finished");
    assert_eq!(rewound.session.events().len(), 2);
}

#[tokio::test]
async fn runners_sharing_an_activity_see_each_others_runs() {
    let fixture = fixture(None).await;
    let other = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(EchoAgent { gate: Arc::new(Gate::default()) }) as Arc<dyn Agent>)
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .session_activity(fixture.runner.session_activity().clone())
        .build()
        .unwrap();
    let run = fixture.start_waiting("alice").await;
    let error = other
        .history_control()
        .fork(&actor("alice"), ForkRequest::new(identity("alice")))
        .await
        .expect_err("the other runner's run blocks the fork");
    assert_eq!(error.code, "session.history.run_active");
    fixture.finish(run).await;
}

#[tokio::test]
async fn a_running_durable_run_blocks_the_session() {
    let fixture = fixture(None).await;
    let store = Arc::new(InMemoryRunStore::new());
    store
        .enqueue(
            NewRun::new("q", APP, "alice", "agent", serde_json::json!({})).with_session_id("s"),
        )
        .await
        .unwrap();
    store.claim("q", "worker-1", Duration::from_secs(60)).await.unwrap().expect("claimed");
    let control = HistoryControl::new(fixture.sessions.clone())
        .with_run_store(store as Arc<dyn DurableRunStore>);
    let error = control
        .rewind(&actor("alice"), &identity("alice"), RewindTarget::Steps(0), None)
        .await
        .expect_err("a worker runs the session");
    assert_eq!(error.code, "session.history.run_active");
    control.rewind(&actor("bob"), &identity("bob"), RewindTarget::Steps(0), None).await.unwrap();
}

#[tokio::test]
async fn rewinding_before_an_invocation_undoes_its_turn_and_is_audited() {
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let fixture = fixture(Some(recorder.clone())).await;
    fixture.turn("alice", "keep this").await;
    fixture.turn("alice", "my card is 4111").await;
    let history = fixture.sessions.history(&identity("alice")).await.unwrap();
    assert_eq!(history.events.len(), 4);
    let second = history.events[2].invocation_id.clone();

    let outcome = fixture
        .runner
        .history_control()
        .rewind(
            &actor("alice"),
            &identity("alice"),
            RewindTarget::BeforeInvocation(second.clone()),
            Some(history.version()),
        )
        .await
        .unwrap();
    let kept: Vec<String> = outcome
        .session
        .events()
        .all()
        .into_iter()
        .filter_map(|event| event.llm_response.content)
        .flat_map(|content| content.parts)
        .filter_map(|part| part.text().map(str::to_string))
        .collect();
    assert_eq!(kept, ["keep this", "echo: keep this"]);

    let records = recorder.records();
    let history_records: Vec<_> =
        records.iter().filter(|record| record.kind == AuditRecordKind::SessionHistory).collect();
    assert_eq!(history_records.len(), 2, "a decision record and an outcome record");
    let (decision, outcome_record) = (history_records[0], history_records[1]);
    assert_eq!(
        (decision.action.as_str(), decision.decision, decision.outcome),
        ("session.rewind", Some(AuditDecision::Allowed), None)
    );
    assert_eq!(outcome_record.outcome, Some(AuditRecordOutcome::Succeeded));
    assert_eq!(
        outcome_record.ext["removed"],
        serde_json::json!([history.events[2].id, history.events[3].id])
    );
    assert!(outcome_record.payload_ref.is_some(), "a digest stands in for the removed content");
    assert_eq!(outcome_record.actor, actor("alice"));
    let written = format!("{history_records:?}");
    assert!(!written.contains("4111"), "the record never carries the removed content");

    let missing = fixture
        .runner
        .history_control()
        .rewind(&actor("alice"), &identity("alice"), RewindTarget::BeforeInvocation(second), None)
        .await
        .expect_err("the invocation is gone");
    assert_eq!(
        (missing.category, missing.code),
        (ErrorCategory::NotFound, "session.invocation_not_found")
    );
}

#[tokio::test]
async fn an_unrecordable_decision_refuses_the_operation() {
    let fixture = fixture(Some(Arc::new(BrokenRecorder))).await;
    // The broken log also refuses runs, so the session is seeded directly.
    for text in ["one", "two"] {
        let mut event = Event::new("inv");
        event.author = "user".into();
        event.llm_response.content = Some(Content::new("user").with_text(text));
        fixture
            .sessions
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: identity("alice"),
                event,
            })
            .await
            .unwrap();
    }
    let control = fixture.runner.history_control();
    let error = control
        .rewind(&actor("alice"), &identity("alice"), RewindTarget::Steps(1), None)
        .await
        .expect_err("the decision cannot be recorded");
    assert_eq!(error.code, "audit.record_failed");
    let fork = control
        .fork(&actor("alice"), ForkRequest::new(identity("alice")).with_session_id("copy"))
        .await
        .expect_err("the fork cannot be recorded");
    assert_eq!(fork.code, "audit.record_failed");
    assert_eq!(fixture.sessions.history(&identity("alice")).await.unwrap().events.len(), 2);
    assert!(
        fixture.sessions.get_for_identity(&identity("alice")).await.is_ok()
            && fixture
                .sessions
                .history(&AdkIdentity::new(
                    AppName::try_from(APP).unwrap(),
                    UserId::try_from("alice").unwrap(),
                    SessionId::try_from("copy").unwrap(),
                ))
                .await
                .is_err(),
        "nothing was written"
    );
}
