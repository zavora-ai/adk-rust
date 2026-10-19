//! Runner plumbing and verdicts shared by every scenario.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Content, Event, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part, SessionId,
    UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

pub const USER: &str = "user-1";

/// Whether a scenario talks to a live provider or runs locally.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Scripted,
    Local,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Scripted => "scripted",
            Self::Local => "local",
        }
    }
}

pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

pub fn pass(detail: impl Into<String>) -> Verdict {
    Verdict::Pass(detail.into())
}

pub fn fail(detail: impl Into<String>) -> Verdict {
    Verdict::Fail(detail.into())
}

pub fn check(ok: bool, detail: impl Into<String>) -> Verdict {
    if ok { pass(detail) } else { fail(detail) }
}

/// The result of one scenario.
pub struct Outcome {
    pub id: &'static str,
    pub item: u8,
    pub mode: Mode,
    pub verdict: Verdict,
}

/// Collects outcomes and prints each as it completes.
#[derive(Default)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn record(&mut self, id: &'static str, item: u8, mode: Mode, verdict: Verdict) {
        let (label, detail) = match &verdict {
            Verdict::Pass(detail) => ("PASS", detail),
            Verdict::Fail(detail) => ("FAIL", detail),
            Verdict::Skip(detail) => ("SKIP", detail),
        };
        println!("{label} {id} [{}] {}", mode.label(), short(detail, 200));
        self.outcomes.push(Outcome { id, item, mode, verdict });
    }

    /// Records the scenario, or a failure when it could not be set up.
    pub fn record_result(
        &mut self,
        id: &'static str,
        item: u8,
        mode: Mode,
        result: anyhow::Result<Verdict>,
    ) {
        let verdict = result.unwrap_or_else(|error| fail(format!("setup failed: {error:#}")));
        self.record(id, item, mode, verdict);
    }
}

/// One error, summarised for a report line.
pub fn describe(error: &AdkError) -> String {
    format!("{:?} {}: {}", error.category, error.code, short(&error.message, 120))
}

/// The provider error code recorded on `error`, if any.
pub fn provider_code(error: &AdkError) -> Option<String> {
    error.details.metadata.get("provider_error_code").and_then(|v| v.as_str()).map(str::to_owned)
}

/// The events and the error of one runner turn.
pub struct Turn {
    pub events: Vec<Event>,
    pub error: Option<AdkError>,
}

impl Turn {
    /// The text of the turn's final model answer.
    pub fn answer(&self) -> String {
        self.events
            .iter()
            .rev()
            .filter(|event| !event.llm_response.partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .find(|content| content.role == "model")
            .map(|content| content.parts.iter().filter_map(Part::text).collect::<String>())
            .unwrap_or_default()
    }

    /// The last non-partial model response of the turn.
    pub fn final_response(&self) -> Option<&LlmResponse> {
        self.events
            .iter()
            .rev()
            .map(|event| &event.llm_response)
            .find(|response| !response.partial && response.content.is_some())
    }
}

/// Runs one user message through `LlmAgent` + `Runner` over an in-memory session service.
pub async fn agent_turn(
    model: Arc<dyn Llm>,
    config: Option<GenerateContentConfig>,
    text: &str,
) -> anyhow::Result<Turn> {
    let mut builder = LlmAgentBuilder::new("assistant")
        .model(model)
        .instruction("Follow the user's instructions exactly.");
    if let Some(config) = config {
        builder = builder.generate_content_config(config);
    }
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: "release-consistency".to_string(),
            user_id: USER.to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("release-consistency")
        .agent(Arc::new(builder.build()?))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;
    let mut stream = runner
        .run_with_config(
            UserId::new(USER)?,
            SessionId::new(session.id())?,
            Content::new("user").with_text(text),
            None,
        )
        .await?;
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(err) => {
                error = Some(err);
                break;
            }
        }
    }
    Ok(Turn { events, error })
}

/// Calls the model directly and drains the response stream; the first error ends it.
pub async fn direct_call(model: &dyn Llm, stream: bool) -> (Vec<LlmResponse>, Option<AdkError>) {
    let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text("Say hi.")]);
    let mut responses = Vec::new();
    match model.generate_content(request, stream).await {
        Ok(mut stream) => {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(response) => responses.push(response),
                    Err(error) => return (responses, Some(error)),
                }
            }
            (responses, None)
        }
        Err(error) => (responses, Some(error)),
    }
}

/// Cuts `text` to at most `max` characters for a one-line report.
pub fn short(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}
