//! Tests for the coding-agent harness (feature `coding`).
#![cfg(feature = "coding")]

use std::sync::Arc;

use adk_agent::coding::{CodingAgent, TodoTool};
use adk_core::{Tool, ToolContext};
use adk_devtools::Workspace;
use async_trait::async_trait;
use serde_json::json;

// ── A trivial mock model so we can build an agent without network access ──

struct MockLlm;

#[async_trait]
impl adk_core::Llm for MockLlm {
    fn name(&self) -> &str {
        "mock-llm"
    }
    async fn generate_content(
        &self,
        _request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text: "done".to_string() }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

// Minimal ToolContext for exercising TodoTool directly.
struct TestCtx;

#[async_trait]
impl adk_core::ReadonlyContext for TestCtx {
    fn invocation_id(&self) -> &str {
        "inv"
    }
    fn agent_name(&self) -> &str {
        "a"
    }
    fn user_id(&self) -> &str {
        "u"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "s"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &adk_core::Content {
        static C: std::sync::OnceLock<adk_core::Content> = std::sync::OnceLock::new();
        C.get_or_init(|| adk_core::Content::new("user").with_text("hi"))
    }
}

#[async_trait]
impl adk_core::CallbackContext for TestCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestCtx {
    fn function_call_id(&self) -> &str {
        "call"
    }
    fn actions(&self) -> adk_core::EventActions {
        adk_core::EventActions::default()
    }
    fn set_actions(&self, _actions: adk_core::EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

#[test]
fn builder_requires_model_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    assert!(CodingAgent::builder().build().is_err());
    assert!(CodingAgent::builder().model(Arc::new(MockLlm)).build().is_err());
    assert!(
        CodingAgent::builder()
            .model(Arc::new(MockLlm))
            .workspace(Workspace::new(dir.path()))
            .build()
            .is_ok()
    );
}

#[test]
fn builds_a_named_agent() {
    let dir = tempfile::tempdir().unwrap();
    let coding = CodingAgent::builder()
        .name("repo-fixer")
        .model(Arc::new(MockLlm))
        .workspace(Workspace::new(dir.path()))
        .build()
        .unwrap();
    assert_eq!(coding.agent().name(), "repo-fixer");
    assert!(coding.todos().is_empty());
}

#[tokio::test]
async fn todo_tool_tracks_plan() {
    let tool = TodoTool::new();
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    let out = Tool::execute(
        &tool,
        ctx,
        json!({"todos": [
            {"content": "read the failing test", "status": "in_progress"},
            {"content": "fix it", "status": "pending"}
        ]}),
    )
    .await
    .unwrap();

    assert_eq!(out["total"], 2);
    assert_eq!(out["remaining"], 2);
    let items = tool.items();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].status, "in_progress");
}
