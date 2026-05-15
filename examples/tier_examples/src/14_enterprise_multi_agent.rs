//! # 14 — Enterprise: Multi-Agent with Artifacts
//!
//! The `enterprise` tier includes everything in `standard` plus RAG, browser,
//! realtime, payments, and AWP. This example demonstrates a multi-agent system
//! with artifact storage — features available from the `standard` tier.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["enterprise"] }
//! ```

use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create a multi-agent system: coordinator delegates to specialists
    let code_agent = Arc::new(
        LlmAgentBuilder::new("coder")
            .description("Writes code snippets")
            .instruction(
                "You are a Rust code expert. When asked, write clean, idiomatic Rust code. \
                 Output only the code, no explanations.",
            )
            .model(model.clone())
            .build()?,
    );

    let reviewer = Arc::new(
        LlmAgentBuilder::new("reviewer")
            .description("Reviews code for quality")
            .instruction(
                "You are a code reviewer. Review the code from the previous agent. \
                 Provide a brief review: what's good, what could improve. Keep it under 50 words.",
            )
            .model(model.clone())
            .build()?,
    );

    // Sequential pipeline: code → review
    let pipeline = SequentialAgent::new("code-review-pipeline", vec![code_agent, reviewer]);

    // Set up artifact storage
    let artifacts = Arc::new(InMemoryArtifactService::new());

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "enterprise-demo".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("enterprise-demo")
        .agent(Arc::new(pipeline) as Arc<dyn Agent>)
        .session_service(sessions)
        .artifact_service(artifacts.clone())
        .build()?;

    println!("Request: Write a Rust function to calculate fibonacci numbers.\n");
    let mut stream = runner
        .run_str(
            "user",
            "s1",
            Content::new("user").with_text("Write a Rust function to calculate fibonacci numbers."),
        )
        .await?;

    let mut full_response = String::new();
    print!("Output: ");
    while let Some(Ok(event)) = stream.next().await {
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                    full_response.push_str(text);
                }
            }
        }
    }
    println!();

    // Save the output as an artifact
    artifacts
        .save(SaveRequest {
            app_name: "enterprise-demo".to_string(),
            user_id: "user".to_string(),
            session_id: "s1".to_string(),
            file_name: "code_review.md".to_string(),
            part: Part::Text { text: full_response },
            version: None,
        })
        .await?;

    println!("\n✅ Multi-agent pipeline with artifacts works with enterprise tier.");
    Ok(())
}
