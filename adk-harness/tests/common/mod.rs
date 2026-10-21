//! Shared fixture for the harness integration tests: a scripted model, a scripted
//! prompter, a temporary workspace, an in-memory audit log, and a `Harness` over them.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::{Event, Part, SessionId, ToolConfirmationRequest, UserId};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessAgentBuilder,
    InstructionsConfig, PromptAnswer,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::Value;

pub const APP: &str = "harness-tests";

type Answer = Box<dyn Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync>;

/// Answers prompts from a queue and records every prompt; an empty queue denies.
#[derive(Default)]
pub struct ScriptedPrompter {
    answers: Mutex<VecDeque<Answer>>,
    pub prompts: Mutex<Vec<ApprovalPrompt>>,
}

impl std::fmt::Debug for ScriptedPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScriptedPrompter")
    }
}

impl ScriptedPrompter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn then(&self, answer: impl Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync + 'static) {
        self.answers.lock().unwrap().push_back(Box::new(answer));
    }

    pub fn approve_once(&self) {
        self.then(|_| PromptAnswer::ApproveOnce);
    }

    pub fn deny(&self) {
        self.then(|_| PromptAnswer::Deny { message: Some("no".into()) });
    }

    pub fn count(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for ScriptedPrompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        let answer = self.answers.lock().unwrap().pop_front();
        let decided = match &answer {
            Some(answer) => answer(&prompt),
            None => PromptAnswer::Deny { message: Some("no scripted answer".into()) },
        };
        self.prompts.lock().unwrap().push(prompt);
        Ok(decided)
    }
}

pub fn no_user_file() -> InstructionsConfig {
    InstructionsConfig { user_file: None, require_trust: false, ..InstructionsConfig::default() }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub model: Arc<ScriptedLlm>,
    pub harness: Harness,
    pub audit: InMemoryAuditRecorder,
    pub user: UserId,
    pub session: SessionId,
}

pub fn user(id: &str) -> UserId {
    UserId::new(id).unwrap()
}

pub fn session_id(id: &str) -> SessionId {
    SessionId::new(id).unwrap()
}

/// A harness over a fresh temporary workspace with `bash` allowed.
pub async fn fixture(
    responses: Vec<ScriptedResponse>,
    configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fixture_in(dir, responses, configure).await
}

/// A harness whose scripted model reports `model_name`, so provider-specific wiring
/// (spec 3.1 §7.20 provider web tools) sees a provider-shaped name.
#[allow(dead_code)]
pub async fn fixture_named(
    model_name: &str,
    responses: Vec<ScriptedResponse>,
    configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    fixture_with_model(dir, Arc::new(ScriptedLlm::new(model_name, responses)), configure).await
}

pub async fn fixture_in(
    dir: tempfile::TempDir,
    responses: Vec<ScriptedResponse>,
    configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
) -> Fixture {
    fixture_with_model(dir, Arc::new(ScriptedLlm::new("scripted", responses)), configure).await
}

async fn fixture_with_model(
    dir: tempfile::TempDir,
    model: Arc<ScriptedLlm>,
    configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
) -> Fixture {
    let builder = HarnessAgent::builder("harness")
        .model(model.clone())
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(no_user_file())
        .without_compaction();
    let agent = Arc::new(configure(builder).build().unwrap());
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let audit = InMemoryAuditRecorder::new();
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let user = user("alice");
    let session = harness.open_session(&user, Some(&session_id("s1"))).await.unwrap();
    Fixture { dir, model, harness, audit, user, session }
}

impl Fixture {
    pub async fn send(&self, text: &str) -> Vec<Event> {
        self.send_as(&self.user, &self.session, text).await
    }

    pub async fn send_as(&self, user: &UserId, session: &SessionId, text: &str) -> Vec<Event> {
        let stream = self.harness.send(user, session, text).await.unwrap();
        collect(stream).await
    }

    pub async fn resume(&self) -> Vec<Event> {
        let stream = self.harness.resume(&self.user, &self.session).await.unwrap();
        collect(stream).await
    }

    pub fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    pub fn records(&self, action: &str) -> Vec<AuditRecordDraft> {
        self.audit.records().into_iter().filter(|record| record.action == action).collect()
    }
}

/// Collects a run's events, failing on a stream error.
pub async fn collect(stream: adk_core::EventStream) -> Vec<Event> {
    let results = tokio::time::timeout(Duration::from_secs(20), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish");
    results.into_iter().map(|result| result.expect("event")).collect()
}

/// Every function response in `events`, as `(tool, response)`.
pub fn responses(events: &[Event]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The response to the first call of `tool` in `events`.
pub fn response_of(events: &[Event], tool: &str) -> Value {
    responses(events)
        .into_iter()
        .find(|(name, _)| name == tool)
        .map(|(_, response)| response)
        .unwrap_or_else(|| panic!("no response for {tool} in {:#?}", responses(events)))
}

/// What happened to a call, judged by its response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Executed,
    Denied,
    Held,
}

pub fn outcome(response: &Value) -> Outcome {
    if adk_core::is_confirmation_pending(response) {
        return Outcome::Held;
    }
    match response.get("error").and_then(Value::as_str) {
        Some(error)
            if error.contains("denied")
                || error.contains("not run")
                || error.contains("refuse") =>
        {
            Outcome::Denied
        }
        _ => Outcome::Executed,
    }
}

pub fn confirmation_requests(events: &[Event]) -> Vec<ToolConfirmationRequest> {
    events.iter().filter_map(|event| event.actions.tool_confirmation.clone()).collect()
}

/// The system instruction text the model received in its latest request.
pub fn last_system_text(model: &ScriptedLlm) -> String {
    let request = model.last_request().expect("a request");
    request
        .contents
        .iter()
        .filter(|content| content.role == "system")
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Records every span opened while it is the thread's default subscriber, as
/// `"<name> <field>=<value> ..."`.
#[derive(Clone, Default)]
pub struct SpanLog(pub Arc<Mutex<Vec<String>>>);

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
}

impl<S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>>
    tracing_subscriber::Layer<S> for SpanLog
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = Fields(String::new());
        attrs.record(&mut fields);
        self.0.lock().unwrap().push(format!("{}{}", attrs.metadata().name(), fields.0));
    }

    fn on_record(
        &self,
        _id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = Fields(String::new());
        values.record(&mut fields);
        self.0.lock().unwrap().push(format!("record{}", fields.0));
    }
}

impl SpanLog {
    /// Installs the log for this thread until the guard drops.
    pub fn install(&self) -> tracing::subscriber::DefaultGuard {
        use tracing_subscriber::layer::SubscriberExt;
        tracing::subscriber::set_default(tracing_subscriber::registry().with(self.clone()))
    }

    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}
