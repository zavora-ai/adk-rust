//! `Runner::with_session_event_refresh`: the run's view of the session follows the service.

use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, Part, Result, SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Stores every event with "secret" redacted, so its stored view differs from what the runner
/// appended, and fails every read after `reads_allowed`.
struct RedactingService {
    inner: InMemorySessionService,
    reads: AtomicUsize,
    reads_allowed: usize,
}

impl RedactingService {
    fn new(reads_allowed: usize) -> Self {
        Self { inner: InMemorySessionService::new(), reads: AtomicUsize::new(0), reads_allowed }
    }

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
}

#[async_trait]
impl SessionService for RedactingService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        self.inner.create(req).await
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        if self.reads.fetch_add(1, Ordering::SeqCst) >= self.reads_allowed {
            return Err(adk_core::AdkError::session("the store is unreachable"));
        }
        self.inner.get(req).await
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        self.inner.append_event(session_id, Self::redact(event)).await
    }

    async fn append_event_for_identity(&self, mut req: AppendEventRequest) -> Result<()> {
        req.event = Self::redact(req.event);
        self.inner.append_event_for_identity(req).await
    }
}

type Seen = Arc<Mutex<Vec<Vec<String>>>>;

/// Yields a final event, a partial chunk and a final event, recording the texts of the
/// session's history after each of the first two.
struct Probe {
    seen: Seen,
    /// Screens user input, replacing "password", as an input guardrail would.
    screens: bool,
}

fn texts(ctx: &dyn InvocationContext) -> Vec<String> {
    ctx.session()
        .conversation_history()
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn model_event(ctx: &dyn InvocationContext, text: &str, partial: bool) -> Event {
    let mut event = Event::new(ctx.invocation_id());
    event.author = "probe".to_string();
    event.llm_response.content = Some(Content::new("model").with_text(text));
    event.llm_response.partial = partial;
    event
}

#[async_trait]
impl Agent for Probe {
    fn name(&self) -> &str {
        "probe"
    }

    fn description(&self) -> &str {
        "records the session view"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        if !self.screens {
            return Ok(None);
        }
        let mut screened = content.clone();
        for part in &mut screened.parts {
            if let Part::Text { text } = part {
                *text = text.replace("password", "[screened]");
            }
        }
        Ok(Some(screened))
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let seen = self.seen.clone();
        Ok(Box::pin(async_stream::stream! {
            yield Ok(model_event(ctx.as_ref(), "the secret is 42", false));
            seen.lock().unwrap().push(texts(ctx.as_ref()));
            yield Ok(model_event(ctx.as_ref(), "chunk", true));
            seen.lock().unwrap().push(texts(ctx.as_ref()));
            yield Ok(model_event(ctx.as_ref(), "done", false));
        }))
    }
}

async fn run_probe(refresh: bool, reads_allowed: usize) -> (Vec<Result<Event>>, Vec<Vec<String>>) {
    run_probe_with(refresh, reads_allowed, None).await
}

/// Runs the probe; `saved_message` is a user message saved before input screening existed.
async fn run_probe_with(
    refresh: bool,
    reads_allowed: usize,
    saved_message: Option<&str>,
) -> (Vec<Result<Event>>, Vec<Vec<String>>) {
    let service = Arc::new(RedactingService::new(reads_allowed));
    service
        .create(CreateRequest {
            app_name: "refresh".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    if let Some(text) = saved_message {
        let mut event = Event::new("earlier-invocation");
        event.author = "user".to_string();
        event.llm_response.content = Some(Content::new("user").with_text(text));
        service
            .inner
            .append_event_for_identity(AppendEventRequest {
                identity: adk_core::AdkIdentity::new(
                    adk_core::AppName::try_from("refresh").unwrap(),
                    UserId::new("user").unwrap(),
                    SessionId::new("session").unwrap(),
                ),
                event,
            })
            .await
            .unwrap();
    }
    let seen = Seen::default();
    let runner = Runner::builder()
        .app_name("refresh")
        .agent(Arc::new(Probe { seen: seen.clone(), screens: saved_message.is_some() })
            as Arc<dyn Agent>)
        .session_service(service)
        .build()
        .unwrap()
        .with_session_event_refresh(refresh);
    let stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("what is the secret?"),
        )
        .await
        .unwrap();
    let results = stream.collect::<Vec<_>>().await;
    let seen = seen.lock().unwrap().clone();
    (results, seen)
}

#[tokio::test]
async fn a_refreshed_run_sees_the_stored_view_and_every_fragment_still_streams() {
    let (results, seen) = run_probe(true, usize::MAX).await;

    let streamed: Vec<_> = results.into_iter().map(|result| result.unwrap()).collect();
    assert_eq!(streamed.len(), 3, "the partial chunk still reaches the caller");
    assert!(streamed[1].llm_response.partial);

    assert!(seen[0].contains(&"the [redacted] is 42".to_string()), "{:?}", seen[0]);
    assert!(seen[0].contains(&"what is the [redacted]?".to_string()), "{:?}", seen[0]);
    assert!(!seen[0].iter().any(|text| text.contains("secret")), "{:?}", seen[0]);
    assert!(!seen[1].iter().any(|text| text.contains("secret")), "{:?}", seen[1]);
}

#[tokio::test]
async fn without_refresh_the_run_keeps_its_own_copy() {
    let (results, seen) = run_probe(false, usize::MAX).await;

    assert!(results.iter().all(Result::is_ok));
    assert!(seen[0].contains(&"the secret is 42".to_string()), "{:?}", seen[0]);
}

#[tokio::test]
async fn a_failed_refresh_ends_the_run_instead_of_using_a_stale_view() {
    // The run's first read loads the session, the second refreshes after the user's message,
    // and the third, after the probe's first event, fails.
    let (results, seen) = run_probe(true, 2).await;

    assert_eq!(results.len(), 1, "nothing follows the failed refresh");
    let error = results.into_iter().next().unwrap().unwrap_err();
    assert!(error.to_string().contains("the store is unreachable"), "{error}");
    assert!(seen.is_empty(), "the agent never continues on the stale view");
}

#[tokio::test]
async fn a_refreshed_view_keeps_the_screened_content_of_a_saved_message() {
    let (results, seen) = run_probe_with(true, usize::MAX, Some("my password is hunter2")).await;

    assert!(results.iter().all(Result::is_ok));
    assert!(seen[0].contains(&"my [screened] is hunter2".to_string()), "{:?}", seen[0]);
    assert!(!seen[0].iter().any(|text| text.contains("password")), "{:?}", seen[0]);
}
