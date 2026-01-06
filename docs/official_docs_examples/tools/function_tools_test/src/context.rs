//! Tool context example - accessing session info
//!
//! Run: cargo run --bin context

use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize)]
struct GreetParams {
    /// Optional custom greeting message
    #[serde(default)]
    message: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Tool that uses context
    let greet_tool = FunctionTool::new(
        "greet",
        "Greet the user with session info",
        |ctx, _args| async move {
            let user_id = ctx.user_id();
            let session_id = ctx.session_id();
            let agent_name = ctx.agent_name();
            Ok(json!({
                "greeting": format!("Hello, user {}!", user_id),
                "session": session_id,
                "served_by": agent_name
            }))
        },
    )
    .with_parameters_schema::<GreetParams>();

    let agent = LlmAgentBuilder::new("context_agent")
        .instruction("Greet users using the greet tool to show session info.")
        .model(Arc::new(model))
        .tool(Arc::new(greet_tool))
        .build()?;

    println!("✅ Context agent ready - tool accesses user_id, session_id, agent_name");
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
