//! Session management for Ralph interactive mode.
//!
//! This module provides session state management including:
//! - Conversation history tracking
//! - Project context awareness
//! - User preferences storage
//! - Session persistence (save/load)
//! - History truncation for token limits

use crate::{PipelinePhase, RalphError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Default session file path within the project directory.
pub const DEFAULT_SESSION_PATH: &str = ".ralph/session.json";

/// Default maximum tokens for conversation history before truncation.
pub const DEFAULT_MAX_HISTORY_TOKENS: usize = 100_000;

/// Approximate tokens per character (conservative estimate).
const TOKENS_PER_CHAR: f32 = 0.25;

/// A message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// Role of the message sender ("user" or "assistant")
    pub role: String,
    /// Content of the message
    pub content: String,
    /// Timestamp when the message was created
    pub timestamp: DateTime<Utc>,
}

impl Message {
    /// Create a new message.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
        }
    }

    /// Create a user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    /// Create an assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }

    /// Estimate the token count for this message.
    pub fn estimate_tokens(&self) -> usize {
        // Rough estimate: ~4 chars per token on average
        let content_tokens = (self.content.len() as f32 * TOKENS_PER_CHAR) as usize;
        let role_tokens = (self.role.len() as f32 * TOKENS_PER_CHAR) as usize;
        content_tokens + role_tokens + 4 // Add overhead for message structure
    }
}

/// Context about the current project state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProjectContext {
    /// Path to the project directory
    pub project_path: PathBuf,
    /// Whether a PRD document exists
    pub has_prd: bool,
    /// Whether a design document exists
    pub has_design: bool,
    /// Whether a tasks file exists
    pub has_tasks: bool,
    /// Current pipeline phase (if in progress)
    pub current_phase: Option<PipelinePhase>,
    /// Detected project language
    pub language: Option<String>,
}

impl ProjectContext {
    /// Create a new project context for the given path.
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        Self {
            project_path: project_path.into(),
            ..Default::default()
        }
    }

    /// Refresh the context by checking for existing files.
    pub fn refresh(&mut self, prd_path: &str, design_path: &str, tasks_path: &str) {
        let base = &self.project_path;
        self.has_prd = base.join(prd_path).exists();
        self.has_design = base.join(design_path).exists();
        self.has_tasks = base.join(tasks_path).exists();
        self.language = self.detect_language();
    }

    /// Detect the project language from common files.
    fn detect_language(&self) -> Option<String> {
        let base = &self.project_path;
        
        if base.join("Cargo.toml").exists() {
            Some("rust".to_string())
        } else if base.join("go.mod").exists() {
            Some("go".to_string())
        } else if base.join("package.json").exists() {
            Some("javascript".to_string())
        } else if base.join("requirements.txt").exists() || base.join("pyproject.toml").exists() {
            Some("python".to_string())
        } else if base.join("pom.xml").exists() || base.join("build.gradle").exists() {
            Some("java".to_string())
        } else {
            None
        }
    }

    /// Get a summary of the project state.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        
        parts.push(format!("Project: {}", self.project_path.display()));
        
        if let Some(ref lang) = self.language {
            parts.push(format!("Language: {}", lang));
        }
        
        let artifacts: Vec<&str> = [
            (self.has_prd, "PRD"),
            (self.has_design, "Design"),
            (self.has_tasks, "Tasks"),
        ]
        .iter()
        .filter_map(|(exists, name)| if *exists { Some(*name) } else { None })
        .collect();
        
        if !artifacts.is_empty() {
            parts.push(format!("Artifacts: {}", artifacts.join(", ")));
        }
        
        if let Some(ref phase) = self.current_phase {
            parts.push(format!("Phase: {}", phase));
        }
        
        parts.join("\n")
    }
}


/// Session state for interactive mode.
///
/// Maintains conversation history, project context, and user preferences
/// across multiple interactions. Can be persisted to disk and restored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// Unique session identifier
    pub id: String,
    /// Conversation history
    pub conversation_history: Vec<Message>,
    /// Current project context
    pub project_context: ProjectContext,
    /// User preferences (key-value pairs)
    pub user_preferences: HashMap<String, String>,
    /// When the session was created
    pub created_at: DateTime<Utc>,
    /// When the session was last updated
    pub updated_at: DateTime<Utc>,
}

impl Session {
    /// Create a new session for the given project path.
    pub fn new(project_path: impl Into<PathBuf>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            conversation_history: Vec::new(),
            project_context: ProjectContext::new(project_path),
            user_preferences: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a message to the conversation history.
    pub fn add_message(&mut self, role: &str, content: &str) {
        self.conversation_history.push(Message::new(role, content));
        self.updated_at = Utc::now();
    }

    /// Add a user message to the conversation history.
    pub fn add_user_message(&mut self, content: &str) {
        self.add_message("user", content);
    }

    /// Add an assistant message to the conversation history.
    pub fn add_assistant_message(&mut self, content: &str) {
        self.add_message("assistant", content);
    }

    /// Get a context summary for the LLM.
    ///
    /// Returns a string summarizing the current session state including
    /// project context and recent conversation highlights.
    pub fn get_context_summary(&self) -> String {
        let mut summary = Vec::new();
        
        // Add project context
        summary.push(self.project_context.summary());
        
        // Add conversation summary if there's history
        if !self.conversation_history.is_empty() {
            let msg_count = self.conversation_history.len();
            summary.push(format!("\nConversation: {} messages", msg_count));
            
            // Include last few messages for context
            let recent_count = 3.min(msg_count);
            if recent_count > 0 {
                summary.push("Recent messages:".to_string());
                for msg in self.conversation_history.iter().rev().take(recent_count).rev() {
                    let preview = if msg.content.len() > 100 {
                        format!("{}...", &msg.content[..100])
                    } else {
                        msg.content.clone()
                    };
                    summary.push(format!("  {}: {}", msg.role, preview));
                }
            }
        }
        
        // Add relevant preferences
        if !self.user_preferences.is_empty() {
            summary.push("\nPreferences:".to_string());
            for (key, value) in &self.user_preferences {
                summary.push(format!("  {}: {}", key, value));
            }
        }
        
        summary.join("\n")
    }

    /// Get a user preference value.
    pub fn get_preference(&self, key: &str) -> Option<&String> {
        self.user_preferences.get(key)
    }

    /// Set a user preference value.
    pub fn set_preference(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.user_preferences.insert(key.into(), value.into());
        self.updated_at = Utc::now();
    }

    /// Remove a user preference.
    pub fn remove_preference(&mut self, key: &str) -> Option<String> {
        let result = self.user_preferences.remove(key);
        if result.is_some() {
            self.updated_at = Utc::now();
        }
        result
    }

    /// Estimate total tokens in conversation history.
    pub fn estimate_history_tokens(&self) -> usize {
        self.conversation_history.iter().map(|m| m.estimate_tokens()).sum()
    }

    /// Truncate conversation history to fit within token limit.
    ///
    /// Preserves the most recent messages while staying under the limit.
    /// Returns the number of messages removed.
    pub fn truncate_history(&mut self, max_tokens: usize) -> usize {
        let initial_count = self.conversation_history.len();
        
        // Calculate current token usage
        let mut total_tokens = self.estimate_history_tokens();
        
        // Remove oldest messages until we're under the limit
        while total_tokens > max_tokens && !self.conversation_history.is_empty() {
            if let Some(removed) = self.conversation_history.first() {
                total_tokens = total_tokens.saturating_sub(removed.estimate_tokens());
            }
            self.conversation_history.remove(0);
        }
        
        let removed_count = initial_count - self.conversation_history.len();
        if removed_count > 0 {
            self.updated_at = Utc::now();
        }
        
        removed_count
    }

    /// Clear all conversation history.
    pub fn clear_history(&mut self) {
        self.conversation_history.clear();
        self.updated_at = Utc::now();
    }

    /// Get the session file path for a project.
    pub fn session_path(project_path: &Path) -> PathBuf {
        project_path.join(DEFAULT_SESSION_PATH)
    }

    /// Save the session to a file.
    ///
    /// Creates the `.ralph` directory if it doesn't exist.
    pub fn save(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                RalphError::file(path.display().to_string(), format!("Failed to create directory: {}", e))
            })?;
        }

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).map_err(|e| {
            RalphError::file(path.display().to_string(), format!("Failed to write session: {}", e))
        })?;

        Ok(())
    }

    /// Load a session from a file.
    ///
    /// Returns an error if the file doesn't exist or is corrupted.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            RalphError::file(path.display().to_string(), format!("Failed to read session: {}", e))
        })?;

        let session: Session = serde_json::from_str(&content).map_err(|e| {
            RalphError::Serialization(format!(
                "Failed to parse session file '{}': {}. The file may be corrupted.",
                path.display(),
                e
            ))
        })?;

        Ok(session)
    }

    /// Try to load a session, returning None if it doesn't exist or is corrupted.
    ///
    /// This is useful for graceful fallback to a new session.
    pub fn try_load(path: &Path) -> Option<Self> {
        Self::load(path).ok()
    }

    /// Save the session to the default location within the project.
    pub fn save_to_project(&self) -> Result<()> {
        let path = Self::session_path(&self.project_context.project_path);
        self.save(&path)
    }

    /// Load a session from the default location within a project.
    pub fn load_from_project(project_path: &Path) -> Result<Self> {
        let path = Self::session_path(project_path);
        Self::load(&path)
    }

    /// Try to load a session from the default location, returning None if not found.
    pub fn try_load_from_project(project_path: &Path) -> Option<Self> {
        let path = Self::session_path(project_path);
        Self::try_load(&path)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new(".")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_message_creation() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.content, "Hello");

        let msg = Message::assistant("Hi there!");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "Hi there!");
    }

    #[test]
    fn test_message_token_estimation() {
        let msg = Message::user("Hello world");
        let tokens = msg.estimate_tokens();
        // Should be reasonable estimate (not zero, not huge)
        assert!(tokens > 0);
        assert!(tokens < 100);
    }

    #[test]
    fn test_session_creation() {
        let session = Session::new("/test/project");
        assert!(!session.id.is_empty());
        assert!(session.conversation_history.is_empty());
        assert_eq!(session.project_context.project_path, PathBuf::from("/test/project"));
    }

    #[test]
    fn test_session_add_message() {
        let mut session = Session::new("/test");
        session.add_user_message("Hello");
        session.add_assistant_message("Hi!");

        assert_eq!(session.conversation_history.len(), 2);
        assert_eq!(session.conversation_history[0].role, "user");
        assert_eq!(session.conversation_history[1].role, "assistant");
    }

    #[test]
    fn test_session_preferences() {
        let mut session = Session::new("/test");
        
        session.set_preference("auto_approve", "true");
        assert_eq!(session.get_preference("auto_approve"), Some(&"true".to_string()));
        
        session.remove_preference("auto_approve");
        assert_eq!(session.get_preference("auto_approve"), None);
    }

    #[test]
    fn test_session_truncation() {
        let mut session = Session::new("/test");
        
        // Add many messages
        for i in 0..100 {
            session.add_user_message(&format!("Message {}", i));
        }
        
        let initial_count = session.conversation_history.len();
        
        // Truncate to a small token limit
        let removed = session.truncate_history(100);
        
        assert!(removed > 0);
        assert!(session.conversation_history.len() < initial_count);
        
        // Most recent messages should be preserved
        let last_msg = session.conversation_history.last().unwrap();
        assert!(last_msg.content.contains("99")); // Last message should be "Message 99"
    }

    #[test]
    fn test_session_persistence() {
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("session.json");
        
        // Create and save session
        let mut session = Session::new(temp_dir.path());
        session.add_user_message("Test message");
        session.set_preference("key", "value");
        session.save(&session_path).unwrap();
        
        // Load and verify
        let loaded = Session::load(&session_path).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.conversation_history.len(), 1);
        assert_eq!(loaded.get_preference("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_session_load_corrupted() {
        let temp_dir = TempDir::new().unwrap();
        let session_path = temp_dir.path().join("session.json");
        
        // Write invalid JSON
        std::fs::write(&session_path, "not valid json").unwrap();
        
        // Should return error
        let result = Session::load(&session_path);
        assert!(result.is_err());
        
        // try_load should return None
        let result = Session::try_load(&session_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_project_context_summary() {
        let mut ctx = ProjectContext::new("/test/project");
        ctx.has_prd = true;
        ctx.language = Some("rust".to_string());
        
        let summary = ctx.summary();
        assert!(summary.contains("/test/project"));
        assert!(summary.contains("rust"));
        assert!(summary.contains("PRD"));
    }

    #[test]
    fn test_context_summary() {
        let mut session = Session::new("/test");
        session.add_user_message("Hello");
        session.set_preference("mode", "incremental");
        
        let summary = session.get_context_summary();
        assert!(summary.contains("/test"));
        assert!(summary.contains("1 messages"));
        assert!(summary.contains("mode"));
    }
}
