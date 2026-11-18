//! An agent with `load_memory` never recalls an expired memory and still recalls a live one.
//!
//! The scripted test runs everywhere. The live test runs an `LlmAgent` on Anthropic
//! `claude-haiku-5-5`: it recalls a memory before its TTL passes, cannot recall it after, and
//! recalls a memory with a longer TTL. It needs `ANTHROPIC_API_KEY`:
//!
//! ```bash
//! cargo nextest run -p adk-tool --features memory-tools --test memory_expiry_tests \
//!   --run-ignored only --no-capture
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Llm, LlmRequest, LlmResponse, Part, SessionId, UserId};
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::memory::LoadMemoryTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "memory-expiry";
const USER: &str = "ada";
const WIFI: &str = "The beach house wifi password is coral-7731.";
const LOCKER: &str = "The gym locker code is 4417.";

/// Replies with each scripted turn in order, then with text, recording every request.
struct ScriptedLlm {
    turns: Mutex<VecDeque<Vec<Part>>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

#[async_trait]
impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        "scripted-llm"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        self.requests.lock().unwrap().push(request);
        let parts = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![Part::Text { text: "done".to_string() }]);
        let response = LlmResponse::new(Content { role: "model".to_string(), parts });
        Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
    }
}

/// A runner over `model` whose agent searches `memory` with `load_memory`, and the text of its
/// answer to `question` in a new session.
async fn ask(
    model: Arc<dyn Llm>,
    memory: Arc<dyn MemoryService>,
    session: &str,
    question: &str,
) -> (String, Vec<String>) {
    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .instruction(
            "You answer questions about the user's own details. Always call load_memory with a \
             short keyword query before answering, and answer only from what it returns. When \
             it returns nothing relevant, reply with exactly UNKNOWN.",
        )
        .tool(Arc::new(LoadMemoryTool::builder().memory_service(memory).build().unwrap()))
        .max_iterations(6)
        .build()
        .unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id: Some(session.into()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            UserId::new(USER).unwrap(),
            SessionId::new(session).unwrap(),
            Content::new("user").with_text(question),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    let mut recalled = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.expect("no run error");
        if event.llm_response.partial {
            continue;
        }
        for part in event.content().map(|content| content.parts.clone()).unwrap_or_default() {
            match part {
                Part::Text { text } if !text.trim().is_empty() => answer = text,
                Part::FunctionResponse { function_response, .. } => {
                    recalled.push(function_response.response.to_string());
                }
                _ => {}
            }
        }
    }
    (answer, recalled)
}

async fn memory_with_wifi_expiring_in(ttl: Duration) -> Arc<dyn MemoryService> {
    let memory = Arc::new(InMemoryMemoryService::new().with_default_ttl(ttl));
    memory
        .add_session(
            APP,
            USER,
            "earlier",
            vec![
                MemoryEntry::new(Content::new("user").with_text(WIFI), "user"),
                MemoryEntry::new(Content::new("user").with_text(LOCKER), "user")
                    .with_ttl(Duration::from_secs(3600)),
            ],
        )
        .await
        .unwrap();
    memory
}

#[tokio::test]
async fn load_memory_returns_the_live_memory_and_not_the_expired_one() {
    let memory = memory_with_wifi_expiring_in(Duration::from_millis(200)).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let call = vec![Part::FunctionCall {
        name: "load_memory".into(),
        args: json!({ "query": "wifi password gym locker code" }),
        id: Some("call_1".into()),
        thought_signature: None,
    }];
    let model = ScriptedLlm { turns: Mutex::new(VecDeque::from([call])), requests: Arc::default() };

    let (_, recalled) = ask(Arc::new(model), memory, "s1", "What are my codes?").await;

    assert_eq!(recalled.len(), 1, "one load_memory call");
    assert!(recalled[0].contains("4417"), "the live memory is recalled: {}", recalled[0]);
    assert!(!recalled[0].contains("coral-7731"), "the expired one is not: {}", recalled[0]);
}

#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn a_live_agent_stops_recalling_a_memory_when_it_expires() {
    dotenvy::dotenv().ok();
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model: Arc<dyn Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(
            adk_model::anthropic::AnthropicConfig::new(key, "claude-haiku-5-5")
                .with_max_tokens(512),
        )
        .unwrap(),
    );
    let ttl = Duration::from_secs(30);
    let written = std::time::Instant::now();
    let memory = memory_with_wifi_expiring_in(ttl).await;

    let question = "What is the wifi password at the beach house?";
    let (before, recalled_before) = ask(model.clone(), memory.clone(), "before", question).await;
    println!("before expiry ({:?} after the write): {before}", written.elapsed());
    println!("  load_memory returned: {recalled_before:?}");
    assert!(written.elapsed() < ttl, "the first question finished before the TTL passed");
    assert!(before.contains("coral-7731"), "a live memory is recalled: {before}");

    tokio::time::sleep(ttl.saturating_sub(written.elapsed()) + Duration::from_secs(1)).await;
    let (after, recalled_after) = ask(model.clone(), memory.clone(), "after", question).await;
    println!("after expiry ({:?} after the write): {after}", written.elapsed());
    println!("  load_memory returned: {recalled_after:?}");
    assert!(!after.contains("coral-7731"), "an expired memory is not recalled: {after}");
    assert!(
        recalled_after.iter().all(|response| !response.contains("coral-7731")),
        "load_memory never returned the expired memory"
    );

    let (locker, recalled_locker) =
        ask(model, memory.clone(), "locker", "What is my gym locker code?").await;
    println!("fresh memory: {locker}");
    println!("  load_memory returned: {recalled_locker:?}");
    assert!(locker.contains("4417"), "a memory with a longer TTL is still recalled: {locker}");

    assert_eq!(memory.purge_expired().await.unwrap(), 1, "the purge removes the expired memory");
}
