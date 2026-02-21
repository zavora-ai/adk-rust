//! AgentSkills example: Basic skill injection and runner-level automation.
//!
//! Demonstrates:
//! 1. Manual skill loading and agent construction.
//! 2. Runner-level automatic skill injection via `with_auto_skills`.
//! 3. Use of `LlmAgentBuilder` with local skills.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_basic

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_skill::SkillInjectorConfig;
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

fn setup_demo_skills_root() -> Result<std::path::PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_basic_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;

    std::fs::write(
        skills_dir.join("general.md"),
        r#"---
name: general
description: A helpful general assistant.
---
I am a helpful assistant. I provide clear and concise answers.
"#,
    )?;

    std::fs::write(
        skills_dir.join("coder.md"),
        r#"---
name: coder
description: Expert in Rust and Web development.
tags: [code, rust]
---
I am an expert programmer. I prioritize safety and performance.
"#,
    )?;

    Ok(root)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let skills_root = setup_demo_skills_root()?;

    let _agent = LlmAgentBuilder::new("manual_agent")
        .description("Agent with manually linked skills")
        .model(model.clone())
        .with_skills_from_root(&skills_root)?
        .build()?;

    // We can run this manually, but let's show Runner Option instead.
    println!("Manual agent built with skills from scope: {}\n", skills_root.display());

    println!("--- Option 2: Automatic Runner-Level Skill Injection ---");
    let base_agent = LlmAgentBuilder::new("runner_agent")
        .description("Simple base agent transformed by the runner")
        .model(model)
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());

    // Create a runner that automatically injects skills based on user messages
    let runner = Runner::new(RunnerConfig {
        app_name: "skills_basic_demo".into(),
        agent: Arc::new(base_agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
    })?
    .with_auto_skills(&skills_root, SkillInjectorConfig::default())?; // <-- Active discovery and injection

    let user_id = "user123".to_string();
    let session = session_service
        .create(CreateRequest {
            app_name: "demo".into(),
            user_id: user_id.clone(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;

    let queries = vec!["I need help with a Rust memory safety issue.", "Just say hello!"];

    for query in queries {
        println!("\nUser: {}", query);
        let mut stream = runner
            .run(user_id.clone(), session.id().to_string(), Content::new("user").with_text(query))
            .await?;

        while let Some(event) = stream.next().await {
            let event = event?;
            if let Some(content) = event.llm_response.content {
                let text = content
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                println!("Agent: {}", text);
            }
        }
    }

    Ok(())
}
