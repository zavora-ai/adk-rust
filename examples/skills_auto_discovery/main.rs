//! AgentSkills example: auto discovery from `.skills/` using `with_auto_skills`.
//!
//! Run:
//!   cargo run --manifest-path examples/Cargo.toml --example skills_auto_discovery
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
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct DirGuard {
    original: PathBuf,
}

impl DirGuard {
    fn enter(path: &Path) -> Result<Self> {
        let original = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self { original })
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

fn setup_demo_skills_root() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("adk_skills_auto_discovery_demo");
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }

    let skills_dir = root.join(".skills");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("code_search.md"),
        "---\nname: code_search\ndescription: Search source code for symbols and TODOs\ntags: [code, search]\n---\nPrefer ripgrep. First list files with `rg --files`, then query with `rg <pattern>`.\n",
    )?;
    std::fs::write(
        skills_dir.join("release_notes.md"),
        "---\nname: release_notes\ndescription: Create release notes\ntags: [docs, release]\n---\nGroup changes by feature area and keep bullets concise.\n",
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
    let agent = {
        let _dir_guard = DirGuard::enter(&skills_root)?;
        LlmAgentBuilder::new("assistant_auto_skills")
            .description("Assistant using auto-discovered local skills")
            .instruction("Use concise and actionable shell guidance.")
            .model(model)
            .with_auto_skills()?
            .with_skill_budget(320)
            .build()?
    };

    let app_name = "skills_auto_discovery".to_string();
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
                .with_text("Find TODO markers in this repo and provide one command."),
        )
        .await?;

    while let Some(event) = stream.next().await {
        let event = event?;
        if event.author == "assistant_auto_skills" {
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
