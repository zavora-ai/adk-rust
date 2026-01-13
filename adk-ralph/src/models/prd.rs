//! PRD (Product Requirements Document) data structures.
//!
//! This module provides the core data models for managing requirements through a structured PRD,
//! including user stories with acceptance criteria and priority tracking.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Status of a user story.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UserStoryStatus {
    /// Not yet started
    #[default]
    Pending,
    /// Currently being implemented
    InProgress,
    /// Implementation complete, tests passing
    Passing,
    /// Implementation failed or blocked
    Failed,
}

impl std::fmt::Display for UserStoryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserStoryStatus::Pending => write!(f, "pending"),
            UserStoryStatus::InProgress => write!(f, "in_progress"),
            UserStoryStatus::Passing => write!(f, "passing"),
            UserStoryStatus::Failed => write!(f, "failed"),
        }
    }
}

/// An acceptance criterion for a user story.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptanceCriterion {
    /// Unique identifier within the user story (e.g., "1", "2")
    pub id: String,
    /// The criterion text in EARS format
    pub criterion: String,
    /// Whether this criterion has been verified
    pub verified: bool,
}

impl AcceptanceCriterion {
    /// Create a new acceptance criterion.
    pub fn new(id: impl Into<String>, criterion: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            criterion: criterion.into(),
            verified: false,
        }
    }

    /// Mark this criterion as verified.
    pub fn verify(&mut self) {
        self.verified = true;
    }
}

/// A user story representing a requirement in the PRD.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserStory {
    /// Unique identifier for the user story (e.g., "US-001")
    pub id: String,
    /// Short title describing the requirement
    pub title: String,
    /// User story description in "As a... I want... So that..." format
    pub description: String,
    /// List of acceptance criteria that must be met
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    /// Priority level (1=highest, 5=lowest)
    pub priority: u32,
    /// Current status of the user story
    pub status: UserStoryStatus,
    /// Additional notes or learnings from implementation
    #[serde(default)]
    pub notes: String,
}

impl UserStory {
    /// Create a new user story with the given parameters.
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
            acceptance_criteria: Vec::new(),
            priority,
            status: UserStoryStatus::Pending,
            notes: String::new(),
        }
    }

    /// Add an acceptance criterion to this user story.
    pub fn add_criterion(&mut self, criterion: AcceptanceCriterion) {
        self.acceptance_criteria.push(criterion);
    }

    /// Add a simple criterion string (auto-generates ID).
    pub fn add_criterion_text(&mut self, criterion: impl Into<String>) {
        let id = (self.acceptance_criteria.len() + 1).to_string();
        self.acceptance_criteria
            .push(AcceptanceCriterion::new(id, criterion));
    }

    /// Validate the user story data.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("User story ID cannot be empty".to_string());
        }
        if self.title.is_empty() {
            return Err(format!("User story {} has empty title", self.id));
        }
        if self.description.is_empty() {
            return Err(format!("User story {} has empty description", self.id));
        }
        if self.acceptance_criteria.is_empty() {
            return Err(format!("User story {} has no acceptance criteria", self.id));
        }
        if self.priority == 0 || self.priority > 5 {
            return Err(format!(
                "User story {} has invalid priority {} (must be 1-5)",
                self.id, self.priority
            ));
        }
        Ok(())
    }

    /// Check if this user story is complete (passing).
    pub fn is_complete(&self) -> bool {
        self.status == UserStoryStatus::Passing
    }

    /// Check if this user story is pending.
    pub fn is_pending(&self) -> bool {
        self.status == UserStoryStatus::Pending
    }

    /// Mark this user story as passing.
    pub fn mark_passing(&mut self) {
        self.status = UserStoryStatus::Passing;
    }

    /// Mark this user story as in progress.
    pub fn mark_in_progress(&mut self) {
        self.status = UserStoryStatus::InProgress;
    }

    /// Mark this user story as failed.
    pub fn mark_failed(&mut self) {
        self.status = UserStoryStatus::Failed;
    }

    /// Add a note to this user story.
    pub fn add_note(&mut self, note: &str) {
        if !self.notes.is_empty() {
            self.notes.push('\n');
        }
        self.notes.push_str(note);
    }

    /// Convert this user story to a context string for the LLM.
    pub fn to_context(&self) -> String {
        let criteria = self
            .acceptance_criteria
            .iter()
            .map(|c| format!("{}. {}", c.id, c.criterion))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "**User Story ID**: {}\n**Title**: {}\n**Description**: {}\n**Priority**: {}\n**Status**: {}\n\n**Acceptance Criteria**:\n{}",
            self.id, self.title, self.description, self.priority, self.status, criteria
        )
    }
}

/// Product Requirements Document containing project information and user stories.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrdDocument {
    /// Project name
    pub project: String,
    /// Project overview/scope description
    pub overview: String,
    /// Target programming language (if specified)
    #[serde(default)]
    pub language: Option<String>,
    /// List of user stories/requirements
    pub user_stories: Vec<UserStory>,
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

impl PrdDocument {
    /// Create a new PRD document.
    pub fn new(project: impl Into<String>, overview: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            overview: overview.into(),
            language: None,
            user_stories: Vec::new(),
            version: default_version(),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
            updated_at: None,
        }
    }

    /// Load a PRD from a JSON file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read PRD file '{}': {}", path.display(), e))?;

        let prd: PrdDocument = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse PRD JSON '{}': {}", path.display(), e))?;

        prd.validate()?;
        Ok(prd)
    }

    /// Load a PRD from a Markdown file.
    pub fn load_markdown<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read PRD file '{}': {}", path.display(), e))?;

        Self::parse_markdown(&content)
    }

    /// Parse PRD from markdown content.
    pub fn parse_markdown(content: &str) -> Result<Self, String> {
        // Basic markdown parsing - can be enhanced later
        let mut prd = PrdDocument::new("", "");

        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            // Parse project name from title
            if line.starts_with("# ") && prd.project.is_empty() {
                prd.project = line[2..].trim().to_string();
            }
            // Parse overview section
            else if line.starts_with("## Project Overview") || line.starts_with("## Overview") {
                i += 1;
                let mut overview = String::new();
                while i < lines.len() && !lines[i].trim().starts_with("## ") {
                    if !lines[i].trim().is_empty() {
                        if !overview.is_empty() {
                            overview.push(' ');
                        }
                        overview.push_str(lines[i].trim());
                    }
                    i += 1;
                }
                prd.overview = overview;
                continue;
            }
            // Parse user stories
            else if line.starts_with("### US-") || line.starts_with("### Requirement") {
                let story = Self::parse_user_story(&lines, &mut i)?;
                prd.user_stories.push(story);
                continue;
            }

            i += 1;
        }

        if prd.project.is_empty() {
            return Err("PRD must have a project name".to_string());
        }

        prd.validate()?;
        Ok(prd)
    }

    /// Parse a single user story from markdown lines.
    fn parse_user_story(lines: &[&str], i: &mut usize) -> Result<UserStory, String> {
        let header = lines[*i].trim();
        let id = header
            .split(':')
            .next()
            .unwrap_or("")
            .trim_start_matches("### ")
            .trim()
            .to_string();

        let title = header
            .split(':')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string();

        *i += 1;
        let mut description = String::new();
        let mut criteria = Vec::new();
        let mut priority = 3u32; // Default priority

        while *i < lines.len() && !lines[*i].trim().starts_with("### ") {
            let line = lines[*i].trim();

            if line.starts_with("**Priority**:") {
                if let Some(p) = line.split(':').nth(1) {
                    priority = p.trim().parse().unwrap_or(3);
                }
            } else if line.starts_with("**Description**:") || line.starts_with("**User Story**:") {
                if let Some(desc) = line.split(':').nth(1) {
                    description = desc.trim().to_string();
                }
            } else if line.starts_with("As a ") {
                description = line.to_string();
            } else if line.starts_with("#### Acceptance Criteria") {
                *i += 1;
                while *i < lines.len()
                    && !lines[*i].trim().starts_with("### ")
                    && !lines[*i].trim().starts_with("#### ")
                {
                    let crit_line = lines[*i].trim();
                    if crit_line.starts_with("1.") || crit_line.starts_with("- ") {
                        let crit_text = crit_line
                            .trim_start_matches(|c: char| c.is_numeric() || c == '.' || c == '-')
                            .trim();
                        if !crit_text.is_empty() {
                            criteria.push(AcceptanceCriterion::new(
                                (criteria.len() + 1).to_string(),
                                crit_text,
                            ));
                        }
                    }
                    *i += 1;
                }
                continue;
            }

            *i += 1;
        }

        let mut story = UserStory::new(id, title, description, priority);
        story.acceptance_criteria = criteria;
        Ok(story)
    }

    /// Save the PRD to a JSON file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let path = path.as_ref();
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize PRD: {}", e))?;

        fs::write(path, content)
            .map_err(|e| format!("Failed to write PRD file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Save the PRD to a Markdown file.
    pub fn save_markdown<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let content = self.to_markdown();
        let path = path.as_ref();

        fs::write(path, content)
            .map_err(|e| format!("Failed to write PRD file '{}': {}", path.display(), e))?;

        Ok(())
    }

    /// Convert PRD to markdown format.
    pub fn to_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str(&format!("# {}\n\n", self.project));
        md.push_str("## Project Overview\n\n");
        md.push_str(&self.overview);
        md.push_str("\n\n");

        if let Some(lang) = &self.language {
            md.push_str(&format!("**Target Language**: {}\n\n", lang));
        }

        md.push_str("## User Stories\n\n");

        for story in &self.user_stories {
            md.push_str(&format!("### {}: {}\n\n", story.id, story.title));
            md.push_str(&format!("**Priority**: {}\n", story.priority));
            md.push_str(&format!("**Status**: {}\n\n", story.status));
            md.push_str(&format!("**User Story**: {}\n\n", story.description));
            md.push_str("#### Acceptance Criteria\n\n");

            for criterion in &story.acceptance_criteria {
                let check = if criterion.verified { "x" } else { " " };
                md.push_str(&format!("- [{}] {}. {}\n", check, criterion.id, criterion.criterion));
            }

            if !story.notes.is_empty() {
                md.push_str(&format!("\n**Notes**: {}\n", story.notes));
            }

            md.push('\n');
        }

        md
    }

    /// Validate the PRD data.
    pub fn validate(&self) -> Result<(), String> {
        if self.project.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        if self.overview.is_empty() {
            return Err("Project overview cannot be empty".to_string());
        }
        if self.user_stories.is_empty() {
            return Err("PRD must have at least one user story".to_string());
        }

        for story in &self.user_stories {
            story.validate()?;
        }

        // Check for duplicate IDs
        let mut ids = HashSet::new();
        for story in &self.user_stories {
            if !ids.insert(&story.id) {
                return Err(format!("Duplicate user story ID: {}", story.id));
            }
        }

        Ok(())
    }

    /// Add a user story to the PRD.
    pub fn add_user_story(&mut self, story: UserStory) {
        self.user_stories.push(story);
        self.updated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Get the next pending user story by priority.
    pub fn get_next_pending(&self) -> Option<&UserStory> {
        self.user_stories
            .iter()
            .filter(|s| s.is_pending())
            .min_by_key(|s| s.priority)
    }

    /// Get a mutable reference to a user story by ID.
    pub fn get_story_mut(&mut self, id: &str) -> Option<&mut UserStory> {
        self.user_stories.iter_mut().find(|s| s.id == id)
    }

    /// Get a reference to a user story by ID.
    pub fn get_story(&self, id: &str) -> Option<&UserStory> {
        self.user_stories.iter().find(|s| s.id == id)
    }

    /// Mark a user story as passing by ID.
    pub fn mark_story_passing(&mut self, id: &str) -> Result<(), String> {
        match self.get_story_mut(id) {
            Some(story) => {
                story.mark_passing();
                self.updated_at = Some(chrono::Utc::now().to_rfc3339());
                Ok(())
            }
            None => Err(format!("User story not found: {}", id)),
        }
    }

    /// Get completion statistics.
    pub fn get_stats(&self) -> PrdStats {
        let total = self.user_stories.len();
        let completed = self
            .user_stories
            .iter()
            .filter(|s| s.is_complete())
            .count();
        let in_progress = self
            .user_stories
            .iter()
            .filter(|s| s.status == UserStoryStatus::InProgress)
            .count();
        let failed = self
            .user_stories
            .iter()
            .filter(|s| s.status == UserStoryStatus::Failed)
            .count();
        let remaining = total - completed - failed;
        let completion_rate = if total > 0 {
            (completed as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        PrdStats {
            total,
            completed,
            in_progress,
            failed,
            remaining,
            completion_rate,
        }
    }

    /// Check if all user stories are complete.
    pub fn is_complete(&self) -> bool {
        self.user_stories.iter().all(|s| s.is_complete())
    }
}

/// Statistics about PRD completion status.
#[derive(Debug, Clone, PartialEq)]
pub struct PrdStats {
    /// Total number of user stories
    pub total: usize,
    /// Number of completed (passing) user stories
    pub completed: usize,
    /// Number of in-progress user stories
    pub in_progress: usize,
    /// Number of failed user stories
    pub failed: usize,
    /// Number of remaining (pending) user stories
    pub remaining: usize,
    /// Completion rate as a percentage (0.0 - 100.0)
    pub completion_rate: f64,
}

impl std::fmt::Display for PrdStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Progress: {}/{} completed ({:.1}%), {} in progress, {} remaining",
            self.completed, self.total, self.completion_rate, self.in_progress, self.remaining
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_story_creation() {
        let story = UserStory::new("US-001", "Test Story", "As a user, I want to test", 1);
        assert_eq!(story.id, "US-001");
        assert_eq!(story.priority, 1);
        assert!(story.is_pending());
    }

    #[test]
    fn test_prd_stats() {
        let mut prd = PrdDocument::new("Test", "Test project");
        let mut story1 = UserStory::new("US-001", "Story 1", "Description 1", 1);
        story1.add_criterion_text("Criterion 1");
        story1.mark_passing();

        let mut story2 = UserStory::new("US-002", "Story 2", "Description 2", 2);
        story2.add_criterion_text("Criterion 2");

        prd.add_user_story(story1);
        prd.add_user_story(story2);

        let stats = prd.get_stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.remaining, 1);
    }
}
