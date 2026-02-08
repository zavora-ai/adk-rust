//! AgentSkills example: explicit skill index + tag-based selection policy.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_policy_filters
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

fn setup_demo_skills_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_policy_filters_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }

    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("security_review.md"),
        "---\nname: security_review\ndescription: Security review checklist for auth and secrets\ntags: [security, auth]\n---\nAudit token lifetime, key storage, and least-privilege scopes.\n",
    )?;
    std::fs::write(
        skills_dir.join("release_notes.md"),
        "---\nname: release_notes\ndescription: Release notes formatting\ntags: [release, docs]\n---\nSummarize user-facing changes in bullets.\n",
    )?;
    std::fs::write(
        skills_dir.join("code_search.md"),
        "---\nname: code_search\ndescription: Search the repository efficiently\ntags: [code, search]\n---\nUse `rg --files` and `rg <pattern>` for focused navigation.\n",
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

    let policy = SelectionPolicy {
        top_k: 1,
        min_score: 0.1,
        include_tags: vec!["security".to_string()],
        exclude_tags: vec!["release".to_string()],
    };

    let agent = LlmAgentBuilder::new("assistant_policy_skills")
        .description("Assistant using explicit skill policy")
        .instruction("Respond with exactly two concise bullets.")
        .model(model)
        .with_skills(skills_index)
        .with_skill_policy(policy)
        .with_skill_budget(240)
        .build()?;

    let app_name = "skills_policy_filters".to_string();
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
            Content::new("user").with_text(
                "Review service account token handling and list the top two hardening checks.",
            ),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.author == "assistant_policy_skills" {
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
