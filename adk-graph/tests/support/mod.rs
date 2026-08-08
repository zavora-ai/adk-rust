//! Shared helpers for adk-graph integration tests.
//!
//! A minimal `InvocationContext` so a test can drive `GraphAgent::run` without
//! standing up a `Runner`. Extracted from `agent_node_context_tests.rs`, which
//! needed the same thing.

#![allow(dead_code)]

use adk_core::{
    Agent, Content, InvocationContext, Part, Result, RunConfig, SecretRequest, Session, State,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub struct SentinelState;

impl State for SentinelState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

pub struct SentinelSession;

impl Session for SentinelSession {
    fn id(&self) -> &str {
        "caller-session"
    }
    fn app_name(&self) -> &str {
        "caller-app"
    }
    fn user_id(&self) -> &str {
        "caller-user"
    }
    fn state(&self) -> &dyn State {
        &SentinelState
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct SentinelMemory;

#[async_trait]
impl adk_core::Memory for SentinelMemory {
    async fn search(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
        Ok(Vec::new())
    }
}

/// A context with a non-default value for every capability under test.
pub struct SentinelContext {
    user_content: Content,
    session: SentinelSession,
    cancelled: bool,
}

impl SentinelContext {
    fn new(cancelled: bool) -> Self {
        Self {
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "go".to_string() }],
            },
            session: SentinelSession,
            cancelled,
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for SentinelContext {
    fn invocation_id(&self) -> &str {
        "caller-invocation"
    }
    fn agent_name(&self) -> &str {
        "caller-agent"
    }
    fn user_id(&self) -> &str {
        "caller-user"
    }
    fn app_name(&self) -> &str {
        "caller-app"
    }
    fn session_id(&self) -> &str {
        "caller-session"
    }
    fn branch(&self) -> &str {
        "caller-branch"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for SentinelContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for SentinelContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised")
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        Some(Arc::new(SentinelMemory))
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
    fn user_scopes(&self) -> Vec<String> {
        vec!["tools:read".to_string(), "secrets:api".to_string()]
    }
    fn request_metadata(&self) -> HashMap<String, Value> {
        HashMap::from([("tenant".to_string(), json!("acme"))])
    }
    async fn get_secret(&self, _name: &str) -> Result<Option<String>> {
        Ok(Some("sentinel-secret".to_string()))
    }
    async fn get_secret_for(&self, _request: &SecretRequest) -> Result<Option<String>> {
        Ok(Some("sentinel-secret".to_string()))
    }
}

/// A context suitable for driving `GraphAgent::run` in a test.
pub fn test_context(_session_id: &str) -> Arc<dyn InvocationContext> {
    Arc::new(SentinelContext::new(false)) as Arc<dyn InvocationContext>
}
