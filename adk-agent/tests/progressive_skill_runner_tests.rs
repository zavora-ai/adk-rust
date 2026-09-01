//! Runner-level regression coverage for progressive skill disclosure.
//!
//! This exercises the production state path: `load_skill` emits an event
//! delta, the runner persists it into the mutable session, and the next model
//! turn receives the skill's newly activated business tool declaration.
#![cfg(feature = "skills-progressive-disclosure")]

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result,
    SessionId, Tool, ToolContext, ToolRegistry, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_skill::{SkillToolset, SkillToolsetConfig, load_skill_index};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

struct ScriptedModel {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self { requests: Arc::new(Mutex::new(Vec::new())), responses: Mutex::new(responses.into()) }
    }

    fn function_call(name: &str, args: Value) -> LlmResponse {
        Self::response(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args,
                id: Some(format!("call-{name}")),
                thought_signature: None,
            }],
        })
    }

    fn text(text: &str) -> LlmResponse {
        Self::response(Content::new("model").with_text(text))
    }

    fn response(content: Content) -> LlmResponse {
        LlmResponse {
            content: Some(content),
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
        }
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        self.requests.lock().expect("requests lock").push(request);
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .unwrap_or_else(|| Self::text("done"));
        Ok(Box::pin(async_stream::stream! {
            yield Ok(response);
        }))
    }
}

struct WeatherTool;

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str {
        "weather_lookup"
    }

    fn description(&self) -> &str {
        "Looks up the weather."
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(json!({ "temperatureC": 22 }))
    }
}

struct WeatherRegistry;

impl ToolRegistry for WeatherRegistry {
    fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        (name == "weather_lookup").then(|| Arc::new(WeatherTool) as Arc<dyn Tool>)
    }
}

#[tokio::test]
async fn runner_persists_skill_activation_before_the_next_model_turn() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skill_dir = temp.path().join(".skills/weather");
    std::fs::create_dir_all(&skill_dir).expect("skill directory");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: weather\ndescription: Answer weather questions\nallowed-tools: [weather_lookup]\n---\nUse weather_lookup for weather questions.\n",
    )
    .expect("skill file");

    let model = Arc::new(ScriptedModel::new(vec![
        ScriptedModel::function_call("load_skill", json!({ "skill_name": "weather" })),
        ScriptedModel::function_call("weather_lookup", json!({ "city": "Shanghai" })),
        ScriptedModel::text("It is 22°C in Shanghai."),
    ]));
    let toolset = SkillToolset::new(
        Arc::new(load_skill_index(temp.path()).expect("skill index")),
        Arc::new(WeatherRegistry),
        SkillToolsetConfig::default(),
    );
    let agent = LlmAgentBuilder::new("weather-agent")
        .model(model.clone())
        .toolset(Arc::new(toolset))
        .build()
        .expect("agent builds");
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "progressive-test".to_string(),
            user_id: "user-1".to_string(),
            session_id: Some("session-1".to_string()),
            state: HashMap::new(),
        })
        .await
        .expect("session creates");
    let runner = Runner::builder()
        .app_name("progressive-test")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .expect("runner builds");

    let mut stream = runner
        .run(
            UserId::new("user-1").expect("user ID"),
            SessionId::new("session-1").expect("session ID"),
            Content::new("user").with_text("What is the weather in Shanghai?"),
        )
        .await
        .expect("runner starts");
    while let Some(event) = stream.next().await {
        event.expect("runner event");
    }

    {
        let requests = model.requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 3);
        assert!(!requests[0].tools.contains_key("weather_lookup"));
        assert!(requests[1].tools.contains_key("weather_lookup"));
    }

    let session = sessions
        .get(GetRequest {
            app_name: "progressive-test".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("session loads");
    let key = SkillToolset::activation_state_key("weather-agent");
    let activations = session.state().get(&key).expect("activation state persists");
    assert_eq!(activations[0]["name"], "weather");
}
