//! Runner plumbing and verdicts shared by every scenario.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{
    Agent, BeforeModelCallback, BeforeModelResult, Content, Event, Part, SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::Value;

pub const APP: &str = "release-tool-fixes";
pub const USER: &str = "user-1";
pub const LIVE_MODEL: &str = "gemini-3.5-flash-lite";

/// Whether a scenario talks to a live provider, a scripted local upstream, or runs in process.
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

/// Passes when every check holds; the evidence lists each check.
pub fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let evidence = checks
        .iter()
        .map(|(ok, text)| format!("[{}] {text}", if *ok { "ok" } else { "FAILED" }))
        .collect::<Vec<_>>()
        .join("; ");
    if checks.iter().all(|(ok, _)| *ok) { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

pub fn skip(detail: impl Into<String>) -> Verdict {
    Verdict::Skip(detail.into())
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
        println!("{label} {id} [{}] {}", mode.label(), short(detail, 400));
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
        let verdict =
            result.unwrap_or_else(|error| Verdict::Fail(format!("setup failed: {error:#}")));
        self.record(id, item, mode, verdict);
    }
}

/// The events and the error of one runner turn.
pub struct Turn {
    pub events: Vec<Event>,
    pub error: Option<String>,
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

    /// Arguments of every call the model made to `tool`, in order.
    pub fn calls_to(&self, tool: &str) -> Vec<Value> {
        self.events
            .iter()
            .filter(|event| !event.llm_response.partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::FunctionCall { name, args, .. } if name == tool => Some(args.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Runs one user message through `Runner` over an in-memory session service.
pub async fn run_turn(agent: Arc<dyn Agent>, text: &str) -> anyhow::Result<Turn> {
    let sessions = Arc::new(InMemorySessionService::new());
    let session = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
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
                error = Some(format!("{} ({})", err.message, err.code));
                break;
            }
        }
    }
    Ok(Turn { events, error })
}

/// Every function response sent to the model, as `(tool name, response)`, in request order.
pub type SeenResponses = Arc<Mutex<Vec<(String, Value)>>>;

/// A before-model callback that records the function responses in each model request.
pub fn record_function_responses() -> (BeforeModelCallback, SeenResponses) {
    let seen: SeenResponses = Arc::default();
    let sink = seen.clone();
    let callback: BeforeModelCallback = Box::new(move |_ctx, request| {
        let sink = sink.clone();
        Box::pin(async move {
            let mut seen = sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            seen.clear();
            for content in &request.contents {
                for part in &content.parts {
                    if let Part::FunctionResponse { function_response, .. } = part {
                        seen.push((
                            function_response.name.clone(),
                            function_response.response.clone(),
                        ));
                    }
                }
            }
            drop(seen);
            Ok(BeforeModelResult::Continue(request))
        })
    });
    (callback, seen)
}

/// Whether `value` has an object key named `key` at any depth.
pub fn has_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(name, inner)| name == key || has_key(inner, key)),
        Value::Array(items) => items.iter().any(|inner| has_key(inner, key)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
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
