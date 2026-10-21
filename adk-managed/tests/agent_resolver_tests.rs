//! An `AgentResolver` builds a definition in place of `build_agent`, on `create` and when
//! another runtime instance rebuilds a stored session (spec 3.1 §7.18).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{Agent, Llm};
use adk_managed::resolver::ResolverResult;
use adk_managed::testing::{ScriptedLlm, ScriptedTurn};
use adk_managed::types::{
    ContentBlock, ManagedAgentDef, ModelRef, RuntimeError, SessionEvent, UserEvent,
};
use adk_managed::{
    AgentResolver, DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, ManagedStores,
    ModelResolver,
};
use adk_session::SessionService;
use async_trait::async_trait;
use futures::StreamExt;

/// Resolves every model reference to one scripted model.
struct Scripted(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for Scripted {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(self.0.clone() as Arc<dyn Llm>)
    }
}

/// Builds definitions whose metadata names `resolved`, counting each build.
#[derive(Default)]
struct Marked {
    builds: AtomicUsize,
}

#[async_trait]
impl AgentResolver for Marked {
    async fn resolve(
        &self,
        def: &ManagedAgentDef,
        models: &dyn ModelResolver,
    ) -> Result<Option<Arc<dyn Agent>>, RuntimeError> {
        let Some(kind) = def.metadata.as_ref().and_then(|metadata| metadata.get("kind")) else {
            return Ok(None);
        };
        if kind != "resolved" {
            return Err(RuntimeError::invalid_request(format!("unknown kind {kind}")));
        }
        self.builds.fetch_add(1, Ordering::SeqCst);
        let model = models
            .resolve(&def.model)
            .await
            .map_err(|error| RuntimeError::provider_error("scripted", error.to_string()))?;
        let agent = adk_agent::LlmAgentBuilder::new(format!("{}-resolved", def.name))
            .model(model)
            .build()
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        Ok(Some(Arc::new(agent)))
    }
}

fn def(kind: Option<&str>) -> ManagedAgentDef {
    let def = ManagedAgentDef::new("agent", ModelRef::Shorthand("gemini-3.7-flash".into()));
    match kind {
        Some(kind) => def.with_metadata([("kind".to_string(), kind.to_string())].into()),
        None => def,
    }
}

fn text(reply: &str) -> ScriptedTurn {
    ScriptedTurn { text: Some(reply.to_string()), tool_calls: vec![] }
}

fn message(text: &str) -> UserEvent {
    UserEvent::Message { content: vec![ContentBlock::Text { text: text.to_string() }] }
}

/// The text of the agent messages up to the next `status.idle`.
async fn turn(stream: &mut futures::stream::BoxStream<'static, SessionEvent>) -> Vec<String> {
    let mut texts = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no status.idle")
            .expect("stream ended");
        match event {
            SessionEvent::Message { content, .. } => {
                texts.extend(content.iter().filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                }))
            }
            SessionEvent::StatusIdle { .. } => return texts,
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_resolved_agent_runs_in_place_of_the_built_one_and_after_a_rebuild() {
    let llm = Arc::new(ScriptedLlm::new("scripted", vec![text("first"), text("second")]));
    let sessions = Arc::new(adk_session::InMemorySessionService::new());
    let stores = ManagedStores::in_memory();
    let resolver = Arc::new(Marked::default());
    let runtime = |resolver: Arc<Marked>| {
        DefaultManagedAgentRuntime::new(Arc::new(Scripted(llm.clone())), sessions.clone())
            .with_stores(stores.clone())
            .with_agent_resolver(resolver)
    };

    let first = runtime(resolver.clone());
    let agent = first.create(def(Some("resolved"))).await.unwrap();
    let owner = ManagedOwner::new("app", "ana").unwrap();
    let session = first.start_session(&agent, &owner, None).await.unwrap();
    let mut stream = first.stream_events(&session, None).await.unwrap();
    first.send_event(&session, message("hello")).await.unwrap();
    assert_eq!(turn(&mut stream).await, ["first"]);

    // A second instance over the same stores rebuilds the session through its resolver.
    let second = runtime(resolver.clone());
    let mut stream = second.stream_events(&session, None).await.unwrap();
    second.send_event(&session, message("again")).await.unwrap();
    assert_eq!(turn(&mut stream).await, ["second"]);
    assert_eq!(resolver.builds.load(Ordering::SeqCst), 2, "create and the rebuild");

    // The resolved agent, not `build_agent`'s, authored the conversation.
    let stored = sessions
        .get(adk_session::GetRequest {
            app_name: "app".into(),
            user_id: "ana".into(),
            session_id: session.0.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let authors: Vec<String> = stored
        .events()
        .all()
        .into_iter()
        .filter(|event| event.author != "user")
        .map(|event| event.author)
        .collect();
    assert_eq!(authors, ["agent-resolved", "agent-resolved"]);
}

#[tokio::test]
async fn a_definition_the_resolver_declines_is_built_as_before() {
    let llm = Arc::new(ScriptedLlm::new("scripted", vec![text("plain")]));
    let resolver = Arc::new(Marked::default());
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(Scripted(llm)),
        Arc::new(adk_session::InMemorySessionService::new()),
    )
    .with_agent_resolver(resolver.clone());
    let agent = runtime.create(def(None)).await.unwrap();
    let owner = ManagedOwner::new("app", "ana").unwrap();
    let session = runtime.start_session(&agent, &owner, None).await.unwrap();
    let mut stream = runtime.stream_events(&session, None).await.unwrap();
    runtime.send_event(&session, message("hello")).await.unwrap();
    assert_eq!(turn(&mut stream).await, ["plain"]);
    assert_eq!(resolver.builds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_resolver_error_fails_create() {
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(Scripted(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedTurn>::new())))),
        Arc::new(adk_session::InMemorySessionService::new()),
    )
    .with_agent_resolver(Arc::new(Marked::default()));
    let error = runtime.create(def(Some("unknown"))).await.unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidRequest { .. }), "{error:?}");
}
