//! Fork and rewind of a live conversation on `claude-sonnet-5-5`, through `Runner` and
//! `LlmAgent`: a fork taken over REST continues differently from its source, the source is
//! untouched, and a rewind before a turn makes the model forget what that turn said.
//!
//! Run with `ANTHROPIC_API_KEY` set:
//!
//! ```bash
//! cargo nextest run -p adk-server --test session_history_live --run-ignored only --no-capture
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{
    AuditActor, AuditRecordKind, AuditRecordOutcome, AuditTrail, InMemoryAuditRecorder,
};
use adk_core::identity::AdkIdentity;
use adk_core::principal::SubjectRef;
use adk_core::{Agent, Content, SessionId, UserId};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_runner::{RewindTarget, Runner};
use adk_server::{ServerConfig, create_app};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const APP: &str = "memory";
const MODEL: &str = "claude-sonnet-5-5";
const ASK: &str = "Answer in exactly this form and nothing else: colour=<my favourite colour>; \
                   number=<my favourite number, or unknown if I have not told you>";

struct Loader(Arc<dyn Agent>);

#[async_trait]
impl adk_core::AgentLoader for Loader {
    async fn load_agent(&self, _app: &str) -> adk_core::Result<Arc<dyn Agent>> {
        Ok(self.0.clone())
    }

    fn list_agents(&self) -> Vec<String> {
        vec![APP.to_string()]
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        self.0.clone()
    }
}

/// Runs one turn and returns the model's reply text.
async fn turn(runner: &Runner, session: &str, text: &str) -> String {
    let events = runner
        .run(
            UserId::try_from("alice").unwrap(),
            SessionId::try_from(session).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .expect("run starts");
    let events: Vec<_> = events.collect().await;
    let reply: String = events
        .into_iter()
        .map(|event| event.expect("the turn succeeds"))
        .filter(|event| event.author != "user" && !event.llm_response.partial)
        .filter_map(|event| event.llm_response.content)
        .flat_map(|content| content.parts)
        .filter_map(|part| part.text().map(str::to_string))
        .collect();
    eprintln!("[{session}] user: {text}\n[{session}] model: {}", reply.trim());
    reply.to_lowercase()
}

fn identity(session: &str) -> AdkIdentity {
    AdkIdentity::new(
        APP.try_into().unwrap(),
        "alice".try_into().unwrap(),
        session.try_into().unwrap(),
    )
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn live_fork_and_rewind_on_claude_sonnet_5_5() {
    adk_core::ensure_crypto_provider();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model = Arc::new(AnthropicClient::new(AnthropicConfig::new(key, MODEL)).unwrap());
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("assistant")
            .model(model)
            .instruction(
                "You are a terse assistant. Remember what the user tells you in this \
                 conversation, and follow the reply format the user asks for exactly.",
            )
            .build()
            .unwrap(),
    );

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: Some("main".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let governance = adk_core::GovernanceControl::new();
    governance.attach_audit(AuditTrail::new(recorder.clone()));
    let config = ServerConfig::new(
        Arc::new(Loader(agent.clone())),
        sessions.clone() as Arc<dyn SessionService>,
    )
    .with_governance(governance);
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .session_activity(config.session_activity.clone())
        .audit_log(recorder.clone())
        .build()
        .unwrap();
    let app = create_app(config);

    // Two facts in the main conversation.
    turn(&runner, "main", "My favourite colour is teal. Reply with just: noted").await;
    turn(&runner, "main", "My favourite number is 7. Reply with just: noted").await;
    let before_fork = sessions.history(&identity("main")).await.unwrap();
    let colour_turn_end = before_fork.events[1].id.clone();
    let number_invocation = before_fork.events[2].invocation_id.clone();
    assert_eq!(before_fork.events.len(), 4);

    // Fork over REST after the first fact only.
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/apps/{APP}/users/alice/sessions/main/fork"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"eventId": colour_turn_end, "sessionId": "branch"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = http_body_util::BodyExt::collect(response.into_body()).await.unwrap().to_bytes();
    let fork: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fork["session"]["events"].as_array().unwrap().len(), 2);
    assert_eq!(fork["change"]["forkOrigin"]["sessionId"], "main");

    // The fork continues differently and never learned the number.
    let branch =
        turn(&runner, "branch", &format!("Actually my favourite colour is orange now. {ASK}"))
            .await;
    assert!(branch.contains("orange") && branch.contains("unknown"), "fork answer: {branch}");

    // The source is untouched by the fork and its continuation.
    let main = turn(&runner, "main", ASK).await;
    assert!(main.contains("teal") && main.contains('7'), "source answer: {main}");
    let after = sessions.history(&identity("main")).await.unwrap();
    assert_eq!(
        after.events[..4].iter().map(|event| &event.id).collect::<Vec<_>>(),
        before_fork.events.iter().map(|event| &event.id).collect::<Vec<_>>()
    );

    // Rewind the source to before the number turn, then continue it.
    let actor = AuditActor::subject(SubjectRef::human("alice"));
    let rewound = runner
        .history_control()
        .rewind(&actor, &identity("main"), RewindTarget::BeforeInvocation(number_invocation), None)
        .await
        .unwrap();
    assert_eq!(rewound.session.events().len(), 2);
    let forgotten = turn(&runner, "main", ASK).await;
    assert!(
        forgotten.contains("teal") && forgotten.contains("unknown"),
        "after rewind: {forgotten}"
    );

    let history: Vec<_> = recorder
        .records()
        .into_iter()
        .filter(|record| record.kind == AuditRecordKind::SessionHistory)
        .map(|record| (record.action, record.outcome))
        .collect();
    assert_eq!(
        history,
        [
            ("session.fork".to_string(), None),
            ("session.fork".to_string(), Some(AuditRecordOutcome::Succeeded)),
            ("session.rewind".to_string(), None),
            ("session.rewind".to_string(), Some(AuditRecordOutcome::Succeeded)),
        ]
    );
}
