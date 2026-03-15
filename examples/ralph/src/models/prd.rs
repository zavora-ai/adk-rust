//! PRD (Product Requirements Document) models

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

/// Product Requirements Document
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prd {
    pub project: String,
    pub branch_name: String,
    pub description: String,
    pub user_stories: Vec<UserStory>,
}

/// A single user story with acceptance criteria
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStory {
    pub id: String,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: u32,
    pub passes: bool,
    #[serde(default)]
    pub notes: String,
}

impl Prd {
    /// Load PRD from a JSON file
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let prd: Prd = serde_json::from_str(&content)?;
        Ok(prd)
    }

    /// Save PRD to a JSON file
    pub fn save(&self, path: &str) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    /// Get the next incomplete task by priority
    pub fn get_next_task(&self) -> Option<&UserStory> {
        self.user_stories.iter().filter(|story| !story.passes).min_by_key(|story| story.priority)
    }

    /// Mark a task as complete
    pub fn mark_complete(&mut self, task_id: &str) -> Result<()> {
        if let Some(story) = self.user_stories.iter_mut().find(|s| s.id == task_id) {
            story.passes = true;
        }
        Ok(())
    }

    /// Check if all tasks are complete
    pub fn is_complete(&self) -> bool {
        self.user_stories.iter().all(|story| story.passes)
    }

    /// Get completion statistics
    pub fn stats(&self) -> (usize, usize) {
        let complete = self.user_stories.iter().filter(|s| s.passes).count();
        let total = self.user_stories.len();
        (complete, total)
    }
}

impl UserStory {
    /// Convert to context string for worker agent
    #[allow(dead_code)]
    pub fn to_context(&self) -> String {
        let criteria = self
            .acceptance_criteria
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}", i + 1, c))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "Task ID: {}\nTitle: {}\nDescription: {}\n\nAcceptance Criteria:\n{}",
            self.id, self.title, self.description, criteria
        )
    }
}
