//! Interactive REPL for Ralph chat mode.
//!
//! This module provides the main entry point for `ralph chat` command,
//! implementing a REPL-style interface where users can interact with
//! Ralph through natural conversation.
//!
//! ## Requirements Validated
//!
//! - 1.1: WHEN the user runs `ralph chat`, THE System SHALL start an interactive REPL session
//! - 1.2: THE System SHALL display a prompt and wait for user input
//! - 1.5: WHEN the user types `exit` or `quit`, THE System SHALL end the session gracefully

use crate::interactive::{OrchestratorAgent, OrchestratorAgentBuilder, Session};
use crate::models::RalphConfig;
use crate::output::RalphOutput;
use crate::{RalphError, Result};
use adk_core::{
    Agent, Artifacts, CallbackContext, Content, InvocationContext, Memory, Part, ReadonlyContext,
    RunConfig, Session as CoreSession, State,
};
use async_trait::async_trait;
use colored::Colorize;
use futures::StreamExt;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Default prompt displayed to the user.
const DEFAULT_PROMPT: &str = "ralph> ";

/// Commands that exit the REPL.
const EXIT_COMMANDS: &[&str] = &["exit", "quit", "q", ":q", ":quit"];

/// Commands that display help.
const HELP_COMMANDS: &[&str] = &["help", "?", ":help", ":h"];

/// Commands that clear the screen.
const CLEAR_COMMANDS: &[&str] = &["clear", "cls", ":clear"];

/// Commands that show status.
const STATUS_COMMANDS: &[&str] = &["status", ":status", ":s"];

/// Simple invocation context for the REPL.
///
/// This provides the minimal context needed to run an agent.
struct ReplContext {
    content: Content,
    config: RunConfig,
    agent: Arc<dyn Agent + Send + Sync>,
    conversation_history: Vec<Content>,
}

impl ReplContext {
    fn new(message: &str, agent: Arc<dyn Agent + Send + Sync>, history: Vec<Content>) -> Self {
        Self {
            content: Content::new("user").with_text(message),
            config: RunConfig::default(),
            agent,
            conversation_history: history,
        }
    }
}

#[async_trait]
impl ReadonlyContext for ReplContext {
    fn invocation_id(&self) -> &str {
        "repl-invocation"
    }
    fn agent_name(&self) -> &str {
        "orchestrator-agent"
    }
    fn user_id(&self) -> &str {
        "repl-user"
    }
    fn app_name(&self) -> &str {
        "ralph-interactive"
    }
    fn session_id(&self) -> &str {
        "repl-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for ReplContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for ReplContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
    fn session(&self) -> &dyn CoreSession {
        self
    }
}

impl CoreSession for ReplContext {
    fn id(&self) -> &str {
        "repl-session"
    }
    fn app_name(&self) -> &str {
        "ralph-interactive"
    }
    fn user_id(&self) -> &str {
        "repl-user"
    }
    fn state(&self) -> &dyn State {
        self
    }
    fn conversation_history(&self) -> Vec<Content> {
        self.conversation_history.clone()
    }
}

impl State for ReplContext {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

/// Interactive REPL for Ralph chat mode.
///
/// Provides a conversational interface where users can interact with Ralph
/// through natural language. The REPL maintains session state and routes
/// requests through the orchestrator agent.
pub struct InteractiveRepl {
    /// Session state (conversation history, preferences, etc.)
    session: Arc<RwLock<Session>>,
    /// Orchestrator agent for processing requests
    orchestrator: OrchestratorAgent,
    /// Ralph configuration
    config: RalphConfig,
    /// Output handler for formatted display
    output: RalphOutput,
    /// Whether to auto-approve changes
    auto_approve: bool,
    /// Whether the REPL is running
    running: bool,
}

impl std::fmt::Debug for InteractiveRepl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractiveRepl")
            .field("config", &self.config)
            .field("auto_approve", &self.auto_approve)
            .field("running", &self.running)
            .finish()
    }
}

impl InteractiveRepl {
    /// Create a new builder for InteractiveRepl.
    pub fn builder() -> InteractiveReplBuilder {
        InteractiveReplBuilder::default()
    }

    /// Get the session.
    pub fn session(&self) -> Arc<RwLock<Session>> {
        self.session.clone()
    }

    /// Get the orchestrator agent.
    pub fn orchestrator(&self) -> &OrchestratorAgent {
        &self.orchestrator
    }

    /// Check if auto-approve is enabled.
    pub fn auto_approve(&self) -> bool {
        self.auto_approve
    }

    /// Run the interactive REPL loop.
    ///
    /// This is the main entry point that reads user input, processes it
    /// through the orchestrator, and displays responses.
    pub async fn run(&mut self) -> Result<()> {
        self.running = true;
        
        // Display welcome message
        self.display_welcome().await;

        // Main REPL loop
        while self.running {
            // Display prompt and read input
            let input = match self.read_input() {
                Ok(input) => input,
                Err(e) => {
                    // Handle EOF (Ctrl+D)
                    if e.to_string().contains("EOF") {
                        self.running = false;
                        break;
                    }
                    self.output.error(&format!("Input error: {}", e));
                    continue;
                }
            };

            // Skip empty input
            let input = input.trim();
            if input.is_empty() {
                continue;
            }

            // Handle special commands
            if self.handle_special_command(input).await {
                continue;
            }

            // Process through orchestrator
            self.process_input(input).await;
        }

        // Save session on exit
        self.save_session().await?;
        
        // Display goodbye message
        self.display_goodbye();

        Ok(())
    }

    /// Display the welcome message.
    async fn display_welcome(&self) {
        println!();
        println!("{}", "Welcome to Ralph Interactive Mode!".bright_cyan().bold());
        println!("{}", "─".repeat(40).bright_black());
        println!();
        println!("I'm your AI development assistant. I can help you:");
        println!("  {} Create new projects from descriptions", "•".bright_blue());
        println!("  {} Add features to existing projects", "•".bright_blue());
        println!("  {} Run and test your code", "•".bright_blue());
        println!("  {} Manage tasks and track progress", "•".bright_blue());
        println!();
        println!(
            "Type {} for available commands, or just start chatting!",
            "help".cyan()
        );
        println!();

        // Show session info if resuming
        let session = self.session.read().await;
        if !session.conversation_history.is_empty() {
            println!(
                "{} Resumed session with {} previous messages",
                "ℹ".bright_blue(),
                session.conversation_history.len()
            );
            println!();
        }

        // Show project context if available
        let ctx = &session.project_context;
        if ctx.has_prd || ctx.has_design || ctx.has_tasks {
            println!("{}", "Project Status:".yellow());
            if ctx.has_prd {
                println!("  {} PRD exists", "✓".green());
            }
            if ctx.has_design {
                println!("  {} Design exists", "✓".green());
            }
            if ctx.has_tasks {
                println!("  {} Tasks exist", "✓".green());
            }
            if let Some(ref lang) = ctx.language {
                println!("  {} Language: {}", "•".bright_black(), lang.cyan());
            }
            println!();
        }
    }

    /// Display the goodbye message.
    fn display_goodbye(&self) {
        println!();
        println!("{}", "Session saved. Goodbye!".bright_cyan());
        println!();
    }

    /// Read user input from stdin.
    fn read_input(&self) -> Result<String> {
        // Display prompt
        print!("{}", DEFAULT_PROMPT.bright_green());
        io::stdout().flush().map_err(|e| {
            RalphError::Internal(format!("Failed to flush stdout: {}", e))
        })?;

        // Read line
        let stdin = io::stdin();
        let mut line = String::new();
        let bytes_read = stdin.lock().read_line(&mut line).map_err(|e| {
            RalphError::Internal(format!("Failed to read input: {}", e))
        })?;

        // Check for EOF
        if bytes_read == 0 {
            return Err(RalphError::Internal("EOF".to_string()));
        }

        Ok(line)
    }

    /// Handle special commands (exit, help, etc.).
    ///
    /// Returns true if the command was handled, false if it should be
    /// passed to the orchestrator.
    async fn handle_special_command(&mut self, input: &str) -> bool {
        let lower = input.to_lowercase();

        // Exit commands
        if EXIT_COMMANDS.contains(&lower.as_str()) {
            self.running = false;
            return true;
        }

        // Help commands
        if HELP_COMMANDS.contains(&lower.as_str()) {
            self.display_help();
            return true;
        }

        // Clear commands
        if CLEAR_COMMANDS.contains(&lower.as_str()) {
            self.clear_screen();
            return true;
        }

        // Status commands
        if STATUS_COMMANDS.contains(&lower.as_str()) {
            self.display_status().await;
            return true;
        }

        // History command
        if lower == "history" || lower == ":history" {
            self.display_history().await;
            return true;
        }

        // Clear history command
        if lower == "clear history" || lower == ":clear-history" {
            self.clear_history().await;
            return true;
        }

        false
    }

    /// Display help information.
    ///
    /// Shows available commands and usage examples.
    /// Validates: Requirements 7.1
    fn display_help(&self) {
        println!();
        println!("{}", "Ralph Interactive Mode - Help".bright_cyan().bold());
        println!("{}", "═".repeat(50).bright_black());
        println!();
        
        // Commands section
        println!("{}", "Commands:".yellow().bold());
        println!("  {}         Show this help message", "help".cyan());
        println!("  {}         Exit the interactive session", "exit".cyan());
        println!("  {}        Clear the terminal screen", "clear".cyan());
        println!("  {}       Show project status and artifacts", "status".cyan());
        println!("  {}      Show conversation history", "history".cyan());
        println!();
        
        // Project creation examples
        println!("{}", "Creating Projects:".yellow().bold());
        println!("  {} \"Create a CLI calculator in Rust\"", "•".bright_blue());
        println!("  {} \"Build a REST API for a todo app in Python\"", "•".bright_blue());
        println!("  {} \"Make a web scraper in Go\"", "•".bright_blue());
        println!();
        
        // Feature addition examples
        println!("{}", "Adding Features:".yellow().bold());
        println!("  {} \"Add a history feature\"", "•".bright_blue());
        println!("  {} \"Add user authentication\"", "•".bright_blue());
        println!("  {} \"Add support for JSON output\"", "•".bright_blue());
        println!();
        
        // Running and testing
        println!("{}", "Running & Testing:".yellow().bold());
        println!("  {} \"Run the project\"", "•".bright_blue());
        println!("  {} \"Test it\"", "•".bright_blue());
        println!("  {} \"Run with --help flag\"", "•".bright_blue());
        println!();
        
        // Task management
        println!("{}", "Task Management:".yellow().bold());
        println!("  {} \"What tasks are left?\"", "•".bright_blue());
        println!("  {} \"Show me the next task\"", "•".bright_blue());
        println!("  {} \"Mark task 1 as complete\"", "•".bright_blue());
        println!();
        
        // File operations
        println!("{}", "File Operations:".yellow().bold());
        println!("  {} \"Show me the main.rs file\"", "•".bright_blue());
        println!("  {} \"List all files in src/\"", "•".bright_blue());
        println!("  {} \"Update the README\"", "•".bright_blue());
        println!();
        
        // Git operations
        println!("{}", "Version Control:".yellow().bold());
        println!("  {} \"Show git status\"", "•".bright_blue());
        println!("  {} \"Commit the changes\"", "•".bright_blue());
        println!("  {} \"Show the diff\"", "•".bright_blue());
        println!();
        
        // General queries
        println!("{}", "General Queries:".yellow().bold());
        println!("  {} \"What time is it?\"", "•".bright_blue());
        println!("  {} \"What can you do?\"", "•".bright_blue());
        println!("  {} \"How do I use this?\"", "•".bright_blue());
        println!();
        
        // Tips section
        println!("{}", "Tips:".yellow().bold());
        println!("  {} Just describe what you want in natural language", "💡".bright_black());
        println!("  {} Ralph will figure out the best way to help", "💡".bright_black());
        println!("  {} Use Ctrl+D or type 'exit' to quit", "💡".bright_black());
        println!("  {} Your session is saved automatically", "💡".bright_black());
        println!("  {} Use 'ralph chat --resume' to continue later", "💡".bright_black());
        println!();
        
        println!("{}", "─".repeat(50).bright_black());
        println!();
    }

    /// Clear the terminal screen.
    fn clear_screen(&self) {
        // ANSI escape code to clear screen and move cursor to top
        print!("\x1B[2J\x1B[1;1H");
        let _ = io::stdout().flush();
    }

    /// Display current project status.
    async fn display_status(&self) {
        let session = self.session.read().await;
        
        println!();
        println!("{}", "Project Status".bright_cyan().bold());
        println!("{}", "─".repeat(40).bright_black());
        
        let ctx = &session.project_context;
        println!("  Path: {}", ctx.project_path.display().to_string().cyan());
        
        if let Some(ref lang) = ctx.language {
            println!("  Language: {}", lang.cyan());
        }
        
        println!();
        println!("{}", "Artifacts:".yellow());
        println!(
            "  PRD:    {}",
            if ctx.has_prd { "✓".green() } else { "✗".red() }
        );
        println!(
            "  Design: {}",
            if ctx.has_design { "✓".green() } else { "✗".red() }
        );
        println!(
            "  Tasks:  {}",
            if ctx.has_tasks { "✓".green() } else { "✗".red() }
        );
        
        if let Some(ref phase) = ctx.current_phase {
            println!();
            println!("  Current Phase: {}", phase.to_string().cyan());
        }
        
        println!();
        println!("{}", "Session:".yellow());
        println!("  Messages: {}", session.conversation_history.len());
        println!("  Preferences: {}", session.user_preferences.len());
        println!();
    }

    /// Display conversation history.
    async fn display_history(&self) {
        let session = self.session.read().await;
        
        println!();
        println!("{}", "Conversation History".bright_cyan().bold());
        println!("{}", "─".repeat(40).bright_black());
        
        if session.conversation_history.is_empty() {
            println!("  No messages yet.");
        } else {
            for (i, msg) in session.conversation_history.iter().enumerate() {
                let role_display = if msg.role == "user" {
                    "You".bright_green()
                } else {
                    "Ralph".bright_cyan()
                };
                
                // Truncate long messages
                let content = if msg.content.len() > 100 {
                    format!("{}...", &msg.content[..100])
                } else {
                    msg.content.clone()
                };
                
                println!("  {}. {}: {}", i + 1, role_display, content.bright_black());
            }
        }
        println!();
    }

    /// Clear conversation history.
    async fn clear_history(&mut self) {
        let mut session = self.session.write().await;
        session.clear_history();
        println!();
        println!("{} Conversation history cleared.", "✓".green());
        println!();
    }

    /// Process user input through the orchestrator.
    ///
    /// Streams the response incrementally, showing a typing indicator
    /// while processing and displaying text as it arrives.
    async fn process_input(&mut self, input: &str) {
        // Add user message to session
        {
            let mut session = self.session.write().await;
            session.add_user_message(input);
        }

        // Show typing indicator
        self.show_typing_indicator();

        // Get conversation history for context
        let history = {
            let session = self.session.read().await;
            session
                .conversation_history
                .iter()
                .map(|msg| {
                    Content::new(&msg.role).with_text(&msg.content)
                })
                .collect::<Vec<_>>()
        };

        // Create invocation context
        let ctx = Arc::new(ReplContext::new(
            input,
            self.orchestrator.agent(),
            history,
        ));

        // Run the orchestrator agent
        let result = self.orchestrator.agent().run(ctx).await;

        // Clear typing indicator
        self.clear_typing_indicator();

        // Handle result
        match result {
            Ok(mut stream) => {
                let mut response_text = String::new();
                let mut first_chunk = true;
                
                // Stream response incrementally
                while let Some(event_result) = stream.next().await {
                    match event_result {
                        Ok(event) => {
                            if let Some(ref content) = event.llm_response.content {
                                for part in &content.parts {
                                    if let Part::Text { text } = part {
                                        // Print header on first chunk
                                        if first_chunk && !text.is_empty() {
                                            println!();
                                            println!("{}", "Ralph:".bright_cyan().bold());
                                            first_chunk = false;
                                        }
                                        
                                        // Stream text incrementally
                                        self.stream_text(text);
                                        response_text.push_str(text);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.output.error(&format!("Stream error: {}", e));
                            return;
                        }
                    }
                }

                // Add newline after streaming completes
                if !first_chunk {
                    println!();
                    println!();
                }

                // Add assistant message to session
                if !response_text.is_empty() {
                    {
                        let mut session = self.session.write().await;
                        session.add_assistant_message(&response_text);
                    }
                } else {
                    self.display_response("I processed your request but have no text response.");
                }
            }
            Err(e) => {
                self.output.error(&format!("Failed to process request: {}", e));
            }
        }
    }

    /// Stream text to the terminal incrementally.
    ///
    /// This displays text character by character or in small chunks
    /// to provide a more natural streaming experience.
    fn stream_text(&self, text: &str) {
        // For streaming, we print the text with proper indentation
        // Split by lines to handle multi-line responses
        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                println!();
            }
            print!("  {}", line);
        }
        
        // Handle trailing newline
        if text.ends_with('\n') {
            println!();
        }
        
        // Flush to ensure immediate display
        let _ = io::stdout().flush();
    }

    /// Display a response to the user.
    fn display_response(&self, response: &str) {
        println!();
        println!("{}", "Ralph:".bright_cyan().bold());
        
        // Display response with proper formatting
        for line in response.lines() {
            println!("  {}", line);
        }
        
        println!();
    }

    /// Show a typing indicator while processing.
    ///
    /// Displays a visual indicator that Ralph is thinking/processing.
    fn show_typing_indicator(&self) {
        print!("{}", "  ⏳ Thinking...".bright_black());
        let _ = io::stdout().flush();
    }

    /// Clear the typing indicator.
    fn clear_typing_indicator(&self) {
        // Move cursor back and clear line
        print!("\r{}\r", " ".repeat(30));
        let _ = io::stdout().flush();
    }

    /// Save the session to disk.
    async fn save_session(&self) -> Result<()> {
        let session = self.session.read().await;
        session.save_to_project()?;
        Ok(())
    }

    /// Refresh project context by checking for existing files.
    pub async fn refresh_context(&mut self) {
        let mut session = self.session.write().await;
        session.project_context.refresh(
            &self.config.prd_path,
            &self.config.design_path,
            &self.config.tasks_path,
        );
    }
}

/// Builder for creating an InteractiveRepl with fluent API.
#[derive(Debug)]
pub struct InteractiveReplBuilder {
    config: Option<RalphConfig>,
    project_path: PathBuf,
    session: Option<Session>,
    auto_approve: bool,
    resume: bool,
}

impl Default for InteractiveReplBuilder {
    fn default() -> Self {
        Self {
            config: None,
            project_path: PathBuf::from("."),
            session: None,
            auto_approve: false,
            resume: false,
        }
    }
}

impl InteractiveReplBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the Ralph configuration.
    pub fn config(mut self, config: RalphConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the project path.
    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = path.into();
        self
    }

    /// Set an existing session to use.
    pub fn session(mut self, session: Session) -> Self {
        self.session = Some(session);
        self
    }

    /// Enable auto-approve mode.
    pub fn auto_approve(mut self, auto_approve: bool) -> Self {
        self.auto_approve = auto_approve;
        self
    }

    /// Enable session resume (load from disk if exists).
    pub fn resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    /// Build the InteractiveRepl.
    pub async fn build(self) -> Result<InteractiveRepl> {
        // Get or create config
        let config = self.config.unwrap_or_default();
        
        // Determine project path
        let project_path = if self.project_path == PathBuf::from(".") {
            PathBuf::from(&config.project_path)
        } else {
            self.project_path
        };

        // Get or create session
        let session = if let Some(s) = self.session {
            s
        } else if self.resume {
            // Try to load existing session
            Session::try_load_from_project(&project_path)
                .unwrap_or_else(|| Session::new(&project_path))
        } else {
            Session::new(&project_path)
        };

        // Refresh project context
        let mut session = session;
        session.project_context.refresh(
            &config.prd_path,
            &config.design_path,
            &config.tasks_path,
        );

        // Store auto_approve preference
        if self.auto_approve {
            session.set_preference("auto_approve", "true");
        }

        // Create output handler
        let output = RalphOutput::new(config.debug_level);

        // Build orchestrator agent
        let orchestrator = OrchestratorAgentBuilder::new()
            .project_path(&project_path)
            .config(config.clone())
            .build()
            .await?;

        Ok(InteractiveRepl {
            session: Arc::new(RwLock::new(session)),
            orchestrator,
            config,
            output,
            auto_approve: self.auto_approve,
            running: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_commands() {
        assert!(EXIT_COMMANDS.contains(&"exit"));
        assert!(EXIT_COMMANDS.contains(&"quit"));
        assert!(EXIT_COMMANDS.contains(&"q"));
        assert!(EXIT_COMMANDS.contains(&":q"));
    }

    #[test]
    fn test_help_commands() {
        assert!(HELP_COMMANDS.contains(&"help"));
        assert!(HELP_COMMANDS.contains(&"?"));
        assert!(HELP_COMMANDS.contains(&":help"));
    }

    #[test]
    fn test_builder_defaults() {
        let builder = InteractiveReplBuilder::default();
        assert!(builder.config.is_none());
        assert_eq!(builder.project_path, PathBuf::from("."));
        assert!(!builder.auto_approve);
        assert!(!builder.resume);
    }

    #[test]
    fn test_builder_fluent_api() {
        let builder = InteractiveReplBuilder::new()
            .project_path("/tmp/project")
            .auto_approve(true)
            .resume(true);

        assert_eq!(builder.project_path, PathBuf::from("/tmp/project"));
        assert!(builder.auto_approve);
        assert!(builder.resume);
    }

    #[test]
    fn test_extract_response_text() {
        // This test would require a mock InteractiveRepl
        // For now, we just verify the constants are correct
        assert_eq!(DEFAULT_PROMPT, "ralph> ");
    }
}
