//! Runner plumbing, tools, and verdicts shared by every scenario.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, Event, GenerateContentConfig, Llm, Part, Result as AdkResult, RunConfig, SessionId,
    Tool, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

pub const USER: &str = "user-1";

/// Whether a scenario talks to a live provider or to the scripted local upstream.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Live,
    Scripted,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Scripted => "scripted",
        }
    }
}

pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// The result of one scenario.
pub struct Outcome {
    pub id: &'static str,
    pub finding: &'static str,
    pub mode: Mode,
    pub verdict: Verdict,
}

pub fn pass(detail: impl Into<String>) -> Verdict {
    Verdict::Pass(detail.into())
}

pub fn fail(detail: impl Into<String>) -> Verdict {
    Verdict::Fail(detail.into())
}

/// API keys from the environment; `None` when unset or blank.
pub struct Keys {
    pub openai: Option<String>,
    pub anthropic: Option<String>,
    pub groq: Option<String>,
    pub deepseek: Option<String>,
}

impl Keys {
    pub fn from_env() -> Self {
        let key = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        Self {
            openai: key("OPENAI_API_KEY"),
            anthropic: key("ANTHROPIC_API_KEY"),
            groq: key("GROQ_API_KEY"),
            deepseek: key("DEEPSEEK_API_KEY"),
        }
    }
}

type Handler = dyn Fn(&Value) -> Value + Send + Sync;

/// A deterministic tool that counts its executions.
pub struct FnTool {
    name: &'static str,
    description: &'static str,
    schema: Value,
    handler: Box<Handler>,
    executions: AtomicUsize,
}

impl FnTool {
    pub fn new(
        name: &'static str,
        description: &'static str,
        schema: Value,
        handler: impl Fn(&Value) -> Value + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            description,
            schema,
            handler: Box::new(handler),
            executions: AtomicUsize::new(0),
        })
    }

    pub fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool for FnTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.schema.clone())
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok((self.handler)(&args))
    }
}

fn city_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"city": {"type": "string", "description": "City name"}},
        "required": ["city"],
        "additionalProperties": false
    })
}

/// `get_weather(city)`: a fixed temperature per city.
pub fn weather_tool() -> Arc<FnTool> {
    FnTool::new("get_weather", "Returns the current weather for one city.", city_schema(), |args| {
        let city = args["city"].as_str().unwrap_or("unknown");
        let temperature = 12 + city.len() as i64;
        json!({"city": city, "temperature_c": temperature, "conditions": "clear"})
    })
}

/// `get_time(city)`: a fixed local time per city.
pub fn time_tool() -> Arc<FnTool> {
    FnTool::new("get_time", "Returns the current local time in one city.", city_schema(), |args| {
        let city = args["city"].as_str().unwrap_or("unknown");
        json!({"city": city, "local_time": format!("{:02}:15", 8 + city.len() % 10)})
    })
}

/// `render_badge()`: returns a PNG of a blue circle on white, as tool-result inline data.
pub fn image_tool() -> Arc<FnTool> {
    FnTool::new(
        "render_badge",
        "Renders the team badge and returns it as an image.",
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
        |_| {
            json!({
                "response": {"status": "rendered", "format": "png"},
                "inline_data": [{"mime_type": "image/png", "data": badge_png()}]
            })
        },
    )
}

/// A 64x64 PNG of a blue filled circle on a white background.
pub fn badge_png() -> Vec<u8> {
    const SIZE: usize = 64;
    let mut raw = Vec::with_capacity(SIZE * (SIZE * 3 + 1));
    for y in 0..SIZE {
        raw.push(0); // filter: none
        for x in 0..SIZE {
            let (dx, dy) = (x as i64 - 32, y as i64 - 32);
            let pixel = if dx * dx + dy * dy <= 24 * 24 { [20, 60, 230] } else { [255, 255, 255] };
            raw.extend_from_slice(&pixel);
        }
    }
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(SIZE as u32).to_be_bytes());
    ihdr.extend_from_slice(&(SIZE as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    png.extend_from_slice(&crc_input);
    png.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65_535).collect();
    for (index, block) in blocks.iter().enumerate() {
        out.push(u8::from(index + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// An agent, a runner over an in-memory session service, and a fresh session.
pub struct Harness {
    pub runner: Runner,
    pub session_id: String,
}

pub struct AgentSpec {
    pub instruction: &'static str,
    pub tools: Vec<Arc<dyn Tool>>,
    pub config: Option<GenerateContentConfig>,
    pub output_schema: Option<Value>,
}

impl AgentSpec {
    pub fn new(instruction: &'static str) -> Self {
        Self { instruction, tools: Vec::new(), config: None, output_schema: None }
    }

    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn config(mut self, config: GenerateContentConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }
}

pub fn build_runner(
    app: &str,
    model: Arc<dyn Llm>,
    spec: &AgentSpec,
    sessions: Arc<InMemorySessionService>,
) -> anyhow::Result<Runner> {
    let mut builder = LlmAgentBuilder::new("assistant").model(model).instruction(spec.instruction);
    for tool in &spec.tools {
        builder = builder.tool(tool.clone());
    }
    if let Some(config) = &spec.config {
        builder = builder.generate_content_config(config.clone());
    }
    if let Some(schema) = &spec.output_schema {
        builder = builder.output_schema(schema.clone());
    }
    let agent = builder.build()?;
    Ok(Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?)
}

impl Harness {
    pub async fn new(app: &str, model: Arc<dyn Llm>, spec: &AgentSpec) -> anyhow::Result<Self> {
        let sessions = Arc::new(InMemorySessionService::new());
        let session = sessions
            .create(CreateRequest {
                app_name: app.to_string(),
                user_id: USER.to_string(),
                session_id: None,
                state: HashMap::new(),
            })
            .await?;
        let runner = build_runner(app, model, spec, sessions)?;
        Ok(Self { runner, session_id: session.id().to_string() })
    }

    pub async fn turn(&self, text: &str) -> Turn {
        run_turn(&self.runner, &self.session_id, text, None).await
    }

    pub async fn turn_with(&self, text: &str, config: RunConfig) -> Turn {
        run_turn(&self.runner, &self.session_id, text, Some(config)).await
    }
}

/// The events and first error of one runner turn.
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
            .map(|content| {
                content
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    /// The largest number of function responses carried by one event.
    pub fn max_responses_in_one_event(&self) -> usize {
        self.events
            .iter()
            .filter_map(|event| event.llm_response.content.as_ref())
            .map(|content| {
                content
                    .parts
                    .iter()
                    .filter(|part| matches!(part, Part::FunctionResponse { .. }))
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    /// Thinking parts of the turn's final (non-partial) model events.
    pub fn thinking(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| !event.llm_response.partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::Thinking { thinking, .. } => Some(thinking.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Runs one user message and drains the stream, keeping the first error.
pub async fn run_turn(
    runner: &Runner,
    session_id: &str,
    text: &str,
    run_config: Option<RunConfig>,
) -> Turn {
    let started = async {
        runner
            .run_with_config(
                UserId::new(USER)?,
                SessionId::new(session_id)?,
                Content::new("user").with_text(text),
                run_config,
            )
            .await
    };
    let mut stream = match started.await {
        Ok(stream) => stream,
        Err(error) => return Turn { events: Vec::new(), error: Some(error.to_string()) },
    };
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

/// Cuts `text` to at most `max` characters for a one-line report.
pub fn short(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}
