//! A coding-agent harness built on [`LlmAgent`].
//!
//! [`CodingAgent`] wires the [`adk_devtools`] developer toolset, a planning
//! [`TodoTool`], and a minimal coding system prompt into a ready-to-run agent —
//! the implementation of §7.2 of `docs/design/coding-agent.md`.
//!
//! ```rust,ignore
//! use adk_agent::coding::CodingAgent;
//! use adk_devtools::Workspace;
//!
//! let agent = CodingAgent::builder()
//!     .model(model)
//!     .workspace(Workspace::new("./my-repo"))
//!     .build()?;
//! // agent.agent() -> Arc<dyn Agent> for a Runner
//! ```

mod todo;

pub use todo::{TodoItem, TodoTool};

use std::sync::Arc;

use adk_core::{Agent, Llm, Result, Tool};
use adk_devtools::{DevToolset, Workspace};

use crate::llm_agent::{LlmAgent, LlmAgentBuilder};

/// The minimal coding system prompt (kept small; capabilities come from tools
/// and, later, lazily-loaded skills — see the design doc).
pub const CODING_SYSTEM_PROMPT: &str = "\
You are a precise software engineering agent working inside a sandboxed workspace.

Tools:
- read_file/glob/grep to explore before you change anything.
- write_file/edit_file to make changes. You must read a file before editing it.
- bash to build, run tests, and run commands (it runs in the workspace root).
- write_todos to track a multi-step plan; keep it updated as you work.

Operating rules:
- For non-trivial tasks, first write_todos with a short plan, then execute it.
- Make the smallest change that solves the task; verify by running the build/tests.
- Never invent file contents — read first. Report what you actually did.
- When the task is complete and verified, summarize the changes concisely.";

/// A configured coding agent: an [`LlmAgent`] plus a shared handle to its todo
/// list.
pub struct CodingAgent {
    agent: Arc<LlmAgent>,
    todos: TodoTool,
}

impl CodingAgent {
    /// Start building a coding agent.
    pub fn builder() -> CodingAgentBuilder {
        CodingAgentBuilder::new()
    }

    /// The underlying agent, ready to hand to a `Runner`.
    pub fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    /// Consume the wrapper and return the underlying agent.
    pub fn into_agent(self) -> Arc<dyn Agent> {
        self.agent
    }

    /// A snapshot of the current todo list (as the model has set it).
    pub fn todos(&self) -> Vec<TodoItem> {
        self.todos.items()
    }
}

/// Builder for [`CodingAgent`].
pub struct CodingAgentBuilder {
    name: String,
    model: Option<Arc<dyn Llm>>,
    workspace: Option<Workspace>,
    instruction: Option<String>,
    include_todo: bool,
    extra_tools: Vec<Arc<dyn Tool>>,
}

impl CodingAgentBuilder {
    /// Create a builder with default settings.
    pub fn new() -> Self {
        Self {
            name: "coding-agent".to_string(),
            model: None,
            workspace: None,
            instruction: None,
            include_todo: true,
            extra_tools: Vec::new(),
        }
    }

    /// Set the agent name (default `"coding-agent"`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the model (required).
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the workspace the agent operates in (required).
    pub fn workspace(mut self, workspace: Workspace) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Append extra guidance to the base coding prompt (e.g. project conventions).
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Disable the planning `write_todos` tool (enabled by default).
    pub fn without_todos(mut self) -> Self {
        self.include_todo = false;
        self
    }

    /// Register an additional tool (e.g. an MCP or function tool).
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.extra_tools.push(tool);
        self
    }

    /// Build the coding agent.
    ///
    /// # Errors
    /// Returns a config error if the model or workspace was not set.
    pub fn build(self) -> Result<CodingAgent> {
        let model =
            self.model.ok_or_else(|| adk_core::AdkError::config("CodingAgent requires a model"))?;
        let workspace = self
            .workspace
            .ok_or_else(|| adk_core::AdkError::config("CodingAgent requires a workspace"))?;

        let instruction = match &self.instruction {
            Some(extra) => format!("{CODING_SYSTEM_PROMPT}\n\n{extra}"),
            None => CODING_SYSTEM_PROMPT.to_string(),
        };

        let mut builder = LlmAgentBuilder::new(self.name)
            .model(model)
            .description(
                "A coding agent that reads, edits, and runs code in a sandboxed workspace.",
            )
            .instruction(instruction)
            .toolset(Arc::new(DevToolset::new(workspace)));

        let todos = TodoTool::new();
        if self.include_todo {
            builder = builder.tool(Arc::new(todos.clone()));
        }
        for tool in self.extra_tools {
            builder = builder.tool(tool);
        }

        let agent = builder.build()?;
        Ok(CodingAgent { agent: Arc::new(agent), todos })
    }
}

impl Default for CodingAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}
