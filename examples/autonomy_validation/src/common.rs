//! Provider setup, counting wrappers, and session-history assertions shared by every scenario.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{
    Content, Event, Llm, LlmRequest, LlmResponseStream, Part, Result as AdkResult, SchemaAdapter,
    Tool, ToolContext,
};
use adk_model::anthropic::AnthropicConfig;
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{AnthropicClient, OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{GetRequest, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

/// Model calls made by every provider client in this process.
pub static API_CALLS: AtomicUsize = AtomicUsize::new(0);

/// The provider a scenario runs against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// OpenAI through the Responses API, which current models require for function tools.
    OpenAi,
    /// OpenAI through Chat Completions (`OpenAIClient`).
    OpenAiChat,
    Anthropic,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiChat => "openai-chat",
            Self::Anthropic => "anthropic",
        }
    }
}

/// The outcome of one scenario run.
#[derive(Debug, Clone)]
pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
    /// The model did not follow a directive prompt; the harness retries once.
    Retry(String),
}

/// Builds the result of a check that the run itself could not complete.
pub fn fail(context: &str, error: impl std::fmt::Display) -> Verdict {
    Verdict::Fail(format!("{context}: {error}"))
}

/// API keys and model ids, read from the environment.
pub struct Config {
    openai_key: Option<String>,
    anthropic_key: Option<String>,
    pub openai_model: String,
    pub openai_chat_model: String,
    pub anthropic_model: String,
    pub anthropic_web_model: String,
}

impl Config {
    pub fn from_env() -> Self {
        let key = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        Self {
            openai_key: key("OPENAI_API_KEY"),
            anthropic_key: key("ANTHROPIC_API_KEY"),
            openai_model: key("OPENAI_MODEL").unwrap_or_else(|| "gpt-5.6-luna".to_string()),
            openai_chat_model: key("OPENAI_CHAT_MODEL")
                .unwrap_or_else(|| "gpt-5.4-mini".to_string()),
            anthropic_model: key("ANTHROPIC_MODEL")
                .unwrap_or_else(|| "claude-haiku-5-5".to_string()),
            anthropic_web_model: key("ANTHROPIC_WEB_MODEL")
                .unwrap_or_else(|| "claude-sonnet-5-5".to_string()),
        }
    }

    pub fn has_key(&self, provider: Provider) -> bool {
        match provider {
            Provider::OpenAi | Provider::OpenAiChat => self.openai_key.is_some(),
            Provider::Anthropic => self.anthropic_key.is_some(),
        }
    }

    /// The scenario's default model for `provider`, counted.
    pub fn model(&self, provider: Provider) -> anyhow::Result<Arc<CountingLlm>> {
        match provider {
            Provider::OpenAi => {
                let key = self.openai_key.clone().unwrap_or_default();
                let client = OpenAIResponsesClient::new(OpenAIResponsesConfig::new(
                    key,
                    &self.openai_model,
                ))?;
                Ok(CountingLlm::wrap(Arc::new(client)))
            }
            Provider::OpenAiChat => {
                let key = self.openai_key.clone().unwrap_or_default();
                let client = OpenAIClient::new(OpenAIConfig::new(key, &self.openai_chat_model))?;
                Ok(CountingLlm::wrap(Arc::new(client)))
            }
            Provider::Anthropic => self.anthropic(&self.anthropic_model, |config| config),
        }
    }

    /// An Anthropic model with a customised configuration, counted.
    pub fn anthropic(
        &self,
        model: &str,
        configure: impl FnOnce(AnthropicConfig) -> AnthropicConfig,
    ) -> anyhow::Result<Arc<CountingLlm>> {
        let key = self.anthropic_key.clone().unwrap_or_default();
        let client = AnthropicClient::new(configure(AnthropicConfig::new(key, model)))?;
        Ok(CountingLlm::wrap(Arc::new(client)))
    }
}

/// Counts model calls, per wrapper and process-wide.
pub struct CountingLlm {
    inner: Arc<dyn Llm>,
    calls: AtomicUsize,
}

impl CountingLlm {
    pub fn wrap(inner: Arc<dyn Llm>) -> Arc<Self> {
        Arc::new(Self { inner, calls: AtomicUsize::new(0) })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Llm for CountingLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        API_CALLS.fetch_add(1, Ordering::SeqCst);
        self.inner.generate_content(req, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }
}

type Handler = dyn Fn(Arc<dyn ToolContext>, &Value) -> AdkResult<Value> + Send + Sync;

/// A deterministic tool that records every execution and its arguments.
pub struct CountingTool {
    name: String,
    description: String,
    schema: Value,
    executions: AtomicUsize,
    seen: Mutex<Vec<Value>>,
    handler: Box<Handler>,
}

impl CountingTool {
    pub fn new(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(Arc<dyn ToolContext>, &Value) -> AdkResult<Value> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            description: description.to_string(),
            schema,
            executions: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }

    pub fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }

    pub fn seen(&self) -> Vec<Value> {
        self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.schema.clone())
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(args.clone());
        (self.handler)(ctx, &args)
    }
}

/// The events and first error of one runner turn.
pub struct Turn {
    pub events: Vec<Event>,
    pub error: Option<String>,
}

impl Turn {
    /// `(author, target)` for every transfer the turn made.
    pub fn transfers(&self) -> Vec<(String, String)> {
        self.events
            .iter()
            .filter_map(|event| {
                event
                    .actions
                    .transfer_to_agent
                    .as_ref()
                    .map(|target| (event.author.clone(), target.clone()))
            })
            .collect()
    }
}

/// Runs one user message and drains the stream, keeping the first error.
///
/// A provider error reported inside a response (`error_code` / `error_message`) counts as an
/// error, as does an `Err` item.
pub async fn run_turn(runner: &Runner, user_id: &str, session_id: &str, text: &str) -> Turn {
    let stream =
        match runner.run_str(user_id, session_id, Content::new("user").with_text(text)).await {
            Ok(stream) => stream,
            Err(error) => return Turn { events: Vec::new(), error: Some(error.to_string()) },
        };
    let mut stream = stream;
    let mut events = Vec::new();
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                if error.is_none()
                    && (event.llm_response.error_code.is_some()
                        || event.llm_response.error_message.is_some())
                {
                    error = Some(format!(
                        "{}: {}",
                        event.llm_response.error_code.clone().unwrap_or_default(),
                        event.llm_response.error_message.clone().unwrap_or_default()
                    ));
                }
                events.push(event);
            }
            Err(err) => {
                error = Some(err.to_string());
                break;
            }
        }
    }
    Turn { events, error }
}

/// Loads every persisted event of a session.
pub async fn session_events(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> anyhow::Result<Vec<Event>> {
    let session = service
        .get(GetRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(session.events().all())
}

/// A function call in session history and the responses that answer it.
#[derive(Debug)]
pub struct CallPairing {
    pub id: String,
    pub name: String,
    pub responses: Vec<Value>,
}

/// Pairs every function call id in `events` with the function responses carrying the same id.
///
/// Returns the pairings and a list of problems: calls without an id, calls answered zero or
/// several times, and responses whose id matches no call.
pub fn pair_calls(events: &[Event]) -> (Vec<CallPairing>, Vec<String>) {
    let mut calls: BTreeMap<String, CallPairing> = BTreeMap::new();
    let mut order = Vec::new();
    let mut responses: HashMap<String, Vec<Value>> = HashMap::new();
    let mut problems = Vec::new();
    for event in events.iter().filter(|event| !event.llm_response.partial) {
        let Some(content) = &event.llm_response.content else { continue };
        for part in &content.parts {
            match part {
                Part::FunctionCall { name, id: Some(id), .. } => {
                    if calls.contains_key(id) {
                        problems.push(format!("call id {id} appears twice"));
                    } else {
                        order.push(id.clone());
                        calls.insert(
                            id.clone(),
                            CallPairing {
                                id: id.clone(),
                                name: name.clone(),
                                responses: Vec::new(),
                            },
                        );
                    }
                }
                Part::FunctionCall { name, id: None, .. } => {
                    problems.push(format!("call to {name} has no id"));
                }
                Part::FunctionResponse { function_response, id: Some(id), .. } => {
                    responses
                        .entry(id.clone())
                        .or_default()
                        .push(function_response.response.clone());
                }
                Part::FunctionResponse { function_response, id: None, .. } => {
                    problems.push(format!("response from {} has no id", function_response.name));
                }
                _ => {}
            }
        }
    }
    for (id, answers) in responses {
        match calls.get_mut(&id) {
            Some(call) => call.responses = answers,
            None => problems.push(format!("response id {id} answers no call")),
        }
    }
    let pairings: Vec<CallPairing> = order.into_iter().filter_map(|id| calls.remove(&id)).collect();
    for call in &pairings {
        if call.responses.len() != 1 {
            problems.push(format!(
                "{} ({}) has {} responses",
                call.name,
                call.id,
                call.responses.len()
            ));
        }
    }
    (pairings, problems)
}

/// Shortens provider error text for the results table.
pub fn brief(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 220 {
        format!("{}…", flat.chars().take(220).collect::<String>())
    } else {
        flat
    }
}
