//! Orchestrator Agent for Ralph interactive mode.
//!
//! The OrchestratorAgent is an LLM-powered agent that interprets user requests
//! and routes them to appropriate tools. It uses dynamic decision-making rather
//! than hardcoded intent classification.
//!
//! ## Requirements Validated
//!
//! - 2.1: THE Orchestrator_Agent SHALL be powered by an LLM that decides actions dynamically
//! - 2.2: THE Orchestrator_Agent SHALL NOT use hardcoded intent classification or fixed rules
//! - 2.3: THE Orchestrator_Agent SHALL have access to all required tools

use crate::models::{ModelConfig, RalphConfig};
use crate::tools::{
    AddFeatureTool, FileTool, GetTimeTool, GitTool, ProgressTool, RunPipelineTool,
    RunProjectTool, TaskTool, WebSearchTool,
};
use crate::{RalphError, Result};
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm, Tool};
use std::path::PathBuf;
use std::sync::Arc;

/// Instruction prompt for the Orchestrator Agent.
///
/// This prompt guides the LLM to intelligently route requests to appropriate tools
/// without hardcoded rules. The LLM reasons about user intent and chooses actions.
const ORCHESTRATOR_INSTRUCTION: &str = r#"You are Ralph, an intelligent development assistant that helps users create, modify, and run software projects through natural conversation.

## Your Capabilities

You have access to these tools:

### Project Creation & Management
- **run_pipeline**: Execute the full development pipeline (PRD → Design → Implementation). Use when the user wants to create a NEW project from scratch.
- **add_feature**: Add a feature to an existing project. Supports 'incremental' (quick) or 'pipeline' (full re-design) modes. Ask the user which mode they prefer for complex features.

### File Operations
- **file**: Read, write, list, or delete files in the project. Operations: read, write, list, delete.

### Task Management
- **tasks**: Manage implementation tasks. Operations: list, get_next, update_status, complete, get.

### Progress Tracking
- **progress**: Track learnings and completed work. Operations: read, append, summary.

### Version Control
- **git**: Git operations for version control. Operations: status, add, commit, diff.

### Project Execution
- **run_project**: Run or test the generated project. Automatically detects language (Rust, Go, Python, Node, etc.) and uses appropriate commands.

### Utilities
- **get_time**: Get the current date and time.
- **web_search**: Search the internet for information (placeholder - not yet fully implemented).

## Decision Guidelines

1. **New Project Requests**: When the user describes a new project to create (e.g., "create a todo app", "build a CLI calculator"), use `run_pipeline`.

2. **Feature Additions**: When the user wants to add a feature to an existing project:
   - For simple features: suggest 'incremental' mode
   - For complex features that affect architecture: suggest 'pipeline' mode
   - Ask the user if you're unsure which mode is appropriate

3. **Running/Testing**: When the user says "run it", "test it", "execute", use `run_project`.

4. **Status Queries**: When the user asks "what's left?", "show progress", "what tasks remain?", use `tasks` with operation 'list'.

5. **General Questions**: For questions about time, general knowledge, or Ralph's capabilities, respond conversationally or use appropriate tools.

6. **File Operations**: When the user wants to see, edit, or manage files, use the `file` tool.

7. **Git Operations**: When the user mentions commits, diffs, or version control, use the `git` tool.

8. **Compound Requests**: When the user makes a multi-step request (e.g., "add authentication and then implement it"), break it down and execute each step in sequence. Explain what you're doing at each step.

## Conversation Style

- Be helpful and conversational
- Explain what you're doing and why (e.g., "I'll use the run_pipeline tool to create your project...")
- Ask clarifying questions when the request is ambiguous
- Provide progress updates during long operations
- Offer suggestions based on project state
- Remember context from earlier in the conversation
- When taking significant actions, briefly explain your reasoning

## Handling Unclear Requests

When a request is unclear or could be interpreted multiple ways:
1. Ask a clarifying question before proceeding
2. Offer options when appropriate (e.g., "Would you like me to add this feature quickly, or do a full redesign?")
3. Make reasonable assumptions for minor ambiguities, but explain what you assumed

## Important Notes

- Always check if a project exists before trying to add features (look for prd.md, design.md, tasks.json)
- When running the pipeline, the user's description becomes the project specification
- For feature additions, assess complexity before recommending a mode
- Be proactive in offering help based on the current project state
- If a tool fails, explain what went wrong and suggest alternatives
"#;

/// Orchestrator Agent that routes user requests to appropriate tools.
///
/// This agent uses an LLM to dynamically decide which tools to invoke
/// based on user input, without hardcoded intent classification.
pub struct OrchestratorAgent {
    agent: Arc<dyn Agent + Send + Sync>,
    project_path: PathBuf,
    tools: Vec<Arc<dyn Tool>>,
}

impl std::fmt::Debug for OrchestratorAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestratorAgent")
            .field("name", &self.agent.name())
            .field("project_path", &self.project_path)
            .field("tools_count", &self.tools.len())
            .finish()
    }
}

impl OrchestratorAgent {
    /// Create a new builder for OrchestratorAgent.
    pub fn builder() -> OrchestratorAgentBuilder {
        OrchestratorAgentBuilder::default()
    }

    /// Get the instruction prompt.
    pub fn instruction() -> &'static str {
        ORCHESTRATOR_INSTRUCTION
    }

    /// Get the underlying agent for running.
    pub fn agent(&self) -> Arc<dyn Agent + Send + Sync> {
        self.agent.clone()
    }

    /// Get the project path.
    pub fn project_path(&self) -> &PathBuf {
        &self.project_path
    }

    /// Get the list of registered tools.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// Get the names of all registered tools.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// Check if a specific tool is registered.
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name() == name)
    }
}

/// Builder for creating an OrchestratorAgent with fluent API.
pub struct OrchestratorAgentBuilder {
    model: Option<Arc<dyn Llm>>,
    model_config: ModelConfig,
    project_path: PathBuf,
    config: Option<RalphConfig>,
    max_iterations: u32,
}

impl std::fmt::Debug for OrchestratorAgentBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestratorAgentBuilder")
            .field("model", &self.model.as_ref().map(|m| m.name()))
            .field("model_config", &self.model_config)
            .field("project_path", &self.project_path)
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

impl Default for OrchestratorAgentBuilder {
    fn default() -> Self {
        Self {
            model: None,
            model_config: ModelConfig::new("gemini", "gemini-2.5-flash-preview-05-20"),
            project_path: PathBuf::from("."),
            config: None,
            max_iterations: 50,
        }
    }
}

impl OrchestratorAgentBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the LLM model to use.
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the model configuration (used if model is not provided).
    pub fn model_config(mut self, config: ModelConfig) -> Self {
        self.model_config = config;
        self
    }

    /// Set the project path.
    pub fn project_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_path = path.into();
        self
    }

    /// Set the Ralph configuration (for pipeline tool).
    pub fn config(mut self, config: RalphConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the maximum number of LLM iterations.
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Build the OrchestratorAgent.
    pub async fn build(self) -> Result<OrchestratorAgent> {
        let model = match self.model {
            Some(m) => m,
            None => create_model_from_config(&self.model_config).await?,
        };

        let project_path = self.project_path.clone();

        // Create all tools
        let tools = create_tools(&project_path, self.config.clone())?;

        // Build the LlmAgent with all tools
        let mut builder = LlmAgentBuilder::new("orchestrator-agent")
            .description("Intelligent orchestrator that routes user requests to appropriate tools")
            .model(model)
            .instruction(ORCHESTRATOR_INSTRUCTION)
            .max_iterations(self.max_iterations);

        // Add all tools to the agent
        for tool in &tools {
            builder = builder.tool(tool.clone());
        }

        let agent = builder.build().map_err(|e| RalphError::Agent {
            agent: "orchestrator".to_string(),
            message: e.to_string(),
        })?;

        Ok(OrchestratorAgent {
            agent: Arc::new(agent),
            project_path,
            tools,
        })
    }
}

/// Create all tools for the orchestrator.
fn create_tools(
    project_path: &PathBuf,
    config: Option<RalphConfig>,
) -> Result<Vec<Arc<dyn Tool>>> {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

    // Pipeline tool - for creating new projects
    let ralph_config = config.unwrap_or_default();
    tools.push(Arc::new(RunPipelineTool::new(
        ralph_config.clone(),
        project_path.clone(),
    )));

    // Add feature tool - for incremental feature additions
    tools.push(Arc::new(AddFeatureTool::new(project_path.clone())));

    // File operations tool
    tools.push(Arc::new(FileTool::new(project_path.clone())));

    // Git operations tool
    tools.push(Arc::new(GitTool::new(project_path.clone())));

    // Task management tool
    let tasks_path = project_path.join(&ralph_config.tasks_path);
    tools.push(Arc::new(TaskTool::new(tasks_path)));

    // Progress tracking tool
    let progress_path = project_path.join(&ralph_config.progress_path);
    tools.push(Arc::new(ProgressTool::new(
        progress_path,
        "interactive-project",
    )));

    // Run project tool - for executing generated projects
    tools.push(Arc::new(RunProjectTool::new(project_path.clone())));

    // Time tool - for general queries
    tools.push(Arc::new(GetTimeTool::new()));

    // Web search tool (placeholder)
    tools.push(Arc::new(WebSearchTool::new()));

    Ok(tools)
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

/// List of required tool names for the orchestrator.
pub const REQUIRED_TOOLS: &[&str] = &[
    "run_pipeline",
    "add_feature",
    "file",
    "git",
    "tasks",
    "progress",
    "run_project",
    "get_time",
    "web_search",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orchestrator_instruction_content() {
        let instruction = OrchestratorAgent::instruction();
        assert!(instruction.contains("Ralph"));
        assert!(instruction.contains("run_pipeline"));
        assert!(instruction.contains("add_feature"));
        assert!(instruction.contains("run_project"));
        assert!(instruction.contains("get_time"));
    }

    #[test]
    fn test_orchestrator_builder_defaults() {
        let builder = OrchestratorAgentBuilder::default();
        assert!(builder.model.is_none());
        assert_eq!(builder.model_config.provider, "gemini");
        assert_eq!(builder.project_path, PathBuf::from("."));
        assert_eq!(builder.max_iterations, 50);
    }

    #[test]
    fn test_orchestrator_builder_fluent_api() {
        let config = ModelConfig::new("openai", "gpt-4o");
        let builder = OrchestratorAgentBuilder::new()
            .model_config(config)
            .project_path("/tmp/project")
            .max_iterations(100);

        assert_eq!(builder.model_config.provider, "openai");
        assert_eq!(builder.project_path, PathBuf::from("/tmp/project"));
        assert_eq!(builder.max_iterations, 100);
    }

    #[test]
    fn test_required_tools_list() {
        assert!(REQUIRED_TOOLS.contains(&"run_pipeline"));
        assert!(REQUIRED_TOOLS.contains(&"add_feature"));
        assert!(REQUIRED_TOOLS.contains(&"file"));
        assert!(REQUIRED_TOOLS.contains(&"git"));
        assert!(REQUIRED_TOOLS.contains(&"tasks"));
        assert!(REQUIRED_TOOLS.contains(&"progress"));
        assert!(REQUIRED_TOOLS.contains(&"run_project"));
        assert!(REQUIRED_TOOLS.contains(&"get_time"));
        assert!(REQUIRED_TOOLS.contains(&"web_search"));
    }

    #[test]
    fn test_create_tools() {
        let project_path = PathBuf::from("/tmp/test");
        let tools = create_tools(&project_path, None).unwrap();

        // Verify all required tools are created
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        
        for required in REQUIRED_TOOLS {
            assert!(
                tool_names.contains(required),
                "Missing required tool: {}",
                required
            );
        }
    }
}
