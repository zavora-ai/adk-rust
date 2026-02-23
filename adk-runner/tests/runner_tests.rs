use adk_core::{Agent, Content, EventStream, InvocationContext, Part, Result};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{Event, Events, GetRequest, Session, SessionService, State};
use adk_skill::{SelectionPolicy, SkillInjector, SkillInjectorConfig};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use std::fs;
use std::sync::{Arc, Mutex};

// Mock Agent
struct MockAgent {
    name: String,
}

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Mock agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

// Mock Events
struct MockEvents {
    events: Vec<Event>,
}

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

// Mock State
struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

impl adk_session::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn set(&mut self, _key: String, _value: serde_json::Value) {}

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

// Mock Session
struct MockSession {
    id: String,
    app_name: String,
    user_id: String,
    events: MockEvents,
    state: MockState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn events(&self) -> &dyn Events {
        &self.events
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

// Mock SessionService
struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, _req: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!()
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: req.app_name,
            user_id: req.user_id,
            events: MockEvents { events: vec![] },
            state: MockState,
        }))
    }

    async fn list(&self, _req: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }

    async fn delete(&self, _req: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> Result<()> {
        Ok(())
    }
}

#[test]
fn test_runner_creation() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let session_service = Arc::new(MockSessionService);

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    });

    assert!(runner.is_ok());
}

#[tokio::test]
async fn test_runner_run() {
    let agent = Arc::new(MockAgent { name: "test_agent".to_string() });

    let session_service = Arc::new(MockSessionService);

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "Hello".to_string() }] };

    let result = runner.run("user123".to_string(), "session456".to_string(), content).await;

    assert!(result.is_ok());
}

#[test]
fn test_find_agent_in_tree() {
    let sub_agent: Arc<dyn Agent> = Arc::new(MockAgent { name: "sub_agent".to_string() });

    let root_agent: Arc<dyn Agent> = Arc::new(MockAgentWithSubs {
        name: "root".to_string(),
        sub_agents: vec![sub_agent.clone()],
    });

    // Find root
    let found = Runner::find_agent(&root_agent, "root");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name(), "root");

    // Find sub-agent
    let found = Runner::find_agent(&root_agent, "sub_agent");
    assert!(found.is_some());
    assert_eq!(found.unwrap().name(), "sub_agent");

    // Not found
    let found = Runner::find_agent(&root_agent, "nonexistent");
    assert!(found.is_none());
}

#[tokio::test]
async fn test_find_agent_to_run_with_history() {
    let sub_agent: Arc<dyn Agent> = Arc::new(MockAgent { name: "assistant".to_string() });

    let root_with_subs: Arc<dyn Agent> = Arc::new(MockAgentWithSubs {
        name: "root".to_string(),
        sub_agents: vec![sub_agent.clone()],
    });

    // Session with assistant event
    let mut events = vec![];
    let mut event = adk_session::Event::new("inv-1");
    event.author = "assistant".to_string();
    events.push(event);

    let session = MockSession {
        id: "session1".to_string(),
        app_name: "test".to_string(),
        user_id: "user1".to_string(),
        events: MockEvents { events },
        state: MockState,
    };

    let agent = Runner::find_agent_to_run(&root_with_subs, &session);
    assert_eq!(agent.name(), "assistant");
}

#[tokio::test]
async fn test_find_agent_to_run_defaults_to_root() {
    let root_agent: Arc<dyn Agent> = Arc::new(MockAgent { name: "root".to_string() });

    // Empty session
    let session = MockSession {
        id: "session1".to_string(),
        app_name: "test".to_string(),
        user_id: "user1".to_string(),
        events: MockEvents { events: vec![] },
        state: MockState,
    };

    let agent = Runner::find_agent_to_run(&root_agent, &session);
    assert_eq!(agent.name(), "root");
}

#[tokio::test]
async fn test_find_agent_to_run_skips_user_events() {
    let root_agent: Arc<dyn Agent> = Arc::new(MockAgent { name: "root".to_string() });

    // Session with only user events
    let mut events = vec![];
    let mut event = adk_session::Event::new("inv-1");
    event.author = "user".to_string();
    events.push(event);

    let session = MockSession {
        id: "session1".to_string(),
        app_name: "test".to_string(),
        user_id: "user1".to_string(),
        events: MockEvents { events },
        state: MockState,
    };

    let agent = Runner::find_agent_to_run(&root_agent, &session);
    assert_eq!(agent.name(), "root");
}

// Mock agent with sub-agents
struct MockAgentWithSubs {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

#[async_trait]
impl Agent for MockAgentWithSubs {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Mock agent with subs"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct EchoUserContentAgent;

#[async_trait]
impl Agent for EchoUserContentAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes current user content"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let input_text = ctx
            .user_content()
            .parts
            .iter()
            .find_map(|p| match p {
                Part::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let mut event = Event::new(ctx.invocation_id());
        event.author = "echo".to_string();
        event.llm_response.content =
            Some(Content::new("model").with_text(format!("agent-saw:{input_text}")));

        let s = futures::stream::iter(vec![Ok(event)]);
        Ok(Box::pin(s))
    }
}

#[tokio::test]
async fn test_plugin_callback_order_and_mutation() {
    let call_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let before_order = call_order.clone();
    let on_user_order = call_order.clone();
    let on_event_order = call_order.clone();
    let after_order = call_order.clone();

    let plugin = Plugin::new(PluginConfig {
        name: "test-plugin".to_string(),
        before_run: Some(Box::new(move |_ctx| {
            let before_order = before_order.clone();
            Box::pin(async move {
                before_order.lock().unwrap().push("before_run".to_string());
                Ok(None)
            })
        })),
        on_user_message: Some(Box::new(move |_ctx, mut content| {
            let on_user_order = on_user_order.clone();
            Box::pin(async move {
                on_user_order.lock().unwrap().push("on_user_message".to_string());
                if let Some(Part::Text { text }) = content.parts.first_mut() {
                    *text = format!("{text} [plugin]");
                }
                Ok(Some(content))
            })
        })),
        on_event: Some(Box::new(move |_ctx, mut event| {
            let on_event_order = on_event_order.clone();
            Box::pin(async move {
                on_event_order.lock().unwrap().push("on_event".to_string());
                if let Some(content) = &mut event.llm_response.content {
                    content.parts.push(Part::Text { text: "[event-mutated]".to_string() });
                }
                Ok(Some(event))
            })
        })),
        after_run: Some(Box::new(move |_ctx| {
            let after_order = after_order.clone();
            Box::pin(async move {
                after_order.lock().unwrap().push("after_run".to_string());
            })
        })),
        ..Default::default()
    });

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent: Arc::new(EchoUserContentAgent),
        session_service: Arc::new(MockSessionService),
        artifact_service: None,
        memory_service: None,
        plugin_manager: Some(Arc::new(PluginManager::new(vec![plugin]))),
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let content = Content::new("user").with_text("hello");
    let mut stream =
        runner.run("user123".to_string(), "session456".to_string(), content).await.unwrap();

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }

    assert_eq!(
        call_order.lock().unwrap().clone(),
        vec!["before_run", "on_user_message", "on_event", "after_run"]
    );

    assert_eq!(events.len(), 1);
    let text_parts: Vec<String> = events[0]
        .llm_response
        .content
        .as_ref()
        .unwrap()
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();

    assert!(text_parts.iter().any(|t| t.contains("agent-saw:hello [plugin]")));
    assert!(text_parts.iter().any(|t| t == "[event-mutated]"));
}

#[tokio::test]
async fn test_plugin_error_propagates_from_on_user_message() {
    let plugin = Plugin::new(PluginConfig {
        name: "failing-plugin".to_string(),
        on_user_message: Some(Box::new(|_ctx, _content| {
            Box::pin(async move { Err(adk_core::AdkError::Agent("boom".to_string())) })
        })),
        ..Default::default()
    });

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent: Arc::new(EchoUserContentAgent),
        session_service: Arc::new(MockSessionService),
        artifact_service: None,
        memory_service: None,
        plugin_manager: Some(Arc::new(PluginManager::new(vec![plugin]))),
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let mut stream = runner
        .run(
            "user123".to_string(),
            "session456".to_string(),
            Content::new("user").with_text("hello"),
        )
        .await
        .unwrap();

    let first = stream.next().await.expect("expected stream item");
    assert!(first.is_err());
}

#[tokio::test]
async fn test_skill_injector_plugin_mutates_user_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join(".skills")).unwrap();
    fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: search repository code\ntags: [search, code]\n---\nUse `rg --files` then `rg <pattern>`.\n",
    )
    .unwrap();

    let injector = SkillInjector::from_root(
        root,
        SkillInjectorConfig {
            policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() },
            max_injected_chars: 500,
        },
    )
    .unwrap();

    let plugin_manager = Arc::new(injector.build_plugin_manager("skills"));
    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent: Arc::new(EchoUserContentAgent),
        session_service: Arc::new(MockSessionService),
        artifact_service: None,
        memory_service: None,
        plugin_manager: Some(plugin_manager),
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap();

    let mut stream = runner
        .run(
            "user123".to_string(),
            "session456".to_string(),
            Content::new("user").with_text("Please search this repository quickly"),
        )
        .await
        .unwrap();

    let first = stream.next().await.unwrap().unwrap();
    let text = first
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("agent-saw:[skill:search]"));
    assert!(text.contains("Use `rg --files` then `rg <pattern>`."));
}

#[tokio::test]
async fn test_runner_with_auto_skills_mutates_user_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join(".skills")).unwrap();
    fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: search repository code\ntags: [search, code]\n---\nUse `rg` first.\n",
    )
    .unwrap();

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent: Arc::new(EchoUserContentAgent),
        session_service: Arc::new(MockSessionService),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })
    .unwrap()
    .with_auto_skills(
        root,
        SkillInjectorConfig {
            policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() },
            max_injected_chars: 500,
        },
    )
    .unwrap();

    let mut stream = runner
        .run(
            "user123".to_string(),
            "session456".to_string(),
            Content::new("user").with_text("Please search this repository quickly"),
        )
        .await
        .unwrap();

    let first = stream.next().await.unwrap().unwrap();
    let text = first
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("agent-saw:[skill:search]"));
    assert!(text.contains("Use `rg` first."));
}
