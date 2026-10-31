//! A confirmation nobody answers in time waits in the approval inbox instead of being
//! denied, and a later decision resumes it once (spec 3.3 §7.3, §10 C2, §11 S-I3).
//!
//! The live run asks `claude-sonnet-5-5` with `ANTHROPIC_API_KEY` from the environment, or
//! from the file `HARNESS_ENV_FILE` names, else the repository's `.env`:
//!
//! ```text
//! cargo nextest run -p adk-harness --run-ignored only --test integration \
//!   -E 'test(/^confirmation_wait_tests::/) and test(live_)'
//! ```

#![cfg(feature = "runner")]

use crate::common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use std::sync::Mutex;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApprovalState, ApprovalStore, ApproverSelector, Content, Event, InMemoryApprovalStore,
    InboxQuery, Llm, RunConfig, SessionId, StoreApprovalService, SubjectRef, Tool,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, ToolContext,
    UserId,
};
use adk_harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer};
use adk_model::{ScriptedLlm, ScriptedResponse};
use common::{Outcome, collect, confirmation_requests, fixture, outcome, response_of};
use serde_json::{Value, json};

/// The handler's deadline: far shorter than any person, so every prompt times out.
const TIMEOUT: Duration = Duration::from_millis(50);

/// A person who never answers.
#[derive(Debug)]
struct Away;

#[async_trait::async_trait]
impl ApprovalPrompter for Away {
    async fn prompt(&self, _prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        std::future::pending().await
    }
}

fn append_line() -> ScriptedResponse {
    ScriptedResponse::function_call("bash", json!({ "command": "echo ran >> ran.txt" }))
}

/// An inbox whose requests the `owners` group decides.
fn inbox(store: Arc<dyn ApprovalStore>) -> Arc<StoreApprovalService> {
    Arc::new(
        StoreApprovalService::new(store)
            .with_router(ApprovalPolicy::new([ApproverSelector::group("owners")])),
    )
}

fn owner() -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human("bob")).with_groups(["owners"])
}

/// The scenario on `store`: the run ends with the call waiting in the inbox, not denied;
/// an owner allows it; the next run runs it once.
async fn a_timed_out_call_waits_and_runs_once_after_a_decision(store: Arc<dyn ApprovalStore>) {
    let service = inbox(store);
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;

    let events = f.send("append a line").await;
    assert_eq!(
        outcome(&response_of(&events, "bash")),
        Outcome::Held,
        "the timed-out call waits instead of being denied"
    );
    assert_eq!(confirmation_requests(&events).len(), 1, "the run ends with the call waiting");
    assert!(!f.path("ran.txt").exists(), "nothing runs before the decision");
    let waiting = |user: &UserId, session: &SessionId| {
        let service = service.clone();
        let scope = ApprovalScope::new(common::APP, user.to_string(), session.to_string());
        async move { service.inbox(InboxQuery::default().with_session(&scope)).await.unwrap().items }
    };
    let items = waiting(&f.user, &f.session).await;
    assert_eq!(items.len(), 1, "the call is in the session's inbox");
    assert_eq!(items[0].tool_name, "bash");
    assert_eq!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().len(), 1);

    service.decide(&items[0].id, items[0].version, owner()).await.unwrap();
    let events = f.resume().await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(f.path("ran.txt")).unwrap(), "ran\n", "it ran once");
    assert!(waiting(&f.user, &f.session).await.is_empty(), "nothing waits any more");
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_timed_out_call_waits_in_memory() {
    a_timed_out_call_waits_and_runs_once_after_a_decision(Arc::new(InMemoryApprovalStore::new()))
        .await;
}

#[tokio::test]
async fn a_timed_out_call_waits_in_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("approvals.db").display());
    let store = adk_session::SqliteApprovalStore::new(&url).await.unwrap();
    store.migrate().await.unwrap();
    a_timed_out_call_waits_and_runs_once_after_a_decision(Arc::new(store)).await;
}

/// Answers prompts from a script; `None` never answers. Counts every prompt.
#[derive(Debug, Default)]
struct Prompts {
    script: Mutex<Vec<Option<PromptAnswer>>>,
    asked: AtomicUsize,
}

impl Prompts {
    fn new(script: Vec<Option<PromptAnswer>>) -> Arc<Self> {
        Arc::new(Self { script: Mutex::new(script), asked: AtomicUsize::new(0) })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for Prompts {
    async fn prompt(&self, _prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let next = {
            let mut script = self.script.lock().unwrap();
            if script.is_empty() { None } else { script.remove(0) }
        };
        match next {
            Some(answer) => Ok(answer),
            None => std::future::pending().await,
        }
    }
}

/// Every request of `service` for `scope`, in any state.
async fn requests(service: &StoreApprovalService, scope: &ApprovalScope) -> Vec<ApprovalRequest> {
    let query = InboxQuery::default().with_session(scope).with_states(ApprovalState::ALL);
    service.inbox(query).await.unwrap().items
}

/// Review finding 1: while a call waits in the inbox, a new message reaches the model; the
/// person is not asked about the waiting call again (spec 3.3 §7.3, §7.4).
#[tokio::test]
async fn a_new_message_reaches_the_model_while_a_call_waits() {
    let prompts = Prompts::new(vec![]);
    let f = fixture(vec![append_line(), ScriptedResponse::text("hello again")], |b| {
        b.prompter(prompts.clone()).confirmation_timeout(TIMEOUT)
    })
    .await;
    let events = f.send("append a line").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Held);

    let events = f.send("never mind, just say hello").await;
    assert!(
        events.iter().any(|event| event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| part.text() == Some("hello again"))
        })),
        "the model answers the new message"
    );
    assert!(confirmation_requests(&events).is_empty(), "the waiting call is not held again");
    assert_eq!(prompts.asked(), 1, "the person is asked about the waiting call once");
    assert_eq!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().len(), 1);
    assert!(!f.path("ran.txt").exists());
}

/// Review finding 2: deciding a waiting call in the harness cancels its inbox request, so
/// approving the request later cannot decide the call a second time.
#[tokio::test]
async fn deciding_in_the_harness_cancels_the_inbox_request() {
    let service = inbox(Arc::new(InMemoryApprovalStore::new()));
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    f.send("append a line").await;
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, adk_harness::HarnessDecision::Approve)
        .await
        .unwrap();

    let request = requests(&service, &scope).await.remove(0);
    assert_eq!(request.state, ApprovalState::Cancelled, "the inbox request is closed");
    assert!(service.decide(&request.id, request.version, owner()).await.is_err());
    f.resume().await;
    assert_eq!(std::fs::read_to_string(f.path("ran.txt")).unwrap(), "ran\n", "it ran once");
}

/// An in-memory approval store that records every decision written to it.
#[derive(Debug, Default)]
struct RecordingStore {
    inner: InMemoryApprovalStore,
    written: Mutex<Vec<ToolConfirmationDecision>>,
}

#[async_trait::async_trait]
impl ApprovalStore for RecordingStore {
    async fn record_pending(
        &self,
        scope: &ApprovalScope,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending(scope, request).await
    }
    async fn pending(
        &self,
        scope: &ApprovalScope,
    ) -> adk_core::Result<Vec<adk_core::PendingApproval>> {
        self.inner.pending(scope).await
    }
    async fn decide(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        approval: adk_core::ToolApproval,
    ) -> adk_core::Result<()> {
        self.written.lock().unwrap().push(approval.decision);
        self.inner.decide(scope, fingerprint, approval).await
    }
    async fn take_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<ToolConfirmationDecision>> {
        self.inner.take_decision(scope, fingerprint).await
    }
    async fn has_decision(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<bool> {
        self.inner.has_decision(scope, fingerprint).await
    }
    async fn record_pending_keyed(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<()> {
        self.inner.record_pending_keyed(scope, fingerprint, request).await
    }
    async fn take_approval(
        &self,
        scope: &ApprovalScope,
        fingerprint: &str,
    ) -> adk_core::Result<Option<adk_core::ToolApproval>> {
        self.inner.take_approval(scope, fingerprint).await
    }
    async fn create_request(&self, request: &ApprovalRequest) -> adk_core::Result<ApprovalRequest> {
        self.inner.create_request(request).await
    }
    async fn get_request(&self, id: &str) -> adk_core::Result<Option<ApprovalRequest>> {
        self.inner.get_request(id).await
    }
    async fn replace_request(
        &self,
        request: &ApprovalRequest,
        expected_version: u64,
    ) -> adk_core::Result<bool> {
        self.inner.replace_request(request, expected_version).await
    }
    async fn list_requests(
        &self,
        query: &adk_core::ApprovalRequestQuery,
    ) -> adk_core::Result<Vec<ApprovalRequest>> {
        self.inner.list_requests(query).await
    }
    async fn put_delegation(
        &self,
        delegation: &adk_core::ApprovalDelegation,
    ) -> adk_core::Result<()> {
        self.inner.put_delegation(delegation).await
    }
    async fn delegations_to(
        &self,
        to: &SubjectRef,
    ) -> adk_core::Result<Vec<adk_core::ApprovalDelegation>> {
        self.inner.delegations_to(to).await
    }
    async fn delete_delegation(&self, id: &str) -> adk_core::Result<bool> {
        self.inner.delete_delegation(id).await
    }
}

/// Review finding 4 of #1044: deciding in the harness withdraws the inbox request
/// without writing a denial, so the only decision written for the call is the person's
/// and no run can take another first.
#[tokio::test]
async fn deciding_in_the_harness_writes_only_the_persons_decision() {
    let store = Arc::new(RecordingStore::default());
    let service = inbox(store.clone());
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    f.send("append a line").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, adk_harness::HarnessDecision::Approve)
        .await
        .unwrap();

    assert_eq!(*store.written.lock().unwrap(), [ToolConfirmationDecision::Approve]);
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());
    let request = requests(&service, &scope).await.remove(0);
    assert_eq!(request.state, ApprovalState::Cancelled, "the inbox request is closed");
    f.resume().await;
    assert_eq!(std::fs::read_to_string(f.path("ran.txt")).unwrap(), "ran\n", "it ran once");
}

/// An approver who decided the inbox request first keeps the decision: the harness
/// refuses the person's, rather than write one after it (spec 3.3 §7.3).
#[tokio::test]
async fn deciding_in_the_harness_after_the_inbox_is_refused() {
    let service = inbox(Arc::new(InMemoryApprovalStore::new()));
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    f.send("append a line").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());
    let request = requests(&service, &scope).await.remove(0);
    // Resolved by the approver, its outcome not yet written.
    let mut resolved = request.clone();
    resolved.state = ApprovalState::Rejected;
    resolved.version += 1;
    assert!(
        service
            .approval_store()
            .unwrap()
            .replace_request(&resolved, request.version)
            .await
            .unwrap()
    );

    let error = f
        .harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, adk_harness::HarnessDecision::Approve)
        .await
        .unwrap_err();
    assert_eq!(error.code, "harness.decision.invalid");
    assert!(
        !service
            .approval_store()
            .unwrap()
            .has_decision(&scope, &pending[0].fingerprint)
            .await
            .unwrap()
    );
}

/// Review finding 2: when the person approves the call again live, the request left
/// waiting for it is cancelled and its entry cleared, so the next identical call is asked
/// about, not run on the stale request.
#[tokio::test]
async fn a_live_approval_closes_the_request_left_waiting() {
    let service = inbox(Arc::new(InMemoryApprovalStore::new()));
    let prompts = Prompts::new(vec![
        None,
        Some(PromptAnswer::ApproveOnce),
        Some(PromptAnswer::Deny { message: Some("not again".into()) }),
    ]);
    let script = vec![
        append_line(),
        append_line(),
        ScriptedResponse::text("done"),
        append_line(),
        ScriptedResponse::text("refused"),
    ];
    let f = fixture(script, |b| {
        b.prompter(prompts.clone()).confirmation_timeout(TIMEOUT).approval_service(service.clone())
    })
    .await;
    let scope = ApprovalScope::new(common::APP, f.user.to_string(), f.session.to_string());

    f.send("append a line").await;
    let waiting = requests(&service, &scope).await;
    assert_eq!(waiting.len(), 1);

    let events = f.send("do it now, I am here").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    let closed = service.get(&waiting[0].id).await.unwrap().unwrap();
    assert_eq!(closed.state, ApprovalState::Cancelled, "the stale request is closed");
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());
    assert!(service.decide(&closed.id, closed.version, owner()).await.is_err());

    let events = f.send("and once more").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied, "asked and refused");
    assert_eq!(prompts.asked(), 3);
    assert_eq!(std::fs::read_to_string(f.path("ran.txt")).unwrap(), "ran\n", "it ran once");
}

/// `deny_on_confirmation_timeout` denies the same call, with the harness's store in place.
#[tokio::test]
async fn the_opt_out_denies_the_same_call() {
    let f = fixture(vec![append_line(), ScriptedResponse::text("done")], |b| {
        b.prompter(Arc::new(Away)).confirmation_timeout(TIMEOUT)
    })
    .await;
    let runner = f.harness.runner();
    let config = RunConfig { deny_on_confirmation_timeout: true, ..runner.run_config().clone() };
    let stream = runner
        .run_with_config(
            f.user.clone(),
            f.session.clone(),
            Content::new("user").with_text("append a line"),
            Some(config),
        )
        .await
        .unwrap();
    let events = collect(stream).await;

    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());
    assert!(!f.path("ran.txt").exists());
}

/// Sends one invoice per call.
#[derive(Default)]
struct SendInvoice(AtomicUsize);

#[async_trait::async_trait]
impl Tool for SendInvoice {
    fn name(&self) -> &str {
        "send_invoice"
    }

    fn description(&self) -> &str {
        "Sends the invoice to the client"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "sent": true }))
    }
}

/// A handler that never answers.
#[derive(Debug)]
struct NeverAnswers;

#[async_trait::async_trait]
impl ToolConfirmationHandler for NeverAnswers {
    async fn decide(
        &self,
        _request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        std::future::pending().await
    }
}

/// An agent outside the harness: a timeout without a store denies the call, as before;
/// with a store the call waits and a decision runs it once.
#[tokio::test]
async fn an_agent_without_a_store_still_denies_and_with_one_waits() {
    async fn run(store: Option<Arc<InMemoryApprovalStore>>, tool: Arc<SendInvoice>) -> Runner {
        let model = Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call("send_invoice", json!({ "invoice": "INV-10" })),
                ScriptedResponse::text("sent"),
                ScriptedResponse::text("sent"),
            ],
        ));
        let agent = adk_agent::LlmAgentBuilder::new("billing")
            .model(model)
            .tool(tool)
            .require_tool_confirmation("send_invoice")
            .build()
            .unwrap();
        let sessions: Arc<dyn adk_session::SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        sessions
            .create(adk_session::CreateRequest {
                app_name: "billing".into(),
                user_id: "ana".into(),
                session_id: Some("s1".into()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        let config = RunConfig::builder()
            .tool_confirmation_handler(Arc::new(NeverAnswers))
            .tool_confirmation_timeout(TIMEOUT)
            .build();
        let builder = adk_runner::Runner::builder()
            .app_name("billing")
            .agent(Arc::new(agent))
            .session_service(sessions)
            .run_config(config);
        let builder = match store {
            Some(store) => builder.approval_store(store),
            None => builder,
        };
        builder.build().unwrap()
    }
    type Runner = adk_runner::Runner;
    async fn turn(runner: &Runner, text: &str) -> Vec<Event> {
        let user = UserId::new("ana").unwrap();
        let session = SessionId::new("s1").unwrap();
        collect(runner.run(user, session, Content::new("user").with_text(text)).await.unwrap())
            .await
    }

    let tool = Arc::new(SendInvoice::default());
    let runner = run(None, tool.clone()).await;
    let events = turn(&runner, "send INV-10").await;
    assert_eq!(
        response_of(&events, "send_invoice"),
        json!({ "error": "Tool 'send_invoice' execution denied by confirmation policy" }),
        "without a store a timeout denies, as before"
    );
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);

    let store = Arc::new(InMemoryApprovalStore::new());
    let tool = Arc::new(SendInvoice::default());
    let runner = run(Some(store.clone()), tool.clone()).await;
    let events = turn(&runner, "send INV-10").await;
    assert_eq!(outcome(&response_of(&events, "send_invoice")), Outcome::Held);
    let scope = adk_core::ApprovalScope::new("billing", "ana", "s1");
    let pending = store.pending(&scope).await.unwrap();
    assert_eq!(pending.len(), 1, "the call waits in the store");
    assert_eq!(tool.0.load(Ordering::SeqCst), 0);

    store.decide(&scope, &pending[0].fingerprint, adk_core::ToolApproval::approve()).await.unwrap();
    let events = turn(&runner, "").await;
    assert_eq!(outcome(&response_of(&events, "send_invoice")), Outcome::Executed);
    assert_eq!(tool.0.load(Ordering::SeqCst), 1, "the invoice is sent once");
}

/// `KEY=VALUE` lines of the env file, without printing any value.
fn env_key(name: &str) -> Option<String> {
    if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
        return Some(value);
    }
    let path = std::env::var("HARNESS_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(".env"));
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let line = line.trim();
        let (key, value) = line.strip_prefix("export ").unwrap_or(line).split_once('=')?;
        let value = value.trim().trim_matches('"').trim_matches('\'');
        (key.trim() == name && !value.is_empty()).then(|| value.to_string())
    })
}

/// The scenario on `claude-sonnet-5-5`: the model's call waits in the inbox through the
/// timeout; a new message is answered by the model without asking about the waiting call
/// again; the call runs once after an owner allows it.
#[tokio::test]
#[ignore = "calls the Anthropic API with ANTHROPIC_API_KEY"]
async fn live_a_timed_out_call_waits_and_runs_once() {
    let Some(key) = env_key("ANTHROPIC_API_KEY") else {
        println!("skipped: no ANTHROPIC_API_KEY");
        return;
    };
    let model: Arc<dyn Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key,
            "claude-sonnet-5-5",
        ))
        .unwrap(),
    );
    let service = inbox(Arc::new(InMemoryApprovalStore::new()));
    let prompts = Prompts::new(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let agent = Arc::new(
        adk_harness::HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(common::no_user_file())
            .prompter(prompts.clone())
            .confirmation_timeout(TIMEOUT)
            .approval_service(service.clone())
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder()
                .app_name(common::APP)
                .agent(agent.clone())
                .session_service(sessions),
        )
        .build()
        .unwrap();
    let harness = adk_harness::Harness::with_runner(agent, runner).unwrap();
    let user = common::user("alice");
    let session = harness.open_session(&user, Some(&common::session_id("s1"))).await.unwrap();

    let task = "Use the bash tool to run exactly this command: echo live >> live.txt \
                Then reply with one word: done.";
    let events = collect(harness.send(&user, &session, task).await.unwrap()).await;
    let held = confirmation_requests(&events);
    assert_eq!(held.len(), 1, "the call waits: {:#?}", common::responses(&events));
    let written = dir.path().join("live.txt");
    assert!(!written.exists(), "nothing runs before the decision");
    let scope = ApprovalScope::new(common::APP, user.to_string(), session.to_string());
    let items = service.inbox(InboxQuery::default().with_session(&scope)).await.unwrap().items;
    assert_eq!(items.len(), 1, "the call is in the session's inbox");
    println!("waiting: {} {}", items[0].tool_name, items[0].args_redacted);

    let aside = "Do not call any tool. Reply with exactly one word: hello.";
    let events = collect(harness.send(&user, &session, aside).await.unwrap()).await;
    let reply = final_text(&events);
    assert!(!reply.is_empty(), "the model answers the new message");
    assert_eq!(prompts.asked(), 1, "the waiting call is not asked about again");
    assert!(!written.exists(), "the waiting call has not run");
    println!("while waiting, the model replied: {reply:?}");

    service.decide(&items[0].id, items[0].version, owner()).await.unwrap();
    let events = collect(harness.resume(&user, &session).await.unwrap()).await;
    let text = std::fs::read_to_string(&written).unwrap();
    assert_eq!(text.matches("live").count(), 1, "it ran once: {text:?}");
    println!("ran once; model replied: {:?}", final_text(&events));
}

/// The text of the run's last complete model response.
fn final_text(events: &[Event]) -> String {
    events
        .iter()
        .filter(|event| !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content.as_ref())
        .filter(|content| content.role == "model")
        .map(|content| content.parts.iter().filter_map(|part| part.text()).collect::<String>())
        .rfind(|text| !text.is_empty())
        .unwrap_or_default()
}
