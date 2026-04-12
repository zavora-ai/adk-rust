//! LLM-Powered Parallel Shared State Example
//!
//! Three LLM agents coordinate via SharedState to produce a document:
//! - **WriterAgent** drafts content and publishes it to shared state
//! - **ReviewerAgent** waits for the draft, reviews it, publishes feedback
//! - **EditorAgent** waits for the draft, edits it, publishes the final version
//!
//! Run: cargo run --manifest-path examples/parallel_shared_state/Cargo.toml --bin llm

use adk_agent::LlmAgentBuilder;
use adk_core::{
    AdkError, Agent, Content, SessionId, Tool, ToolContext, UserId,
};
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

// ── Tools that use SharedState ───────────────────────────────────────────

/// Tool: publish a key-value pair to shared state.
struct PublishTool;

#[derive(Deserialize, JsonSchema)]
struct PublishArgs {
    /// The key to publish under (e.g. "draft", "review", "final")
    key: String,
    /// The content to publish
    content: String,
}

#[async_trait]
impl Tool for PublishTool {
    fn name(&self) -> &str {
        "publish"
    }

    fn description(&self) -> &str {
        "Publish content to shared state so other agents can access it. Use this to share your work."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The key to publish under (e.g. 'draft', 'review', 'final')"
                },
                "content": {
                    "type": "string",
                    "description": "The content to publish"
                }
            },
            "required": ["key", "content"]
        }))
    }

    async fn execute(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, AdkError> {
        let args: PublishArgs =
            serde_json::from_value(args).map_err(|e| AdkError::tool(e.to_string()))?;

        let shared = ctx.shared_state().ok_or_else(|| {
            AdkError::tool("shared state not available — agent must run inside ParallelAgent with_shared_state()")
        })?;

        shared
            .set_shared(&args.key, serde_json::json!(args.content))
            .await
            .map_err(|e| AdkError::tool(e.to_string()))?;

        tracing::info!(key = %args.key, len = args.content.len(), "published to shared state");

        Ok(serde_json::json!({
            "status": "published",
            "key": args.key,
            "length": args.content.len()
        }))
    }
}

/// Tool: wait for and retrieve content from shared state.
struct WaitForTool;

#[derive(Deserialize, JsonSchema)]
struct WaitForArgs {
    /// The key to wait for
    key: String,
}

#[async_trait]
impl Tool for WaitForTool {
    fn name(&self) -> &str {
        "wait_for"
    }

    fn description(&self) -> &str {
        "Wait for content published by another agent. Blocks until the key is available (up to 60s)."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The key to wait for (e.g. 'draft')"
                }
            },
            "required": ["key"]
        }))
    }

    async fn execute(
        &self,
        ctx: Arc<dyn ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, AdkError> {
        let args: WaitForArgs =
            serde_json::from_value(args).map_err(|e| AdkError::tool(e.to_string()))?;

        let shared = ctx.shared_state().ok_or_else(|| {
            AdkError::tool("shared state not available")
        })?;

        tracing::info!(key = %args.key, "waiting for shared state key...");

        let value = shared
            .wait_for_key(&args.key, Duration::from_secs(60))
            .await
            .map_err(|e| AdkError::tool(e.to_string()))?;

        tracing::info!(key = %args.key, "received shared state key");

        Ok(serde_json::json!({
            "key": args.key,
            "content": value
        }))
    }
}

// ── Agent builders ───────────────────────────────────────────────────────

fn build_writer_agent(model: Arc<dyn adk_core::Llm>) -> Arc<dyn Agent> {
    let agent = LlmAgentBuilder::new("writer-agent")
        .description("Drafts content on a given topic")
        .model(model)
        .instruction(
            "You are a writer. When given a topic, write a short 2-3 paragraph draft about it. \
             Then use the 'publish' tool to publish your draft under the key 'draft'. \
             You MUST call the publish tool with key='draft' and your written content.",
        )
        .tool(Arc::new(PublishTool))
        .build()
        .expect("failed to build writer agent");
    Arc::new(agent)
}

fn build_reviewer_agent(model: Arc<dyn adk_core::Llm>) -> Arc<dyn Agent> {
    let agent = LlmAgentBuilder::new("reviewer-agent")
        .description("Reviews drafts and provides feedback")
        .model(model)
        .instruction(
            "You are a reviewer. First, use the 'wait_for' tool with key='draft' to get the draft \
             written by the writer agent. Then review it and provide constructive feedback. \
             Finally, use the 'publish' tool to publish your review under the key 'review'.",
        )
        .tool(Arc::new(WaitForTool))
        .tool(Arc::new(PublishTool))
        .build()
        .expect("failed to build reviewer agent");
    Arc::new(agent)
}

fn build_editor_agent(model: Arc<dyn adk_core::Llm>) -> Arc<dyn Agent> {
    let agent = LlmAgentBuilder::new("editor-agent")
        .description("Edits drafts into polished final versions")
        .model(model)
        .instruction(
            "You are an editor. First, use the 'wait_for' tool with key='draft' to get the draft \
             written by the writer agent. Then improve it — fix grammar, improve flow, tighten prose. \
             Finally, use the 'publish' tool to publish the edited version under the key 'final'.",
        )
        .tool(Arc::new(WaitForTool))
        .tool(Arc::new(PublishTool))
        .build()
        .expect("failed to build editor agent");
    Arc::new(agent)
}

// ── Main ─────────────────────────────────────────────────────────────────

fn detect_model() -> Result<(Arc<dyn adk_core::Llm>, &'static str), Box<dyn std::error::Error>> {
    if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
        let model = adk_model::GeminiModel::new(&key, "gemini-2.5-flash")?;
        return Ok((Arc::new(model), "Gemini (gemini-2.5-flash)"));
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let config = adk_model::openai::OpenAIConfig::new(key, "gpt-4o-mini");
        let model = adk_model::openai::OpenAIClient::new(config)?;
        return Ok((Arc::new(model), "OpenAI (gpt-4o-mini)"));
    }
    Err("Set GOOGLE_API_KEY or OPENAI_API_KEY".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let (model, provider_name) = detect_model()?;
    println!("=== LLM Parallel Shared State: Document Pipeline ===");
    println!("LLM: {provider_name}\n");

    // Build three LLM agents
    let writer = build_writer_agent(model.clone());
    let reviewer = build_reviewer_agent(model.clone());
    let editor = build_editor_agent(model);

    // Wire into ParallelAgent with shared state
    let parallel = adk_agent::ParallelAgent::new(
        "document-team",
        vec![writer, reviewer, editor],
    )
    .with_shared_state()
    .with_description("Three LLM agents coordinate to draft, review, and edit a document");

    // Set up runner
    let session_service: Arc<dyn adk_session::SessionService> =
        Arc::new(InMemorySessionService::new());

    session_service
        .create(adk_session::CreateRequest {
            app_name: "document-team".to_string(),
            user_id: "user-1".to_string(),
            session_id: Some("session-1".to_string()),
            state: std::collections::HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: "document-team".to_string(),
        agent: Arc::new(parallel),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?;

    println!("Topic: The future of renewable energy\n");
    println!("Running 3 agents in parallel...\n");

    let content = Content::new("user").with_text("Write about the future of renewable energy");

    let mut stream = runner
        .run(
            UserId::new("user-1")?,
            SessionId::new("session-1")?,
            content,
        )
        .await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(ref content) = event.llm_response.content {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            if !text.is_empty() {
                                println!("[{}] {}", event.author, &text[..text.len().min(200)]);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
            }
        }
    }

    println!("\n✓ All agents completed");
    Ok(())
}
