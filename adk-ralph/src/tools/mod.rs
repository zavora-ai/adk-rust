//! Tools for the Ralph multi-agent autonomous development system.
//!
//! This module provides specialized tools for:
//! - File operations (read, write, list, delete)
//! - Git operations (status, add, commit, diff)
//! - Progress tracking (append-only log of learnings)
//! - Task management (priority-based selection with dependencies)
//! - Test execution (multi-language support)

pub mod file_tool;
pub mod git_tool;
pub mod progress_tool;
pub mod task_tool;
pub mod test_tool;

// Unified tools with operation-based interface
pub use file_tool::FileTool;
pub use git_tool::GitTool;

// Individual file tools (legacy)
pub use file_tool::{ListFilesTool, ReadFileTool, WriteFileTool};

// Core tools
pub use progress_tool::ProgressTool;
pub use task_tool::TaskTool;
pub use test_tool::TestTool;
