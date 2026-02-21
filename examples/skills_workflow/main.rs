//! AgentSkills example: apply skills to multi-agent workflows.
//!
//! Highlights the `SequentialAgent::with_skills_from_root` pattern,
//! enabling complex workflows to share a skills index.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_workflow

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::{Content, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

fn setup_demo_skills_root() -> Result<std::path::PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_workflow_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("search.md"),
        "---
name: search
description: Search source code
tags: [search, code]
---
Use rg --files, then rg <pattern>.
",
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

    // 1. Define individual agents
    let planner = LlmAgentBuilder::new("planner")
        .description("Plans the search strategy")
        .instruction("Given the request, produce a short 2-step search plan.")
        .model(model.clone())
        .build()?;

    let executor = LlmAgentBuilder::new("executor")
        .description("Executes the planned search")
        .instruction("Provide concrete ripgrep commands to execute the plan.")
        .model(model)
        .build()?;

    // 2. Wrap them in a SequentialAgent and link skills to the entire workflow
    let workflow =
        SequentialAgent::new("search_workflow", vec![Arc::new(planner), Arc::new(executor)])
            .with_skills_from_root(&skills_root)?;

    let app_name = "skills_workflow_demo".to_string();
    let user_id = "user".to_string();
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name,
        agent: Arc::new(workflow),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
    })?;

    let mut stream = runner
        .run(
            user_id,
            session_id,
            Content::new("user").with_text("Please search this repository for TODO markers."),
        )
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
                .join(
                    "
",
                );
            println!("{} -> {}", event.author, text);
        }
    }

    Ok(())
}
