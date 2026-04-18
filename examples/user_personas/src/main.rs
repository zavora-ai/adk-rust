//! # User Personas Example
//!
//! Demonstrates the user personas feature from `adk-eval` — loading persona
//! definitions from JSON files and using `UserSimulator` to drive multi-turn
//! conversations that reflect different user styles.
//!
//! ## What This Shows
//!
//! - Loading persona profiles from a directory via `PersonaRegistry`
//! - Creating a `UserSimulator` for each persona with a separate LLM instance
//! - Running a 5-turn multi-turn conversation between each persona and an
//!   agent under test
//! - Comparing how the same agent responds differently to different persona
//!   styles (terse expert vs. verbose beginner)
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/user_personas/Cargo.toml
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_core::model::Llm;
use adk_core::types::Content;
use adk_eval::personas::{PersonaRegistry, UserSimulator};
use adk_model::GeminiModel;
use futures::StreamExt;
use tracing_subscriber::EnvFilter;

const NUM_TURNS: usize = 5;

// ---------------------------------------------------------------------------
// Helper: require an environment variable or exit with a descriptive message
// ---------------------------------------------------------------------------

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {name}\n\
             Set it in your .env file or export it in your shell.\n\
             See .env.example for all required variables."
        )
    })
}

// ---------------------------------------------------------------------------
// Helper: run a multi-turn conversation between a UserSimulator and an agent
// ---------------------------------------------------------------------------

async fn run_conversation(
    simulator: &UserSimulator,
    agent_model: Arc<dyn Llm>,
    agent_instructions: &str,
) -> anyhow::Result<()> {
    let mut history: Vec<Content> = Vec::new();

    for turn in 1..=NUM_TURNS {
        // --- User turn: simulator generates a message based on persona ---
        let user_message = simulator.generate_message(&history).await.map_err(|e| {
            anyhow::anyhow!(
                "Persona '{}' failed to generate message: {e}",
                simulator.persona().name
            )
        })?;

        let user_text =
            user_message.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");

        println!("  [Turn {turn}] 🧑 User ({}):", simulator.persona().name);
        println!("    {user_text}\n");

        history.push(user_message);

        // --- Agent turn: send history to the agent model and collect response ---
        let mut agent_contents = vec![
            Content::new("user")
                .with_text(format!("You are a helpful coding assistant. {agent_instructions}")),
            Content::new("model").with_text("Understood. I'm ready to help."),
        ];
        agent_contents.extend(history.iter().cloned());

        let request = adk_core::model::LlmRequest::new(agent_model.name(), agent_contents);

        let mut stream = agent_model.generate_content(request, false).await?;

        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            let response = result?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        response_text.push_str(text);
                    }
                }
            }
        }

        println!("  [Turn {turn}] 🤖 Agent:");
        println!("    {response_text}\n");

        history.push(Content::new("model").with_text(&response_text));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  User Personas — ADK-Rust v0.7.0         ║");
    println!("╚══════════════════════════════════════════╝\n");

    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // Load persona profiles from the personas/ directory
    // -----------------------------------------------------------------------

    // Resolve the personas directory relative to the example crate root.
    // When run via `cargo run --manifest-path`, the CWD is the workspace root,
    // so we use the CARGO_MANIFEST_DIR or fall back to a relative path.
    let personas_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("personas");
    let registry = PersonaRegistry::load_directory(&personas_dir).map_err(|e| {
        anyhow::anyhow!("Failed to load personas from {}: {e}", personas_dir.display())
    })?;

    let personas = registry.list();
    println!("📋 Loaded {} persona(s):\n", personas.len());
    for persona in &personas {
        println!(
            "  • {} — {} (expertise: {:?}, verbosity: {:?})",
            persona.name,
            persona.description,
            persona.traits.expertise_level,
            persona.traits.verbosity,
        );
    }
    println!();

    // -----------------------------------------------------------------------
    // Agent instructions (same for all personas to show contrast)
    // -----------------------------------------------------------------------

    let agent_instructions = "Answer questions about Rust programming. \
         Adapt your response style to match the user's apparent expertise level. \
         If the user seems experienced, be concise and code-focused. \
         If the user seems new, be patient and explain concepts step by step.";

    // -----------------------------------------------------------------------
    // Run a multi-turn conversation for each persona
    // -----------------------------------------------------------------------

    for persona in &personas {
        println!("═══════════════════════════════════════════");
        println!("  Persona: {} — {}", persona.name, persona.description);
        println!(
            "  Style: {}, Verbosity: {:?}, Expertise: {:?}",
            persona.traits.communication_style,
            persona.traits.verbosity,
            persona.traits.expertise_level,
        );
        println!("═══════════════════════════════════════════\n");

        // Each persona gets its own LLM instance for the simulator
        let simulator_llm: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
        let simulator = UserSimulator::new(simulator_llm, (*persona).clone());

        // The agent under test also gets its own LLM instance
        let agent_llm: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

        run_conversation(&simulator, agent_llm, agent_instructions).await?;

        println!();
    }

    // -----------------------------------------------------------------------
    // Summary
    // -----------------------------------------------------------------------

    println!("═══════════════════════════════════════════");
    println!("  Summary");
    println!("═══════════════════════════════════════════\n");
    println!("  The same agent was tested with {} different personas.", personas.len());
    println!("  Notice how the conversation style differs:");
    println!("    • The expert persona asks terse, code-focused questions");
    println!("    • The beginner persona asks verbose, exploratory questions");
    println!("  This demonstrates how user personas help evaluate agent");
    println!("  adaptability across different user types.\n");

    println!("✅ User Personas example completed successfully.");
    Ok(())
}
