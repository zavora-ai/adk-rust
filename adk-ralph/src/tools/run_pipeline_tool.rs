//! Run Pipeline Tool for executing the full Ralph development pipeline.
//!
//! This tool wraps the existing RalphOrchestrator to allow the interactive
//! orchestrator agent to invoke the full PRD → Design → Implementation workflow.
//!
//! ## Requirements Validated
//!
//! - 2.3: THE Orchestrator_Agent SHALL have access to `run_pipeline` tool
//! - 2.1.1: WHEN the user describes a new project, THE Orchestrator_Agent SHALL invoke `run_pipeline`

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::models::RalphConfig;
use crate::orchestrator::{PipelinePhase, RalphOrchestrator};

/// Tool for running the full Ralph development pipeline.
///
/// This tool wraps the RalphOrchestrator and executes the complete
/// PRD → Design → Implementation workflow.
///
/// # Input
///
/// ```json
/// {
///     "description": "Create a CLI calculator in Rust"
/// }
/// ```
///
/// # Output
///
/// ```json
/// {
///     "success": true,
///     "phase": "Complete",
///     "tasks_completed": 5,
///     "files_created": ["prd.md", "design.md", "tasks.json", "src/main.rs"]
/// }
/// ```
pub struct RunPipelineTool {
    config: RalphConfig,
    project_path: PathBuf,
    /// Shared orchestrator state for resuming
    orchestrator: Arc<RwLock<Option<RalphOrchestrator>>>,
}

impl RunPipelineTool {
    /// Create a new RunPipelineTool with the given configuration.
    pub fn new(config: RalphConfig, project_path: impl Into<PathBuf>) -> Self {
        Self {
            config,
            project_path: project_path.into(),
            orchestrator: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a new RunPipelineTool with default configuration.
    pub fn with_defaults(project_path: impl Into<PathBuf>) -> Self {
        Self::new(RalphConfig::default(), project_path)
    }

    /// Get or create the orchestrator instance.
    async fn get_or_create_orchestrator(&self) -> Result<RalphOrchestrator> {
        let mut guard = self.orchestrator.write().await;
        
        if guard.is_none() {
            let mut config = self.config.clone();
            config.project_path = self.project_path.to_string_lossy().to_string();
            
            let orchestrator = RalphOrchestrator::new(config)
                .map_err(|e| AdkError::Tool(format!("Failed to create orchestrator: {}", e)))?;
            
            *guard = Some(orchestrator);
        }
        
        // Clone the orchestrator for use (we need to return ownership)
        // Since RalphOrchestrator doesn't implement Clone, we create a new one
        let mut config = self.config.clone();
        config.project_path = self.project_path.to_string_lossy().to_string();
        
        RalphOrchestrator::new(config)
            .map_err(|e| AdkError::Tool(format!("Failed to create orchestrator: {}", e)))
    }
}

impl std::fmt::Debug for RunPipelineTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPipelineTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for RunPipelineTool {
    fn name(&self) -> &str {
        "run_pipeline"
    }

    fn description(&self) -> &str {
        "Execute the full Ralph development pipeline: PRD generation → Design & task breakdown → Implementation. Use this when the user wants to create a new project from scratch."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Project description or prompt (e.g., 'Create a CLI calculator in Rust')"
                }
            },
            "required": ["description"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            description: String,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        tracing::info!(
            description = %args.description,
            project_path = %self.project_path.display(),
            "Starting pipeline execution"
        );

        // Create and run the orchestrator
        let mut orchestrator = self.get_or_create_orchestrator().await?;
        
        let status = orchestrator.run(&args.description).await
            .map_err(|e| AdkError::Tool(format!("Pipeline execution failed: {}", e)))?;

        // Collect information about created files
        let files_created = collect_created_files(&self.project_path);
        
        // Extract completion details
        let (tasks_completed, message) = match &status {
            crate::agents::CompletionStatus::Complete { tasks_completed, message, .. } => {
                (*tasks_completed, message.clone())
            }
            crate::agents::CompletionStatus::MaxIterationsReached { tasks_completed, tasks_remaining, .. } => {
                (*tasks_completed, format!("{} tasks remaining", tasks_remaining))
            }
            crate::agents::CompletionStatus::AllTasksBlocked { tasks_completed, reason, .. } => {
                (*tasks_completed, format!("Blocked: {}", reason))
            }
        };

        let phase = orchestrator.phase();

        tracing::info!(
            phase = %phase,
            tasks_completed = tasks_completed,
            "Pipeline execution complete"
        );

        Ok(json!({
            "success": phase == PipelinePhase::Complete,
            "phase": phase.to_string(),
            "tasks_completed": tasks_completed,
            "message": message,
            "files_created": files_created,
            "project_path": self.project_path.display().to_string()
        }))
    }
}

/// Collect a list of files created in the project directory.
fn collect_created_files(project_path: &PathBuf) -> Vec<String> {
    let mut files = Vec::new();
    
    // Check for standard Ralph files
    let standard_files = ["prd.md", "design.md", "tasks.json", "progress.txt"];
    for file in &standard_files {
        if project_path.join(file).exists() {
            files.push(file.to_string());
        }
    }
    
    // Check for src directory
    let src_path = project_path.join("src");
    if src_path.exists() {
        if let Ok(entries) = std::fs::read_dir(&src_path) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name().to_string_lossy().to_string();
                files.push(format!("src/{}", name));
            }
        }
    }
    
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_pipeline_tool_name() {
        let tool = RunPipelineTool::with_defaults("/tmp/test");
        assert_eq!(tool.name(), "run_pipeline");
    }

    #[test]
    fn test_run_pipeline_tool_description() {
        let tool = RunPipelineTool::with_defaults("/tmp/test");
        assert!(tool.description().contains("pipeline"));
        assert!(tool.description().contains("PRD"));
    }

    #[test]
    fn test_run_pipeline_tool_schema() {
        let tool = RunPipelineTool::with_defaults("/tmp/test");
        let schema = tool.parameters_schema().unwrap();
        
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["description"].is_object());
        assert!(schema["required"].as_array().unwrap().contains(&json!("description")));
    }

    #[test]
    fn test_collect_created_files_empty() {
        let temp_dir = std::env::temp_dir().join("ralph_test_empty");
        let _ = std::fs::create_dir_all(&temp_dir);
        
        let files = collect_created_files(&temp_dir);
        // Should be empty or only contain files that exist
        assert!(files.is_empty() || files.iter().all(|f| temp_dir.join(f).exists()));
        
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
