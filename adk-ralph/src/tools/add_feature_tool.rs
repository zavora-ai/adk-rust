//! Add Feature Tool for incrementally adding features to existing projects.
//!
//! This tool supports two modes:
//! - Incremental: Quick addition by updating PRD and adding tasks
//! - Pipeline: Full re-run of the design and implementation phases
//!
//! ## Requirements Validated
//!
//! - 2.3: THE Orchestrator_Agent SHALL have access to `add_feature` tool

use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

use crate::models::{PrdDocument, TaskList, UserStory, AcceptanceCriterion};

/// Mode for adding features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddFeatureMode {
    /// Quick addition: update PRD, add task, optionally implement
    Incremental,
    /// Full pipeline: re-run design and implementation phases
    Pipeline,
}

impl std::fmt::Display for AddFeatureMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddFeatureMode::Incremental => write!(f, "incremental"),
            AddFeatureMode::Pipeline => write!(f, "pipeline"),
        }
    }
}

/// Tool for adding features to an existing project.
///
/// Supports both incremental (quick) and pipeline (full) modes.
///
/// # Input
///
/// ```json
/// {
///     "feature": "Add user authentication",
///     "mode": "incremental",
///     "implement": true
/// }
/// ```
///
/// # Output
///
/// ```json
/// {
///     "success": true,
///     "mode": "incremental",
///     "tasks_added": 2,
///     "implemented": true,
///     "user_story_id": "US-005"
/// }
/// ```
pub struct AddFeatureTool {
    project_path: PathBuf,
}

impl AddFeatureTool {
    /// Create a new AddFeatureTool for the given project path.
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
        }
    }

    /// Get the path to the PRD file.
    fn prd_path(&self) -> PathBuf {
        self.project_path.join("prd.md")
    }

    /// Get the path to the tasks file.
    fn tasks_path(&self) -> PathBuf {
        self.project_path.join("tasks.json")
    }

    /// Load the existing PRD document.
    fn load_prd(&self) -> Result<PrdDocument> {
        let path = self.prd_path();
        if !path.exists() {
            return Err(AdkError::Tool(
                "No PRD found. Run the pipeline first to create a project.".to_string(),
            ));
        }
        
        PrdDocument::load_markdown(&path)
            .map_err(|e| AdkError::Tool(format!("Failed to load PRD: {}", e)))
    }

    /// Save the PRD document.
    fn save_prd(&self, prd: &PrdDocument) -> Result<()> {
        let path = self.prd_path();
        let markdown = prd.to_markdown();
        std::fs::write(&path, markdown)
            .map_err(|e| AdkError::Tool(format!("Failed to save PRD: {}", e)))
    }

    /// Load the existing task list.
    fn load_tasks(&self) -> Result<TaskList> {
        let path = self.tasks_path();
        if !path.exists() {
            return Err(AdkError::Tool(
                "No tasks found. Run the pipeline first to create a project.".to_string(),
            ));
        }
        
        TaskList::load(&path)
            .map_err(|e| AdkError::Tool(format!("Failed to load tasks: {}", e)))
    }

    /// Save the task list.
    fn save_tasks(&self, tasks: &TaskList) -> Result<()> {
        let path = self.tasks_path();
        tasks.save(&path)
            .map_err(|e| AdkError::Tool(format!("Failed to save tasks: {}", e)))
    }

    /// Generate the next user story ID.
    fn next_user_story_id(&self, prd: &PrdDocument) -> String {
        let max_id = prd.user_stories.iter()
            .filter_map(|s| {
                s.id.strip_prefix("US-")
                    .and_then(|n| n.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0);
        
        format!("US-{:03}", max_id + 1)
    }

    /// Generate the next task ID.
    fn next_task_id(&self, tasks: &TaskList) -> String {
        let max_id = tasks.tasks.iter()
            .filter_map(|t| {
                t.id.strip_prefix("T-")
                    .and_then(|n| n.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0);
        
        format!("T-{:03}", max_id + 1)
    }

    /// Add a feature incrementally (quick mode).
    async fn add_incremental(&self, feature: &str) -> Result<Value> {
        // Load existing PRD
        let mut prd = self.load_prd()?;
        
        // Create a new user story for the feature
        let story_id = self.next_user_story_id(&prd);
        let mut user_story = UserStory::new(
            &story_id,
            feature,
            &format!("As a user, I want {}, so that I can enhance my workflow.", feature.to_lowercase()),
            2, // Default priority
        );
        
        // Add basic acceptance criteria
        user_story.acceptance_criteria.push(AcceptanceCriterion::new(
            "1",
            &format!("WHEN the user requests {}, THE system SHALL provide the functionality", feature.to_lowercase()),
        ));
        
        prd.user_stories.push(user_story);
        
        // Save updated PRD
        self.save_prd(&prd)?;
        
        // Load and update tasks
        let mut tasks = self.load_tasks()?;
        let task_id = self.next_task_id(&tasks);
        
        // Create a new task for the feature
        let task = crate::models::Task::new(
            &task_id,
            format!("Implement: {}", feature),
            format!("Implement the {} feature as specified in {}", feature, story_id),
            2, // Default priority
        ).with_user_story(&story_id);
        
        tasks.tasks.push(task);
        
        // Save updated tasks
        self.save_tasks(&tasks)?;
        
        tracing::info!(
            feature = %feature,
            story_id = %story_id,
            task_id = %task_id,
            "Feature added incrementally"
        );
        
        Ok(json!({
            "success": true,
            "mode": "incremental",
            "tasks_added": 1,
            "implemented": false,
            "user_story_id": story_id,
            "task_id": task_id,
            "message": format!("Added feature '{}' as {} with task {}", feature, story_id, task_id)
        }))
    }

    /// Add a feature using the full pipeline (re-run design phase).
    async fn add_pipeline(&self, feature: &str) -> Result<Value> {
        // For pipeline mode, we update the PRD and signal that the design
        // phase should be re-run. The actual re-run would be handled by
        // the orchestrator calling run_pipeline or the architect agent.
        
        // Load existing PRD
        let mut prd = self.load_prd()?;
        
        // Create a new user story for the feature
        let story_id = self.next_user_story_id(&prd);
        let mut user_story = UserStory::new(
            &story_id,
            feature,
            &format!("As a user, I want {}, so that I can enhance my workflow.", feature.to_lowercase()),
            2, // Default priority
        );
        
        // Add basic acceptance criteria
        user_story.acceptance_criteria.push(AcceptanceCriterion::new(
            "1",
            &format!("WHEN the user requests {}, THE system SHALL provide the functionality", feature.to_lowercase()),
        ));
        
        prd.user_stories.push(user_story);
        
        // Save updated PRD
        self.save_prd(&prd)?;
        
        tracing::info!(
            feature = %feature,
            story_id = %story_id,
            "Feature added to PRD, pipeline mode requested"
        );
        
        Ok(json!({
            "success": true,
            "mode": "pipeline",
            "tasks_added": 0,
            "implemented": false,
            "user_story_id": story_id,
            "message": format!(
                "Added feature '{}' as {}. Re-run the design phase to generate tasks and implementation plan.",
                feature, story_id
            ),
            "next_action": "run_design_phase"
        }))
    }
}

impl std::fmt::Debug for AddFeatureTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddFeatureTool")
            .field("project_path", &self.project_path)
            .finish()
    }
}

#[async_trait]
impl Tool for AddFeatureTool {
    fn name(&self) -> &str {
        "add_feature"
    }

    fn description(&self) -> &str {
        "Add a new feature to an existing project. Supports 'incremental' mode (quick: update PRD and add task) or 'pipeline' mode (full: re-run design phase for comprehensive planning)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "feature": {
                    "type": "string",
                    "description": "Description of the feature to add (e.g., 'Add user authentication')"
                },
                "mode": {
                    "type": "string",
                    "enum": ["incremental", "pipeline"],
                    "description": "Mode for adding the feature: 'incremental' for quick addition, 'pipeline' for full re-design"
                },
                "implement": {
                    "type": "boolean",
                    "description": "Whether to immediately implement the feature (only for incremental mode)"
                }
            },
            "required": ["feature", "mode"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            feature: String,
            mode: AddFeatureMode,
            #[serde(default)]
            implement: bool,
        }

        let args: Args = serde_json::from_value(args)
            .map_err(|e| AdkError::Tool(format!("Invalid arguments: {}", e)))?;

        tracing::info!(
            feature = %args.feature,
            mode = %args.mode,
            implement = args.implement,
            "Adding feature"
        );

        match args.mode {
            AddFeatureMode::Incremental => self.add_incremental(&args.feature).await,
            AddFeatureMode::Pipeline => self.add_pipeline(&args.feature).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_feature_tool_name() {
        let tool = AddFeatureTool::new("/tmp/test");
        assert_eq!(tool.name(), "add_feature");
    }

    #[test]
    fn test_add_feature_tool_description() {
        let tool = AddFeatureTool::new("/tmp/test");
        assert!(tool.description().contains("feature"));
        assert!(tool.description().contains("incremental"));
        assert!(tool.description().contains("pipeline"));
    }

    #[test]
    fn test_add_feature_tool_schema() {
        let tool = AddFeatureTool::new("/tmp/test");
        let schema = tool.parameters_schema().unwrap();
        
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["feature"].is_object());
        assert!(schema["properties"]["mode"].is_object());
        assert!(schema["properties"]["implement"].is_object());
    }

    #[test]
    fn test_add_feature_mode_display() {
        assert_eq!(AddFeatureMode::Incremental.to_string(), "incremental");
        assert_eq!(AddFeatureMode::Pipeline.to_string(), "pipeline");
    }

    #[test]
    fn test_prd_path() {
        let tool = AddFeatureTool::new("/tmp/test");
        assert_eq!(tool.prd_path(), PathBuf::from("/tmp/test/prd.md"));
    }

    #[test]
    fn test_tasks_path() {
        let tool = AddFeatureTool::new("/tmp/test");
        assert_eq!(tool.tasks_path(), PathBuf::from("/tmp/test/tasks.json"));
    }
}
