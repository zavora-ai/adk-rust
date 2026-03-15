//! # Ralph
//!
//! An autonomous agent loop that runs continuously until all PRD items are complete.
//! No bash scripts needed — everything runs within ADK-Rust.
//!
//! ## Architecture
//!
//! - **Loop Agent**: Main orchestrator that cycles through tasks
//! - **Worker Agent**: Executes individual tasks with focused context
//! - **Custom Tools**: Git, file operations, quality checks, and PRD management
//!
//! ## Usage
//!
//! ```bash
//! # Set your API key
//! export GOOGLE_API_KEY=your-key-here
//!
//! # Run Ralph
//! cargo run -p ralph
//! ```

use adk_agent::LoopAgent;
use adk_core::Tool;
use anyhow::Result;
use colored::Colorize;
use std::sync::{Arc, Mutex};

mod agents;
mod models;
mod tools;

use agents::create_loop_agent;
use models::{Prd, RalphConfig};
use tools::{FileTool, GitTool, PrdTool, TestTool};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("ralph=info,adk=info").init();
    dotenvy::dotenv().ok();

    let config = RalphConfig::from_env()?;
    let prd = Arc::new(Mutex::new(Prd::load(&config.prd_path)?));

    println!("{}", "🤖 Ralph Starting...".bright_green().bold());
    println!("Project: {}", prd.lock().unwrap().project);
    println!("Description: {}", prd.lock().unwrap().description);

    let project_root = std::env::current_dir()?
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Create shared tools
    let prd_tool: Arc<dyn Tool> =
        Arc::new(PrdTool::new(prd.clone(), config.prd_path.clone(), config.progress_path.clone()));
    let git_tool: Arc<dyn Tool> = Arc::new(GitTool::new(project_root.clone()));
    let file_tool: Arc<dyn Tool> = Arc::new(FileTool::new(project_root.clone()));
    let test_tool: Arc<dyn Tool> = Arc::new(TestTool::new(project_root.clone()));

    let tools: Vec<Arc<dyn Tool>> =
        vec![prd_tool.clone(), git_tool.clone(), file_tool.clone(), test_tool.clone()];

    // Create the orchestrator loop agent
    let loop_agent = create_loop_agent(&config.api_key, &config.model_name, tools.clone())?;

    // Wrap in LoopAgent for autonomous execution
    let ralph =
        LoopAgent::new("ralph", vec![loop_agent]).with_max_iterations(config.max_iterations);

    println!("\n{} Max iterations: {}", "⚙️".bright_cyan(), config.max_iterations);

    let (complete, total) = prd.lock().unwrap().stats();
    println!("{} Tasks: {}/{} complete", "📋".bright_cyan(), complete, total);
    println!();

    // Run Ralph using the CLI console
    adk_cli::console::run_console(
        Arc::new(ralph),
        "ralph_app".to_string(),
        "developer".to_string(),
    )
    .await?;

    // Final stats
    let (complete, total) = prd.lock().unwrap().stats();
    if prd.lock().unwrap().is_complete() {
        println!("\n{}", "✅ All tasks complete!".bright_green().bold());
    } else {
        println!("\n📊 Progress: {}/{} tasks complete", complete, total);
    }

    Ok(())
}
