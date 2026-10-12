//! Capability conformance for the workflow context wrappers.
//!
//! Every wrapper here re-implements [`InvocationContext`] and delegates to an
//! inner context. Most of the trait's capability methods have permissive
//! defaults — `is_cancelled()` returns `false`, `get_secret()` returns `None`,
//! `shared_state()`/`user_scopes()`/`request_metadata()` return empty — so a
//! wrapper that forgets one still compiles and silently drops it. That is how
//! cancellation, secrets, and shared state came to be lost depending on which
//! workflow agent a sub-agent happened to run under.
//!
//! These tests push a sentinel context with a non-default value for every
//! capability through each wrapper and assert all of them survive. A new wrapper,
//! or a new capability method, should be added here.

use super::branch_context::BranchContext;
use super::shared_state_context::SharedStateContext;
use adk_core::{
    Agent, CallbackContext, Content, Event, InvocationContext, Memory, Part, ReadonlyContext,
    Result, RunConfig, Session, SharedState, ToolOutcome,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

// ── Sentinel context: every capability set to a non-default value ──────

const SENTINEL_SECRET_NAME: &str = "sentinel-secret";
const SENTINEL_SECRET_VALUE: &str = "sentinel-value";
const SENTINEL_SCOPE: &str = "sentinel:scope";
const SENTINEL_AGENT_ID: &str = "spiffe://sentinel.test/agent/sentinel";
const SENTINEL_METADATA_KEY: &str = "sentinel-metadata";
const SENTINEL_STATE_KEY: &str = "sentinel-shared-key";

const SENTINEL_ARTIFACT: &str = "sentinel-artifact";
const SENTINEL_MEMORY_AUTHOR: &str = "sentinel-memory-author";

struct SentinelArtifacts;

#[async_trait]
impl adk_core::Artifacts for SentinelArtifacts {
    async fn save(&self, _name: &str, _data: &Part) -> Result<i64> {
        Ok(1)
    }
    async fn load(&self, _name: &str) -> Result<Part> {
        Ok(Part::Text { text: "sentinel".to_string() })
    }
    async fn list(&self) -> Result<Vec<String>> {
        Ok(vec![SENTINEL_ARTIFACT.to_string()])
    }
}

struct SentinelMemory;

#[async_trait]
impl Memory for SentinelMemory {
    async fn search(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![adk_core::MemoryEntry {
            content: Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: "sentinel".to_string() }],
            },
            author: SENTINEL_MEMORY_AUTHOR.to_string(),
        }])
    }
}

struct SentinelState;

impl adk_core::State for SentinelState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

struct SentinelSession {
    state: SentinelState,
}

impl Session for SentinelSession {
    fn id(&self) -> &str {
        "sentinel-session"
    }
    fn app_name(&self) -> &str {
        "sentinel-app"
    }
    fn user_id(&self) -> &str {
        "sentinel-user"
    }
    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct SentinelContext {
    content: Content,
    config: RunConfig,
    session: SentinelSession,
    shared: Arc<SharedState>,
}

impl SentinelContext {
    /// `SharedState` mutation is async, so callers seed it via [`seeded_shared_state`].
    fn with_shared(shared: Arc<SharedState>) -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "sentinel".to_string() }],
            },
            config: RunConfig::default(),
            session: SentinelSession { state: SentinelState },
            shared,
        }
    }
}

/// A `SharedState` carrying the sentinel key.
async fn seeded_shared_state(key: &str) -> Arc<SharedState> {
    let shared = Arc::new(SharedState::new());
    shared.set_shared(key, serde_json::json!(true)).await.expect("seeding shared state failed");
    shared
}

#[async_trait]
impl ReadonlyContext for SentinelContext {
    fn invocation_id(&self) -> &str {
        "sentinel-invocation"
    }
    fn agent_name(&self) -> &str {
        "sentinel-agent"
    }
    fn user_id(&self) -> &str {
        "sentinel-user"
    }
    fn app_name(&self) -> &str {
        "sentinel-app"
    }
    fn session_id(&self) -> &str {
        "sentinel-session"
    }
    fn branch(&self) -> &str {
        "sentinel-branch"
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
    fn acting_context(&self) -> adk_core::ActingContext {
        sentinel_acting()
    }
}

fn sentinel_acting() -> adk_core::ActingContext {
    adk_core::ActingContext::new(adk_core::AgentPrincipal::new(
        adk_core::AgentId::new(SENTINEL_AGENT_ID).expect("valid sentinel agent id"),
    ))
}

#[async_trait]
impl CallbackContext for SentinelContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        Some(Arc::new(SentinelArtifacts))
    }
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        Some(ToolOutcome {
            tool_name: "sentinel-tool".to_string(),
            tool_args: serde_json::json!({}),
            success: true,
            duration: std::time::Duration::from_millis(1),
            error_message: None,
            attempt: 0,
        })
    }
    fn tool_name(&self) -> Option<&str> {
        Some("sentinel-tool")
    }
    fn shared_state(&self) -> Option<Arc<SharedState>> {
        Some(self.shared.clone())
    }
}

#[async_trait]
impl InvocationContext for SentinelContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!("not exercised by wrapper conformance")
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        Some(Arc::new(SentinelMemory))
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }

    // Non-default values for each capability the wrappers must preserve.
    fn is_cancelled(&self) -> bool {
        true
    }
    fn user_scopes(&self) -> Vec<String> {
        vec![SENTINEL_SCOPE.to_string()]
    }
    fn request_metadata(&self) -> HashMap<String, serde_json::Value> {
        HashMap::from([(SENTINEL_METADATA_KEY.to_string(), serde_json::json!(1))])
    }
    async fn get_secret(&self, name: &str) -> Result<Option<String>> {
        Ok(if name == SENTINEL_SECRET_NAME {
            Some(SENTINEL_SECRET_VALUE.to_string())
        } else {
            None
        })
    }
}

// ── Shared assertion ──────────────────────────────────────────────────

/// Assert that every capability set on the sentinel survives `wrapped`.
async fn assert_capabilities_preserved(label: &str, wrapped: Arc<dyn InvocationContext>) {
    assert!(wrapped.is_cancelled(), "{label}: cancellation was dropped");

    assert_eq!(
        wrapped.user_scopes(),
        vec![SENTINEL_SCOPE.to_string()],
        "{label}: authenticated scopes were dropped"
    );
    assert_eq!(wrapped.acting_context(), sentinel_acting(), "{label}: acting context dropped");

    assert!(
        wrapped.request_metadata().contains_key(SENTINEL_METADATA_KEY),
        "{label}: request metadata was dropped"
    );

    let secret = wrapped.get_secret(SENTINEL_SECRET_NAME).await.expect("secret lookup failed");
    assert_eq!(
        secret.as_deref(),
        Some(SENTINEL_SECRET_VALUE),
        "{label}: secret access was dropped"
    );

    let shared =
        wrapped.shared_state().unwrap_or_else(|| panic!("{label}: shared state was dropped"));
    assert!(
        shared.get_shared(SENTINEL_STATE_KEY).await.is_some(),
        "{label}: shared state was replaced rather than forwarded"
    );

    let artifacts =
        wrapped.artifacts().unwrap_or_else(|| panic!("{label}: artifact service was dropped"));
    assert_eq!(
        artifacts.list().await.expect("artifact list failed"),
        vec![SENTINEL_ARTIFACT.to_string()],
        "{label}: artifact service was replaced rather than forwarded"
    );

    let memory = wrapped.memory().unwrap_or_else(|| panic!("{label}: memory service was dropped"));
    let hits = memory.search("sentinel").await.expect("memory search failed");
    assert_eq!(
        hits.first().map(|e| e.author.as_str()),
        Some(SENTINEL_MEMORY_AUTHOR),
        "{label}: memory service was replaced rather than forwarded"
    );

    // Identity must survive untouched too.
    assert_eq!(wrapped.app_name(), "sentinel-app", "{label}: app name was lost");
    assert_eq!(wrapped.user_id(), "sentinel-user", "{label}: user id was lost");
    assert_eq!(wrapped.session_id(), "sentinel-session", "{label}: session id was lost");
}

async fn sentinel() -> Arc<dyn InvocationContext> {
    Arc::new(SentinelContext::with_shared(seeded_shared_state(SENTINEL_STATE_KEY).await))
}

// ── Per-wrapper tests ─────────────────────────────────────────────────

#[tokio::test]
async fn shared_state_context_preserves_capabilities() {
    // This wrapper deliberately replaces `shared_state`, so it is checked
    // separately from the shared assertion.
    let injected = seeded_shared_state("injected").await;
    let wrapped: Arc<dyn InvocationContext> =
        Arc::new(SharedStateContext::new(sentinel().await, injected));

    assert!(wrapped.is_cancelled(), "SharedStateContext: cancellation was dropped");
    assert_eq!(wrapped.user_scopes(), vec![SENTINEL_SCOPE.to_string()]);
    assert_eq!(
        wrapped.acting_context(),
        sentinel_acting(),
        "SharedStateContext: acting context dropped"
    );
    assert!(wrapped.request_metadata().contains_key(SENTINEL_METADATA_KEY));
    let secret = wrapped.get_secret(SENTINEL_SECRET_NAME).await.unwrap();
    assert_eq!(secret.as_deref(), Some(SENTINEL_SECRET_VALUE));
    let shared = wrapped.shared_state().expect("shared state missing");
    assert!(
        shared.get_shared("injected").await.is_some(),
        "SharedStateContext must expose the state it injects"
    );
    assert!(wrapped.artifacts().is_some(), "SharedStateContext: artifacts were dropped");
    assert!(wrapped.memory().is_some(), "SharedStateContext: memory was dropped");
}

#[tokio::test]
async fn branch_context_preserves_capabilities() {
    let wrapped: Arc<dyn InvocationContext> =
        Arc::new(BranchContext::new(sentinel().await, "sentinel-branch.child".to_string()));
    assert_capabilities_preserved("BranchContext", wrapped.clone()).await;
    assert_eq!(wrapped.branch(), "sentinel-branch.child", "BranchContext must override branch");
}

#[tokio::test]
async fn skill_context_preserves_capabilities() {
    let overridden = Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "overridden".to_string() }],
    };
    let wrapped =
        super::skill_context::with_user_content_override(sentinel().await, overridden.clone());
    assert_capabilities_preserved("UserContentOverrideContext", wrapped.clone()).await;
    assert_eq!(
        wrapped.user_content().parts.len(),
        overridden.parts.len(),
        "UserContentOverrideContext must override user content"
    );
}

#[tokio::test]
async fn loop_history_context_preserves_capabilities() {
    let wrapped = super::loop_agent::history_tracking_context_for_test(sentinel().await);
    assert_capabilities_preserved("HistoryTrackingContext", wrapped).await;
}

/// Wrappers compose, so a capability dropped by an inner wrapper is lost even if
/// the outer one forwards correctly. `ParallelAgent` builds exactly this stack.
#[tokio::test]
async fn composed_wrappers_preserve_capabilities() {
    let injected = seeded_shared_state(SENTINEL_STATE_KEY).await;
    let inner: Arc<dyn InvocationContext> =
        Arc::new(SharedStateContext::new(sentinel().await, injected));
    let outer: Arc<dyn InvocationContext> =
        Arc::new(BranchContext::new(inner, "sentinel-branch.parallel.a".to_string()));

    assert_capabilities_preserved("BranchContext(SharedStateContext)", outer.clone()).await;
    assert_eq!(outer.branch(), "sentinel-branch.parallel.a");
}

/// An `Event` carries no capabilities, but this pins the sentinel itself: if the
/// trait gains a capability method, the sentinel must set a non-default value or
/// these tests silently stop covering it.
#[tokio::test]
async fn sentinel_reports_non_default_capabilities() {
    let ctx = SentinelContext::with_shared(seeded_shared_state(SENTINEL_STATE_KEY).await);
    assert!(ctx.is_cancelled());
    assert!(!ctx.user_scopes().is_empty());
    assert!(!ctx.request_metadata().is_empty());
    assert!(ctx.shared_state().is_some());
    assert!(ctx.artifacts().is_some());
    assert!(ctx.memory().is_some());
    let _ = Event::new("sentinel-invocation");
}

#[tokio::test]
async fn acting_agent_context_preserves_capabilities_and_names_its_agent() {
    use crate::acting_agent_context::ActingAgentContext;

    let parent = sentinel().await;
    let same = ActingAgentContext::scope(parent.clone(), parent.agent_name());
    assert!(Arc::ptr_eq(&same, &parent), "a context naming the agent is used as is");

    let wrapped = ActingAgentContext::scope(sentinel().await, "sub-agent");
    assert!(wrapped.is_cancelled(), "ActingAgentContext: cancellation was dropped");
    assert_eq!(wrapped.user_scopes(), vec![SENTINEL_SCOPE.to_string()]);
    assert!(wrapped.request_metadata().contains_key(SENTINEL_METADATA_KEY));
    let secret = wrapped.get_secret(SENTINEL_SECRET_NAME).await.unwrap();
    assert_eq!(secret.as_deref(), Some(SENTINEL_SECRET_VALUE));
    assert!(wrapped.shared_state().is_some(), "ActingAgentContext: shared state was dropped");
    assert!(wrapped.artifacts().is_some(), "ActingAgentContext: artifacts were dropped");
    assert!(wrapped.memory().is_some(), "ActingAgentContext: memory was dropped");
    assert_eq!(wrapped.agent_name(), parent.agent_name(), "the execution identity is unchanged");

    // The subject and chain are the parent's; the principal is the sub-agent's own.
    let expected = adk_core::ActingContext {
        agent: adk_core::AgentPrincipal::derived("sentinel-app", "sub-agent"),
        ..sentinel_acting()
    };
    assert_eq!(wrapped.acting_context(), expected);
}
