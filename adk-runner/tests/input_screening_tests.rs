//! The runner screens a user message with its agent tree before saving it, marks what it
//! saved, and screens a saved message that carries no marker before the agent runs.

use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, Event, EventStream, InvocationContext,
    Result, SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Records the user message and history it runs with; screens input when `screens`.
struct Recorder {
    name: &'static str,
    screens: bool,
    children: Vec<Arc<dyn Agent>>,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Agent for Recorder {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "records what it sees"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.children
    }

    async fn screen_user_input(&self, content: &Content) -> Result<Option<Content>> {
        let own = if self.screens {
            let text: String = content.parts.iter().filter_map(|part| part.text()).collect();
            if text.contains("blocked") {
                return Err(AdkError::new(
                    ErrorComponent::Agent,
                    ErrorCategory::InvalidInput,
                    "test.blocked",
                    "blocked by the test screen",
                ));
            }
            Some(Content::new(content.role.clone()).with_text(text.replace("secret", "[R]")))
        } else {
            None
        };
        let below =
            adk_core::screen_user_input_with(&self.children, own.as_ref().unwrap_or(content))
                .await?;
        Ok(below.or(own))
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let history = serde_json::to_string(&ctx.session().conversation_history()).unwrap();
        let current: String =
            ctx.user_content().parts.iter().filter_map(|part| part.text()).collect();
        self.seen.lock().unwrap().push(format!("{current} | {history}"));
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn leaf(name: &'static str, screens: bool, seen: &Arc<Mutex<Vec<String>>>) -> Arc<dyn Agent> {
    Arc::new(Recorder { name, screens, children: Vec::new(), seen: seen.clone() })
}

async fn runner(agent: Arc<dyn Agent>) -> (Runner, Arc<InMemorySessionService>) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("app")
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    (runner, sessions)
}

async fn run(runner: &Runner, text: &str) -> Vec<Result<Event>> {
    runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .unwrap()
        .collect()
        .await
}

async fn saved(sessions: &InMemorySessionService) -> Vec<Event> {
    sessions
        .get(GetRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .events()
        .all()
}

fn text(event: &Event) -> String {
    event
        .llm_response
        .content
        .iter()
        .flat_map(|content| &content.parts)
        .filter_map(|part| part.text())
        .collect()
}

#[tokio::test]
async fn the_screened_message_is_saved_with_the_marker_and_handed_to_the_agent() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (runner, sessions) = runner(leaf("root", true, &seen)).await;

    assert!(run(&runner, "my secret is 42").await.iter().all(Result::is_ok));

    let events = saved(&sessions).await;
    assert_eq!(events.len(), 1);
    assert_eq!(text(&events[0]), "my [R] is 42");
    assert!(events[0].actions.input_screened);
    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("my [R] is 42 |"), "{seen:?}");
    assert!(!seen[0].contains("secret"), "{seen:?}");
}

#[tokio::test]
async fn a_tree_without_input_screens_saves_the_message_unmarked() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (runner, sessions) = runner(leaf("root", false, &seen)).await;

    assert!(run(&runner, "my secret is 42").await.iter().all(Result::is_ok));

    let events = saved(&sessions).await;
    assert_eq!(text(&events[0]), "my secret is 42");
    assert!(!events[0].actions.input_screened);
}

#[tokio::test]
async fn a_blocked_message_is_not_saved_and_no_agent_runs() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (runner, sessions) = runner(leaf("root", true, &seen)).await;

    let events = run(&runner, "this is blocked").await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].as_ref().err().map(|error| error.code), Some("test.blocked"));
    assert!(saved(&sessions).await.is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_sub_agents_screen_applies_to_the_message_the_root_saves() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let root = Arc::new(Recorder {
        name: "root",
        screens: false,
        children: vec![leaf("child", true, &seen)],
        seen: seen.clone(),
    });
    let (runner, sessions) = runner(root).await;

    assert!(run(&runner, "my secret is 42").await.iter().all(Result::is_ok));

    let events = saved(&sessions).await;
    assert_eq!(text(&events[0]), "my [R] is 42");
    assert!(events[0].actions.input_screened);
}

#[tokio::test]
async fn an_unmarked_saved_message_is_screened_in_the_runs_view_only() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (runner, sessions) = runner(leaf("root", true, &seen)).await;
    let mut earlier = Event::new("inv-earlier");
    earlier.author = "user".to_string();
    earlier.llm_response.content = Some(Content::new("user").with_text("old secret"));
    sessions.append_event("session", earlier).await.unwrap();

    assert!(run(&runner, "hello").await.iter().all(Result::is_ok));

    let seen = seen.lock().unwrap().clone();
    assert!(seen[0].contains("old [R]"), "{seen:?}");
    assert!(!seen[0].contains("secret"), "{seen:?}");
    // The store keeps what was saved; every run screens it again.
    assert_eq!(text(&saved(&sessions).await[0]), "old secret");
}
