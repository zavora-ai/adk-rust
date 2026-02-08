//! Minimal AgentSkills example for `LlmAgentBuilder`.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_llm_minimal
//!
//! Required env:
//!   GOOGLE_API_KEY (or GEMINI_API_KEY)

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

fn setup_demo_skills_root() -> Result<std::path::PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_llm_minimal_demo");
    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("search.md"),
        "---\nname: search\ndescription: Search source code\ntags: [search, code]\n---\nUse rg --files, then rg <pattern>.\n",
    )?;
    Ok(root)
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let skills_root = setup_demo_skills_root()?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("Assistant with local skills")
        .instruction("Respond briefly")
        .model(Arc::new(model))
        .with_skills_from_root(&skills_root)?
        .build()?;

    let app_name = "skills_llm_minimal".to_string();
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
            Content::new("user").with_text("Please search this repository for TODO markers."),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.author == "assistant" {
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
