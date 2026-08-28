//! # Agent orchestrator — discover and delegate via the Govern pillar
//!
//! An `LlmAgent` that discovers registered agents with `AgentSearchTool`,
//! then delegates the task to one of them as a sub-agent through
//! `RemoteReasoningEngineAgent` (`reasoningEngines:streamQuery`).
//!
//! ```bash
//! gcloud auth application-default login
//! cargo run --manifest-path examples/agent_orchestrator/Cargo.toml
//! ```
//!
//! Requires `GOOGLE_API_KEY` (Gemini LLM provider), `GOOGLE_CLOUD_PROJECT`,
//! `GOOGLE_CLOUD_LOCATION`, and `VERTEX_REMOTE_ENGINE` naming a deployed
//! Agent Engine (`projects/*/locations/*/reasoningEngines/*`), e.g. one
//! created with `adk-rust deploy agent-engine`.

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_server::agent_engine::remote::RemoteReasoningEngineAgent;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{AgentRegistryClient, AgentRegistryConfig, AgentSearchTool};
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    println!("──────────────────────────────────────────────");
    println!(" Agent orchestrator: registry search + remote delegation");
    println!("──────────────────────────────────────────────");

    let registry = AgentRegistryClient::new_with_adc(AgentRegistryConfig::from_env()?)?;
    let remote_engine = std::env::var("VERTEX_REMOTE_ENGINE")
        .map_err(|_| anyhow::anyhow!("set VERTEX_REMOTE_ENGINE to a reasoningEngines name"))?;

    // The remote engine becomes an ordinary sub-agent.
    let remote_agent = Arc::new(
        RemoteReasoningEngineAgent::builder("remote_specialist")
            .description("A deployed Agent Engine agent that answers delegated questions")
            .resource_name(&remote_engine)
            .build()
            .await?,
    );

    // The orchestrator can also search the registry mid-conversation.
    let search_tool = Arc::new(AgentSearchTool::new(Arc::new(registry)));

    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
    let orchestrator = LlmAgentBuilder::new("orchestrator")
        .description("Finds registered agents and delegates work to them")
        .instruction(
            "You coordinate other agents. Use agent_search to discover registered \
             agents and their skills. Delegate substantive questions to the \
             remote_specialist sub-agent and summarize its answer.",
        )
        .model(Arc::new(model))
        .tool(search_tool)
        .sub_agent(remote_agent)
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "agent-orchestrator".to_string(),
            user_id: "demo-user".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("agent-orchestrator")
        .agent(Arc::new(orchestrator))
        .session_service(session_service)
        .build()?;

    let question = "What registered agents are available, and what does the \
                    remote specialist say about the meaning of life?";
    println!("\nUser: {question}\n");

    let content = Content::new("user").with_text(question);
    let mut events = runner
        .run(UserId::new("demo-user")?, SessionId::new(session.id().to_string())?, content)
        .await?;

    while let Some(event) = events.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text, .. } = part {
                    if !text.is_empty() {
                        println!("[{}] {text}", event.author);
                    }
                }
            }
        }
    }

    println!("\nDone.");
    Ok(())
}
