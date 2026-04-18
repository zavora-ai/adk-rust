//! # Agent Registry Example
//!
//! Demonstrates the Agent Registry REST API from ADK-Rust v0.7.0.
//!
//! ## What This Shows
//! - Starting an in-process HTTP server hosting the Agent Registry REST API
//! - Registering agent cards via `POST /api/agents`
//! - Listing all agents via `GET /api/agents`
//! - Retrieving a specific agent by name via `GET /api/agents/{name}`
//! - Filtering agents by tag via query parameters
//! - Deleting an agent and verifying removal with 404
//!
//! ## Prerequisites
//! - No LLM provider or API keys required — this is a pure HTTP API demo
//!
//! ## Run
//! ```bash
//! cargo run --manifest-path examples/agent_registry/Cargo.toml
//! ```

use std::sync::Arc;

use adk_server::registry::{AgentCard, InMemoryAgentRegistryStore, registry_router};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Agent Registry — ADK-Rust v0.7.0        ║");
    println!("╚══════════════════════════════════════════╝\n");

    // --- Step 1: Create the in-memory registry store ---
    // The InMemoryAgentRegistryStore provides thread-safe concurrent access
    // using Arc<RwLock<HashMap>>. For production, implement a persistent backend.
    let store = Arc::new(InMemoryAgentRegistryStore::new());
    println!("📦 Created in-memory agent registry store\n");

    // --- Step 2: Build the Axum router with registry routes ---
    // registry_router() returns a Router with CRUD endpoints for agent cards.
    // We nest it under /api to get /api/agents and /api/agents/{name}.
    let app = axum::Router::new().nest("/api", registry_router(store));

    // --- Step 3: Start the HTTP server on a random port ---
    // Binding to 127.0.0.1:0 lets the OS assign an available port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("🚀 Agent Registry server listening on http://{addr}\n");

    // Spawn the server in the background so we can make HTTP requests against it.
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Build a reqwest client for calling the registry API.
    // All routes require an Authorization header (any non-empty value).
    let client = reqwest::Client::new();
    let base_url = format!("http://{addr}/api");
    let auth_header = "Bearer example-token";

    // --- Step 4: Register two agent cards via POST /api/agents ---
    println!("── Register Agents ──────────────────────────────");

    let search_agent = AgentCard {
        name: "search-agent".to_string(),
        version: "1.0.0".to_string(),
        description: Some("An agent that searches the web for information".to_string()),
        tags: vec!["search".to_string(), "web".to_string()],
        endpoint_url: Some("https://example.com/agents/search".to_string()),
        capabilities: vec!["text".to_string(), "tool-calling".to_string()],
        input_modes: vec!["text".to_string()],
        output_modes: vec!["text".to_string()],
        created_at: "2025-01-15T10:00:00Z".to_string(),
        updated_at: None,
    };

    let qa_agent = AgentCard {
        name: "qa-agent".to_string(),
        version: "2.0.0".to_string(),
        description: Some("A question-answering agent for technical support".to_string()),
        tags: vec!["qa".to_string(), "search".to_string(), "support".to_string()],
        endpoint_url: Some("https://example.com/agents/qa".to_string()),
        capabilities: vec!["text".to_string()],
        input_modes: vec!["text".to_string()],
        output_modes: vec!["text".to_string()],
        created_at: "2025-01-15T11:00:00Z".to_string(),
        updated_at: None,
    };

    // Register the search agent
    let resp = client
        .post(format!("{base_url}/agents"))
        .header("Authorization", auth_header)
        .json(&search_agent)
        .send()
        .await?;
    println!(
        "POST /api/agents (search-agent) → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let body: serde_json::Value = resp.json().await?;
    println!("  Response: {}\n", serde_json::to_string_pretty(&body)?);

    // Register the QA agent
    let resp = client
        .post(format!("{base_url}/agents"))
        .header("Authorization", auth_header)
        .json(&qa_agent)
        .send()
        .await?;
    println!(
        "POST /api/agents (qa-agent) → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let body: serde_json::Value = resp.json().await?;
    println!("  Response: {}\n", serde_json::to_string_pretty(&body)?);

    // --- Step 5: List all agents via GET /api/agents ---
    println!("── List All Agents ─────────────────────────────");

    let resp = client
        .get(format!("{base_url}/agents"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "GET /api/agents → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let agents: Vec<serde_json::Value> = resp.json().await?;
    println!("  Found {} agent(s):", agents.len());
    for agent in &agents {
        println!(
            "    - {} v{}: {}",
            agent["name"].as_str().unwrap_or("?"),
            agent["version"].as_str().unwrap_or("?"),
            agent["description"].as_str().unwrap_or("(no description)")
        );
    }
    println!();

    // --- Step 6: Retrieve a specific agent by name via GET /api/agents/{name} ---
    println!("── Get Agent by Name ───────────────────────────");

    let resp = client
        .get(format!("{base_url}/agents/search-agent"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "GET /api/agents/search-agent → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let card: serde_json::Value = resp.json().await?;
    println!("  Agent card:\n{}\n", serde_json::to_string_pretty(&card)?);

    // --- Step 7: Filter agents by tag via query parameters ---
    // The registry supports filtering by tag using the `tag` query parameter.
    // Only agents whose tags list contains the specified value are returned.
    println!("── Filter Agents by Tag ────────────────────────");

    let resp = client
        .get(format!("{base_url}/agents?tag=search"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "GET /api/agents?tag=search → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let filtered: Vec<serde_json::Value> = resp.json().await?;
    println!(
        "  Found {} agent(s) with tag 'search':",
        filtered.len()
    );
    for agent in &filtered {
        println!(
            "    - {} (tags: {:?})",
            agent["name"].as_str().unwrap_or("?"),
            agent["tags"]
        );
    }
    println!();

    // Filter by a tag that only one agent has
    let resp = client
        .get(format!("{base_url}/agents?tag=support"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "GET /api/agents?tag=support → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let filtered: Vec<serde_json::Value> = resp.json().await?;
    println!(
        "  Found {} agent(s) with tag 'support':",
        filtered.len()
    );
    for agent in &filtered {
        println!(
            "    - {}",
            agent["name"].as_str().unwrap_or("?")
        );
    }
    println!();

    // --- Step 8: Delete an agent and verify removal ---
    // DELETE /api/agents/{name} returns 204 No Content on success.
    // A subsequent GET should return 404 Not Found.
    println!("── Delete Agent and Verify ─────────────────────");

    let resp = client
        .delete(format!("{base_url}/agents/search-agent"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "DELETE /api/agents/search-agent → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );

    // Verify the agent is gone — expect 404
    let resp = client
        .get(format!("{base_url}/agents/search-agent"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    println!(
        "GET /api/agents/search-agent → {} {} (expected 404)",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let error_body: serde_json::Value = resp.json().await?;
    println!("  Error: {}\n", error_body["error"].as_str().unwrap_or("?"));

    // Verify the remaining agent list has only one entry
    let resp = client
        .get(format!("{base_url}/agents"))
        .header("Authorization", auth_header)
        .send()
        .await?;
    let remaining: Vec<serde_json::Value> = resp.json().await?;
    println!(
        "  Remaining agents after deletion: {} (expected 1)",
        remaining.len()
    );
    for agent in &remaining {
        println!(
            "    - {} v{}",
            agent["name"].as_str().unwrap_or("?"),
            agent["version"].as_str().unwrap_or("?")
        );
    }
    println!();

    // --- Shutdown ---
    // Abort the background server task to clean up.
    server_handle.abort();

    println!("✅ Agent Registry example completed successfully.");
    Ok(())
}
