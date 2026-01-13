//! Ralph Loop Agent for iterative task implementation.
//!
//! The Ralph Loop Agent is the third phase in the Ralph pipeline:
//! 1. PRD Agent → generates requirements
//! 2. Architect Agent → generates design and tasks
//! 3. **Ralph Loop Agent** → implements tasks iteratively
//!
//! ## Architecture
//!
//! This agent uses ADK's `LoopAgent` workflow pattern:
//! - An inner `LlmAgent` has access to all tools (progress, tasks, test, file, git)
//! - The `LoopAgent` wrapper runs the inner agent repeatedly
//! - The LLM decides when to call `exit_loop` to signal completion
//!
//! ## Key Features
//!
//! - LLM-driven workflow (no hardcoded logic)
//! - Priority-based task selection via TaskTool
//! - Test-before-commit workflow via TestTool
//! - Progress recording via ProgressTool
//! - Completion detection via ExitLoopTool
//! - Configurable debug output levels
//!
//! ## Requirements Validated
//!
//! - 4.1: WHEN starting an iteration, THE Ralph_Loop_Agent SHALL read `tasks.json`
//! - 5.1: THE Ralph_Loop_Agent SHALL work on ONLY ONE task per iteration
//! - 7.4: WHEN starting each iteration, THE Ralph_Loop_Agent SHALL read `progress.json`

use crate::models::{DesignDocument, ModelConfig, RalphConfig};
use crate::output::{process_event_part, RalphOutput};
use crate::tools::{FileTool, GitTool, ProgressTool, TaskTool, TestTool};
use crate::{RalphError, Result};
use adk_agent::{LlmAgentBuilder, LoopAgent};
use adk_core::{Agent, Llm, Tool};
use adk_tool::ExitLoopTool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Instruction prompt for the Ralph Loop Agent.
///
/// This instruction gives the LLM full autonomy to:
/// - Read progress and tasks
/// - Select and implement tasks
/// - Write files and run tests
/// - Commit changes and record progress
/// - Decide when to exit the loop
const RALPH_LOOP_INSTRUCTION: &str = r#"You are Ralph, an autonomous development agent. Your role is to implement tasks one at a time until the project is complete.

## Available Tools

- `progress`: Read/append progress log (operations: read, append, summary)
- `tasks`: Manage task list (operations: list, get_next, update_status, complete)
- `test`: Run tests (operations: run, detect, check)
- `file`: File operations (operations: read, write, list, delete)
- `git`: Git operations (operations: status, add, commit, diff)
- `exit_loop`: Signal completion or end of iteration

## Workflow for Each Iteration

### 1. Read Context
- Call `progress` with operation "read" to understand past work and learnings
- Call `tasks` with operation "get_next" to get the highest priority pending task

### 2. Implement the Task
- Read relevant files using `file` with operation "read"
- Write implementation code using `file` with operation "write"
- Create tests for the implementation

### 3. Verify Implementation
- Call `test` with operation "run" to run the test suite
- If tests fail, fix the code and re-run tests (max 3 attempts)
- If tests still fail after 3 attempts, mark task as blocked

### 4. Commit and Record
- If tests pass, call `git` with operation "add" then "commit"
- Call `progress` with operation "append" to record:
  - approach: How you implemented it
  - learnings: What you learned
  - gotchas: Pitfalls to avoid
  - files_created/files_modified: What changed
  - test_results: Pass/fail counts
- Call `tasks` with operation "complete" to mark the task done

### 5. Check Completion
- Call `tasks` with operation "list" to check overall status
- If ALL tasks are completed, call `exit_loop` with the completion message
- If more tasks remain, continue to the next task (DO NOT call exit_loop)

## Critical Rules

1. **Complete ALL tasks** - Keep working until all tasks are done
2. **Test before commit** - NEVER commit code that doesn't pass tests
3. **Record learnings** - Always append to progress after completing a task
4. **Read progress first** - Learn from past work before starting
5. **Only exit when done** - ONLY call `exit_loop` when ALL tasks are completed

## Completion Detection

When you call `tasks` with operation "list" and see all tasks are "completed":
- Call `exit_loop` with message: "All tasks completed successfully!"
- The loop will terminate

IMPORTANT: Do NOT call `exit_loop` until ALL tasks are completed. Keep working on tasks one by one.
"#;

/// Ralph Loop Agent that iteratively implements tasks until completion.
///
/// This agent wraps ADK's `LoopAgent` with an `LlmAgent` sub-agent that has
/// access to all the tools needed for autonomous development.
///
/// # Example
///
/// ```rust,ignore
/// use adk_ralph::agents::RalphLoopAgent;
/// use adk_ralph::RalphConfig;
///
/// let config = RalphConfig::from_env()?;
/// let agent = RalphLoopAgent::builder()
///     .config(config)
///     .build()
///     .await?;
///
/// // Run with ADK runner
/// let runner = Runner::new(agent);
/// runner.run(session, content).await?;
/// ```
pub struct RalphLoopAgent {
    /// The underlying ADK LoopAgent (stored as Arc<dyn Agent>)
    agent: Arc<dyn Agent>,
    /// Model configuration (for reference)
    model_config: ModelConfig,
    /// Ralph configuration
    config: RalphConfig,
    /// Project base directory
    project_path: PathBuf,
}

impl std::fmt::Debug for RalphLoopAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RalphLoopAgent")
            .field("name", &self.agent.name())
            .field("model_config", &self.model_config)
            .field("project_path", &self.project_path)
            .field("max_iterations", &self.config.max_iterations)
            .finish()
    }
}

impl RalphLoopAgent {
    /// Create a new builder for RalphLoopAgent.
    pub fn builder() -> RalphLoopAgentBuilder {
        RalphLoopAgentBuilder::default()
    }

    /// Get the instruction prompt for the Ralph Loop Agent.
    pub fn instruction() -> &'static str {
        RALPH_LOOP_INSTRUCTION
    }

    /// Get the model configuration.
    pub fn model_config(&self) -> &ModelConfig {
        &self.model_config
    }

    /// Get the Ralph configuration.
    pub fn config(&self) -> &RalphConfig {
        &self.config
    }

    /// Get the project path.
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the underlying agent.
    pub fn inner(&self) -> &Arc<dyn Agent> {
        &self.agent
    }

    /// Get the completion promise text.
    pub fn completion_promise(&self) -> &str {
        &self.config.completion_promise
    }

    /// Read the design document for implementation guidance.
    pub fn read_design(&self) -> Result<DesignDocument> {
        let design_path = self.project_path.join(&self.config.design_path);
        DesignDocument::load_markdown(&design_path).map_err(RalphError::Design)
    }

    /// Convert to Arc<dyn Agent> for use with ADK runner.
    pub fn into_agent(self) -> Arc<dyn Agent> {
        self.agent
    }
}

/// Builder for creating a RalphLoopAgent with fluent API.
pub struct RalphLoopAgentBuilder {
    model: Option<Arc<dyn Llm>>,
    model_config: ModelConfig,
    config: RalphConfig,
    project_path: PathBuf,
    additional_tools: Vec<Arc<dyn Tool>>,
    custom_instruction: Option<String>,
}

impl std::fmt::Debug for RalphLoopAgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RalphLoopAgentBuilder")
            .field("model", &self.model.as_ref().map(|m| m.name()))
            .field("model_config", &self.model_config)
            .field("project_path", &self.project_path)
            .field("additional_tools_count", &self.additional_tools.len())
            .finish()
    }
}

impl Default for RalphLoopAgentBuilder {
    fn default() -> Self {
        let config = RalphConfig::default();
        Self {
            model: None,
            model_config: config.agents.ralph_model.clone(),
            config,
            project_path: PathBuf::from("."),
            additional_tools: Vec::new(),
            custom_instruction: None,
        }
    }
}

impl RalphLoopAgentBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the LLM model directly.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the model configuration.
    pub fn model_config(mut self, config: ModelConfig) -> Self {
        self.model_config = config;
        self
    }

    /// Set the Ralph configuration.
    pub fn config(mut self, config: RalphConfig) -> Self {
        self.model_config = config.agents.ralph_model.clone();
        self.config = config;
        self
    }

    /// Set the project base directory.
    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = path.into();
        self
    }

    /// Add an additional tool.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.additional_tools.push(tool);
        self
    }

    /// Add multiple additional tools.
    pub fn tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.additional_tools.extend(tools);
        self
    }

    /// Set a custom instruction (overrides default).
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.custom_instruction = Some(instruction.into());
        self
    }

    /// Build the RalphLoopAgent.
    ///
    /// If no model is provided, this will create one based on the model_config.
    pub async fn build(mut self) -> Result<RalphLoopAgent> {
        let model = match self.model.take() {
            Some(m) => m,
            None => create_model_from_config(&self.model_config).await?,
        };

        self.build_with_model(model)
    }

    /// Build the RalphLoopAgent with a pre-existing model (sync version).
    pub fn build_with_model(self, model: Arc<dyn Llm>) -> Result<RalphLoopAgent> {
        // Create the core tools
        let progress_path = self.project_path.join(&self.config.progress_path);
        let tasks_path = self.project_path.join(&self.config.tasks_path);

        let progress_tool = Arc::new(ProgressTool::new(progress_path, &self.config.prd_path));
        let task_tool = Arc::new(TaskTool::new(tasks_path));
        let test_tool = Arc::new(TestTool::new(&self.project_path));
        let file_tool = Arc::new(FileTool::new(&self.project_path));
        let git_tool = Arc::new(GitTool::new(&self.project_path));
        let exit_loop_tool = Arc::new(ExitLoopTool::new());

        // Build instruction with design context if available
        let instruction = self.custom_instruction.unwrap_or_else(|| {
            let mut inst = RALPH_LOOP_INSTRUCTION.to_string();
            
            // Try to add design context
            let design_path = self.project_path.join(&self.config.design_path);
            if let Ok(design) = DesignDocument::load_markdown(&design_path) {
                inst.push_str("\n\n## Project Context\n\n");
                inst.push_str(&format!("Project: {}\n", design.project));
                if let Some(ref tech) = design.technology_stack {
                    inst.push_str(&format!("Language: {}\n", tech.language));
                }
                if !design.overview.is_empty() {
                    inst.push_str(&format!("\nOverview: {}\n", design.overview));
                }
            }
            
            // Add completion promise
            inst.push_str(&format!(
                "\n\n## Completion Promise\n\nWhen all tasks are done, output: \"{}\"\n",
                self.config.completion_promise
            ));
            
            inst
        });

        // Build the inner LlmAgent with all tools
        let mut llm_builder = LlmAgentBuilder::new("ralph-worker")
            .description("Implements tasks autonomously using available tools")
            .instruction(instruction)
            .model(model)
            .tool(progress_tool)
            .tool(task_tool)
            .tool(test_tool)
            .tool(file_tool)
            .tool(git_tool)
            .tool(exit_loop_tool);

        // Add any additional tools
        for tool in self.additional_tools {
            llm_builder = llm_builder.tool(tool);
        }

        let llm_agent = llm_builder.build().map_err(|e| RalphError::Agent {
            agent: "ralph-worker".to_string(),
            message: e.to_string(),
        })?;

        // Wrap in LoopAgent
        let loop_agent = LoopAgent::new("ralph-loop", vec![Arc::new(llm_agent)])
            .with_description("Iteratively implements tasks until completion")
            .with_max_iterations(self.config.max_iterations as u32);

        Ok(RalphLoopAgent {
            agent: Arc::new(loop_agent),
            model_config: self.model_config,
            config: self.config,
            project_path: self.project_path,
        })
    }
}

/// Completion status returned by the Ralph Loop Agent.
#[derive(Debug, Clone, PartialEq)]
pub enum CompletionStatus {
    /// All tasks completed successfully
    Complete {
        /// Total iterations used
        iterations: u32,
        /// Total tasks completed
        tasks_completed: usize,
        /// Completion promise message
        message: String,
    },
    /// Max iterations reached with remaining work
    MaxIterationsReached {
        /// Iterations used
        iterations: u32,
        /// Tasks completed
        tasks_completed: usize,
        /// Tasks remaining
        tasks_remaining: usize,
    },
    /// All remaining tasks are blocked
    AllTasksBlocked {
        /// Iterations used
        iterations: u32,
        /// Tasks completed
        tasks_completed: usize,
        /// Tasks blocked
        tasks_blocked: usize,
        /// Reason for blockage
        reason: String,
    },
}

impl std::fmt::Display for CompletionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompletionStatus::Complete {
                iterations,
                tasks_completed,
                message,
            } => {
                write!(
                    f,
                    "✅ {} ({} tasks in {} iterations)",
                    message, tasks_completed, iterations
                )
            }
            CompletionStatus::MaxIterationsReached {
                iterations,
                tasks_completed,
                tasks_remaining,
            } => {
                write!(
                    f,
                    "⚠️ Max iterations ({}) reached: {} completed, {} remaining",
                    iterations, tasks_completed, tasks_remaining
                )
            }
            CompletionStatus::AllTasksBlocked {
                iterations,
                tasks_completed,
                tasks_blocked,
                reason,
            } => {
                write!(
                    f,
                    "🚫 All tasks blocked after {} iterations: {} completed, {} blocked ({})",
                    iterations, tasks_completed, tasks_blocked, reason
                )
            }
        }
    }
}

impl RalphLoopAgent {
    /// Run the Ralph loop using ADK's Runner.
    ///
    /// This method:
    /// 1. Creates an ADK Runner with the inner LoopAgent
    /// 2. Runs until completion or max iterations
    /// 3. Returns a CompletionStatus based on task state
    ///
    /// Output verbosity is controlled by the `debug_level` config setting.
    pub async fn run(&self) -> Result<CompletionStatus> {
        use adk_core::{Content, Part};
        use adk_runner::{Runner, RunnerConfig};
        use adk_session::{CreateRequest, InMemorySessionService, SessionService};
        use futures::StreamExt;

        // Create output handler based on debug level
        let output = RalphOutput::new(self.config.debug_level);

        // Show startup info based on debug level
        if output.level().is_normal() {
            output.phase("Ralph Loop Agent");
            if output.level().is_verbose() {
                output.debug("config", &format!("project: {}", self.project_path.display()));
                output.debug("config", &format!("max_iterations: {}", self.config.max_iterations));
            }
        }

        // Create session service and session
        let session_service = Arc::new(InMemorySessionService::new());
        let session_id = format!("ralph-session-{}", uuid::Uuid::new_v4());

        let _session = session_service
            .create(CreateRequest {
                app_name: "ralph".to_string(),
                user_id: "ralph-agent".to_string(),
                session_id: Some(session_id.clone()),
                state: std::collections::HashMap::new(),
            })
            .await
            .map_err(|e| RalphError::Agent {
                agent: "ralph-loop".to_string(),
                message: format!("Failed to create session: {}", e),
            })?;

        // Create runner with the loop agent
        let runner = Runner::new(RunnerConfig {
            app_name: "ralph".to_string(),
            agent: self.agent.clone(),
            session_service,
            artifact_service: None,
            memory_service: None,
            run_config: None,
        })
        .map_err(|e| RalphError::Agent {
            agent: "ralph-loop".to_string(),
            message: format!("Failed to create runner: {}", e),
        })?;

        // Initial prompt to start the loop
        let initial_content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: "Start implementing tasks. Read progress first, then get the next task and implement it.".to_string(),
            }],
        };

        output.debug("runner", "Sending initial prompt to LLM");

        // Run the agent
        let mut event_stream = runner
            .run("ralph-agent".to_string(), session_id, initial_content)
            .await
            .map_err(|e| RalphError::Agent {
                agent: "ralph-loop".to_string(),
                message: format!("Runner failed: {}", e),
            })?;

        // Track iterations and tool calls
        let mut iteration_count = 0u32;
        let mut tool_call_count = 0u32;

        // Process events with level-appropriate output
        while let Some(event_result) = event_stream.next().await {
            match event_result {
                Ok(event) => {
                    // Process content parts
                    if let Some(ref content) = event.llm_response.content {
                        for part in &content.parts {
                            // Track tool calls
                            if matches!(part, Part::FunctionCall { .. }) {
                                tool_call_count += 1;
                            }
                            // Output based on debug level
                            process_event_part(&output, part);
                        }
                    }

                    // Count iterations (each escalate = one iteration complete)
                    if event.actions.escalate {
                        iteration_count += 1;
                        output.iteration(iteration_count, self.config.max_iterations);
                    }
                }
                Err(e) => {
                    output.error(&e.to_string());
                    tracing::error!(error = %e, "Agent error");
                    return Err(RalphError::Agent {
                        agent: "ralph-loop".to_string(),
                        message: e.to_string(),
                    });
                }
            }
        }

        // Determine completion status by reading task state
        let tasks_path = self.project_path.join(&self.config.tasks_path);
        let task_list = crate::models::TaskList::load(&tasks_path).map_err(RalphError::Task)?;
        let stats = task_list.get_stats();

        // Output summary
        let success = task_list.is_complete();
        output.summary(iteration_count, stats.completed, stats.total, success);

        // Debug: show detailed stats
        if output.level().is_debug() {
            output.debug("stats", &format!("tool_calls: {}", tool_call_count));
            output.debug("stats", &format!("blocked: {}", stats.blocked));
            output.debug("stats", &format!("pending: {}", stats.pending));
            output.debug("stats", &format!("in_progress: {}", stats.in_progress));
        }

        if task_list.is_complete() {
            Ok(CompletionStatus::Complete {
                iterations: iteration_count,
                tasks_completed: stats.completed,
                message: self.config.completion_promise.clone(),
            })
        } else if stats.blocked > 0 && stats.pending == 0 && stats.in_progress == 0 {
            Ok(CompletionStatus::AllTasksBlocked {
                iterations: iteration_count,
                tasks_completed: stats.completed,
                tasks_blocked: stats.blocked,
                reason: "All remaining tasks are blocked".to_string(),
            })
        } else {
            Ok(CompletionStatus::MaxIterationsReached {
                iterations: iteration_count,
                tasks_completed: stats.completed,
                tasks_remaining: stats.pending + stats.in_progress,
            })
        }
    }
}

/// Create an LLM model from configuration.
async fn create_model_from_config(config: &ModelConfig) -> Result<Arc<dyn Llm>> {
    use std::env;

    let model: Arc<dyn Llm> = match config.provider.to_lowercase().as_str() {
        "anthropic" => {
            use adk_model::anthropic::{AnthropicClient, AnthropicConfig};

            let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| {
                RalphError::Configuration("ANTHROPIC_API_KEY environment variable not set".into())
            })?;
            let anthropic_config = AnthropicConfig::new(api_key, &config.model_name);
            let client = AnthropicClient::new(anthropic_config).map_err(|e| RalphError::Model {
                provider: "anthropic".into(),
                message: e.to_string(),
            })?;
            Arc::new(client)
        }
        "openai" => {
            use adk_model::openai::{OpenAIClient, OpenAIConfig};

            let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
                RalphError::Configuration("OPENAI_API_KEY environment variable not set".into())
            })?;
            let openai_config = OpenAIConfig::new(api_key, &config.model_name);
            let client = OpenAIClient::new(openai_config).map_err(|e| RalphError::Model {
                provider: "openai".into(),
                message: e.to_string(),
            })?;
            Arc::new(client)
        }
        "gemini" => {
            use adk_model::gemini::GeminiModel;

            let api_key = env::var("GEMINI_API_KEY")
                .or_else(|_| env::var("GOOGLE_API_KEY"))
                .map_err(|_| {
                    RalphError::Configuration(
                        "GEMINI_API_KEY or GOOGLE_API_KEY environment variable not set".into(),
                    )
                })?;
            let client = GeminiModel::new(api_key, &config.model_name).map_err(|e| {
                RalphError::Model {
                    provider: "gemini".into(),
                    message: e.to_string(),
                }
            })?;
            Arc::new(client)
        }
        provider => {
            return Err(RalphError::Configuration(format!(
                "Unsupported model provider: {}. Supported: anthropic, openai, gemini",
                provider
            )));
        }
    };

    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_defaults() {
        let builder = RalphLoopAgentBuilder::default();
        assert!(builder.model.is_none());
        assert_eq!(builder.project_path, PathBuf::from("."));
        assert!(builder.additional_tools.is_empty());
    }

    #[test]
    fn test_instruction_content() {
        let instruction = RalphLoopAgent::instruction();

        // Verify key instruction elements
        assert!(instruction.contains("progress"));
        assert!(instruction.contains("tasks"));
        assert!(instruction.contains("test"));
        assert!(instruction.contains("file"));
        assert!(instruction.contains("git"));
        assert!(instruction.contains("exit_loop"));
        assert!(instruction.contains("one at a time"));
        assert!(instruction.contains("Test before commit"));
    }

    #[test]
    fn test_builder_fluent_api() {
        let builder = RalphLoopAgentBuilder::new()
            .project_path("/tmp/test")
            .instruction("Custom instruction");

        assert_eq!(builder.project_path, PathBuf::from("/tmp/test"));
        assert_eq!(
            builder.custom_instruction,
            Some("Custom instruction".to_string())
        );
    }
}
