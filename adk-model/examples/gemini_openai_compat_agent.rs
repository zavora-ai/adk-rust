//! Gemini (OpenAI-compatible) driving a normal `LlmAgent` in a `Runner`.
//!
//! The `OpenAICompatibleConfig::gemini(...)` preset produces an `Llm` that talks
//! to Gemini through the OpenAI Chat Completions wire format. Because it is just
//! another `Llm`, the standard `LlmAgent` + `Runner` + tool loop drive it with no
//! special-casing — the same code path you would use for any provider.
//!
//! Uses a `GEMINI_API_KEY` (or `GOOGLE_API_KEY`).
//!
//! ```bash
//! GEMINI_API_KEY=... cargo run -p adk-model --features openai --example gemini_openai_compat_agent
//! ```

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Part, SessionId, UserId};
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

const APP_NAME: &str = "gemini-openai-compat-agent";
const USER_ID: &str = "demo-user";
const SESSION_ID: &str = "demo-session";
const MODEL: &str = "gemini-3.5-flash";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .expect("set GEMINI_API_KEY or GOOGLE_API_KEY");

    // The Gemini OpenAI-compatible client is just an `Llm`.
    let model = Arc::new(OpenAICompatible::new(OpenAICompatibleConfig::gemini(api_key, MODEL))?);

    // A normal LlmAgent — no Gemini- or transport-specific wiring.
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .model(model)
            .instruction("You are a concise, helpful assistant.")
            .build()?,
    );

    // Standard session + runner setup.
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: Some(SESSION_ID.to_string()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name(APP_NAME)
        .agent(agent as Arc<dyn Agent>)
        .session_service(sessions)
        .build()?;

    println!("=== Gemini (OpenAI-compatible) agent via Runner ===\n");

    let prompt = "In one sentence, what is the Rust borrow checker for?";
    println!(">> {prompt}\n");

    let mut stream = runner
        .run(
            UserId::new(USER_ID)?,
            SessionId::new(SESSION_ID)?,
            Content::new("user").with_text(prompt),
        )
        .await?;

    // The runner streams the response as partial events; the final event marks
    // completion. Print text from each event's content as it arrives.
    print!("<< ");
    use std::io::Write;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
            }
        }
    }
    println!("\n\n=== Done ===");
    Ok(())
}
