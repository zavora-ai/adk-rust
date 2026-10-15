//! A tool result that ends the turn is the final response, so output guardrails screen it
//! before the client or the session sees it.
#![cfg(feature = "guardrails")]

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{Guardrail, GuardrailResult, GuardrailSet, Severity};
use adk_core::{
    Content, Event, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    SessionId, Tool, ToolContext, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Calls `lookup` on its first turn and answers with text afterwards.
struct CallingModel;

#[async_trait]
impl Llm for CallingModel {
    fn name(&self) -> &str {
        "calling-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        let answered = request.contents.iter().any(|content| {
            content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
        });
        let part = if answered {
            Part::Text { text: "summary".to_string() }
        } else {
            Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }
        };
        let response = LlmResponse {
            content: Some(Content { role: "model".to_string(), parts: vec![part] }),
            turn_complete: true,
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Returns a secret and ends the turn with its own output.
struct TerminalLookup;

#[async_trait]
impl Tool for TerminalLookup {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Looks up an account."
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let mut actions = ctx.actions();
        actions.skip_summarization = true;
        ctx.set_actions(actions);
        Ok(json!({ "account": "acct-1", "note": "SECRET" }))
    }
}

struct BlockSecret;

#[async_trait]
impl Guardrail for BlockSecret {
    fn name(&self) -> &str {
        "block_secret"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        if text.contains("SECRET") {
            GuardrailResult::fail("secret in output", Severity::High)
        } else {
            GuardrailResult::pass()
        }
    }
}

struct RedactSecret;

#[async_trait]
impl Guardrail for RedactSecret {
    fn name(&self) -> &str {
        "redact_secret"
    }

    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts.iter().filter_map(Part::text).collect();
        if text.contains("SECRET") {
            GuardrailResult::transform(
                Content::new("model").with_text(text.replace("SECRET", "[redacted]")),
                "secret redacted",
            )
        } else {
            GuardrailResult::pass()
        }
    }
}

/// Runs one turn and returns what the client received and what the session persisted.
async fn run(guardrails: GuardrailSet) -> (Vec<Result<Event>>, Vec<Event>) {
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .model(Arc::new(CallingModel))
            .tool(Arc::new(TerminalLookup))
            .output_guardrails(guardrails)
            .build()
            .unwrap(),
    );
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "guarded".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("guarded")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("look up my account"),
        )
        .await
        .unwrap();
    let received = stream.collect::<Vec<_>>().await;
    let persisted = sessions
        .get(GetRequest {
            app_name: "guarded".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all();
    (received, persisted)
}

fn mentions(event: &Event, needle: &str) -> bool {
    serde_json::to_string(&event.llm_response.content).unwrap().contains(needle)
}

#[tokio::test]
async fn a_blocked_terminal_tool_result_reaches_neither_the_client_nor_the_session() {
    let (received, persisted) = run(GuardrailSet::new().with(BlockSecret)).await;

    assert!(
        received
            .iter()
            .any(|item| item.as_ref().is_err_and(|e| e.to_string().contains("block_secret"))),
        "the block must end the run with an error"
    );
    assert!(!received.iter().flatten().any(|event| mentions(event, "SECRET")));
    assert!(!persisted.iter().any(|event| mentions(event, "SECRET")));
}

#[tokio::test]
async fn a_transformed_terminal_tool_result_is_what_is_sent_and_persisted() {
    let (received, persisted) = run(GuardrailSet::new().with(RedactSecret)).await;

    assert!(received.iter().all(Result::is_ok));
    let responses: Vec<&Value> = persisted
        .iter()
        .filter_map(|event| event.content())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => Some(&function_response.response),
            _ => None,
        })
        .collect();
    assert_eq!(responses, [&json!({ "account": "acct-1", "note": "[redacted]" })]);
    assert!(!received.iter().flatten().any(|event| mentions(event, "SECRET")));
}
