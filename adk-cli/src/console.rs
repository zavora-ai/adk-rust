use adk_core::{Agent, Content, Part};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;
use rustyline::DefaultEditor;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

#[allow(dead_code)] // Part of CLI API, not currently used
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
        app_name: app_name.clone(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
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

                let mut stream_printer = StreamPrinter::default();

                while let Some(event) = events.next().await {
                    match event {
                        Ok(evt) => {
                            if let Some(content) = &evt.llm_response.content {
                                for part in &content.parts {
                                    stream_printer.handle_part(part);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("\nError: {}", e);
                        }
                    }
                }

                stream_printer.finish();
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

/// StreamPrinter handles streaming output with special handling for:
/// - `<think>` blocks: Displayed as `[think] ...` for reasoning models
/// - Tool calls and responses: Formatted output
/// - Regular text: Streamed directly to stdout
#[derive(Default)]
struct StreamPrinter {
    in_think_block: bool,
    think_buffer: String,
}

impl StreamPrinter {
    fn handle_part(&mut self, part: &Part) {
        match part {
            Part::Text { text } => self.handle_text_chunk(text),
            Part::Thinking { thinking, .. } => {
                print!("\n[thinking] {}\n", thinking);
                let _ = io::stdout().flush();
            }
            Part::FunctionCall { name, args, .. } => self.print_tool_call(name, args),
            Part::FunctionResponse { function_response, .. } => {
                self.print_tool_response(&function_response.name, &function_response.response)
            }
            Part::InlineData { mime_type, data } => self.print_inline_data(mime_type, data.len()),
            Part::FileData { mime_type, file_uri } => self.print_file_data(mime_type, file_uri),
        }
    }

    fn handle_text_chunk(&mut self, chunk: &str) {
        const THINK_START: &str = "<think>";
        const THINK_END: &str = "</think>";

        let mut remaining = chunk;

        while !remaining.is_empty() {
            if self.in_think_block {
                if let Some(end_idx) = remaining.find(THINK_END) {
                    self.think_buffer.push_str(&remaining[..end_idx]);
                    self.flush_think();
                    self.in_think_block = false;
                    remaining = &remaining[end_idx + THINK_END.len()..];
                } else {
                    self.think_buffer.push_str(remaining);
                    break;
                }
            } else if let Some(start_idx) = remaining.find(THINK_START) {
                let visible = &remaining[..start_idx];
                self.print_visible(visible);
                self.in_think_block = true;
                self.think_buffer.clear();
                remaining = &remaining[start_idx + THINK_START.len()..];
            } else {
                self.print_visible(remaining);
                break;
            }
        }
    }

    fn print_visible(&self, text: &str) {
        if text.is_empty() {
            return;
        }

        print!("{}", text);
        let _ = io::stdout().flush();
    }

    fn flush_think(&mut self) {
        let content = self.think_buffer.trim();
        if content.is_empty() {
            self.think_buffer.clear();
            return;
        }

        print!("\n[think] {}\n", content);
        let _ = io::stdout().flush();
        self.think_buffer.clear();
    }

    fn finish(&mut self) {
        if self.in_think_block {
            self.flush_think();
            self.in_think_block = false;
        }
    }

    fn print_tool_call(&self, name: &str, args: &Value) {
        print!("\n[tool-call] {} {}\n", name, args);
        let _ = io::stdout().flush();
    }

    fn print_tool_response(&self, name: &str, response: &Value) {
        print!("\n[tool-response] {} {}\n", name, response);
        let _ = io::stdout().flush();
    }

    fn print_inline_data(&self, mime_type: &str, len: usize) {
        print!("\n[inline-data] mime={} bytes={}\n", mime_type, len);
        let _ = io::stdout().flush();
    }

    fn print_file_data(&self, mime_type: &str, file_uri: &str) {
        print!("\n[file-data] mime={} uri={}\n", mime_type, file_uri);
        let _ = io::stdout().flush();
    }
}
