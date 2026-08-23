//! Demonstrates MCP protocol revision `2026-07-28` through an `LlmAgent`.
//!
//! Three things happen, in order:
//!
//! 1. **Handshake.** The client probes with `server/discover` and settles on
//!    `2026-07-28`. The same binary also connects with the default handshake so
//!    you can see both revisions negotiated against one server.
//! 2. **Tasks (SEP-2663).** The client declares the tasks extension, so the
//!    server is allowed to answer a slow call with a task. `McpToolset` polls it
//!    to completion; the agent sees an ordinary tool result.
//! 3. **The agent.** A Gemini-backed `LlmAgent` calls both tools by name.
//!
//! Requires `GOOGLE_API_KEY`. Run with:
//!
//! ```bash
//! cargo run --manifest-path examples/mcp_protocol_revisions/Cargo.toml --bin revisions-agent
//! ```

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, ReadonlyContext, Toolset as _};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService as _};
use adk_tool::mcp::rmcp::model::ProtocolVersion;
use adk_tool::mcp::rmcp::transport::TokioChildProcess;
use adk_tool::mcp::rmcp::{ServiceExt, service::RunningService};
use adk_tool::mcp::{
    AdkClientHandler, AutoDeclineElicitationHandler, ClientLifecycleMode, ClientServiceExt,
    McpTaskConfig, McpToolset,
};
use futures::StreamExt;

/// Minimal context, so the toolset can be asked for its tools outside a run.
struct ListContext {
    content: Content,
}

#[async_trait::async_trait]
impl ReadonlyContext for ListContext {
    fn invocation_id(&self) -> &str {
        "mcp-protocol-revisions"
    }
    fn agent_name(&self) -> &str {
        "warehouse_agent"
    }
    fn user_id(&self) -> &str {
        "demo-user"
    }
    fn app_name(&self) -> &str {
        "mcp_protocol_revisions"
    }
    fn session_id(&self) -> &str {
        "demo-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

/// Builds the command that starts the bundled server.
fn server_command() -> tokio::process::Command {
    let mut binary = std::env::current_exe().expect("current exe");
    binary.pop();
    binary.push("revisions-server");
    tokio::process::Command::new(binary)
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(ListContext { content: Content::new("user") }) as Arc<dyn ReadonlyContext>
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("warn").init();

    println!("\n=== 1. The default handshake ===\n");
    let legacy: RunningService<_, _> = ().serve(TokioChildProcess::new(server_command())?).await?;
    let legacy_version =
        legacy.peer_info().map(|info| info.protocol_version.clone()).unwrap_or_default();
    println!("  negotiated {}", legacy_version.as_str());
    println!("  every MCP server understands this handshake, so it stays the default");
    legacy.cancel().await.ok();

    println!("\n=== 2. Probing with server/discover ===\n");
    let handler = AdkClientHandler::new(Arc::new(AutoDeclineElicitationHandler)).with_tasks();
    let client = handler
        .serve_with_lifecycle(
            TokioChildProcess::new(server_command())?,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await?;
    let version = client.peer_info().map(|info| info.protocol_version.clone()).unwrap_or_default();
    println!("  negotiated {}", version.as_str());
    println!("  had the server refused the probe, Auto would have used the default handshake");

    // `with_task_support` sets how the client polls. The declaration made by
    // `with_tasks` above is what allows the server to answer with a task at all.
    let toolset = McpToolset::new(client).with_task_support(McpTaskConfig::enabled());
    let tools = toolset.tools(ctx()).await?;

    println!("\n=== 3. Tools, and which may run as tasks ===\n");
    for tool in &tools {
        println!(
            "  {:<20} long-running: {}",
            tool.name(),
            if tool.is_long_running() { "yes" } else { "no" }
        );
    }
    println!("\n  SEP-2663 removed the per-tool task contract, so this reports per");
    println!("  connection: the server may answer any call with a task.");

    println!("\n=== 4. A slow call, answered as a task ===\n");
    let restock = tools
        .iter()
        .find(|tool| tool.name() == "restock_warehouse")
        .expect("the server publishes restock_warehouse");
    let started = std::time::Instant::now();
    let raw = restock
        .execute(
            Arc::new(adk_tool::SimpleToolContext::new("demo")) as Arc<dyn adk_core::ToolContext>,
            serde_json::json!({ "item": "widgets", "units": 12 }),
        )
        .await?;
    println!("  called restock_warehouse directly, took {:?}", started.elapsed());
    println!("  result: {raw}");
    println!("\n  The server answered with a task handle and the toolset polled");
    println!("  tasks/get until it completed. The marker in the text above comes");
    println!("  from the server's task branch, which it takes only for a client");
    println!("  that declared the extension.");

    let Ok(api_key) = std::env::var("GOOGLE_API_KEY") else {
        println!("\nSet GOOGLE_API_KEY to run the agent against these tools.");
        return Ok(());
    };

    println!("\n=== 5. An LlmAgent calling both tools ===\n");
    let model_id = std::env::var("GEMINI_MODEL")
        .unwrap_or_else(|_| "gemini-3.7-flash".to_string());
    println!("  model: {model_id}\n");
    let model = GeminiModel::new(&api_key, &model_id)?;
    let mut builder = LlmAgentBuilder::new("warehouse_agent")
        .description("Answers questions about warehouse stock.")
        .instruction(
            "You manage a warehouse. Use count_stock to read stock levels and \
             restock_warehouse to add units. Report what the tools return.",
        )
        .model(Arc::new(model));
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build()?);

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(adk_session::CreateRequest {
            app_name: "mcp_protocol_revisions".to_string(),
            user_id: "demo-user".to_string(),
            session_id: Some("demo-session".to_string()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("mcp_protocol_revisions")
        .agent(agent)
        .session_service(sessions)
        .build()?;
    for prompt in [
        "How many widgets are in stock?",
        "Restock widgets with 12 units, then tell me the new total.",
    ] {
        println!("  user: {prompt}");
        let mut events = runner
            .run_str("demo-user", "demo-session", Content::new("user").with_text(prompt))
            .await?;
        while let Some(event) = events.next().await {
            let event = event?;
            if let Some(content) = event.content() {
                let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
                if !text.trim().is_empty() {
                    println!("  agent: {}", text.trim());
                }
            }
        }
        println!();
    }

    println!("The agent saw ordinary tool results throughout: the task lifecycle");
    println!("is handled inside McpToolset and never reaches the agent.");
    Ok(())
}
