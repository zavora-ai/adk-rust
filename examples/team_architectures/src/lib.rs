//! Shared helpers for the portable team architecture examples.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{
    CompiledTeam, LlmAgentBuilder, RelationshipKind, TeamBudget, TeamContextPolicy,
    TeamFailurePolicy, TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec,
};
use adk_core::{AdkError, Agent, Content, Llm, Part, Tool, ToolContext};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

/// Creates the OpenAI model shared by the members in an example.
pub fn openai_model() -> anyhow::Result<Arc<dyn Llm>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("set OPENAI_API_KEY to run this example"))?;
    let model_id = std::env::var("TEAM_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());
    println!("OpenAI model: {model_id}\n");
    Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, model_id))?))
}

/// A deliberately small, bounded policy used by all four examples.
pub fn bounded_policy() -> TeamPolicy {
    TeamPolicy {
        max_transfer_depth: 3,
        max_delegation_depth: 3,
        max_concurrent_delegations: 3,
        context: TeamContextPolicy::Shared,
        failure: TeamFailurePolicy::Propagate,
        budget: TeamBudget {
            // Streaming providers may emit one event per small text delta. Keep the
            // example bounded without cutting off an otherwise short final answer.
            max_events: Some(4_096),
            max_model_requests: Some(32),
            max_tool_calls: Some(32),
            max_delegations: Some(8),
            max_handoffs: Some(8),
            max_wall_time_ms: Some(120_000),
            ..TeamBudget::default()
        },
        ..TeamPolicy::default()
    }
}

/// Builds the supervisor-handoff team used by the CLI and embedded UI examples.
pub fn supervisor_handoff_team() -> anyhow::Result<(TeamSpec, CompiledTeam)> {
    let model = openai_model()?;
    let billing = Arc::new(
        LlmAgentBuilder::new("billing")
            .description("Resolves invoices, payments, and subscription charges")
            .instruction(
                "You are the billing specialist. Answer the user's billing question directly. \
                 You are now in control; do not try to return to the supervisor.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let technical = Arc::new(
        LlmAgentBuilder::new("technical")
            .description("Troubleshoots errors, configuration, and product behavior")
            .instruction(
                "You are the technical specialist. Give concise troubleshooting steps. \
                 You are now in control; do not try to return to the supervisor.",
            )
            .model(model.clone())
            .build()?,
    ) as Arc<dyn Agent>;
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Routes each request to the one specialist allowed to handle it")
            .instruction(
                "You supervise billing and technical support. For any billing request, call \
                 transfer_to_agent with billing. For any technical request, call it with \
                 technical. A handoff transfers control: do not answer the specialist's part.",
            )
            .model(model)
            .build()?,
    ) as Arc<dyn Agent>;

    let spec = TeamSpec {
        name: "support_team".to_string(),
        description: "A supervisor hands control to exactly one allowed specialist".to_string(),
        coordinator: "supervisor".to_string(),
        members: vec![
            TeamMemberSpec::new("supervisor"),
            TeamMemberSpec::new("billing"),
            TeamMemberSpec::new("technical"),
        ],
        relationships: vec![
            TeamRelationship::new("supervisor", "billing", RelationshipKind::Handoff),
            TeamRelationship::new("supervisor", "technical", RelationshipKind::Handoff),
        ],
        policy: bounded_policy(),
    };
    let team = spec.clone().compile([supervisor, billing, technical])?;
    Ok((spec, team))
}

/// Prints the portable definition before running its compiled root.
pub fn print_spec(spec: &TeamSpec) -> anyhow::Result<()> {
    println!("Portable TeamSpec:\n{}\n", serde_json::to_string_pretty(spec)?);
    Ok(())
}

/// Runs one message through a compiled team and renders control flow.
pub async fn run_team(team: CompiledTeam, app_name: &str, prompt: &str) -> anyhow::Result<()> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: "example-user".to_string(),
            session_id: Some("example-session".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name(app_name)
        .agent(Arc::new(team) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("User: {prompt}\n");
    let mut stream = runner
        .run_str("example-user", "example-session", Content::new("user").with_text(prompt))
        .await?;
    let mut current_author = String::new();
    while let Some(result) = stream.next().await {
        let event = result?;
        if event.author != current_author {
            current_author.clone_from(&event.author);
            println!("\n[agent: {}]", event.author);
        }
        if let Some(content) = event.content() {
            for part in &content.parts {
                match part {
                    Part::Text { text } => print!("{text}"),
                    Part::FunctionCall { name, args, .. } => {
                        println!("\n  call {name}({args})");
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!(
                            "  return {}: {}",
                            function_response.name, function_response.response
                        );
                    }
                    _ => {}
                }
            }
        }
        if let Some(target) = &event.actions.transfer_to_agent {
            println!("  handoff -> {target}");
        }
    }
    println!();
    Ok(())
}

/// Tool that publishes an LLM result to `ParallelAgent` shared state.
pub struct PublishSharedTool {
    name: &'static str,
    description: &'static str,
    key: &'static str,
}

impl PublishSharedTool {
    /// Creates a publisher whose only writable key is fixed in Rust.
    pub fn new(name: &'static str, description: &'static str, key: &'static str) -> Self {
        Self { name, description, key }
    }
}

#[async_trait]
impl Tool for PublishSharedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The completed research note to share"
                }
            },
            "required": ["content"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("publish tool requires a string 'content' field"))?;
        let shared = ctx.shared_state().ok_or_else(|| {
            AdkError::tool(
                "shared state is unavailable; run this member inside ParallelAgent::with_shared_state()",
            )
        })?;
        shared.set_shared(self.key, json!(content)).await?;
        Ok(json!({ "published": self.key }))
    }
}

/// Tool that waits for both research branches and returns their shared notes.
pub struct ReadResearchTool;

#[async_trait]
impl Tool for ReadResearchTool {
    fn name(&self) -> &str {
        "read_research"
    }

    fn description(&self) -> &str {
        "Wait for the facts and risks research branches, then return both notes."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        let shared =
            ctx.shared_state().ok_or_else(|| AdkError::tool("shared state is unavailable"))?;
        let (facts, risks) = tokio::try_join!(
            shared.wait_for_key("facts", Duration::from_secs(60)),
            shared.wait_for_key("risks", Duration::from_secs(60)),
        )?;
        Ok(json!({ "facts": facts, "risks": risks }))
    }
}
