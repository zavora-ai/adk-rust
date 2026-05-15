//! # 11 — Standard: Graph Workflow
//!
//! Graph workflows are available in the `standard` tier.
//! This example builds a two-node sequential graph using `GraphAgent::builder()`.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["standard"] }
//! ```

use adk_rust::graph::node::AgentNode;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create two LLM agents for different tasks
    let translator = Arc::new(
        LlmAgentBuilder::new("translator")
            .model(model.clone())
            .instruction("Translate the user's input text to French. Output only the translation.")
            .build()?,
    );

    let summarizer = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .model(model.clone())
            .instruction("Summarize the user's input text in one sentence.")
            .build()?,
    );

    // Build a sequential graph: translator → summarizer
    let graph_agent = GraphAgent::builder("text_processor")
        .description("Translates text to French, then summarizes it")
        .node(AgentNode::new(translator))
        .node(AgentNode::new(summarizer))
        .edge(START, "translator")
        .edge("translator", "summarizer")
        .edge("summarizer", END)
        .build()?;

    // Run it through the standard Runner
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "graph-demo".into(),
            user_id: "user".into(),
            session_id: Some("s1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("graph-demo")
        .agent(Arc::new(graph_agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("Input: AI is transforming how we build software.\n");
    let mut stream = runner
        .run_str(
            "user",
            "s1",
            Content::new("user").with_text("AI is transforming how we build software."),
        )
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
    println!("\n\n✅ Graph workflow works with standard tier.");
    Ok(())
}
