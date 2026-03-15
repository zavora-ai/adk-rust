//! Worker Agent - Task executor for Ralph
//!
//! Note: WorkerAgentBuilder is provided for future multi-agent implementations
//! where the loop agent delegates to specialized worker agents.

#![allow(dead_code)]

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Tool};
use adk_model::GeminiModel;
use anyhow::Result;
use std::sync::Arc;

const WORKER_INSTRUCTION: &str = r#"
# Worker Agent Instructions

You are a worker agent responsible for completing ONE user story.

## Your Process

1. **Analyze Task**: Understand requirements and acceptance criteria
2. **Implement**: Make necessary code changes using the file tool
3. **Verify**: Run quality_check with check_type "all"
4. **Commit**: Add and commit changes if checks pass
5. **Report**: Provide a summary of what was done

## Critical Rules

- Focus ONLY on the current task
- ALWAYS run quality_check before committing
- NEVER commit if checks fail
- Write clear, descriptive commit messages
- Document any important discoveries

## Available Tools

- `file`: Read, write, append files
- `git`: Git operations  
- `quality_check`: Run tests and checks
- `prd_manager`: Check task details

## Success Criteria

A task is complete when:
1. All acceptance criteria are met
2. All quality checks pass
3. Changes are committed to git

When task is complete, call the exit_loop tool with a summary.
"#;

/// Builder for creating worker agents
pub struct WorkerAgentBuilder {
    api_key: String,
    model_name: String,
    tools: Vec<Arc<dyn Tool>>,
}

impl WorkerAgentBuilder {
    pub fn new(api_key: &str, model_name: &str) -> Self {
        Self { api_key: api_key.to_string(), model_name: model_name.to_string(), tools: Vec::new() }
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn build(self, task_context: &str) -> Result<Arc<dyn Agent>> {
        let instruction = format!("{}\n\n## Current Task\n{}", WORKER_INSTRUCTION, task_context);

        let model = GeminiModel::new(&self.api_key, &self.model_name)?;

        let mut builder = LlmAgentBuilder::new("ralph_worker")
            .description("Executes individual PRD tasks")
            .instruction(instruction)
            .model(Arc::new(model));

        for tool in self.tools {
            builder = builder.tool(tool);
        }

        // Add exit loop tool for completion
        builder = builder.tool(Arc::new(adk_tool::ExitLoopTool::new()));

        Ok(Arc::new(builder.build()?))
    }
}
