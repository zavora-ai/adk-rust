use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env file
    dotenvy::dotenv().ok();

    // Ensure API key is set
    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set in .env or environment");
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Define the agent with dynamic instructions
    // The placeholders {user:name}, {user:language}, etc. will be replaced
    // by the values from the session state at runtime.
    let agent = LlmAgentBuilder::new("multilingual_assistant")
        .description("An assistant that adapts to user language")
        .instruction(
            "You are assisting {user:name} in {user:language}. \
             Respond in {user:language}. \
             User expertise level: {user:expertise}. \
             Adjust your explanations accordingly.",
        )
        .model(Arc::new(model))
        .build()?;

    println!("Multilingual agent created: {}", agent.name());

    // --- Custom Runner Loop to support pre-seeded state ---
    use adk_runner::{Runner, RunnerConfig};
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::io::{self, BufRead, Write};

    let app_name = "tutorial_app";
    let user_id = "user_123";

    let session_service = Arc::new(InMemorySessionService::new());

    // Prepare initial state
    let mut state = HashMap::new();
    state.insert("user:name".to_string(), "Alice".into());
    state.insert("user:language".to_string(), "French".into());
    state.insert("user:expertise".to_string(), "intermediate".into());

    // Create session with initial state
    let session = session_service
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: None,
            state,
        })
        .await?;

    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: app_name.to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    println!("🤖 Agent ready! Type your questions (or 'exit' to quit).\n");
    println!("(Context: User=Alice, Language=French, Expertise=Intermediate)\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("You: ");
        stdout.flush()?;

        let mut input = String::new();
        let bytes_read = stdin.lock().read_line(&mut input)?;

        if bytes_read == 0 {
            break;
        }

        let input = input.trim();
        if input == "exit" || input == "quit" {
            break;
        }
        if input.is_empty() {
            continue;
        }

        let content = Content::new("user").with_text(input);
        let mut events = runner.run(user_id.to_string(), session_id.clone(), content).await?;

        print!("Assistant: ");
        stdout.flush()?;

        while let Some(event) = events.next().await {
            match event {
                Ok(evt) => {
                    if let Some(content) = evt.llm_response.content {
                        for part in content.parts {
                            if let Some(text) = part.text() {
                                print!("{}", text);
                                stdout.flush()?;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("\nError: {}", e),
            }
        }
        println!("\n");
    }

    Ok(())
}
