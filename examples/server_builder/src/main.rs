//! # ServerBuilder Example
//!
//! Demonstrates the `ServerBuilder` API from ADK-Rust for registering custom
//! Axum controllers alongside the built-in REST, A2A, and UI routes.
//!
//! ## What This Shows
//! - Creating a `ServerConfig` with a minimal agent and in-memory session service
//! - Defining custom controller routers for domain-specific endpoints
//! - Using `ServerBuilder::new(config).add_api_routes(...)` to compose them
//! - Custom routes living under `/api` alongside built-in health, session, and runtime routes
//! - Verifying that both custom and built-in endpoints work together
//!
//! ## Prerequisites
//! - No LLM provider or API keys required — this is a pure HTTP API demo
//!
//! ## Run
//! ```bash
//! cargo run --manifest-path examples/server_builder/Cargo.toml
//! ```

use std::sync::Arc;

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Minimal no-op agent — ServerBuilder requires a ServerConfig which needs an
// agent loader. We don't call the agent in this demo; we only exercise the
// custom HTTP routes and the built-in /api/health endpoint.
// ---------------------------------------------------------------------------

struct NoOpAgent;

#[async_trait]
impl Agent for NoOpAgent {
    fn name(&self) -> &str {
        "no-op"
    }

    fn description(&self) -> &str {
        "Placeholder agent for the ServerBuilder demo"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn InvocationContext>,
    ) -> AdkResult<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

// ---------------------------------------------------------------------------
// Domain types for the custom controllers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct Project {
    id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct Automation {
    id: String,
    name: String,
    trigger: String,
}

// ---------------------------------------------------------------------------
// Custom controller: /api/projects
//
// Routes added via `add_api_routes()` are nested under `/api` and receive the
// same auth middleware as the built-in session and runtime routes.
// ---------------------------------------------------------------------------

fn project_routes() -> Router {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
}

async fn list_projects() -> Json<Vec<Project>> {
    Json(vec![
        Project {
            id: "proj-1".into(),
            name: "Website Redesign".into(),
            status: "active".into(),
        },
        Project {
            id: "proj-2".into(),
            name: "API Migration".into(),
            status: "planning".into(),
        },
    ])
}

async fn create_project() -> (axum::http::StatusCode, Json<Project>) {
    // In a real app you'd parse the request body. Here we return a hardcoded
    // project to keep the demo focused on ServerBuilder wiring.
    (
        axum::http::StatusCode::CREATED,
        Json(Project {
            id: "proj-3".into(),
            name: "New Project".into(),
            status: "created".into(),
        }),
    )
}

// ---------------------------------------------------------------------------
// Custom controller: /api/automations
//
// A second set of routes demonstrating that `add_api_routes()` can be called
// multiple times — each call merges additional routes into the API router.
// ---------------------------------------------------------------------------

fn automation_routes() -> Router {
    Router::new()
        .route("/automations", get(list_automations))
        .route("/automations/{id}", get(get_automation))
}

async fn list_automations() -> Json<Vec<Automation>> {
    Json(vec![
        Automation {
            id: "auto-1".into(),
            name: "Nightly Build".into(),
            trigger: "cron: 0 2 * * *".into(),
        },
        Automation {
            id: "auto-2".into(),
            name: "PR Review Bot".into(),
            trigger: "webhook: pull_request.opened".into(),
        },
    ])
}

async fn get_automation(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Automation>, axum::http::StatusCode> {
    // Simulate a lookup — return the automation if the ID matches, else 404.
    match id.as_str() {
        "auto-1" => Ok(Json(Automation {
            id: "auto-1".into(),
            name: "Nightly Build".into(),
            trigger: "cron: 0 2 * * *".into(),
        })),
        "auto-2" => Ok(Json(Automation {
            id: "auto-2".into(),
            name: "PR Review Bot".into(),
            trigger: "webhook: pull_request.opened".into(),
        })),
        _ => Err(axum::http::StatusCode::NOT_FOUND),
    }
}

// ---------------------------------------------------------------------------
// Main — wire everything together with ServerBuilder
// ---------------------------------------------------------------------------

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
    println!("║  ServerBuilder — ADK-Rust v0.7.0         ║");
    println!("╚══════════════════════════════════════════╝\n");

    // --- Step 1: Create a minimal ServerConfig ---
    // ServerConfig requires an agent loader and a session service. We use a
    // no-op agent and in-memory sessions since this demo focuses on custom
    // HTTP routes, not agent execution.
    let agent = Arc::new(NoOpAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);

    println!("📦 Created ServerConfig with no-op agent and in-memory sessions\n");

    // --- Step 2: Build the server with custom routes ---
    //
    // ServerBuilder composes the final Axum router:
    //   - Built-in routes: /api/health, /api/sessions, /api/run, /ui, etc.
    //   - Custom API routes: merged under /api with the same auth middleware
    //   - Custom root routes: merged at the top level (no auth middleware)
    //   - Optional A2A protocol: /.well-known/agent.json, /a2a, /a2a/stream
    //   - Optional shutdown endpoint: POST /api/shutdown for graceful shutdown
    //
    // All routes share the middleware stack: CORS, tracing, timeout, security
    // headers (X-Content-Type-Options, X-Frame-Options, X-XSS-Protection).
    let (app, shutdown_handle) = ServerBuilder::new(config)
        .add_api_routes(project_routes())     // GET/POST /api/projects
        .add_api_routes(automation_routes())   // GET /api/automations, GET /api/automations/{id}
        .enable_shutdown_endpoint()           // POST /api/shutdown for graceful shutdown
        .build_with_shutdown();

    println!("🔧 Built server with custom routes + shutdown endpoint\n");

    // --- Step 3: Start the HTTP server on a random port ---
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("🚀 Server listening on http://{addr}\n");

    // Spawn the server in the background so we can make HTTP requests.
    // with_graceful_shutdown wires the ShutdownHandle so that POST /api/shutdown
    // (or Ctrl+C / SIGTERM) triggers a clean shutdown.
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_handle.signal())
            .await
            .ok();
    });

    let client = reqwest::Client::new();
    let base_url = format!("http://{addr}");

    // --- Step 4: Call the built-in /api/health endpoint ---
    println!("── Built-in: Health Check ───────────────────────");
    let resp = client.get(format!("{base_url}/api/health")).send().await?;
    println!(
        "GET /api/health → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let body: serde_json::Value = resp.json().await?;
    println!("  Response: {}\n", serde_json::to_string_pretty(&body)?);

    // --- Step 5: Call the custom /api/projects endpoints ---
    println!("── Custom: Projects ────────────────────────────");

    let resp = client.get(format!("{base_url}/api/projects")).send().await?;
    println!(
        "GET /api/projects → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let projects: Vec<serde_json::Value> = resp.json().await?;
    println!("  Found {} project(s):", projects.len());
    for p in &projects {
        println!(
            "    - {} ({}): {}",
            p["name"].as_str().unwrap_or("?"),
            p["id"].as_str().unwrap_or("?"),
            p["status"].as_str().unwrap_or("?")
        );
    }
    println!();

    let resp = client.post(format!("{base_url}/api/projects")).send().await?;
    println!(
        "POST /api/projects → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let created: serde_json::Value = resp.json().await?;
    println!(
        "  Created: {} ({})\n",
        created["name"].as_str().unwrap_or("?"),
        created["id"].as_str().unwrap_or("?")
    );

    // --- Step 6: Call the custom /api/automations endpoints ---
    println!("── Custom: Automations ─────────────────────────");

    let resp = client.get(format!("{base_url}/api/automations")).send().await?;
    println!(
        "GET /api/automations → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let automations: Vec<serde_json::Value> = resp.json().await?;
    println!("  Found {} automation(s):", automations.len());
    for a in &automations {
        println!(
            "    - {} ({}): {}",
            a["name"].as_str().unwrap_or("?"),
            a["id"].as_str().unwrap_or("?"),
            a["trigger"].as_str().unwrap_or("?")
        );
    }
    println!();

    let resp = client
        .get(format!("{base_url}/api/automations/auto-1"))
        .send()
        .await?;
    println!(
        "GET /api/automations/auto-1 → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let auto: serde_json::Value = resp.json().await?;
    println!("  Automation: {}\n", serde_json::to_string_pretty(&auto)?);

    // Try a non-existent automation — expect 404
    let resp = client
        .get(format!("{base_url}/api/automations/auto-999"))
        .send()
        .await?;
    println!(
        "GET /api/automations/auto-999 → {} {} (expected 404)\n",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );

    // --- Step 7: Graceful shutdown via POST /api/shutdown ---
    //
    // The shutdown endpoint triggers graceful shutdown: the server stops
    // accepting new connections, completes in-flight requests, and exits.
    // This is how the Electron shell cleanly shuts down the Rust sidecar.
    println!("── Graceful Shutdown ────────────────────────────");

    let resp = client.post(format!("{base_url}/api/shutdown")).send().await?;
    println!(
        "POST /api/shutdown → {} {}",
        resp.status().as_u16(),
        resp.status().canonical_reason().unwrap_or("")
    );
    let body: serde_json::Value = resp.json().await?;
    println!("  Response: {}\n", serde_json::to_string_pretty(&body)?);

    // Wait for the server to finish shutting down
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server_handle).await;

    println!("✅ ServerBuilder example completed successfully.");
    Ok(())
}
