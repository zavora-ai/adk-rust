//! AgentSkills example: use convention files with live Gemini calls.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_conventions_llm
//!
//! Required env:
//!   GOOGLE_API_KEY (or GEMINI_API_KEY)

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

fn setup_demo_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_conventions_llm_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(root.join(".skills"))?;

    std::fs::write(
        root.join("AGENTS.md"),
        "# Repo Agent Rules\nAlways run targeted cargo tests and keep patches focused.\n",
    )?;
    std::fs::write(
        root.join("GEMINI.md"),
        "# Gemini Setup\nFor Gemini usage, set GOOGLE_API_KEY or GEMINI_API_KEY and use gemini-2.5-flash unless a stronger model is required.\n",
    )?;
    std::fs::write(
        root.join(".skills/release.md"),
        "---\nname: release\ndescription: release note generation\ntags: [release, docs]\n---\nSummarize user-visible changes.\n",
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
    let root = setup_demo_root()?;
    let skills_index = load_skill_index(&root)?;

    let policy = SelectionPolicy {
        top_k: 1,
        min_score: 0.1,
        include_tags: vec!["gemini-md".to_string()],
        exclude_tags: vec![],
    };

    let agent = LlmAgentBuilder::new("assistant_convention_skills")
        .description("Assistant using convention instruction files")
        .instruction("Respond with exactly two bullets.")
        .model(model)
        .with_skills(skills_index)
        .with_skill_policy(policy)
        .with_skill_budget(300)
        .build()?;

    let app_name = "skills_conventions_llm".to_string();
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
        agent: Arc::new(agent),
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
            Content::new("user")
                .with_text("How should I configure Gemini access for this project?"),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.author == "assistant_convention_skills" {
            let text = event
                .llm_response
                .content
                .unwrap_or_else(|| Content { role: "model".to_string(), parts: vec![] })
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            println!("{text}");
        }
    }

    Ok(())
}
