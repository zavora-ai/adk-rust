//! An archived session takes no turn, and `Runner::prewarm` readies the agent tree and the
//! session under the kill switch and a budget.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    AdkIdentity, Agent, AppName, Content, ErrorCategory, EventStream, InvocationContext, Result,
    SessionId, UserId,
};
use adk_runner::{Prewarm, Runner};
use adk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

const APP: &str = "app";
const USER: &str = "ana";
const SESSION: &str = "trip";

/// Answers "done", and counts its runs and prewarms; a slow one takes a minute to prewarm.
struct Counting {
    sub_agents: Vec<Arc<dyn Agent>>,
    runs: Arc<AtomicUsize>,
    prewarms: Arc<AtomicUsize>,
    slow: bool,
}

impl Counting {
    fn new() -> Self {
        Self { sub_agents: Vec::new(), runs: Arc::default(), prewarms: Arc::default(), slow: false }
    }
}

#[async_trait]
impl Agent for Counting {
    fn name(&self) -> &str {
        "counting"
    }

    fn description(&self) -> &str {
        "counts"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn prewarm(&self) -> Result<()> {
        self.prewarms.fetch_add(1, Ordering::SeqCst);
        if self.slow {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        for agent in &self.sub_agents {
            agent.prewarm().await?;
        }
        Ok(())
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let mut event = Event::new(ctx.invocation_id());
        event.author = "counting".to_string();
        event.llm_response.content = Some(Content::new("model").with_text("done"));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

async fn world(agent: Counting) -> (Runner, Arc<InMemorySessionService>, AdkIdentity) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(SESSION.into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions.clone())
        .build()
        .unwrap();
    let identity = AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(USER).unwrap(),
        SessionId::try_from(SESSION).unwrap(),
    );
    (runner, sessions, identity)
}

async fn turn(runner: &Runner, text: &str) -> Vec<Result<Event>> {
    runner
        .run(
            UserId::new(USER).unwrap(),
            SessionId::new(SESSION).unwrap(),
            Content::new("user").with_text(text),
        )
        .await
        .unwrap()
        .collect()
        .await
}

#[tokio::test]
async fn an_archived_session_takes_no_turn_until_unarchived() {
    let agent = Counting::new();
    let runs = agent.runs.clone();
    let (runner, sessions, identity) = world(agent).await;
    assert!(turn(&runner, "hello").await.iter().all(Result::is_ok));
    let before = sessions.get_for_identity(&identity).await.unwrap().events().len();

    sessions.archive(&identity, Some("done".into())).await.unwrap();
    let refused = turn(&runner, "one more thing").await;
    let error = refused.into_iter().find_map(Result::err).expect("the turn is refused");
    assert_eq!((error.code, error.category), ("session.archived", ErrorCategory::Conflict));
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the agent did not run");
    let after = sessions.get_for_identity(&identity).await.unwrap().events().len();
    assert_eq!(after, before + 1, "only the archive marker was added, not the message");
    let error = runner.recover_unanswered_calls(USER, SESSION).await.unwrap_err();
    assert_eq!(error.code, "session.archived");

    sessions.unarchive(&identity).await.unwrap();
    assert!(turn(&runner, "one more thing").await.iter().all(Result::is_ok));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn prewarm_readies_the_agent_tree_and_reads_the_session() {
    let mut root = Counting::new();
    let child = Counting::new();
    let child_prewarms = child.prewarms.clone();
    root.sub_agents.push(Arc::new(child));
    let prewarms = root.prewarms.clone();
    let runs = root.runs.clone();
    let (runner, sessions, identity) = world(root).await;

    let report = runner.prewarm(Prewarm::new()).await.unwrap();
    assert!(!report.session_read);
    assert_eq!((prewarms.load(Ordering::SeqCst), child_prewarms.load(Ordering::SeqCst)), (1, 1));
    let report = runner
        .prewarm(
            Prewarm::new().session(UserId::new(USER).unwrap(), SessionId::new(SESSION).unwrap()),
        )
        .await
        .unwrap();
    assert!(report.session_read);
    assert!(report.warm_until > chrono::Utc::now());
    assert_eq!(runs.load(Ordering::SeqCst), 0, "a prewarm runs no turn");
    assert_eq!(sessions.get_for_identity(&identity).await.unwrap().events().len(), 0);

    // A missing or archived session is refused before anyone types.
    let missing = runner
        .prewarm(
            Prewarm::new().session(UserId::new(USER).unwrap(), SessionId::new("nope").unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(missing.category, ErrorCategory::NotFound);
    sessions.archive(&identity, None).await.unwrap();
    let archived = runner
        .prewarm(
            Prewarm::new().session(UserId::new(USER).unwrap(), SessionId::new(SESSION).unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(archived.code, "session.archived");
}

#[tokio::test]
async fn prewarm_respects_the_kill_switch_and_its_budget() {
    let (runner, _sessions, _identity) = world(Counting::new()).await;
    runner.governance().freeze("incident 7");
    let frozen = runner.prewarm(Prewarm::new()).await.unwrap_err();
    assert_eq!(frozen.code, adk_core::GOVERNANCE_FROZEN_CODE);

    let slow = Counting { slow: true, ..Counting::new() };
    let (runner, _sessions, _identity) = world(slow).await;
    let started = std::time::Instant::now();
    let error = runner.prewarm(Prewarm::new().budget(Duration::from_millis(50))).await.unwrap_err();
    assert_eq!((error.code, error.category), ("runner.prewarm.timeout", ErrorCategory::Timeout));
    assert!(started.elapsed() < Duration::from_secs(5));
}
