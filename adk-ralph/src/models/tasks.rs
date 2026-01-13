//! Task list data structures.
//!
//! This module provides data models for structured task management,
//! including tasks with priorities, dependencies, status tracking,
//! and organization into sprints and phases.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Status of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not yet started
    #[default]
    Pending,
    /// Currently being worked on
    InProgress,
    /// Successfully completed
    Completed,
    /// Blocked by dependencies or issues
    Blocked,
    /// Skipped (optional task not implemented)
    Skipped,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Blocked => write!(f, "blocked"),
            TaskStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl TaskStatus {
    /// Check if this status represents a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Blocked | TaskStatus::Skipped
        )
    }

    /// Check if this status allows the task to be selected for work.
    pub fn is_workable(&self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::InProgress)
    }
}

/// Complexity estimate for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskComplexity {
    /// Simple task, quick to implement
    Low,
    /// Average complexity
    #[default]
    Medium,
    /// Complex task, requires significant effort
    High,
}

impl std::fmt::Display for TaskComplexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskComplexity::Low => write!(f, "low"),
            TaskComplexity::Medium => write!(f, "medium"),
            TaskComplexity::High => write!(f, "high"),
        }
    }
}

/// A single task in the task list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    /// Unique identifier (e.g., "TASK-001")
    pub id: String,
    /// Short title describing the task
    pub title: String,
    /// Detailed description of what needs to be done
    pub description: String,
    /// Priority level (1=highest, 5=lowest)
    pub priority: u32,
    /// Current status
    pub status: TaskStatus,
    /// IDs of tasks that must be completed before this one
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Reference to the user story this task implements
    #[serde(default)]
    pub user_story_id: Option<String>,
    /// Estimated complexity
    #[serde(default)]
    pub estimated_complexity: TaskComplexity,
    /// Files created by this task
    #[serde(default)]
    pub files_created: Vec<String>,
    /// Files modified by this task
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Git commit hash when completed
    #[serde(default)]
    pub commit_hash: Option<String>,
    /// Number of attempts made on this task
    #[serde(default)]
    pub attempts: u32,
    /// Notes or learnings from implementation
    #[serde(default)]
    pub notes: String,
}

impl Task {
    /// Create a new task.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        priority: u32,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            priority,
            status: TaskStatus::Pending,
            dependencies: Vec::new(),
            user_story_id: None,
            estimated_complexity: TaskComplexity::Medium,
            files_created: Vec::new(),
            files_modified: Vec::new(),
            commit_hash: None,
            attempts: 0,
            notes: String::new(),
        }
    }

    /// Add a dependency.
    pub fn add_dependency(&mut self, dep_id: impl Into<String>) {
        self.dependencies.push(dep_id.into());
    }

    /// Set the user story reference.
    pub fn with_user_story(mut self, story_id: impl Into<String>) -> Self {
        self.user_story_id = Some(story_id.into());
        self
    }

    /// Set the complexity.
    pub fn with_complexity(mut self, complexity: TaskComplexity) -> Self {
        self.estimated_complexity = complexity;
        self
    }

    /// Check if this task is pending.
    pub fn is_pending(&self) -> bool {
        self.status == TaskStatus::Pending
    }

    /// Check if this task is completed.
    pub fn is_completed(&self) -> bool {
        self.status == TaskStatus::Completed
    }

    /// Check if this task is blocked.
    pub fn is_blocked(&self) -> bool {
        self.status == TaskStatus::Blocked
    }

    /// Mark this task as in progress.
    pub fn start(&mut self) {
        self.status = TaskStatus::InProgress;
        self.attempts += 1;
    }

    /// Mark this task as completed.
    pub fn complete(&mut self, commit_hash: Option<String>) {
        self.status = TaskStatus::Completed;
        self.commit_hash = commit_hash;
    }

    /// Mark this task as blocked.
    pub fn block(&mut self, reason: &str) {
        self.status = TaskStatus::Blocked;
        self.add_note(&format!("Blocked: {}", reason));
    }

    /// Add a note to this task.
    pub fn add_note(&mut self, note: &str) {
        if !self.notes.is_empty() {
            self.notes.push('\n');
        }
        self.notes.push_str(note);
    }

    /// Record a file created by this task.
    pub fn add_file_created(&mut self, path: impl Into<String>) {
        self.files_created.push(path.into());
    }

    /// Record a file modified by this task.
    pub fn add_file_modified(&mut self, path: impl Into<String>) {
        self.files_modified.push(path.into());
    }

    /// Convert to context string for LLM.
    pub fn to_context(&self) -> String {
        let deps = if self.dependencies.is_empty() {
            "None".to_string()
        } else {
            self.dependencies.join(", ")
        };

        format!(
            "**Task ID**: {}\n**Title**: {}\n**Description**: {}\n**Priority**: {}\n**Status**: {}\n**Dependencies**: {}\n**Complexity**: {}",
            self.id, self.title, self.description, self.priority, self.status, deps, self.estimated_complexity
        )
    }
}

/// A sprint grouping related tasks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sprint {
    /// Sprint identifier (e.g., "sprint-1")
    pub id: String,
    /// Sprint name
    pub name: String,
    /// Tasks in this sprint
    pub tasks: Vec<Task>,
}

impl Sprint {
    /// Create a new sprint.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            tasks: Vec::new(),
        }
    }

    /// Add a task to this sprint.
    pub fn add_task(&mut self, task: Task) {
        self.tasks.push(task);
    }

    /// Get all pending tasks in this sprint.
    pub fn get_pending_tasks(&self) -> Vec<&Task> {
        self.tasks.iter().filter(|t| t.is_pending()).collect()
    }

    /// Check if all tasks in this sprint are completed.
    pub fn is_complete(&self) -> bool {
        self.tasks.iter().all(|t| t.status.is_terminal())
    }

    /// Get completion percentage.
    pub fn completion_percentage(&self) -> f64 {
        if self.tasks.is_empty() {
            return 100.0;
        }
        let completed = self.tasks.iter().filter(|t| t.is_completed()).count();
        (completed as f64 / self.tasks.len() as f64) * 100.0
    }
}

/// A phase grouping multiple sprints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Phase {
    /// Phase identifier (e.g., "phase-1")
    pub id: String,
    /// Phase name
    pub name: String,
    /// Sprints in this phase
    pub sprints: Vec<Sprint>,
}

impl Phase {
    /// Create a new phase.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            sprints: Vec::new(),
        }
    }

    /// Add a sprint to this phase.
    pub fn add_sprint(&mut self, sprint: Sprint) {
        self.sprints.push(sprint);
    }

    /// Get all tasks in this phase.
    pub fn get_all_tasks(&self) -> Vec<&Task> {
        self.sprints.iter().flat_map(|s| s.tasks.iter()).collect()
    }

    /// Check if all sprints in this phase are complete.
    pub fn is_complete(&self) -> bool {
        self.sprints.iter().all(|s| s.is_complete())
    }
}

/// Complete task list for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskList {
    /// Project name (should match PRD and Design)
    pub project: String,
    /// Target programming language
    pub language: String,
    /// Phases containing sprints and tasks
    #[serde(default)]
    pub phases: Vec<Phase>,
    /// Flat list of tasks (alternative to phases for simple projects)
    #[serde(default)]
    pub tasks: Vec<Task>,
    /// Document version
    #[serde(default = "default_version")]
    pub version: String,
    /// Creation timestamp
    #[serde(default)]
    pub created_at: Option<String>,
    /// Last update timestamp
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_version() -> String {
    "1.0".to_string()
}

impl TaskList {
    /// Create a new task list.
    pub fn new(project: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            language: language.into(),
            phases: Vec::new(),
            tasks: Vec::new(),
            version: default_version(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            updated_at: None,
        }
    }

    /// Load a task list from a JSON file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read tasks file '{}': {}", path.display(), e))?;

        let tasks: TaskList = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse tasks JSON '{}': {}", path.display(), e))?;

        tasks.validate()?;
        Ok(tasks)
    }

    /// Save the task list to a JSON file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize tasks: {}", e))?;

        fs::write(path, content)
            .map_err(|e| format!("Failed to write tasks file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Validate the task list.
    pub fn validate(&self) -> Result<(), String> {
        if self.project.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        if self.language.is_empty() {
            return Err("Language cannot be empty".to_string());
        }

        // Check for duplicate task IDs
        let mut ids = HashSet::new();
        for task in self.get_all_tasks() {
            if !ids.insert(&task.id) {
                return Err(format!("Duplicate task ID: {}", task.id));
            }
        }

        // Validate dependencies exist
        let all_ids: HashSet<_> = self.get_all_tasks().iter().map(|t| &t.id).collect();
        for task in self.get_all_tasks() {
            for dep in &task.dependencies {
                if !all_ids.contains(dep) {
                    return Err(format!(
                        "Task {} has unknown dependency: {}",
                        task.id, dep
                    ));
                }
            }
        }

        Ok(())
    }

    /// Add a phase.
    pub fn add_phase(&mut self, phase: Phase) {
        self.phases.push(phase);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Add a task to the flat list.
    pub fn add_task(&mut self, task: Task) {
        self.tasks.push(task);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Get all tasks (from both phases and flat list).
    pub fn get_all_tasks(&self) -> Vec<&Task> {
        let mut all_tasks: Vec<&Task> = self.tasks.iter().collect();
        for phase in &self.phases {
            for sprint in &phase.sprints {
                all_tasks.extend(sprint.tasks.iter());
            }
        }
        all_tasks
    }

    /// Get all tasks mutably.
    pub fn get_all_tasks_mut(&mut self) -> Vec<&mut Task> {
        let mut all_tasks: Vec<&mut Task> = self.tasks.iter_mut().collect();
        for phase in &mut self.phases {
            for sprint in &mut phase.sprints {
                all_tasks.extend(sprint.tasks.iter_mut());
            }
        }
        all_tasks
    }

    /// Get a task by ID.
    pub fn get_task(&self, id: &str) -> Option<&Task> {
        self.get_all_tasks().into_iter().find(|t| t.id == id)
    }

    /// Get a mutable task by ID.
    pub fn get_task_mut(&mut self, id: &str) -> Option<&mut Task> {
        // First check flat tasks
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            return Some(task);
        }
        // Then check phases
        for phase in &mut self.phases {
            for sprint in &mut phase.sprints {
                if let Some(task) = sprint.tasks.iter_mut().find(|t| t.id == id) {
                    return Some(task);
                }
            }
        }
        None
    }

    /// Get the next task to work on based on priority and dependencies.
    pub fn get_next_task(&self) -> Option<&Task> {
        let completed_ids: HashSet<_> = self
            .get_all_tasks()
            .iter()
            .filter(|t| t.is_completed())
            .map(|t| t.id.as_str())
            .collect();

        self.get_all_tasks()
            .into_iter()
            .filter(|t| t.is_pending())
            .filter(|t| {
                // All dependencies must be completed
                t.dependencies
                    .iter()
                    .all(|dep| completed_ids.contains(dep.as_str()))
            })
            .min_by_key(|t| t.priority)
    }

    /// Update task status by ID.
    pub fn update_task_status(&mut self, id: &str, status: TaskStatus) -> Result<(), String> {
        match self.get_task_mut(id) {
            Some(task) => {
                task.status = status;
                self.updated_at = Some(chrono::Utc::now().to_rfc3339());
                Ok(())
            }
            None => Err(format!("Task not found: {}", id)),
        }
    }

    /// Mark a task as completed.
    pub fn complete_task(&mut self, id: &str, commit_hash: Option<String>) -> Result<(), String> {
        match self.get_task_mut(id) {
            Some(task) => {
                task.complete(commit_hash);
                self.updated_at = Some(chrono::Utc::now().to_rfc3339());
                Ok(())
            }
            None => Err(format!("Task not found: {}", id)),
        }
    }

    /// Get task statistics.
    pub fn get_stats(&self) -> TaskStats {
        let all_tasks = self.get_all_tasks();
        let total = all_tasks.len();

        let mut by_status: HashMap<TaskStatus, usize> = HashMap::new();
        for task in &all_tasks {
            *by_status.entry(task.status).or_insert(0) += 1;
        }

        let completed = *by_status.get(&TaskStatus::Completed).unwrap_or(&0);
        let in_progress = *by_status.get(&TaskStatus::InProgress).unwrap_or(&0);
        let blocked = *by_status.get(&TaskStatus::Blocked).unwrap_or(&0);
        let pending = *by_status.get(&TaskStatus::Pending).unwrap_or(&0);

        let completion_rate = if total > 0 {
            (completed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        TaskStats {
            total,
            completed,
            in_progress,
            blocked,
            pending,
            completion_rate,
        }
    }

    /// Check if all tasks are completed.
    pub fn is_complete(&self) -> bool {
        self.get_all_tasks()
            .iter()
            .all(|t| t.status.is_terminal())
    }
}

/// Statistics about task completion.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStats {
    /// Total number of tasks
    pub total: usize,
    /// Number of completed tasks
    pub completed: usize,
    /// Number of in-progress tasks
    pub in_progress: usize,
    /// Number of blocked tasks
    pub blocked: usize,
    /// Number of pending tasks
    pub pending: usize,
    /// Completion rate as percentage
    pub completion_rate: f64,
}

impl std::fmt::Display for TaskStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Tasks: {}/{} completed ({:.1}%), {} in progress, {} pending, {} blocked",
            self.completed,
            self.total,
            self.completion_rate,
            self.in_progress,
            self.pending,
            self.blocked
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_creation() {
        let task = Task::new("TASK-001", "Test Task", "Test description", 1);
        assert_eq!(task.id, "TASK-001");
        assert!(task.is_pending());
        assert!(!task.is_completed());
    }

    #[test]
    fn test_task_lifecycle() {
        let mut task = Task::new("TASK-001", "Test", "Desc", 1);
        assert!(task.is_pending());

        task.start();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.attempts, 1);

        task.complete(Some("abc123".to_string()));
        assert!(task.is_completed());
        assert_eq!(task.commit_hash, Some("abc123".to_string()));
    }

    #[test]
    fn test_task_list_next_task() {
        let mut list = TaskList::new("Test", "rust");

        let task1 = Task::new("TASK-001", "First", "Desc", 2);
        let task2 = Task::new("TASK-002", "Second", "Desc", 1);
        let mut task3 = Task::new("TASK-003", "Third", "Desc", 1);
        task3.add_dependency("TASK-001");

        list.add_task(task1);
        list.add_task(task2);
        list.add_task(task3);

        // Should get TASK-002 (priority 1, no deps)
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-002");
    }

    #[test]
    fn test_task_list_dependency_check() {
        let mut list = TaskList::new("Test", "rust");

        let task1 = Task::new("TASK-001", "First", "Desc", 1);
        let mut task2 = Task::new("TASK-002", "Second", "Desc", 1);
        task2.add_dependency("TASK-001");

        list.add_task(task1);
        list.add_task(task2);

        // TASK-002 should not be selectable until TASK-001 is done
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-001");

        // Complete TASK-001
        list.complete_task("TASK-001", None).unwrap();

        // Now TASK-002 should be selectable
        let next = list.get_next_task().unwrap();
        assert_eq!(next.id, "TASK-002");
    }
}
