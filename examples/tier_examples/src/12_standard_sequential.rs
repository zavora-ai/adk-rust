//! # 12 — Standard: Sequential Workflow
//!
//! Sequential and parallel workflow agents are available in the `standard` tier.
//! This example chains a researcher → writer pipeline.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["standard"] }
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

    let researcher = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Researches a topic and provides key facts")
            .instruction(
                "You are a researcher. Given a topic, provide 3 key facts about it. \
                 Be concise — bullet points only.",
            )
            .model(model.clone())
            .build()?,
    );

    let writer = Arc::new(
        LlmAgentBuilder::new("writer")
            .description("Writes a short paragraph from research notes")
            .instruction(
                "You are a writer. Take the research notes from the previous agent \
                 and write a single engaging paragraph. Keep it under 100 words.",
            )
            .model(model.clone())
            .build()?,
    );

    let pipeline = SequentialAgent::new("research-pipeline", vec![researcher, writer]);

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "seq-demo".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("seq-demo")
        .agent(Arc::new(pipeline) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("Topic: The Rust programming language\n");
    let mut stream = runner
        .run_str("user", "s1", Content::new("user").with_text("The Rust programming language"))
        .await?;

    print!("Output: ");
    while let Some(Ok(event)) = stream.next().await {
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!("\n\n✅ Sequential workflow works with standard tier.");
    Ok(())
}
