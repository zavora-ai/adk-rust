//! Long-running tool example - proper non-blocking implementation
//!
//! Demonstrates:
//! 1. Tool returns immediately with task_id (doesn't block)
//! 2. Background work runs asynchronously
//! 3. Status check tool to poll progress
//!
//! Run: cargo run --bin long_running

use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(JsonSchema, Serialize, Deserialize)]
struct ReportParams {
    /// The topic for the report
    topic: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
struct StatusParams {
    /// The task ID to check
    task_id: String,
}

#[derive(Clone)]
struct TaskState {
    topic: String,
    status: String,
    progress: u8,
    result: Option<String>,
}

type TaskStore = Arc<RwLock<HashMap<String, TaskState>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let tasks: TaskStore = Arc::new(RwLock::new(HashMap::new()));
    let tasks1 = tasks.clone();
    let tasks2 = tasks.clone();

    // Tool 1: Start report (returns immediately, work runs in background)
    let start_tool = FunctionTool::new(
        "generate_report",
        "Start generating a report. Returns task_id immediately.",
        move |_ctx, args| {
            let tasks = tasks1.clone();
            async move {
                let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("general").to_string();
                let task_id = format!("task_{}", rand::random::<u32>());
                
                // Store initial state
                tasks.write().await.insert(task_id.clone(), TaskState {
                    topic: topic.clone(),
                    status: "processing".to_string(),
                    progress: 0,
                    result: None,
                });

                // Spawn background work (non-blocking)
                let tasks_bg = tasks.clone();
                let tid = task_id.clone();
                tokio::spawn(async move {
                    for i in 1..=5 {
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        if let Some(t) = tasks_bg.write().await.get_mut(&tid) {
                            t.progress = i * 20;
                            if i == 5 {
                                t.status = "completed".to_string();
                                t.result = Some(format!("Comprehensive report on '{}': 15 pages of analysis.", t.topic));
                            }
                        }
                    }
                });

                Ok(json!({"task_id": task_id, "status": "processing", "estimated_time": "10 seconds"}))
            }
        },
    )
    .with_parameters_schema::<ReportParams>()
    .with_long_running(true);

    // Tool 2: Check status
    let status_tool = FunctionTool::new(
        "check_report_status",
        "Check the status of a report generation task",
        move |_ctx, args| {
            let tasks = tasks2.clone();
            async move {
                let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(t) = tasks.read().await.get(task_id) {
                    Ok(json!({
                        "task_id": task_id,
                        "status": t.status,
                        "progress": format!("{}%", t.progress),
                        "result": t.result
                    }))
                } else {
                    Ok(json!({"error": "Task not found"}))
                }
            }
        },
    )
    .with_parameters_schema::<StatusParams>();

    let agent = LlmAgentBuilder::new("report_agent")
        .instruction("You help generate reports. Use generate_report to start a report (returns task_id). Use check_report_status to check progress. Always tell the user the task_id so they can check status later.")
        .model(Arc::new(model))
        .tool(Arc::new(start_tool))
        .tool(Arc::new(status_tool))
        .build()?;

    println!("✅ Long-running tool demo");
    println!("   Try: 'Generate a report on AI'");
    println!("   Then wait 10s and: 'Check status of task_XXXXX'");
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
