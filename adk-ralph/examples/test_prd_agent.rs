//! Test the PRD Agent with a simple prompt.
//!
//! Run with: cargo run -p adk-ralph --example test_prd_agent

use adk_ralph::agents::PrdAgent;
use adk_core::Part;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for debug output
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .init();

    // Load .env file
    dotenvy::dotenv().ok();

    // Create project directory
    let project_path = PathBuf::from("/tmp/ralph-prd-test");
    std::fs::create_dir_all(&project_path)?;

    println!("🚀 Testing PRD Agent");
    println!("📁 Project path: {}", project_path.display());
    println!();

    // Build the PRD agent
    println!("⏳ Building PRD agent...");
    let prd_agent = PrdAgent::builder()
        .project_path(&project_path)
        .build()
        .await?;
    println!("✅ PRD agent built");

    // Test prompt
    let prompt = r#"
Build a simple command-line calculator in Rust.

Features:
- Basic arithmetic operations (add, subtract, multiply, divide)
- Support for parentheses
- History of last 10 calculations
- Clear command to reset
"#;

    println!("📝 Prompt:");
    println!("{}", prompt);
    println!();
    println!("⏳ Generating PRD...");

    // Generate PRD
    match prd_agent.generate(prompt).await {
        Ok(prd) => {
            println!("✅ PRD Generated!");
            println!();
            println!("📊 Summary:");
            println!("   Project: {}", prd.project);
            println!("   User Stories: {}", prd.user_stories.len());
            println!();

            // Show user stories
            for story in &prd.user_stories {
                println!("   {} - {} (Priority: {})", story.id, story.title, story.priority);
            }

            println!();
            println!("📄 PRD saved to: {}/prd.md", project_path.display());
        }
        Err(e) => {
            println!("❌ Error: {}", e);
            
            // Check if file was created anyway
            let prd_path = project_path.join("prd.md");
            if prd_path.exists() {
                println!("📄 File exists at: {}", prd_path.display());
                let content = std::fs::read_to_string(&prd_path)?;
                println!("Content length: {} bytes", content.len());
            } else {
                println!("📄 No prd.md file was created");
            }
        }
    }

    Ok(())
}
