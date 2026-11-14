//! A session service that stores something other than what the runner gave it (a redaction)
//! returns the stored event from `append_event_returning_stored`, and the rest of the run, a
//! real `LlmAgent` included, reads that event. Only the run's own events change: the history
//! window holds, foreign writes stay out, and a failed append delivers nothing.

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_core::{
    AdkError, Agent, Content, Event, LlmRequest, Part, Result, RunConfig, SessionId, Tool, UserId,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const APP: &str = "stored-view";
const USER: &str = "user";
const SESSION: &str = "session";

/// Stores every event with "secret" redacted. `returns_stored` controls whether it reports the
/// stored event back; `fail_on` makes an append fail when the event's text contains it.
struct RedactingService {
    inner: InMemorySessionService,
    returns_stored: bool,
    fail_on: Option<&'static str>,
}

impl RedactingService {
    fn redact(mut event: Event) -> Event {
        if let Some(content) = event.llm_response.content.as_mut() {
            for part in &mut content.parts {
                if let Part::Text { text } = part {
                    *text = text.replace("secret", "[redacted]");
                }
            }
        }
        event
    }

    fn check(&self, event: &Event) -> Result<()> {
        match self.fail_on {
            Some(needle) if text_of(event).contains(needle) => {
                Err(AdkError::session("the store is unreachable"))
            }
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl SessionService for RedactingService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        self.inner.create(req).await
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        self.inner.get(req).await
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        self.check(&event)?;
        self.inner.append_event(session_id, Self::redact(event)).await
    }

    async fn append_event_for_identity(&self, mut req: AppendEventRequest) -> Result<()> {
        self.check(&req.event)?;
        req.event = Self::redact(req.event);
        self.inner.append_event_for_identity(req).await
    }

    async fn append_event_returning_stored(&self, req: AppendEventRequest) -> Result<Event> {
        if !self.returns_stored {
            let event = req.event.clone();
            self.append_event_for_identity(req).await?;
            return Ok(event);
        }
        self.check(&req.event)?;
        let stored = Self::redact(req.event);
        self.inner
            .append_event_for_identity(AppendEventRequest {
                identity: req.identity,
                event: stored.clone(),
            })
            .await?;
        Ok(stored)
    }
}

fn text_of(event: &Event) -> String {
    event
        .llm_response
        .content
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every text part of every non-system content of the request.
fn request_text(request: &LlmRequest) -> String {
    request
        .contents
        .iter()
        .filter(|content| content.role != "system")
        .flat_map(|content| &content.parts)
        .filter_map(Part::text)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn service(returns_stored: bool, fail_on: Option<&'static str>) -> Arc<RedactingService> {
    let service = Arc::new(RedactingService {
        inner: InMemorySessionService::new(),
        returns_stored,
        fail_on,
    });
    service
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    service
}

fn identity() -> adk_core::AdkIdentity {
    adk_core::AdkIdentity::new(
        adk_core::AppName::try_from(APP).unwrap(),
        UserId::new(USER).unwrap(),
        SessionId::new(SESSION).unwrap(),
    )
}

/// Runs `agent` on `prompt`; returns the yielded events and the error that ended the run.
async fn run(
    service: Arc<RedactingService>,
    agent: Arc<dyn Agent>,
    prompt: &str,
    config: Option<RunConfig>,
) -> (Vec<Event>, Option<AdkError>) {
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(service as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut stream = runner
        .run_with_config(
            UserId::new(USER).unwrap(),
            SessionId::new(SESSION).unwrap(),
            Content::new("user").with_text(prompt),
            config,
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => events.push(event),
            Err(error) => return (events, Some(error)),
        }
    }
    (events, None)
}

/// A writer and a reviewer `LlmAgent` in a `SequentialAgent`, with their scripted models.
fn pipeline(
    writer_replies: Vec<ScriptedResponse>,
    writer_tools: Vec<Arc<dyn Tool>>,
) -> (Arc<dyn Agent>, Arc<ScriptedLlm>, Arc<ScriptedLlm>) {
    let writer_model = Arc::new(ScriptedLlm::new("writer-model", writer_replies));
    let reviewer_model =
        Arc::new(ScriptedLlm::new("reviewer-model", [ScriptedResponse::text("approved")]));
    let mut writer = LlmAgentBuilder::new("writer").model(writer_model.clone());
    for tool in writer_tools {
        writer = writer.tool(tool);
    }
    let reviewer = LlmAgentBuilder::new("reviewer").model(reviewer_model.clone()).build().unwrap();
    let agent = Arc::new(SequentialAgent::new(
        "pipeline",
        vec![Arc::new(writer.build().unwrap()), Arc::new(reviewer)],
    ));
    (agent, writer_model, reviewer_model)
}

#[tokio::test]
async fn the_stored_events_reach_every_llm_agent_and_the_caller_gets_what_was_yielded() {
    let service = service(true, None).await;
    let (agent, writer_model, reviewer_model) =
        pipeline(vec![ScriptedResponse::text("the secret is 42")], vec![]);

    let (events, error) = run(service.clone(), agent, "keep my secret plan", None).await;

    assert!(error.is_none(), "{error:?}");
    // The user's message as stored reaches the first model, which reads it from the context.
    let writer_sees = request_text(&writer_model.requests()[0]);
    assert!(writer_sees.contains("keep my [redacted] plan"), "{writer_sees}");
    assert!(!writer_sees.contains("secret"), "{writer_sees}");
    // The next sub-agent reads the writer's turn as stored.
    let reviewer_sees = request_text(&reviewer_model.requests()[0]);
    assert!(reviewer_sees.contains("the [redacted] is 42"), "{reviewer_sees}");
    assert!(!reviewer_sees.contains("secret"), "{reviewer_sees}");
    // The caller's stream carries the event the agent yielded.
    assert!(
        events.iter().any(|event| event.author == "writer" && text_of(event) == "the secret is 42"),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_service_that_does_not_return_the_stored_event_leaves_the_runners_copy() {
    let service = service(false, None).await;
    let (agent, writer_model, reviewer_model) =
        pipeline(vec![ScriptedResponse::text("the secret is 42")], vec![]);

    let (_, error) = run(service, agent, "keep my secret plan", None).await;

    assert!(error.is_none(), "{error:?}");
    assert!(request_text(&writer_model.requests()[0]).contains("keep my secret plan"));
    assert!(request_text(&reviewer_model.requests()[0]).contains("the secret is 42"));
}

#[tokio::test]
async fn the_history_window_holds() {
    let service = service(true, None).await;
    for index in 0..10 {
        let mut event = Event::new("earlier");
        event.author = "user".to_string();
        event.llm_response.content = Some(Content::new("user").with_text(format!("old {index}")));
        service
            .append_event_for_identity(AppendEventRequest { identity: identity(), event })
            .await
            .unwrap();
    }
    let (agent, writer_model, reviewer_model) =
        pipeline(vec![ScriptedResponse::text("the secret is 42")], vec![]);

    let config = RunConfig::builder().history_max_events(Some(2)).build();
    let (_, error) = run(service, agent, "now", Some(config)).await;

    assert!(error.is_none(), "{error:?}");
    for request in [&writer_model.requests()[0], &reviewer_model.requests()[0]] {
        let sees = request_text(request);
        assert!(sees.contains("old 9"), "{sees}");
        assert!(!sees.contains("old 0"), "older than the window: {sees}");
    }
    assert!(request_text(&reviewer_model.requests()[0]).contains("the [redacted] is 42"));
}

#[tokio::test]
async fn a_write_from_outside_the_run_stays_out_of_its_view() {
    let service = service(true, None).await;
    let writes = service.clone();
    let host_note: Arc<dyn Tool> =
        Arc::new(FunctionTool::new("note", "Records a note.", move |_ctx, _args| {
            let writes = writes.clone();
            async move {
                // Another writer on the same session while the run is going.
                let mut event = Event::new("another-run");
                event.author = "user".to_string();
                event.llm_response.content =
                    Some(Content::new("user").with_text("unscreened host note"));
                writes
                    .append_event_for_identity(AppendEventRequest { identity: identity(), event })
                    .await?;
                Ok(json!({"noted": true}))
            }
        }));
    let (agent, _, reviewer_model) = pipeline(
        vec![
            ScriptedResponse::function_call("note", json!({})),
            ScriptedResponse::text("noted the secret"),
        ],
        vec![host_note],
    );

    let (_, error) = run(service.clone(), agent, "go", None).await;

    assert!(error.is_none(), "{error:?}");
    let reviewer_sees = request_text(&reviewer_model.requests()[0]);
    assert!(!reviewer_sees.contains("host note"), "{reviewer_sees}");
    assert!(reviewer_sees.contains("noted the [redacted]"), "{reviewer_sees}");
    let stored = service.get_for_identity(&identity()).await.unwrap().events().all();
    assert!(stored.iter().any(|event| text_of(event) == "unscreened host note"));
}

#[tokio::test]
async fn a_failed_append_ends_the_run_without_delivering_the_event() {
    let service = service(true, Some("42")).await;
    let (agent, _, reviewer_model) =
        pipeline(vec![ScriptedResponse::text("the secret is 42")], vec![]);

    let (events, error) = run(service.clone(), agent, "go", None).await;

    assert!(error.is_some(), "the failed append ends the run");
    assert!(!events.iter().any(|event| text_of(event).contains("42")), "{events:?}");
    assert!(reviewer_model.requests().is_empty(), "the reviewer never runs");
    let stored = service.get_for_identity(&identity()).await.unwrap().events().all();
    assert!(!stored.iter().any(|event| text_of(event).contains("42")), "nothing stored");
}
