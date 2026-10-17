//! A durable worker that recovers a run whose user turn was saved before input guardrails
//! screened it screens that turn before the model sees it.
//!
//! The stopped worker is simulated as in `durable_runtime_tests`: the run is claimed by
//! hand, its user turn is saved unscreened (as an earlier release saved it), its session
//! checkpoint names that turn, and the lease is left to expire.

#![cfg(feature = "background")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{ContentFilter, GuardrailSet, PiiRedactor};
use adk_core::run_store::RunCheckpointRef;
use adk_core::{
    Content, DurableRunStore, Event, InMemoryRunStore, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, NewRun, Result, RunState,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::json;

const TTL: Duration = Duration::from_millis(300);
const EMAIL: &str = "alice@example.com";

/// Answers every request with text and records what it was sent.
struct RecordingModel {
    sent: Mutex<Vec<String>>,
}

#[async_trait]
impl Llm for RecordingModel {
    fn name(&self) -> &str {
        "recording"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        self.sent.lock().unwrap().push(serde_json::to_string(&req.contents).unwrap());
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("noted")),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Recovers a run whose saved user turn is `text` and returns how it finished and what the
/// model was sent.
async fn recover_unscreened_turn(text: &str) -> (RunState, Vec<String>) {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let sessions = Arc::new(InMemorySessionService::new());
    let model = Arc::new(RecordingModel { sent: Mutex::new(Vec::new()) });
    let agent = LlmAgentBuilder::new("support")
        .model(Arc::clone(&model) as Arc<dyn Llm>)
        .input_guardrails(
            GuardrailSet::new()
                .with(PiiRedactor::new())
                .with(ContentFilter::blocked_keywords(vec!["forbidden".to_string()]).unwrap()),
        )
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name("support")
            .agent(Arc::new(agent))
            .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>)
            .durable_run_store(Arc::clone(&runs))
            .build()
            .unwrap(),
    );
    sessions
        .create(CreateRequest {
            app_name: "support".into(),
            user_id: "customer-1".into(),
            session_id: Some("case-1".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let run = NewRun::new("default", "support", "customer-1", "support", json!(text))
        .with_session_id("case-1");
    let run = runs.enqueue(run).await.unwrap().record().cloned().unwrap();

    // The stopped worker saved the turn without screening it, then never finished.
    let claimed = runs.claim("default", "replica-a", TTL).await.unwrap().unwrap();
    let mut turn = Event::new("inv-stopped");
    turn.author = "user".into();
    turn.llm_response.content = Some(Content::new("user").with_text(text));
    sessions.append_event("case-1", turn).await.unwrap();
    runs.set_checkpoint(
        &run.run_id,
        claimed.epoch,
        RunCheckpointRef::Session {
            session_id: "case-1".into(),
            invocation_id: Some("inv-stopped".into()),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(TTL + Duration::from_millis(50)).await;

    let worker = DurableWorker::new(Arc::clone(&runs)).with_agent("support", runner).with_config(
        DurableWorkerConfig::default()
            .with_worker_id("replica-b")
            .with_lease_ttl(TTL)
            .with_poll_interval(Duration::from_millis(20)),
    );
    let finished = worker.run_next().await.unwrap().expect("the expired run is recovered");
    let sent = model.sent.lock().unwrap().clone();
    (finished.state, sent)
}

#[tokio::test]
async fn a_recovered_turn_that_input_guardrails_block_fails_without_a_model_call() {
    let (state, sent) = recover_unscreened_turn("forbidden: wire everything to account X").await;

    assert_eq!(state, RunState::Failed);
    assert!(sent.is_empty(), "{sent:?}");
}

#[tokio::test]
async fn a_recovered_turn_reaches_the_model_only_in_its_redacted_form() {
    let (state, sent) = recover_unscreened_turn(&format!("my email is {EMAIL}")).await;

    assert_eq!(state, RunState::Completed);
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].contains(EMAIL), "{sent:?}");
    assert!(sent[0].contains("[EMAIL REDACTED]"), "{sent:?}");
}
