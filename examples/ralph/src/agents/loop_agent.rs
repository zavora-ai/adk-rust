//! Loop Agent - Main orchestrator for Ralph

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Tool};
use adk_model::GeminiModel;
use anyhow::Result;
use std::sync::Arc;

const LOOP_INSTRUCTION: &str = r#"
# Loop Agent - Ralph Orchestrator

You orchestrate PRD task completion. Work efficiently within each iteration.

## Each Iteration (do this quickly)

1. Call `prd_manager` with action "get_stats" to check progress
2. If all complete, call `exit_loop` with a success message
3. Otherwise, call `prd_manager` with action "get_next_task"
4. For the task, call `prd_manager` with action "mark_complete" and the task_id
5. Call `exit_loop` with status update

IMPORTANT: Complete each task iteration quickly. The loop will continue automatically.

## Example Flow

1. prd_manager(get_stats) -> see 0/3 complete
2. prd_manager(get_next_task) -> get US-001
3. prd_manager(mark_complete, task_id="US-001") -> mark it done
4. exit_loop("Completed US-001, continuing...") -> next iteration starts

Keep it simple - mark tasks complete and move on. The loop handles repetition.
"#;

/// Create the loop agent (main orchestrator)
pub fn create_loop_agent(
    api_key: &str,
    model_name: &str,
    tools: Vec<Arc<dyn Tool>>,
) -> Result<Arc<dyn Agent>> {
    let model = GeminiModel::new(api_key, model_name)?;

    let mut builder = LlmAgentBuilder::new("ralph_loop")
        .description("Main orchestrator that coordinates PRD task completion")
        .instruction(LOOP_INSTRUCTION)
        .model(Arc::new(model));

    for tool in tools {
        builder = builder.tool(tool);
    }

    // Add exit loop tool for completion
    builder = builder.tool(Arc::new(adk_tool::ExitLoopTool::new()));

    Ok(Arc::new(builder.build()?))
}
