//! Test the RalphLoopAgent with a single task.
//!
//! Run with: cargo run -p adk-ralph --example test_loop_agent

use adk_ralph::{RalphConfig, RalphLoopAgent, Result};
use std::fs;
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("adk_ralph=debug,adk=info")
        .init();

    // Create a temp directory for the test
    let test_dir = Path::new("/tmp/ralph-loop-test");
    if test_dir.exists() {
        fs::remove_dir_all(test_dir)?;
    }
    fs::create_dir_all(test_dir)?;

    println!("📁 Test directory: {}", test_dir.display());

    // Create a simple tasks.json with TWO tasks
    let tasks_json = r#"{
  "project": "test-project",
  "language": "text",
  "version": "1.0",
  "tasks": [
    {
      "id": "T-001",
      "title": "Create hello world",
      "description": "Create a simple hello.txt file with 'Hello, World!' content",
      "priority": 1,
      "status": "pending",
      "dependencies": []
    },
    {
      "id": "T-002",
      "title": "Create goodbye file",
      "description": "Create a goodbye.txt file with 'Goodbye, World!' content",
      "priority": 2,
      "status": "pending",
      "dependencies": ["T-001"]
    }
  ]
}"#;

    fs::write(test_dir.join("tasks.json"), tasks_json)?;
    println!("✅ Created tasks.json with 2 tasks");

    // Note: progress.json will be created automatically by the ProgressTool

    // Create a minimal design.md
    let design_md = r#"# Test Project Design

## Project
test-project

## Overview
A simple test project to verify the loop agent works.

## Technology Stack
- Language: Text files
- Testing: Manual verification
"#;

    fs::write(test_dir.join("design.md"), design_md)?;
    println!("✅ Created design.md");

    // Initialize git repo (needed for git tool)
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(test_dir)
        .output()?;
    println!("✅ Initialized git repo");

    // Configure git user for commits
    std::process::Command::new("git")
        .args(["config", "user.email", "ralph@test.com"])
        .current_dir(test_dir)
        .output()?;
    std::process::Command::new("git")
        .args(["config", "user.name", "Ralph"])
        .current_dir(test_dir)
        .output()?;

    // Create config
    let config = RalphConfig {
        tasks_path: "tasks.json".to_string(),
        progress_path: "progress.json".to_string(),
        design_path: "design.md".to_string(),
        prd_path: "prd.md".to_string(),
        max_iterations: 10, // Higher limit for 2 tasks
        completion_promise: "Task completed successfully!".to_string(),
        ..Default::default()
    };

    println!("\n🚀 Starting RalphLoopAgent...\n");

    // Build and run the loop agent
    let ralph_loop = RalphLoopAgent::builder()
        .config(config)
        .project_path(test_dir)
        .build()
        .await?;

    let status = ralph_loop.run().await?;

    println!("\n📋 Final Status: {}", status);

    // Check if the files were created
    let hello_path = test_dir.join("hello.txt");
    if hello_path.exists() {
        let content = fs::read_to_string(&hello_path)?;
        println!("\n📄 hello.txt content: {}", content);
    } else {
        println!("\n⚠️  hello.txt was not created");
    }

    let goodbye_path = test_dir.join("goodbye.txt");
    if goodbye_path.exists() {
        let content = fs::read_to_string(&goodbye_path)?;
        println!("📄 goodbye.txt content: {}", content);
    } else {
        println!("⚠️  goodbye.txt was not created");
    }

    // Show final task status
    let final_tasks = fs::read_to_string(test_dir.join("tasks.json"))?;
    println!("\n📋 Final tasks.json:");
    println!("{}", final_tasks);

    Ok(())
}
