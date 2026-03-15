// MCP OAuth Authentication Example
//
// This example demonstrates connecting to authenticated MCP servers
// using bearer tokens or OAuth2.
//
// Features demonstrated:
// - Bearer token authentication (GitHub Copilot MCP)
// - OAuth2 client credentials flow
// - Authenticated HTTP transport
//
// To run this example:
//   GITHUB_TOKEN=your_token cargo run --example mcp_oauth --features http-transport
//
// GitHub Copilot MCP server:
// - https://api.githubcopilot.com/mcp/
// - Requires a valid GitHub token with Copilot access

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, ReadonlyContext, Toolset};
use adk_model::GeminiModel;
use adk_tool::{McpAuth, McpHttpClientBuilder};
use anyhow::Result;
use rmcp::service::Service;
use std::sync::Arc;
use std::time::Duration;

/// Minimal context for tool discovery
struct SimpleContext;

#[async_trait::async_trait]
impl ReadonlyContext for SimpleContext {
    fn invocation_id(&self) -> &str {
        "init"
    }
    fn agent_name(&self) -> &str {
        "mcp-oauth-agent"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "mcp_oauth_example"
    }
    fn session_id(&self) -> &str {
        "init"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        static CONTENT: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        CONTENT.get_or_init(|| Content::new("user").with_text("init"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    println!("MCP OAuth Authentication Example");
    println!("=================================\n");

    // Check for required environment variables
    let google_api_key = std::env::var("GOOGLE_API_KEY").ok();
    let github_token = std::env::var("GITHUB_TOKEN").ok();

    if google_api_key.is_none() {
        println!("⚠️  GOOGLE_API_KEY not set - agent creation will be skipped");
    }

    if github_token.is_none() {
        println!("⚠️  GITHUB_TOKEN not set - showing usage patterns instead\n");
        print_usage_patterns();
        return Ok(());
    }

    let github_token = github_token.unwrap();

    // GitHub Copilot MCP endpoint
    let copilot_endpoint = "https://api.githubcopilot.com/mcp/";

    println!("Connecting to GitHub Copilot MCP server...");
    println!("Endpoint: {}\n", copilot_endpoint);

    // Connect with bearer token authentication
    let toolset_result = McpHttpClientBuilder::new(copilot_endpoint)
        .with_auth(McpAuth::bearer(&github_token))
        .timeout(Duration::from_secs(60))
        .connect()
        .await;

    match toolset_result {
        Ok(toolset) => {
            println!("✅ Connected to GitHub Copilot MCP server!\n");
            run_with_toolset(toolset, google_api_key).await?;
        }
        Err(e) => {
            let error_msg = e.to_string();
            println!("❌ Failed to connect: {}\n", error_msg);

            // Provide specific guidance based on error type
            if error_msg.contains("Auth required") {
                println!(
                    "The server requires authentication but didn't receive valid credentials."
                );
                println!("Check that your GITHUB_TOKEN is valid and has Copilot access.\n");
            } else if error_msg.contains("Unexpected content type") {
                println!("The server returned an unexpected response format.");
                println!("This usually means:");
                println!("  - The token is invalid or expired");
                println!("  - The token doesn't have the required permissions");
                println!("  - The endpoint URL may have changed\n");
            } else if error_msg.contains("401") || error_msg.contains("403") {
                println!("Authentication failed. Check your token permissions.\n");
            }

            println!("Falling back to public MCP servers to demonstrate auth patterns...\n");

            // Demonstrate with public servers that don't require auth
            demonstrate_public_servers(google_api_key).await?;
        }
    }

    Ok(())
}

async fn run_with_toolset<S>(
    toolset: adk_tool::McpToolset<S>,
    google_api_key: Option<String>,
) -> Result<()>
where
    S: Service<rmcp::RoleClient> + Send + Sync + 'static,
{
    // Create context for tool discovery
    let ctx = Arc::new(SimpleContext) as Arc<dyn ReadonlyContext>;

    // Discover available tools
    match toolset.tools(ctx.clone()).await {
        Ok(tools) => {
            println!("Discovered {} tools:", tools.len());
            for tool in &tools {
                let desc = tool.description();
                let short_desc =
                    if desc.len() > 70 { format!("{}...", &desc[..70]) } else { desc.to_string() };
                println!("  - {}: {}", tool.name(), short_desc);
            }
            println!();

            if tools.is_empty() {
                println!("No tools available from the server.");
                return Ok(());
            }

            // Create agent if we have an API key
            if let Some(api_key) = google_api_key {
                let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

                let mut agent_builder = LlmAgentBuilder::new("mcp-oauth-agent")
                    .description("Agent with authenticated MCP tools")
                    .model(model)
                    .instruction(
                        "You are a helpful assistant with access to MCP tools. \
                         Use these tools to help with tasks.",
                    );

                for tool in tools {
                    agent_builder = agent_builder.tool(tool);
                }

                let agent = agent_builder.build()?;

                println!("✅ Agent created with MCP tools");
                println!("\nStarting interactive console...\n");

                // Run interactive console
                adk_cli::console::run_console(
                    Arc::new(agent),
                    "mcp_oauth_example".to_string(),
                    "user".to_string(),
                )
                .await?;
            } else {
                println!("Set GOOGLE_API_KEY to create an agent with these tools.");
            }
        }
        Err(e) => {
            println!("❌ Failed to discover tools: {}\n", e);
        }
    }

    Ok(())
}

async fn demonstrate_public_servers(google_api_key: Option<String>) -> Result<()> {
    println!("Connecting to public MCP servers (no auth required)...\n");

    // Connect to Fetch MCP server (public, no auth)
    let fetch_server = "https://remote.mcpservers.org/fetch/mcp";
    println!("Connecting to: {}", fetch_server);

    match McpHttpClientBuilder::new(fetch_server).timeout(Duration::from_secs(30)).connect().await {
        Ok(toolset) => {
            println!("✅ Connected to Fetch MCP server!\n");
            run_with_toolset(toolset, google_api_key).await?;
        }
        Err(e) => {
            println!("❌ Failed to connect: {}\n", e);
            print_usage_patterns();
        }
    }

    Ok(())
}

fn print_usage_patterns() {
    println!(
        r#"
MCP Authentication Patterns
===========================

1. BEARER TOKEN (GitHub, API services)
--------------------------------------

use adk_tool::{{McpHttpClientBuilder, McpAuth}};

// GitHub Copilot MCP with personal access token
let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer(std::env::var("GITHUB_TOKEN")?))
    .timeout(Duration::from_secs(60))
    .connect()
    .await?;


2. API KEY (Custom header)
--------------------------

// API key in custom header
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::api_key("X-API-Key", "your-api-key"))
    .connect()
    .await?;


3. OAUTH2 CLIENT CREDENTIALS
----------------------------

use adk_tool::{{McpHttpClientBuilder, McpAuth, OAuth2Config}};

// OAuth2 with client credentials flow
let oauth_config = OAuth2Config::new(
    "your-client-id",
    "https://auth.example.com/oauth/token"
)
.with_secret("your-client-secret")
.with_scopes(vec!["mcp:read".into(), "mcp:write".into()]);

let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::oauth2(oauth_config))
    .connect()
    .await?;


4. CUSTOM HEADERS
-----------------

// Add custom headers alongside authentication
let toolset = McpHttpClientBuilder::new("https://mcp.example.com/v1")
    .with_auth(McpAuth::bearer("token"))
    .header("X-Request-ID", uuid::Uuid::new_v4().to_string())
    .header("X-Client-Version", "1.0.0")
    .connect()
    .await?;


ENVIRONMENT VARIABLES
=====================

For this example, set:

  GITHUB_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
  GOOGLE_API_KEY=your_google_api_key

Run with:

  cargo run --example mcp_oauth --features http-transport

"#
    );
}
