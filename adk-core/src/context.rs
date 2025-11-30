use crate::{types::Content, Agent, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[async_trait]
pub trait ReadonlyContext: Send + Sync {
    fn invocation_id(&self) -> &str;
    fn agent_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn session_id(&self) -> &str;
    fn branch(&self) -> &str;
    fn user_content(&self) -> &Content;
}

// State management traits
pub trait State: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn set(&mut self, key: String, value: Value);
    fn all(&self) -> HashMap<String, Value>;
}

pub trait ReadonlyState: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
    fn all(&self) -> HashMap<String, Value>;
}

// Session trait
pub trait Session: Send + Sync {
    fn id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn state(&self) -> &dyn State;
    /// Returns the conversation history from this session as Content items
    fn conversation_history(&self) -> Vec<Content>;
}

#[async_trait]
pub trait CallbackContext: ReadonlyContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
}

#[async_trait]
pub trait InvocationContext: CallbackContext {
    fn agent(&self) -> Arc<dyn Agent>;
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    fn session(&self) -> &dyn Session;
    fn run_config(&self) -> &RunConfig;
    fn end_invocation(&self);
    fn ended(&self) -> bool;
}

// Placeholder service traits
#[async_trait]
pub trait Artifacts: Send + Sync {
    async fn save(&self, name: &str, data: &crate::Part) -> Result<i64>;
    async fn load(&self, name: &str) -> Result<crate::Part>;
    async fn list(&self) -> Result<Vec<String>>;
}

#[async_trait]
pub trait Memory: Send + Sync {
    async fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub content: Content,
    pub author: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamingMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

/// Controls what parts of prior conversation history is received by llmagent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IncludeContents {
    /// The llmagent operates solely on its current turn (latest user input + any following agent events)
    None,
    /// Default - The llmagent receives the relevant conversation history
    #[default]
    Default,
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub streaming_mode: StreamingMode,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self { streaming_mode: StreamingMode::Auto }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_config_default() {
        let config = RunConfig::default();
        assert_eq!(config.streaming_mode, StreamingMode::Auto);
    }

    #[test]
    fn test_streaming_mode() {
        assert_eq!(StreamingMode::Auto, StreamingMode::Auto);
        assert_ne!(StreamingMode::Auto, StreamingMode::Enabled);
    }
}
