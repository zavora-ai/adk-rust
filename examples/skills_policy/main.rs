//! AgentSkills example: explicit skill index + tag-based selection policy.
//!
//! Demonstrates how to filter the global skill index using `SelectionPolicy`
//! to bind only "security" skills to a specific agent.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_policy

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_skill::{SelectionPolicy, load_skill_index};
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn setup_demo_skills_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_policy_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }

    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("security_review.md"),
        "---
name: security_review
description: Security review checklist for auth and secrets
tags: [security, auth]
---
Audit token lifetime and key storage.
",
    )?;
    std::fs::write(
        skills_dir.join("release_notes.md"),
        "---
name: release_notes
description: Release notes formatting
tags: [release, docs]
---
Summarize user-facing changes.
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
    let skills_index = load_skill_index(&skills_root)?;

    // 1. Create a policy that ONLY includes 'security' and EXCLUDES 'release'
    let policy = SelectionPolicy {
        top_k: 1,
        min_score: 0.1,
        include_tags: vec!["security".to_string()],
        exclude_tags: vec!["release".to_string()],
    };

    // 2. Build the agent with this restricted view of skills
    let agent = LlmAgentBuilder::new("security_officer")
        .description("Assistant focused on security hardening")
        .model(model)
        .with_skills(skills_index)
        .with_skill_policy(policy)
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "policy_demo".into(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
    })?;

    let user_id = "user".to_string();
    let session = session_service
        .create(CreateRequest {
            app_name: "demo".into(),
            user_id: user_id.clone(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;

    println!("Querying with 'Tell me about release notes' (Should NOT find security skill)...");
    let mut stream = runner
        .run(
            user_id.clone(),
            session.id().to_string(),
            Content::new("user").with_text("Tell me about release notes"),
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
                .join("");
            println!("Agent: {}", text);
        }
    }

    Ok(())
}
