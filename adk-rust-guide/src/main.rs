use adk_rust::prelude::*;
use std::sync::Arc;
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run interactive console (default)
    Chat,
    /// Run web server
    Serve {
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .unwrap_or_else(|_| "dummy-key-for-validation".to_string());

    // Create Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with Google Search tool
    let time_agent = LlmAgentBuilder::new("hello_time_agent")
        .description("Tells the current time in a specified city.")
        .instruction("You are a helpful assistant that tells the current time in a city.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    let agent = Arc::new(time_agent);
    
    // Parse CLI args
    let cli = Cli::parse();
    
    match cli.command.unwrap_or(Commands::Chat) {
        Commands::Chat => run_console(agent).await?,
        Commands::Serve { port } => run_serve(agent, port).await?,
    }

    Ok(())
}

async fn run_console(agent: Arc<dyn Agent>) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Create session service
    let session_service = Arc::new(InMemorySessionService::new());

    // Create a session for the user
    use adk_rust::session::{SessionService, CreateRequest};
    use std::collections::HashMap;
    
    let user_id = "user1".to_string();
    let app_name = "my-agent".to_string();
    
    let session = session_service.create(CreateRequest {
        app_name: app_name.clone(),
        user_id: user_id.clone(),
        session_id: None, // Auto-generate session ID
        state: HashMap::new(),
    }).await?;
    
    let session_id = session.id().to_string();

    // Create runner with RunnerConfig
    let runner = Runner::new(RunnerConfig {
        app_name,
        agent: agent.clone(),
        session_service,
        artifact_service: None,
        memory_service: None,
    })?;

    // Start interactive console
    println!("🤖 Agent ready! Type your questions (or 'exit' to quit).\n");
    
    use rustyline::DefaultEditor;
    let mut rl = DefaultEditor::new()?;
    
    loop {
        match rl.readline("You: ") {
            Ok(line) => {
                let input = line.trim();
                if input == "exit" || input == "quit" {
                    println!("👋 Goodbye!");
                    break;
                }
                
                if input.is_empty() {
                    continue;
                }
                
                // Run agent with user input
                let content = Content::new("user").with_text(input);
                let mut events = runner.run(
                    user_id.clone(),
                    session_id.clone(),
                    content
                ).await?;
                
                print!("Assistant: ");
                
                // Stream response
                use futures::StreamExt;
                while let Some(event) = events.next().await {
                    match event {
                        Ok(evt) => {
                            if let Some(content) = evt.content {
                                for part in content.parts {
                                    if let Some(text) = part.text() {
                                        print!("{}", text);
                                    }
                                }
                            }
                        }
                        Err(e) => eprintln!("\nError: {}", e),
                    }
                }
                println!("\n");
            }
            Err(_) => break,
        }
    }

    Ok(())
}

async fn run_serve(agent: Arc<dyn Agent>, port: u16) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use adk_rust::server::{create_app, ServerConfig};
    use adk_rust::SingleAgentLoader;
    
    // Initialize telemetry
    if let Err(e) = adk_rust::telemetry::init_telemetry("adk-server") {
        eprintln!("Failed to initialize telemetry: {}", e);
    }

    let session_service = Arc::new(InMemorySessionService::new());
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    
    let config = ServerConfig {
        agent_loader,
        session_service,
        artifact_service: None,
        backend_url: None,
    };
    
    let app = create_app(config);
    
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    
    println!("ADK Server starting on http://{}", addr);
    println!("Press Ctrl+C to stop");
    
    axum::serve(listener, app).await?;
    
    Ok(())
}
