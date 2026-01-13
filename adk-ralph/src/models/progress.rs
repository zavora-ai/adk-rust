//! Progress log data structures.
//!
//! This module provides data models for tracking progress, learnings,
//! and gotchas discovered during implementation. The progress log is
//! append-only to preserve history.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Test execution results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TestResults {
    /// Number of tests passed
    pub passed: usize,
    /// Number of tests failed
    pub failed: usize,
    /// Number of tests skipped
    pub skipped: usize,
    /// Total test duration in milliseconds
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

impl TestResults {
    /// Create new test results.
    pub fn new(passed: usize, failed: usize, skipped: usize) -> Self {
        Self {
            passed,
            failed,
            skipped,
            duration_ms: None,
        }
    }

    /// Check if all tests passed.
    pub fn all_passed(&self) -> bool {
        self.failed == 0
    }

    /// Get total number of tests.
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }
}

impl std::fmt::Display for TestResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} passed, {} failed, {} skipped",
            self.passed, self.failed, self.skipped
        )
    }
}

/// A single progress entry recording work on a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressEntry {
    /// Task ID that was worked on
    pub task_id: String,
    /// Task title for reference
    pub title: String,
    /// Iteration number when this was completed
    pub iteration: u32,
    /// Timestamp when completed
    pub completed_at: String,
    /// Description of the approach taken
    pub approach: String,
    /// Lessons learned during implementation
    #[serde(default)]
    pub learnings: Vec<String>,
    /// Gotchas or pitfalls discovered
    #[serde(default)]
    pub gotchas: Vec<String>,
    /// Files created during this task
    #[serde(default)]
    pub files_created: Vec<String>,
    /// Files modified during this task
    #[serde(default)]
    pub files_modified: Vec<String>,
    /// Test results if tests were run
    #[serde(default)]
    pub test_results: Option<TestResults>,
    /// Git commit hash if committed
    #[serde(default)]
    pub commit_hash: Option<String>,
}

impl ProgressEntry {
    /// Create a new progress entry.
    pub fn new(
        task_id: impl Into<String>,
        title: impl Into<String>,
        iteration: u32,
        approach: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            title: title.into(),
            iteration,
            completed_at: chrono::Utc::now().to_rfc3339(),
            approach: approach.into(),
            learnings: Vec::new(),
            gotchas: Vec::new(),
            files_created: Vec::new(),
            files_modified: Vec::new(),
            test_results: None,
            commit_hash: None,
        }
    }

    /// Add a learning.
    pub fn add_learning(&mut self, learning: impl Into<String>) {
        self.learnings.push(learning.into());
    }

    /// Add a gotcha.
    pub fn add_gotcha(&mut self, gotcha: impl Into<String>) {
        self.gotchas.push(gotcha.into());
    }

    /// Add a file created.
    pub fn add_file_created(&mut self, path: impl Into<String>) {
        self.files_created.push(path.into());
    }

    /// Add a file modified.
    pub fn add_file_modified(&mut self, path: impl Into<String>) {
        self.files_modified.push(path.into());
    }

    /// Set test results.
    pub fn with_test_results(mut self, results: TestResults) -> Self {
        self.test_results = Some(results);
        self
    }

    /// Set commit hash.
    pub fn with_commit(mut self, hash: impl Into<String>) -> Self {
        self.commit_hash = Some(hash.into());
        self
    }

    /// Convert to context string for LLM.
    pub fn to_context(&self) -> String {
        let mut ctx = format!(
            "**Task**: {} - {}\n**Approach**: {}\n",
            self.task_id, self.title, self.approach
        );

        if !self.learnings.is_empty() {
            ctx.push_str("**Learnings**:\n");
            for learning in &self.learnings {
                ctx.push_str(&format!("- {}\n", learning));
            }
        }

        if !self.gotchas.is_empty() {
            ctx.push_str("**Gotchas**:\n");
            for gotcha in &self.gotchas {
                ctx.push_str(&format!("- {}\n", gotcha));
            }
        }

        ctx
    }
}

/// Summary statistics for the progress log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProgressSummary {
    /// Total tasks completed
    pub tasks_completed: usize,
    /// Total tasks remaining
    pub tasks_remaining: usize,
    /// Total commits made
    pub total_commits: usize,
    /// Total files created
    pub total_files_created: usize,
    /// Total files modified
    pub total_files_modified: usize,
    /// Total tests passed
    pub total_tests_passed: usize,
    /// Total tests failed
    pub total_tests_failed: usize,
}

impl ProgressSummary {
    /// Create a new summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update summary from entries.
    pub fn from_entries(entries: &[ProgressEntry], tasks_remaining: usize) -> Self {
        let mut summary = Self::new();
        summary.tasks_completed = entries.len();
        summary.tasks_remaining = tasks_remaining;

        for entry in entries {
            if entry.commit_hash.is_some() {
                summary.total_commits += 1;
            }
            summary.total_files_created += entry.files_created.len();
            summary.total_files_modified += entry.files_modified.len();

            if let Some(results) = &entry.test_results {
                summary.total_tests_passed += results.passed;
                summary.total_tests_failed += results.failed;
            }
        }

        summary
    }
}

impl std::fmt::Display for ProgressSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} tasks completed, {} remaining, {} commits, {} files created, {} modified",
            self.tasks_completed,
            self.tasks_remaining,
            self.total_commits,
            self.total_files_created,
            self.total_files_modified
        )
    }
}

/// Complete progress log for a project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressLog {
    /// Project name (should match PRD, Design, Tasks)
    pub project: String,
    /// When the project was started
    pub started_at: String,
    /// When the log was last updated
    pub last_updated: String,
    /// Total iterations executed
    pub total_iterations: u32,
    /// Progress entries (append-only)
    pub entries: Vec<ProgressEntry>,
    /// Summary statistics
    #[serde(default)]
    pub summary: ProgressSummary,
}

impl ProgressLog {
    /// Create a new progress log.
    pub fn new(project: impl Into<String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            project: project.into(),
            started_at: now.clone(),
            last_updated: now,
            total_iterations: 0,
            entries: Vec::new(),
            summary: ProgressSummary::new(),
        }
    }

    /// Load a progress log from a JSON file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read progress file '{}': {}", path.display(), e))?;

        let log: ProgressLog = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse progress JSON '{}': {}", path.display(), e))?;

        Ok(log)
    }

    /// Load or create a progress log.
    pub fn load_or_create<P: AsRef<Path>>(path: P, project: &str) -> Result<Self, String> {
        let path = path.as_ref();
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::new(project))
        }
    }

    /// Save the progress log to a JSON file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize progress: {}", e))?;

        fs::write(path, content)
            .map_err(|e| format!("Failed to write progress file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Append a new entry to the log.
    /// This is the primary way to add entries - the log is append-only.
    pub fn append(&mut self, entry: ProgressEntry) {
        self.entries.push(entry);
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Increment the iteration counter.
    pub fn increment_iteration(&mut self) {
        self.total_iterations += 1;
        self.last_updated = chrono::Utc::now().to_rfc3339();
    }

    /// Update the summary statistics.
    pub fn update_summary(&mut self, tasks_remaining: usize) {
        self.summary = ProgressSummary::from_entries(&self.entries, tasks_remaining);
    }

    /// Get the number of entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Get the most recent entry.
    pub fn last_entry(&self) -> Option<&ProgressEntry> {
        self.entries.last()
    }

    /// Get all learnings from all entries.
    pub fn get_all_learnings(&self) -> Vec<&str> {
        self.entries
            .iter()
            .flat_map(|e| e.learnings.iter().map(|s| s.as_str()))
            .collect()
    }

    /// Get all gotchas from all entries.
    pub fn get_all_gotchas(&self) -> Vec<&str> {
        self.entries
            .iter()
            .flat_map(|e| e.gotchas.iter().map(|s| s.as_str()))
            .collect()
    }

    /// Get entries for a specific task.
    pub fn get_entries_for_task(&self, task_id: &str) -> Vec<&ProgressEntry> {
        self.entries
            .iter()
            .filter(|e| e.task_id == task_id)
            .collect()
    }

    /// Convert to context string for LLM (recent entries only).
    pub fn to_context(&self, max_entries: usize) -> String {
        let mut ctx = format!(
            "# Progress Log\n\n**Project**: {}\n**Iterations**: {}\n**Tasks Completed**: {}\n\n",
            self.project,
            self.total_iterations,
            self.entries.len()
        );

        // Include recent learnings
        let all_learnings = self.get_all_learnings();
        if !all_learnings.is_empty() {
            ctx.push_str("## Key Learnings\n\n");
            for learning in all_learnings.iter().rev().take(10) {
                ctx.push_str(&format!("- {}\n", learning));
            }
            ctx.push('\n');
        }

        // Include recent gotchas
        let all_gotchas = self.get_all_gotchas();
        if !all_gotchas.is_empty() {
            ctx.push_str("## Gotchas to Remember\n\n");
            for gotcha in all_gotchas.iter().rev().take(5) {
                ctx.push_str(&format!("- {}\n", gotcha));
            }
            ctx.push('\n');
        }

        // Include recent entries
        ctx.push_str("## Recent Work\n\n");
        for entry in self.entries.iter().rev().take(max_entries) {
            ctx.push_str(&entry.to_context());
            ctx.push('\n');
        }

        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_entry_creation() {
        let mut entry = ProgressEntry::new("TASK-001", "Test Task", 1, "Used TDD approach");
        entry.add_learning("Always write tests first");
        entry.add_gotcha("Watch out for async issues");

        assert_eq!(entry.task_id, "TASK-001");
        assert_eq!(entry.learnings.len(), 1);
        assert_eq!(entry.gotchas.len(), 1);
    }

    #[test]
    fn test_progress_log_append_only() {
        let mut log = ProgressLog::new("Test Project");
        let initial_count = log.entry_count();

        let entry1 = ProgressEntry::new("TASK-001", "First", 1, "Approach 1");
        log.append(entry1);
        assert_eq!(log.entry_count(), initial_count + 1);

        let entry2 = ProgressEntry::new("TASK-002", "Second", 2, "Approach 2");
        log.append(entry2);
        assert_eq!(log.entry_count(), initial_count + 2);

        // Entries should be in order
        assert_eq!(log.entries[0].task_id, "TASK-001");
        assert_eq!(log.entries[1].task_id, "TASK-002");
    }

    #[test]
    fn test_test_results() {
        let results = TestResults::new(10, 2, 1);
        assert!(!results.all_passed());
        assert_eq!(results.total(), 13);

        let passing = TestResults::new(10, 0, 0);
        assert!(passing.all_passed());
    }

    #[test]
    fn test_progress_summary() {
        let mut log = ProgressLog::new("Test");

        let mut entry = ProgressEntry::new("TASK-001", "Test", 1, "Approach");
        entry.files_created = vec!["file1.rs".to_string()];
        entry.test_results = Some(TestResults::new(5, 0, 0));
        entry.commit_hash = Some("abc123".to_string());
        log.append(entry);

        log.update_summary(3);

        assert_eq!(log.summary.tasks_completed, 1);
        assert_eq!(log.summary.tasks_remaining, 3);
        assert_eq!(log.summary.total_commits, 1);
        assert_eq!(log.summary.total_files_created, 1);
        assert_eq!(log.summary.total_tests_passed, 5);
    }
}
