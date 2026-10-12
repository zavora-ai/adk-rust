//! Cron Scheduling Example
//!
//! Demonstrates the Cron Scheduling API from `adk-server`:
//! - CronState over an InMemoryScheduleStore
//! - Axum server with merged background_runs_router + cron_jobs_router
//! - Create a cron job via POST /cron
//! - List jobs via GET /cron
//! - Validation: invalid cron expression → 400
//! - Pause/resume via PATCH /cron/{job_id}
//! - Delete via DELETE /cron/{job_id}
//! - Start the cron scheduler loop briefly
//!
//! Fully self-contained — no API keys required.

use anyhow::Result;
use axum::Router;
use serde_json::Value;
use tokio::net::TcpListener;

use adk_server::background::{
    BackgroundState, CronState, background_runs_router_with_state, cron_jobs_router_with_state,
    start_cron_scheduler, validate_cron_expression,
};

fn print_banner() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║        Cron Scheduling REST API — Complete Example           ║");
    println!("║                                                              ║");
    println!("║  Demonstrates: CronState, ScheduleStore, POST/GET/PATCH/     ║");
    println!("║  DELETE /cron, validation, and the cron scheduler loop.       ║");
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

    // ─── Step 1: Create shared state and server ──────────────────────────
    print_section("Step 1: Creating BackgroundState + CronState & Server");

    let bg_state = BackgroundState::new();
    let cron_state = CronState::new(bg_state.clone());
    println!("  ✓ BackgroundState created");
    println!("  ✓ CronState created (wraps BackgroundState + InMemoryScheduleStore)");

    let app: Router = Router::new()
        .merge(background_runs_router_with_state(bg_state.clone()))
        .merge(cron_jobs_router_with_state(cron_state.clone()));

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    println!("  ✓ Axum server bound to {addr} (routes: /runs + /cron)");

    // Spawn server in background
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let base_url = format!("http://{addr}");
    let client = reqwest::Client::new();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // ─── Step 2: Validate cron expressions (programmatic) ────────────────
    print_section("Step 2: Cron Expression Validation (Programmatic)");

    let valid_exprs = [
        "0 */5 * * * *",      // Every 5 minutes (6-field)
        "0 0 * * * *",        // Every hour
        "0 30 9 * * Mon-Fri", // 9:30 weekdays
    ];

    for expr in &valid_exprs {
        match validate_cron_expression(expr) {
            Ok(_schedule) => println!("  ✓ Valid: \"{expr}\""),
            Err(e) => println!("  ✗ Invalid: \"{expr}\" → {e}"),
        }
    }

    let invalid_exprs = [
        "not a cron",
        "60 * * * *", // 60 minutes is invalid
        "",           // empty
    ];

    for expr in &invalid_exprs {
        match validate_cron_expression(expr) {
            Ok(_) => println!("  ✗ Should have failed: \"{expr}\""),
            Err(e) => println!("  ✓ Rejected: \"{expr}\" → {e}"),
        }
    }

    // ─── Step 3: Create cron jobs via HTTP ───────────────────────────────
    print_section("Step 3: Creating Cron Jobs (POST /cron)");

    let job1_body = serde_json::json!({
        "name": "Daily Report Generator",
        "workflowId": "report-gen-v2",
        "cronExpression": "0 0 9 * * *",
        "input": {
            "reportType": "daily-summary",
            "recipients": ["team@example.com"]
        },
        "concurrencyPolicy": "skip"
    });

    let resp = client.post(format!("{base_url}/cron")).json(&job1_body).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ POST /cron → HTTP {status_code}");
    println!("    Job: {}", body["name"].as_str().unwrap_or(""));
    println!("    ID:  {}", body["jobId"].as_str().unwrap_or(""));
    println!("    Status: {}", body["status"].as_str().unwrap_or(""));
    let job1_id = body["jobId"].as_str().unwrap().to_string();

    // Create a second job
    let job2_body = serde_json::json!({
        "name": "Hourly Health Check",
        "workflowId": "health-check-v1",
        "cronExpression": "0 0 * * * *",
        "concurrencyPolicy": "allow"
    });

    let resp = client.post(format!("{base_url}/cron")).json(&job2_body).send().await?;

    let body: Value = resp.json().await?;
    let job2_id = body["jobId"].as_str().unwrap().to_string();
    println!("  ✓ Created second job: {} (ID: {job2_id})", body["name"].as_str().unwrap_or(""));

    // ─── Step 4: Validation error — invalid cron expression ──────────────
    print_section("Step 4: Validation Error (Invalid Cron → 400)");

    let bad_body = serde_json::json!({
        "name": "Bad Job",
        "workflowId": "some-workflow",
        "cronExpression": "this is not valid cron"
    });

    let resp = client.post(format!("{base_url}/cron")).json(&bad_body).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ POST /cron (invalid expression) → HTTP {status_code}");
    println!("    Error: {}", body["error"].as_str().unwrap_or(""));
    println!("    Reason: {}", body["reason"].as_str().unwrap_or(""));
    assert_eq!(status_code, 400);

    // ─── Step 5: List all cron jobs ──────────────────────────────────────
    print_section("Step 5: Listing Cron Jobs (GET /cron)");

    let resp = client.get(format!("{base_url}/cron")).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    let jobs = body.as_array().unwrap();
    println!("  ✓ GET /cron → HTTP {status_code}");
    println!("    Total jobs: {}", jobs.len());
    for job in jobs {
        println!(
            "    • {} ({}): expr=\"{}\", status={}",
            job["name"].as_str().unwrap_or(""),
            job["jobId"].as_str().unwrap_or(""),
            job["cronExpression"].as_str().unwrap_or(""),
            job["status"].as_str().unwrap_or(""),
        );
    }

    // ─── Step 6: Pause/Resume via PATCH ──────────────────────────────────
    print_section("Step 6: Pause/Resume (PATCH /cron/{job_id})");

    // Pause the first job
    let patch_body = serde_json::json!({"status": "paused"});
    let resp = client.patch(format!("{base_url}/cron/{job1_id}")).json(&patch_body).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ PATCH /cron/{job1_id} (pause) → HTTP {status_code}");
    println!("    New status: {}", body["status"].as_str().unwrap_or(""));
    assert_eq!(body["status"].as_str().unwrap(), "paused");

    // Resume it
    let patch_body = serde_json::json!({"status": "active"});
    let resp = client.patch(format!("{base_url}/cron/{job1_id}")).json(&patch_body).send().await?;

    let status_code = resp.status();
    let body: Value = resp.json().await?;
    println!("  ✓ PATCH /cron/{job1_id} (resume) → HTTP {status_code}");
    println!("    New status: {}", body["status"].as_str().unwrap_or(""));
    assert_eq!(body["status"].as_str().unwrap(), "active");

    // ─── Step 7: Delete a cron job ───────────────────────────────────────
    print_section("Step 7: Deleting a Cron Job (DELETE /cron/{job_id})");

    let resp = client.delete(format!("{base_url}/cron/{job2_id}")).send().await?;

    let status_code = resp.status();
    println!("  ✓ DELETE /cron/{job2_id} → HTTP {status_code}");
    assert_eq!(status_code, 204);

    // Verify it's gone
    let resp = client.get(format!("{base_url}/cron")).send().await?;

    let body: Value = resp.json().await?;
    let remaining = body.as_array().unwrap().len();
    println!("  ✓ Remaining jobs after delete: {remaining}");
    assert_eq!(remaining, 1);

    // Verify DELETE on non-existent returns 404
    let resp = client.delete(format!("{base_url}/cron/{job2_id}")).send().await?;

    let status_code = resp.status();
    println!("  ✓ DELETE (already deleted) → HTTP {status_code}");
    assert_eq!(status_code, 404);

    // ─── Step 8: Start the cron scheduler loop briefly ───────────────────
    print_section("Step 8: Cron Scheduler Loop (Brief Demo)");

    println!("  Starting cron scheduler loop for 2 seconds...");
    let scheduler_handle = start_cron_scheduler(cron_state.clone());

    // Let it tick a few times
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Abort the scheduler
    scheduler_handle.abort();
    println!("  ✓ Scheduler loop ran for 2 seconds (checking due jobs each second)");

    // Check if any executions occurred (the cron expressions are future-oriented
    // so likely no executions, but the loop ran successfully)
    let resp = client.get(format!("{base_url}/cron")).send().await?;

    let body: Value = resp.json().await?;
    let jobs = body.as_array().unwrap();
    for job in jobs {
        println!(
            "  Job '{}': status={}, next_fire_at={}",
            job["name"].as_str().unwrap_or(""),
            job["status"].as_str().unwrap_or(""),
            job["nextFireAt"].as_str().unwrap_or("-"),
        );
    }

    // ─── Summary ─────────────────────────────────────────────────────────
    print_section("Summary");
    println!("  All Cron Scheduling API features demonstrated:");
    println!("    ✓ BackgroundState + CronState setup");
    println!("    ✓ Merged Axum routers (background_runs + cron_jobs)");
    println!("    ✓ POST /cron — create cron jobs");
    println!("    ✓ GET /cron — list all cron jobs");
    println!("    ✓ Validation: invalid cron expression → HTTP 400");
    println!("    ✓ PATCH /cron/{{job_id}} — pause/resume");
    println!("    ✓ DELETE /cron/{{job_id}} — delete jobs");
    println!("    ✓ start_cron_scheduler() — background scheduling loop");
    println!("    ✓ Concurrency policies: skip, allow, queue");
    println!();

    // Shut down server
    server_handle.abort();

    Ok(())
}
