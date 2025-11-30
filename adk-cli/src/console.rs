use anyhow::Result;
use adk_core::{Agent, Content, Part};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // Part of CLI API, not currently used
pub async fn run_console(
    agent: Arc<dyn Agent>,
    app_name: String,
    user_id: String,
) -> Result<()> {
    let session_service = Arc::new(InMemorySessionService::new());
    
    let session = session_service
        .create(CreateRequest {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;
    
    let runner = Runner::new(RunnerConfig {
        app_name: app_name.clone(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
    })?;
    
    let mut rl = DefaultEditor::new()?;
    
    println!("ADK Console Mode");
    println!("Agent: {}", agent.name());
    println!("Type your message and press Enter. Ctrl+C to exit.\n");
    
    loop {
        let readline = rl.readline("User -> ");
        match readline {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                
                rl.add_history_entry(&line)?;
                
                let user_content = Content::new("user").with_text(line);
                
                print!("\nAgent -> ");
                
                let session_id = session.id().to_string();
                let mut events = runner.run(user_id.clone(), session_id, user_content).await?;
                
                while let Some(event) = events.next().await {
                    match event {
                        Ok(evt) => {
                            if let Some(content) = &evt.llm_response.content {
                                for part in &content.parts {
                                    match part {
                                        Part::Text { text } => print!("{}", text),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("\nError: {}", e);
                        }
                    }
                }
                
                println!("\n");
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("Interrupted");
                break;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("EOF");
                break;
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                break;
            }
        }
    }
    
    Ok(())
}
