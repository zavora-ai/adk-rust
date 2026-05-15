//! # 15 — Enterprise: Parallel Agent Execution
//!
//! The `enterprise` tier includes all production features. This example
//! demonstrates parallel agent execution — multiple agents run concurrently
//! and their results are combined.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["enterprise"] }
//! ```

use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create three analysts that run in parallel
    let tech_analyst = Arc::new(
        LlmAgentBuilder::new("tech_analyst")
            .description("Analyzes technology trends")
            .instruction(
                "You are a technology analyst. Given a topic, provide 2 key technology insights. \
                 Be concise — 2 bullet points max.",
            )
            .model(model.clone())
            .build()?,
    );

    let market_analyst = Arc::new(
        LlmAgentBuilder::new("market_analyst")
            .description("Analyzes market trends")
            .instruction(
                "You are a market analyst. Given a topic, provide 2 key market insights. \
                 Be concise — 2 bullet points max.",
            )
            .model(model.clone())
            .build()?,
    );

    let risk_analyst = Arc::new(
        LlmAgentBuilder::new("risk_analyst")
            .description("Analyzes risks")
            .instruction(
                "You are a risk analyst. Given a topic, identify 2 key risks. \
                 Be concise — 2 bullet points max.",
            )
            .model(model.clone())
            .build()?,
    );

    // All three run concurrently
    let parallel =
        ParallelAgent::new("multi-analysis", vec![tech_analyst, market_analyst, risk_analyst]);

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "parallel-demo".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("parallel-demo")
        .agent(Arc::new(parallel) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("Topic: AI agents in enterprise software\n");
    let mut stream = runner
        .run_str("user", "s1", Content::new("user").with_text("AI agents in enterprise software"))
        .await?;

    print!("Output:\n");
    while let Some(Ok(event)) = stream.next().await {
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!("\n\n✅ Parallel agent execution works with enterprise tier.");
    Ok(())
}
