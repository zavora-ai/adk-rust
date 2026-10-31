//! A non-idempotent harness call never runs twice and is never stranded when the process
//! stops after the call ran but before its result was saved (spec 3.1 §7.12, §9.2 I37;
//! evaluation finding F-4, scenario s09).
//!
//! Each "process" is a new harness and runner over the same session, approval and ledger
//! stores, as a restarted process sees a durable backend.

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::{
    ActionLedger, AdkError, AdkIdentity, ApprovalScope, ApprovalStore, Event, InMemoryActionLedger,
    InMemoryApprovalStore, Part, Result, SessionId, UserId, async_trait, held_tool_calls,
};
use adk_harness::{Harness, HarnessAgent, HarnessDecision};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use futures::StreamExt;
use serde_json::{Value, json};

/// Sessions whose next save of a `bash` result fails once armed: the process "stops"
/// after the command ran and before its result reached the session.
struct CrashingSessions {
    inner: InMemorySessionService,
    armed: AtomicBool,
}

impl CrashingSessions {
    fn crashes(&self, event: &Event) -> bool {
        let saves_bash_result = event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| {
                matches!(part, Part::FunctionResponse { function_response, .. }
                    if function_response.name == "bash")
            })
        });
        saves_bash_result && self.armed.swap(false, Ordering::SeqCst)
    }
}

fn crash() -> AdkError {
    AdkError::session("simulated crash before the result is saved")
}

#[async_trait]
impl SessionService for CrashingSessions {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.inner.delete(req).await
    }
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        if self.crashes(&event) {
            return Err(crash());
        }
        self.inner.append_event(session_id, event).await
    }
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        if self.crashes(&req.event) {
            return Err(crash());
        }
        self.inner.append_event_for_identity(req).await
    }
    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        self.inner.get_for_identity(identity).await
    }
}

/// The durable backend every process shares.
struct Backend {
    dir: tempfile::TempDir,
    sessions: Arc<CrashingSessions>,
    approvals: Arc<InMemoryApprovalStore>,
    ledger: Arc<InMemoryActionLedger>,
}

impl Backend {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            sessions: Arc::new(CrashingSessions {
                inner: InMemorySessionService::new(),
                armed: AtomicBool::new(false),
            }),
            approvals: Arc::new(InMemoryApprovalStore::new()),
            ledger: Arc::new(InMemoryActionLedger::new()),
        }
    }

    /// A new process running `script`; the ledger is the harness's own, or the runner's.
    fn process(&self, script: Vec<ScriptedResponse>, ledger_on_runner: bool) -> Harness {
        self.process_with(script, ledger_on_runner, None)
    }

    fn process_with(
        &self,
        script: Vec<ScriptedResponse>,
        ledger_on_runner: bool,
        prompter: Option<Arc<common::ScriptedPrompter>>,
    ) -> Harness {
        let mut builder = HarnessAgent::builder("harness")
            .model(Arc::new(ScriptedLlm::new("scripted", script)))
            .workspace(adk_devtools::Workspace::new(self.dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .approval_store(self.approvals.clone());
        if let Some(prompter) = prompter {
            builder = builder.prompter(prompter);
        }
        if !ledger_on_runner {
            builder = builder.action_ledger(self.ledger.clone());
        }
        let agent = Arc::new(builder.build().unwrap());
        let mut runner = agent.configure_runner(
            Runner::builder()
                .app_name(common::APP)
                .agent(agent.clone())
                .session_service(self.sessions.clone() as Arc<dyn SessionService>),
        );
        if ledger_on_runner {
            runner = runner.action_ledger(self.ledger.clone() as Arc<dyn ActionLedger>);
        }
        Harness::with_runner(agent, runner.build().unwrap()).unwrap()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("log.txt")).unwrap_or_default()
    }

    async fn events(&self, user: &UserId, session: &SessionId) -> Vec<Event> {
        self.sessions
            .get(GetRequest {
                app_name: common::APP.into(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap()
            .events()
            .all()
    }
}

/// A run's events and errors; a crashed run ends with an error.
async fn run(stream: adk_core::EventStream) -> (Vec<Event>, Vec<String>) {
    let results = tokio::time::timeout(Duration::from_secs(20), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(event) => events.push(event),
            Err(error) => errors.push(error.to_string()),
        }
    }
    (events, errors)
}

/// The latest response to `bash` in the session.
fn last_bash_answer(events: &[Event]) -> Option<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "bash" =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .next_back()
}

/// Process 1 holds `echo once >> log.txt`; process 2 approves it, runs it, and stops
/// before its result is saved.
async fn hold_then_crash_after_running(
    backend: &Backend,
    ledger_on_runner: bool,
) -> (UserId, SessionId) {
    let user = common::user("alice");
    let first = backend.process(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "echo once >> log.txt" })),
            ScriptedResponse::text("waiting"),
        ],
        ledger_on_runner,
    );
    let session = first.open_session(&user, Some(&common::session_id("s1"))).await.unwrap();
    let (_, errors) = run(first.send(&user, &session, "append once").await.unwrap()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let pending = first.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(backend.log(), "");

    let second = backend.process(vec![ScriptedResponse::text("done")], ledger_on_runner);
    second
        .decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    backend.sessions.armed.store(true, Ordering::SeqCst);
    let (_, errors) = run(second.resume(&user, &session).await.unwrap()).await;
    assert!(errors.iter().any(|error| error.contains("simulated crash")), "{errors:?}");
    assert_eq!(backend.log(), "once\n", "the command ran once");
    (user, session)
}

#[tokio::test]
async fn a_call_that_ran_before_a_crash_is_answered_from_the_ledger_after_restart() {
    let backend = Backend::new();
    let (user, session) = hold_then_crash_after_running(&backend, false).await;

    let third = backend.process(
        vec![ScriptedResponse::text("it already ran"), ScriptedResponse::text("ok")],
        false,
    );
    let (_, errors) = run(third.send(&user, &session, "did it run?").await.unwrap()).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(backend.log(), "once\n", "never run twice");

    let events = backend.events(&user, &session).await;
    assert!(held_tool_calls(&events, "harness", "").is_empty(), "the call is not stranded");
    let answer = last_bash_answer(&events).expect("bash is answered");
    assert_eq!(answer["status"], "already_succeeded", "{answer}");
    assert!(third.pending_approvals(&user, &session).await.unwrap().is_empty());
}

#[tokio::test]
async fn with_the_runners_ledger_a_stranded_call_is_held_again_and_never_runs_twice() {
    let backend = Backend::new();
    let (user, session) = hold_then_crash_after_running(&backend, true).await;

    let third = backend
        .process(vec![ScriptedResponse::text("waiting"), ScriptedResponse::text("ok")], true);
    let (_, errors) = run(third.send(&user, &session, "next").await.unwrap()).await;
    assert!(errors.is_empty(), "{errors:?}");
    let pending = third.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1, "the stranded call is back in the inbox");

    third.decide(&user, &session, &pending[0].fingerprint, HarnessDecision::Approve).await.unwrap();
    let (_, errors) = run(third.resume(&user, &session).await.unwrap()).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(backend.log(), "once\n", "the resumed call is answered from its record");
    let events = backend.events(&user, &session).await;
    assert!(held_tool_calls(&events, "harness", "").is_empty());
    assert_eq!(last_bash_answer(&events).unwrap()["status"], "already_succeeded");
}

#[tokio::test]
async fn a_held_call_that_lost_its_decision_before_running_is_held_again() {
    let backend = Backend::new();
    let user = common::user("alice");
    let first = backend.process(
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "echo once >> log.txt" })),
            ScriptedResponse::text("waiting"),
        ],
        false,
    );
    let session = first.open_session(&user, Some(&common::session_id("s1"))).await.unwrap();
    run(first.send(&user, &session, "append once").await.unwrap()).await;
    let pending = first.pending_approvals(&user, &session).await.unwrap();
    let scope = ApprovalScope::new(common::APP, user.to_string(), session.to_string());
    // A run took the decision and stopped before the call began.
    backend
        .approvals
        .decide(&scope, &pending[0].fingerprint, adk_core::ToolApproval::approve())
        .await
        .unwrap();
    backend.approvals.take_approval(&scope, &pending[0].fingerprint).await.unwrap();

    let second = backend.process(vec![ScriptedResponse::text("done")], false);
    run(second.resume(&user, &session).await.unwrap()).await;
    let pending = second.pending_approvals(&user, &session).await.unwrap();
    assert_eq!(pending.len(), 1, "held again for a decision");
    assert_eq!(backend.log(), "", "it never ran");
}

#[tokio::test]
async fn on_a_live_surface_a_call_that_ran_before_a_crash_never_runs_twice() {
    let backend = Backend::new();
    let (user, session) = hold_then_crash_after_running(&backend, true).await;

    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let third = backend.process_with(
        vec![ScriptedResponse::text("it already ran"), ScriptedResponse::text("ok")],
        true,
        Some(prompter.clone()),
    );
    let (_, errors) = run(third.send(&user, &session, "did it run?").await.unwrap()).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(backend.log(), "once\n", "never run twice");
    let events = backend.events(&user, &session).await;
    assert!(held_tool_calls(&events, "harness", "").is_empty());
    assert_eq!(last_bash_answer(&events).unwrap()["status"], "already_succeeded");
}
