//! `PreloadMemoryTool`'s before-model callback beside an `LlmAgent` history processor, through
//! a `Runner`: the memories reach every request once, however the history is compacted.

use adk_agent::LlmAgentBuilder;
use adk_core::history::balanced_content_cut;
use adk_core::{Agent, Content, LlmRequest, LlmResponse, Part, SessionId, ToolContext, UserId};
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use adk_tool::memory::PreloadMemoryTool;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Replies with each scripted turn in order, then with text, recording every request.
struct ScriptedLlm {
    turns: Mutex<VecDeque<Vec<Part>>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

#[async_trait]
impl adk_core::Llm for ScriptedLlm {
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

#[tokio::test]
async fn preloaded_memories_reach_each_request_once_under_a_history_processor() {
    let memory = Arc::new(InMemoryMemoryService::new());
    memory
        .add_session(
            "rows",
            "user",
            "earlier",
            vec![MemoryEntry {
                content: Content::new("user").with_text("rows are counted per page"),
                author: "user".into(),
                timestamp: chrono::Utc::now(),
            }],
        )
        .await
        .unwrap();
    let preload = PreloadMemoryTool::builder().memory_service(memory).build().unwrap();

    let turns = (0..4)
        .map(|page| {
            vec![Part::FunctionCall {
                name: "lookup".into(),
                args: json!({ "page": page }),
                id: Some(format!("call_{page}")),
                thought_signature: None,
            }]
        })
        .collect();
    let model = ScriptedLlm { turns: Mutex::new(turns), requests: Arc::default() };
    let requests = model.requests.clone();
    let agent = LlmAgentBuilder::new("counter")
        .model(Arc::new(model))
        .instruction("You count rows.")
        .tool(Arc::new(FunctionTool::new(
            "lookup",
            "Looks up one page of rows",
            |_ctx: Arc<dyn ToolContext>, _args: Value| async move { Ok(json!({ "rows": 3 })) },
        )))
        .before_model_callback(preload.into_before_model_callback())
        // Past four turns, keeps the last tool step behind a summary of the request.
        .history_processor(Box::new(|_ctx, history: Vec<Content>| {
            Box::pin(async move {
                let (system, turns): (Vec<_>, Vec<_>) =
                    history.into_iter().partition(|content| content.role == "system");
                if turns.len() <= 4 {
                    return Ok(system.into_iter().chain(turns).collect());
                }
                let cut = balanced_content_cut(&turns, turns.len() - 2);
                let summary = Content::new("user").with_text("summary: count the rows per page");
                Ok(system.into_iter().chain([summary]).chain(turns.into_iter().skip(cut)).collect())
            })
        }))
        .build()
        .unwrap();

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "rows".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: std::collections::HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("rows")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions)
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("count the rows on every page"),
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.unwrap();
    }

    // Memory blocks and contents per request: the history stays at the instruction, a
    // summary and one tool step, and each request carries one block of memories.
    let per_request = requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            let blocks = request
                .contents
                .iter()
                .flat_map(|content| content.parts.iter().filter_map(Part::text))
                .map(|text| text.matches("--- Relevant Memories ---").count())
                .sum::<usize>();
            (blocks, request.contents.len())
        })
        .collect::<Vec<_>>();
    assert_eq!(per_request, vec![(1, 2), (1, 4), (1, 4), (1, 4), (1, 4)]);
}
