//! Output guardrails on realtime text and transcripts (ROADMAP "No content guardrails on
//! the model's own text or transcript events").
//!
//! On `main` the integrated runner persisted and memorised whatever the model said: no
//! guardrail saw a realtime turn. These tests fail there: `output_guardrails` does not
//! exist on the builder and the raw text reaches the session.

#![cfg(all(feature = "integration", feature = "scripted", feature = "guardrails"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::Content;
use adk_guardrail::{Guardrail, GuardrailResult, GuardrailSet, PiiRedactor, Severity};
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::{RealtimeError, ServerEvent};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;

const APP: &str = "voice";
const USER: &str = "ana";
const SESSION: &str = "s1";

/// Blocks any turn that mentions the project's codename.
struct NoCodename;

#[async_trait]
impl Guardrail for NoCodename {
    fn name(&self) -> &str {
        "no_codename"
    }
    async fn validate(&self, content: &Content) -> GuardrailResult {
        let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
        if text.contains("Bluebird") {
            GuardrailResult::fail("the codename must not be spoken", Severity::High)
        } else {
            GuardrailResult::pass()
        }
    }
}

async fn sessions() -> Arc<InMemorySessionService> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    sessions
}

async fn model_turns(sessions: &InMemorySessionService) -> Vec<String> {
    sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: SESSION.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
        .iter()
        .filter(|event| event.author == "model")
        .filter_map(|event| event.content().and_then(|c| c.parts[0].text().map(str::to_string)))
        .collect()
}

fn runner(
    spoken: &str,
    guardrails: GuardrailSet,
    sessions: Arc<InMemorySessionService>,
) -> (IntegratedRealtimeRunner, ScriptedRealtimeModel) {
    let model =
        ScriptedRealtimeModel::new(vec![ScriptedReply::new(scripted::spoken(spoken)).on_text()])
            .with_idle_timeout(Duration::from_millis(50));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model.clone()))
        .identity(APP, USER, SESSION)
        .session_service(sessions)
        .output_guardrails(guardrails)
        .build()
        .unwrap();
    (runner, model)
}

#[tokio::test]
async fn a_transforming_guardrail_rewrites_the_persisted_transcript() {
    let sessions = sessions().await;
    let (runner, _model) = runner(
        "Mail ana@example.com about it.",
        GuardrailSet::new().with(PiiRedactor::new()),
        sessions.clone(),
    );
    runner.connect().await.unwrap();
    runner.send_text("who do I mail?").await.unwrap();
    while let Some(event) = runner.next_event().await {
        event.unwrap();
    }
    assert_eq!(model_turns(&sessions).await, vec!["Mail [EMAIL REDACTED] about it.".to_string()]);
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_blocking_guardrail_interrupts_and_persists_nothing() {
    let sessions = sessions().await;
    let (runner, model) =
        runner("The codename is Bluebird.", GuardrailSet::new().with(NoCodename), sessions.clone());
    runner.connect().await.unwrap();
    runner.send_text("what's the codename?").await.unwrap();
    let mut blocked = None;
    let mut forwarded = Vec::new();
    while let Some(event) = runner.next_event().await {
        match event {
            Ok(event) => forwarded.push(event),
            Err(error) => blocked = Some(error),
        }
    }
    match blocked {
        Some(RealtimeError::OutputBlocked { guardrail, reason }) => {
            assert_eq!(guardrail, "no_codename");
            assert!(reason.contains("codename"), "{reason}");
        }
        other => panic!("expected the turn to be blocked, got {other:?}"),
    }
    assert_eq!(model.log().interrupts(), 1, "the response was interrupted");
    assert!(model_turns(&sessions).await.is_empty(), "nothing of the turn reached the session");
    // The deltas already streamed were forwarded; the closing event was replaced by the error.
    assert!(forwarded.iter().any(|event| matches!(event, ServerEvent::TranscriptDelta { .. })));
    assert!(!forwarded.iter().any(|event| matches!(event, ServerEvent::ResponseDone { .. })));
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_passing_turn_is_persisted_unchanged() {
    let sessions = sessions().await;
    let (runner, _model) =
        runner("Two tests fail.", GuardrailSet::new().with(NoCodename), sessions.clone());
    runner.connect().await.unwrap();
    runner.send_text("what's failing?").await.unwrap();
    while let Some(event) = runner.next_event().await {
        event.unwrap();
    }
    assert_eq!(model_turns(&sessions).await, vec!["Two tests fail.".to_string()]);
    let _ = runner.close().await;
}
