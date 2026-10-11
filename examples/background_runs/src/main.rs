//! Background Runs Example
//!
//! Demonstrates the Background Runs REST API from `adk-server`:
//! - BackgroundState with the embedded DurableWorker
//! - Axum server with background_runs_router
//! - Submit a run via POST /runs
//! - Poll status via GET /runs/{run_id}
//! - Cancellation via DELETE /runs/{run_id}
//!
//! Fully self-contained — no API keys required.

use anyhow::Result;
use axum::Router;
use serde_json::Value;
use tokio::net::TcpListener;

use adk_server::background::{
    BackgroundState, WorkflowRegistry, background_runs_router_with_state,
};

fn print_banner() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║         Background Runs REST API — Complete Example          ║");
    println!("║                                                              ║");
    println!("║  Demonstrates: BackgroundState, DurableWorker,                 ║");
    println!("║  POST /runs, GET /runs/{{id}}, DELETE /runs/{{id}}                ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

fn print_section(title: &str) {
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ {title:<60}│");
    println!("└─────────────────────────────────────────────────────────────┘");
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").with_target(false).init();

    print_banner();

    // ─── Step 1: Create BackgroundState and server ───────────────────────
    print_section("Step 1: Creating BackgroundState & Axum Server");

    // Runs live in an in-memory DurableRunStore; the embedded DurableWorker claims and
    // executes them. Pass a shared store with `with_run_store` to serve several replicas.
    let registry = WorkflowRegistry::new()
        .register("data-pipeline-v1", |input, _cancel| async move {
            Ok(serde_json::json!({ "rows": 1200, "source": input.get("source").cloned() }))
        })
        .register("long-running-analysis", |_input, cancel| async move {
            cancel.cancelled().await;
            Err("cancelled".to_string())
        });
    let bg_state = BackgroundState::new().with_executor(std::sync::Arc::new(registry));
    println!("  ✓ BackgroundState created (InMemoryRunStore + embedded DurableWorker)");

    let app: Router = Router::new().merge(background_runs_router_with_state(bg_state.clone()));

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("  ✓ Axum server bound to {addr}");

    // Spawn server in background
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Give the server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // ─── Step 2: Submit a run via POST /runs ─────────────────────────────
    print_section("Step 2: Submitting a Background Run (POST /runs)");

    let submit_body = serde_json::json!({
        "workflowId": "data-pipeline-v1",
        "input": {
            "source": "s3://bucket/data.csv",
            "format": "csv"
        },
        "maxRetries": 2
    });

    let resp = client.post(format!("{base_url}/runs")).json(&submit_body).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ POST /runs → HTTP {status_code}");
    println!("    Response: {}", serde_json::to_string_pretty(&body)?);

    let run_id = body["runId"].as_str().unwrap().to_string();
    println!("  ✓ Run ID: {run_id}");

    // ─── Step 3: Poll status via GET /runs/{run_id} ──────────────────────
    print_section("Step 3: Polling Run Status (GET /runs/{run_id})");

    // Poll a few times to show status progression
    for attempt in 1..=3 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let resp = client.get(format!("{base_url}/runs/{run_id}")).send().await?;

        let status_code = resp.status();
        let body: Value = resp.json().await?;
        let run_status = body["status"].as_str().unwrap_or("unknown");
        println!("  [{attempt}] GET /runs/{run_id} → HTTP {status_code}");
        println!("      Status: {run_status}");
        println!("      Updated: {}", body["updatedAt"].as_str().unwrap_or(""));

        if run_status == "completed" || run_status == "failed" {
            if let Some(result) = body.get("result") {
                println!("      Result: {result}");
            }
            break;
        }
    }

    // ─── Step 4: Submit another run and cancel it ────────────────────────
    print_section("Step 4: Cancellation (DELETE /runs/{run_id})");

    // Submit a second run with a longer timeout to ensure we can cancel it
    let submit_body2 = serde_json::json!({
        "workflowId": "long-running-analysis",
        "input": {
            "dataset": "production-logs",
            "window": "7d"
        }
    });

    let resp = client.post(format!("{base_url}/runs")).json(&submit_body2).send().await?;

    let body: Value = resp.json().await?;
    let run_id_2 = body["runId"].as_str().unwrap().to_string();
    println!("  ✓ Submitted second run: {run_id_2}");

    // Let the worker start it, then cancel: the embedded worker sees the request at once.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = client.delete(format!("{base_url}/runs/{run_id_2}")).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    let run_status = body["status"].as_str().unwrap_or("unknown");
    println!("  ✓ DELETE /runs/{run_id_2} → HTTP {status_code}");
    println!("    Status after cancel: {run_status}");

    // ─── Step 5: Verify the first run completed ──────────────────────────
    print_section("Step 5: Final Status Check");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let resp = client.get(format!("{base_url}/runs/{run_id}")).send().await?;

    let body: Value = resp.json().await?;
    println!("  Run 1 ({run_id}):");
    println!("    Status: {}", body["status"].as_str().unwrap_or("unknown"));
    if let Some(result) = body.get("result") {
        if !result.is_null() {
            println!("    Result: {result}");
        }
    }

    let resp = client.get(format!("{base_url}/runs/{run_id_2}")).send().await?;

    let body: Value = resp.json().await?;
    println!("  Run 2 ({run_id_2}):");
    println!("    Status: {}", body["status"].as_str().unwrap_or("unknown"));

    // ─── Step 6: Error handling — non-existent run ───────────────────────
    print_section("Step 6: Error Handling (Non-existent Run)");

    let resp = client.get(format!("{base_url}/runs/non-existent-id")).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ GET /runs/non-existent-id → HTTP {status_code}");
    println!("    Error: {}", body["error"].as_str().unwrap_or(""));

    // ─── Summary ─────────────────────────────────────────────────────────
    print_section("Summary");
    println!("  All Background Runs API features demonstrated:");
    println!("    ✓ BackgroundState with InMemoryRunStore + DurableWorker");
    println!("    ✓ POST /runs — submit workflow runs");
    println!("    ✓ GET /runs/{{run_id}} — poll run status");
    println!("    ✓ DELETE /runs/{{run_id}} — cancel running workflows");
    println!("    ✓ Error handling for non-existent runs");
    println!("    ✓ Run lifecycle: queued → running → completed/cancelled");
    println!();

    // Shut down server
    server_handle.abort();

    Ok(())
}
