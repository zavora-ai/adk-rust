//! Legacy console entry point.
//!
//! New code should use [`Launcher`](crate::Launcher) instead, which provides
//! the same REPL with additional configuration options (memory, artifacts,
//! streaming mode, etc.).

use adk_core::{Agent, SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::sync::Arc;

use crate::launcher::StreamPrinter;

/// Run an interactive console session with the given agent.
///
/// This is a convenience wrapper kept for backward compatibility with
/// existing examples. Prefer [`Launcher`](crate::Launcher) for new code.
pub async fn run_console(agent: Arc<dyn Agent>, app_name: String, user_id: String) -> Result<()> {
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
        app_name,
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
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

                let user_content = adk_core::Content::new("user").with_text(line);
                print!("\nAgent -> ");

                let session_id = session.id().to_string();
                let mut events = runner
                    .run(UserId::new(user_id.clone())?, SessionId::new(session_id)?, user_content)
                    .await?;
                let mut printer = StreamPrinter::default();

                while let Some(event) = events.next().await {
                    match event {
                        Ok(evt) => {
                            if let Some(content) = &evt.llm_response.content {
                                for part in &content.parts {
                                    printer.handle_part(part);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("\nError: {e}");
                        }
                    }
                }

                printer.finish();
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
                eprintln!("Error: {err}");
                break;
            }
        }
    }

    Ok(())
}
