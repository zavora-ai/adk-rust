//! Runs the advanced agent gallery through the embedded ADK Runtime UI.

use std::sync::Arc;

use adk_agent::ambient::{RunnerTriggerConfig, TriggerSessionPolicy};
use adk_agent::{AmbientAgent, CronTrigger, LlmAgentBuilder};
use adk_artifact::InMemoryArtifactService;
use adk_core::{Agent, MultiAgentLoader, Toolset};
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
use adk_realtime::{RealtimeAgent, openai::OpenAIRealtimeModel};
use adk_runner::Runner;
use adk_server::{SecurityConfig, ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::rmcp::model::ProtocolVersion;
use adk_tool::mcp::rmcp::transport::TokioChildProcess;
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode, ClientServiceExt,
    McpTaskConfig, McpToolset,
};
use advanced_agents_example::{mcp_server_command, openai_api_key, openai_chat_model};

const APP_NAME: &str = "advanced-agents";
const AMBIENT_AGENT: &str = "ambient_monitor";
const LOCAL_USER: &str = "local-user";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    adk_core::ensure_crypto_provider();
    let api_key = openai_api_key()?;
    let chat_model = openai_chat_model(&api_key)?;

    let gateway: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("a2a_gateway")
            .description("OpenAI assistant exposed through the Runtime and A2A protocols.")
            .instruction(
                "You are the public gateway for an advanced ADK-Rust demonstration. Reply in \
                 concise Markdown and explain which ADK capability you are using when asked.",
            )
            .model(Arc::clone(&chat_model))
            .build()?,
    );

    let monitor: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new(AMBIENT_AGENT)
            .description("Scheduled OpenAI operations monitor driven by AmbientAgent.")
            .instruction(
                "You are a background operations monitor. Produce a short Markdown pulse with \
                 a status heading, two observations, and one recommended next check.",
            )
            .model(Arc::clone(&chat_model))
            .build()?,
    );

    let realtime_model = std::env::var("OPENAI_REALTIME_MODEL")
        .unwrap_or_else(|_| "gpt-realtime-2".to_string());
    let voice: Arc<dyn Agent> = Arc::new(
        RealtimeAgent::builder("voice_assistant")
            .description("OpenAI Realtime voice coach with streamed transcript and audio.")
            .model(Arc::new(OpenAIRealtimeModel::new(api_key, realtime_model)))
            .instruction(
                "You are a calm voice coach. Keep replies below twenty seconds, use plain \
                 language, and finish with one actionable suggestion.",
            )
            .voice("marin")
            .modalities(vec!["audio".to_string()])
            .build()?,
    );

    let handler = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler)).with_tasks();
    let mcp_client = handler
        .serve_with_lifecycle(
            TokioChildProcess::new(mcp_server_command()?)?,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await?;
    let negotiated = mcp_client
        .peer_info()
        .map(|info| info.protocol_version.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    tracing::info!(protocol.version = negotiated, "connected to advanced MCP server");
    let mcp_toolset: Arc<dyn Toolset> = Arc::new(
        McpToolset::new(mcp_client).with_task_support(McpTaskConfig::enabled()),
    );
    let warehouse: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("mcp_warehouse")
            .description("Warehouse assistant using MCP discovery and SEP-2663 tasks.")
            .instruction(
                "You manage a warehouse. Always use the MCP tools for stock questions and \
                 restocking. Report the exact final stock returned by the tools.",
            )
            .model(Arc::clone(&chat_model))
            .toolset(mcp_toolset)
            .build()?,
    );

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let ambient_runner = Arc::new(
        Runner::builder()
            // Match the loader name so the UI session routes discover these
            // background runs under the selected ambient agent.
            .app_name(AMBIENT_AGENT)
            .agent(Arc::clone(&monitor))
            .session_service(Arc::clone(&sessions))
            .build()?,
    );
    let ambient_schedule = std::env::var("ADVANCED_AMBIENT_CRON")
        .unwrap_or_else(|_| "*/30 * * * * *".to_string());
    let mut ambient = AmbientAgent::new(
        Arc::clone(&monitor),
        Arc::new(CronTrigger::new(&ambient_schedule)?),
    )
    .with_invoker(
        ambient_runner,
        RunnerTriggerConfig::new(LOCAL_USER)
            .with_session_policy(TriggerSessionPolicy::Shared("ambient-monitor".to_string()))
            .with_prompt(|event| {
                format!(
                    "Scheduled operations pulse triggered by {}. Treat this as a demonstration; \
                     do not claim access to external production systems.",
                    event.source
                )
            }),
    );
    ambient.start().await?;

    let loader = Arc::new(MultiAgentLoader::new(vec![gateway, monitor, voice, warehouse])?);
    let memory_service = Arc::new(InMemoryMemoryService::new());
    let memory = Arc::new(MemoryServiceAdapter::new(memory_service, APP_NAME, LOCAL_USER));
    let span_exporter = adk_telemetry::init_with_adk_exporter(APP_NAME)?;
    let config = ServerConfig::new(loader, sessions)
        .with_security(SecurityConfig::development())
        .with_span_exporter(span_exporter)
        .with_artifact_service(Arc::new(InMemoryArtifactService::new()))
        .with_memory_service(memory);

    let address =
        std::env::var("ADK_UI_ADDRESS").unwrap_or_else(|_| "127.0.0.1:8088".to_string());
    let public_url = std::env::var("ADK_A2A_BASE_URL")
        .unwrap_or_else(|_| format!("http://{address}"));
    let app = ServerBuilder::new(config).with_a2a(public_url).build();
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Advanced ADK Runtime: http://{address}/ui/");
    println!("Agents: a2a_gateway, ambient_monitor, voice_assistant, mcp_warehouse");
    println!(
        "Ambient monitor schedule: {ambient_schedule}; session: ambient-monitor."
    );

    let result = axum::serve(listener, app).await;
    ambient.stop().await?;
    result?;
    Ok(())
}
