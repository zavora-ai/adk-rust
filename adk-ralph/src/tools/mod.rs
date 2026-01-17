//! Tools for the Ralph multi-agent autonomous development system.
//!
//! This module provides specialized tools for:
//! - File operations (read, write, list, delete)
//! - Git operations (status, add, commit, diff)
//! - Progress tracking (append-only log of learnings)
//! - Task management (priority-based selection with dependencies)
//! - Test execution (multi-language support)
//! - Pipeline execution (full PRD → Design → Implementation workflow)
//! - Project execution (run/test generated projects)
//! - Feature addition (incremental or pipeline mode)
//! - Time queries (current date/time)
//! - Web search (placeholder for future integration)

pub mod file_tool;
pub mod git_tool;
pub mod progress_tool;
pub mod task_tool;
pub mod test_tool;

// Interactive mode tools
pub mod add_feature_tool;
pub mod run_pipeline_tool;
pub mod run_project_tool;
pub mod time_tool;
pub mod web_search_tool;

// Unified tools with operation-based interface
pub use file_tool::FileTool;
pub use git_tool::GitTool;

// Individual file tools (legacy)
pub use file_tool::{ListFilesTool, ReadFileTool, WriteFileTool};

// Core tools
pub use progress_tool::ProgressTool;
pub use task_tool::TaskTool;
pub use test_tool::TestTool;

// Interactive mode tools
pub use add_feature_tool::{AddFeatureMode, AddFeatureTool};
pub use run_pipeline_tool::RunPipelineTool;
pub use run_project_tool::{Language, RunProjectTool};
pub use time_tool::GetTimeTool;
pub use web_search_tool::{SearchResult, WebSearchTool};
