//! Shared pieces: live model selection, a JSON-schema tool, and a one-turn runner helper.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_artifact::ArtifactService;
use adk_core::{Agent, Content, Llm, Memory, Tool, ToolContext, ToolExecutionStrategy};
use adk_model::anthropic::AnthropicConfig;
use adk_model::{AnthropicClient, GeminiModel, OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::Value;

pub const APP: &str = "validation_app";
pub const USER: &str = "ada";

/// The outcome of one scenario.
pub enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Whether a scenario talks to a live model or only to scripted inputs.
#[derive(Clone, Copy)]
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gemini,
    OpenAi,
    Anthropic,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-3.5-flash-lite",
            Self::OpenAi => "gpt-5.6-terra",
            Self::Anthropic => "claude-sonnet-4-6",
        }
    }

    fn key(self) -> Option<String> {
        let names: &[&str] = match self {
            Self::Gemini => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            Self::OpenAi => &["OPENAI_API_KEY"],
            Self::Anthropic => &["ANTHROPIC_API_KEY"],
        };
        names.iter().find_map(|name| std::env::var(name).ok().filter(|key| !key.is_empty()))
    }

    /// Builds the live model, or `None` when its API key is not set.
    pub fn model(self) -> anyhow::Result<Option<Arc<dyn Llm>>> {
        let Some(key) = self.key() else {
            return Ok(None);
        };
        let model: Arc<dyn Llm> = match self {
            Self::Gemini => Arc::new(GeminiModel::new(key, self.label())?),
            Self::OpenAi => Arc::new(OpenAIClient::new(OpenAIConfig::new(key, self.label()))?),
            Self::Anthropic => {
                Arc::new(AnthropicClient::new(AnthropicConfig::new(key, self.label()))?)
            }
        };
        Ok(Some(model))
    }
}

type Handler = Arc<
    dyn Fn(Arc<dyn ToolContext>, Value) -> BoxFuture<'static, adk_core::Result<Value>>
        + Send
        + Sync,
>;

/// A tool declared with a literal JSON schema.
struct JsonTool {
    name: String,
    description: String,
    schema: Value,
    handler: Handler,
}

/// Builds a tool from a name, a description, a literal JSON schema, and a handler.
pub fn json_tool<F>(name: &str, description: &str, schema: Value, handler: F) -> Arc<dyn Tool>
where
    F: Fn(Arc<dyn ToolContext>, Value) -> BoxFuture<'static, adk_core::Result<Value>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(JsonTool {
        name: name.to_string(),
        description: description.to_string(),
        schema,
        handler: Arc::new(handler),
    })
}

#[async_trait]
impl Tool for JsonTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.schema.clone())
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        (self.handler)(ctx, args).await
    }
}

/// Builds an agent whose tool calls from one model turn run concurrently.
pub fn agent(
    model: Arc<dyn Llm>,
    instruction: &str,
    tools: Vec<Arc<dyn Tool>>,
) -> anyhow::Result<Arc<dyn Agent>> {
    let mut builder = LlmAgentBuilder::new("assistant")
        .description("validation assistant")
        .instruction(instruction)
        .model(model)
        .tool_execution_strategy(ToolExecutionStrategy::Parallel);
    for tool in tools {
        builder = builder.tool(tool);
    }
    Ok(Arc::new(builder.build()?))
}

/// The services a runner is built with.
#[derive(Default)]
pub struct Services {
    pub artifacts: Option<Arc<dyn ArtifactService>>,
    pub memory: Option<Arc<dyn Memory>>,
}

/// Runs one user turn in `session_id` and returns the model's final text.
pub async fn run_turn(
    agent: Arc<dyn Agent>,
    services: Services,
    session_id: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let mut builder = Runner::builder().app_name(APP).agent(agent).session_service(sessions);
    if let Some(artifacts) = services.artifacts {
        builder = builder.artifact_service(artifacts);
    }
    if let Some(memory) = services.memory {
        builder = builder.memory_service(memory);
    }
    let runner = builder.build()?;
    let mut stream =
        runner.run_str(USER, session_id, Content::new("user").with_text(prompt)).await?;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(chunk) = part.text() {
                    text.push_str(chunk);
                }
            }
        }
    }
    Ok(text)
}

/// Shortens model text for a one-line report.
pub fn brief(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 120 {
        format!("{}...", flat.chars().take(120).collect::<String>())
    } else {
        flat
    }
}
