# Quickstart for ADK-Rust

This guide shows you how to get up and running with Agent Development Kit for Rust. Before you start, make sure you have the following installed:

- Rust 1.75 or later
- ADK-Rust v0.1.0 or later

## Create an agent project

Create an agent project with the following files and directory structure through the command line:

```bash
cargo new my_agent
cd my_agent
```

```
my_agent/
    Cargo.toml    # project configuration
    src/
        main.rs   # main agent code
    .env          # API keys or project IDs
```

## Define the agent code

Create the code for a basic agent that uses the built-in Google Search tool. Add the following code to the `my_agent/src/main.rs` file in your project directory:

**my_agent/src/main.rs**

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with Google Search tool
    let time_agent = LlmAgentBuilder::new("hello_time_agent")
        .description("Tells the current time in a specified city.")
        .instruction("You are a helpful assistant that tells the current time in a city.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

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
        agent: Arc::new(time_agent),
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
```

## Configure project and dependencies

Update your `Cargo.toml` file to include the required dependencies:

**my_agent/Cargo.toml**

```toml
[package]
name = "my_agent"
version = "0.1.0"
edition = "2021"

[dependencies]
adk-rust = "0.1"
tokio = { version = "1.40", features = ["full"] }
dotenv = "0.15"
rustyline = "14.0"
futures = "0.3"
```

Then install the dependencies:

```bash
cargo build
```

## Set your API key

This project uses the Gemini API, which requires an API key. If you don't already have a Gemini API key, create a key in [Google AI Studio on the API Keys page](https://aistudio.google.com/app/apikey).

In a terminal window, write your API key into the `.env` file of your project to set environment variables:

### MacOS / Linux

Update: `my_agent/.env`

```bash
echo 'GOOGLE_API_KEY="YOUR_API_KEY"' > .env
```

### Windows

Update: `my_agent/.env`

```powershell
echo GOOGLE_API_KEY="YOUR_API_KEY" > .env
```

> [!TIP]
> **Using other AI models with ADK**
> 
> ADK is model-agnostic. You can implement the `Llm` trait to integrate other model providers like OpenAI, Anthropic, or local models.

## Run your agent

You can run your ADK agent using the interactive command-line interface you defined or the ADK web user interface provided by the ADK Rust command line tool. Both these options allow you to test and interact with your agent.

### Run with command-line interface

Run your agent using the following Rust command:

**Run from: `my_agent/` directory**

```bash
# The .env file will be loaded automatically by dotenv
cargo run
```

![ADK Console Interface](../assets/adk-run.png)

You can now interact with your agent:

```
🤖 Agent ready! Type your questions (or 'exit' to quit).

You: What time is it in Tokyo?
Assistant: Let me search for the current time in Tokyo...
[Searching...]
According to recent information, the current time in Tokyo is...

You: exit
👋 Goodbye!
```

### Run with web interface

For a web-based chat interface, you can use the ADK CLI server mode:

**Install ADK CLI:**

```bash
cargo install adk-cli
```

**Run the web server:**

**Run from: `my_agent/` directory**

```bash
adk-cli serve --port 8080
```

This command starts a web server with a chat interface for your agent. You can access the web interface at [http://localhost:8080](http://localhost:8080). Select your agent at the upper left corner and type a request.

![ADK Web Interface](../assets/adk-web-dev-ui-chat.png)

### Alternative: Use in your own server

You can also integrate the agent into your own web application:

```rust
use adk_rust::prelude::*;
use adk_rust::server::{start_server, ServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // ... create agent as before ...
    
    let config = ServerConfig {
        host: "0.0.0.0".to_string(),
        port: 8080,
        app_name: "my-agent".to_string(),
    };
    
    start_server(config, Arc::new(time_agent), session_service).await?;
    Ok(())
}
```

## Next: Build your agent

Now that you have ADK installed and your first agent running, try building your own agent with our build guides:

- [Core Concepts](concepts.md) - Understand agents, models, and tools
- [Agent Types](agents.md) - Learn about different agent types
- [Adding Tools](tools.md) - Extend your agent with custom tools
- [Workflows](workflows.md) - Build multi-agent workflows
- [Deployment](deployment.md) - Deploy your agent to production

---

**Previous**: [Introduction](introduction.md) | **Next**: [Core Concepts](concepts.md)
